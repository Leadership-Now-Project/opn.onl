//! Secure View: the three calls behind a file link that opens a viewer.
//!
//! * `PUT /links/{id}/secure-view` — the link's editors switch it on or off.
//! * `POST /{code}/view` — the viewer trades the ticket `GET /{code}` gave the
//!   visitor, and an email address, for a view token. This is the visit: the
//!   click is counted here and a one-time or capped link is used up here.
//! * `GET /{code}/view/file` — the bytes, streamed through this process to a
//!   caller holding the view token in a header.
//!
//! The two public calls say nothing to a caller without a valid ticket or
//! token: a code that does not exist, a deleted link, a link that is not
//! Secure View and a bad ticket all get the same 401. No answer here, and no
//! answer of `GET /{code}` for such a link, contains an address of the bucket
//! (see `utils/secure_view.rs`).

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::entity::{link_files, links};
use crate::handlers::json_error;
use crate::handlers::link_access::{LinkAction, live_link_for};
use crate::handlers::links::{
    ClickAccounting, consume_capped_click, get_user_id_from_header, link_response,
    record_click_buffered,
};
use crate::utils::files::FileSharing;
use crate::utils::link_owner::link_owner_is_active;
use crate::utils::secure_view::{
    VIEW_TOKEN_TTL_SECONDS, clean_viewer_email, create_view_token, eligible, open_for_viewer,
    remove_protected_copy, spawn_protected_copy, validate_view_ticket, validate_view_token,
    viewer_kind,
};
use crate::utils::time::utc_rfc3339;

/// Why Secure View cannot be switched on for a file. Also shown by the
/// dashboard next to the greyed-out switch.
pub const NOT_ELIGIBLE: &str = "Secure View works for PDF, PNG, JPEG, GIF and WebP files only";

#[derive(Deserialize, ToSchema)]
pub struct SetSecureViewRequest {
    pub enabled: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct OpenViewRequest {
    /// The ticket from the address `GET /{code}` redirected to.
    pub ticket: String,
    /// The visitor's email address. Not verified. It is drawn on the document
    /// and shown to whoever shared the link.
    pub email: String,
}

#[derive(Serialize, ToSchema)]
pub struct ViewFile {
    /// `pdf` or `image`.
    pub kind: String,
    pub content_type: String,
    pub filename: String,
    pub size_bytes: i64,
}

#[derive(Serialize, ToSchema)]
pub struct ViewWatermark {
    /// The email as it was stored: trimmed and lower case.
    pub email: String,
    /// When the view was opened, RFC 3339 UTC.
    pub at: String,
}

#[derive(Serialize, ToSchema)]
pub struct OpenViewResponse {
    /// Send it as `X-View-Token` to `GET /{code}/view/file`.
    pub view_token: String,
    /// Seconds until `view_token` stops working.
    pub expires_in: i64,
    /// The link's title, or the file's name when it has none.
    pub title: String,
    pub file: ViewFile,
    pub watermark: ViewWatermark,
}

/// The one answer for a caller who has not shown a valid ticket or token,
/// whatever the reason: no such code, a deleted link, a link that is not
/// Secure View, a ticket for another link, an expired one.
fn session_expired() -> Response {
    let mut response = json_error(
        StatusCode::UNAUTHORIZED,
        "This viewing session has expired. Open the link again.",
    );
    no_store(&mut response);
    response
}

fn gone(reason: &str) -> Response {
    let mut response = json_error(StatusCode::GONE, reason);
    no_store(&mut response);
    response
}

fn no_store(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

fn unavailable() -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "The link could not be checked. Try again in a moment.",
    )
}

/// The live link with this code and the file it serves through Secure View.
/// `Ok(None)` for everything a caller without a token must not tell apart.
async fn secure_link(
    state: &AppState,
    code: &str,
) -> Result<Option<(links::Model, link_files::Model, FileSharing)>, ()> {
    let Some(files) = &state.files else {
        return Ok(None);
    };
    let link = links::Entity::find()
        .filter(links::Column::Code.eq(code))
        .filter(links::Column::DeletedAt.is_null())
        .one(&state.db)
        .await
        .map_err(|error| tracing::error!("Failed to load a link for the viewer: {error}"))?;
    let Some(link) = link.filter(links::Model::is_file) else {
        return Ok(None);
    };
    let file = link_files::Entity::find()
        .filter(link_files::Column::LinkId.eq(link.id))
        .filter(link_files::Column::Status.eq(link_files::STATUS_READY))
        .filter(link_files::Column::DeletedAt.is_null())
        .filter(link_files::Column::SecureView.eq(true))
        .one(&state.db)
        .await
        .map_err(|error| tracing::error!("Failed to load a file for the viewer: {error}"))?;
    Ok(file.map(|file| (link, file, files.clone())))
}

/// Switch Secure View on or off for a file link
///
/// With it on, opening the short link shows the file in a viewer that first
/// asks for an email address, instead of handing over the file. Only for a
/// file a browser can show in place (PDF, PNG, JPEG, GIF, WebP).
#[utoipa::path(
    put,
    path = "/links/{id}/secure-view",
    params(("id" = i32, Path, description = "Link ID")),
    request_body = SetSecureViewRequest,
    responses(
        (status = 200, description = "The link as it is now", body = LinkResponse),
        (status = 400, description = "Not a file link, or a file type Secure View cannot show"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden, also for a link that does not exist"),
        (status = 404, description = "File sharing is not set up on this instance"),
        (status = 410, description = "The link's file is no longer available"),
    ),
    tag = "Files",
    security(("bearer_auth" = []), ("api_key" = []))
)]
pub async fn set_secure_view(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    headers: HeaderMap,
    Json(payload): Json<SetSecureViewRequest>,
) -> Response {
    let Some(user_id) = get_user_id_from_header(&state.db, &headers).await else {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    };
    let link = match live_link_for(&state.db, id, user_id, LinkAction::Write).await {
        Ok(link) => link,
        Err(denied) => return denied.into_response(),
    };
    if !link.is_file() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Secure View is only for links that share an uploaded file",
        );
    }
    let Some(files) = &state.files else {
        return json_error(
            StatusCode::NOT_FOUND,
            "File sharing is not set up on this server",
        );
    };
    let file = link_files::Entity::find()
        .filter(link_files::Column::LinkId.eq(link.id))
        .filter(link_files::Column::Status.eq(link_files::STATUS_READY))
        .filter(link_files::Column::DeletedAt.is_null())
        .one(&state.db)
        .await;
    let file = match file {
        Ok(Some(file)) => file,
        Ok(None) => return json_error(StatusCode::GONE, "This file is no longer available"),
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
    };
    if payload.enabled && !eligible(&file) {
        return json_error(StatusCode::BAD_REQUEST, NOT_ELIGIBLE);
    }

    if file.secure_view != payload.enabled {
        let mut changed: link_files::ActiveModel = file.clone().into();
        changed.secure_view = Set(payload.enabled);
        let file = match changed.update(&state.db).await {
            Ok(file) => file,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        };
        if payload.enabled {
            spawn_protected_copy(&state, &file);
        } else {
            // Switched off: the link hands the original over again, so the
            // copy has no use and does not stay in the bucket.
            remove_protected_copy(&state.db, files.store.as_ref(), &file).await;
        }
    }

    match link_response(&state.db, &link).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
    }
}

/// Open a Secure View link
///
/// The viewer page calls this with the ticket from the address `GET /{code}`
/// redirected to and the email the visitor typed. Every check of the link
/// runs again here; then the visit is counted (a one-time or click-capped
/// link is used up by this call, once per success) and a view token for
/// `GET /{code}/view/file` is returned.
#[utoipa::path(
    post,
    path = "/{code}/view",
    params(("code" = String, Path, description = "Short link code")),
    request_body = OpenViewRequest,
    responses(
        (status = 200, description = "The view token and what the viewer shows", body = OpenViewResponse),
        (status = 400, description = "The email is not an email address. The value is not repeated"),
        (status = 401, description = "No valid ticket: missing, expired, for another link, issued before the link's password changed. Also for a code that does not exist and a link that is not Secure View"),
        (status = 410, description = "The link stopped being live after the ticket was issued: expired, used up, disabled, or its file is gone"),
        (status = 503, description = "The link could not be checked"),
    ),
    tag = "Links"
)]
pub async fn open_view(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<OpenViewRequest>,
) -> Response {
    let (link, file, _files) = match secure_link(&state, &code).await {
        Ok(Some(found)) => found,
        Ok(None) => return session_expired(),
        Err(()) => return unavailable(),
    };
    if !validate_view_ticket(&payload.ticket, &link, file.id) {
        return session_expired();
    }

    // The ticket says every check passed when it was issued. Up to fifteen
    // minutes later, they are all asked again.
    if !link.is_active() {
        return gone(link.inactive_reason().unwrap_or("Link is inactive"));
    }
    match link_owner_is_active(&state.db, &link).await {
        Ok(true) => {}
        Ok(false) => return gone("This link has been disabled"),
        Err(_) => return unavailable(),
    }
    let used_up = if link.burn_after_reading {
        "This one-time link has already been opened"
    } else {
        "Link has reached maximum clicks"
    };
    if let Some(max) = link.effective_max_clicks()
        && link.click_count + state.click_buffer.pending_count(link.id) >= i64::from(max)
    {
        return gone(used_up);
    }
    let Some((kind, content_type)) = viewer_kind(&file) else {
        return gone("This file is no longer available");
    };

    let Some(email) = clean_viewer_email(&payload.email) else {
        // Never the value itself: it is whatever a stranger typed.
        return json_error(StatusCode::BAD_REQUEST, "Enter a valid email address");
    };
    let Some(view_token) = create_view_token(&link, file.id, &email) else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to open the viewer",
        );
    };

    // The visit. For a capped link the click is taken with one conditional
    // UPDATE, exactly as `GET /{code}` takes it for an ordinary link, so a
    // one-time link opens for one email and no more.
    let accounting = if link.effective_max_clicks().is_some() {
        match consume_capped_click(&state.db, link.id).await {
            Ok(Some(new_click_count)) => ClickAccounting::Consumed { new_click_count },
            Ok(None) => return gone(used_up),
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        }
    } else {
        ClickAccounting::Buffered {
            db_click_count: link.click_count,
        }
    };
    record_click_buffered(
        &state.click_buffer,
        state.ws_state.as_ref().map(|ws| ws.as_ref()),
        link.id,
        &link.code,
        link.user_id,
        link.org_id,
        accounting,
        None,
        Some(email.clone()),
        &headers,
    );

    let title = link
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| file.filename.clone());
    let mut response = (
        StatusCode::OK,
        Json(OpenViewResponse {
            view_token,
            expires_in: VIEW_TOKEN_TTL_SECONDS,
            title,
            file: ViewFile {
                kind: kind.to_string(),
                content_type: content_type.to_string(),
                filename: file.filename.clone(),
                size_bytes: file.size_bytes,
            },
            watermark: ViewWatermark {
                email,
                at: utc_rfc3339(chrono::Utc::now().naive_utc()),
            },
        }),
    )
        .into_response();
    no_store(&mut response);
    response
}

/// The view token a request carries. Only ever from a header: a token in the
/// address would be written to access logs and browser history.
fn view_token(headers: &HeaderMap) -> Option<&str> {
    let named = headers
        .get("x-view-token")
        .and_then(|value| value.to_str().ok());
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    named.or(bearer).map(str::trim).filter(|t| !t.is_empty())
}

/// The file of a Secure View link
///
/// The bytes the viewer draws, streamed from storage through this server.
/// Needs the view token from `POST /{code}/view` in the `X-View-Token` header
/// (or `Authorization: Bearer`); a token in the query string is ignored. May
/// be repeated while the token lives; it counts no click.
#[utoipa::path(
    get,
    path = "/{code}/view/file",
    params(
        ("code" = String, Path, description = "Short link code"),
        ("X-View-Token" = String, Header, description = "The `view_token` from `POST /{code}/view`"),
    ),
    responses(
        (status = 200, description = "The file", body = Vec<u8>, content_type = ["application/pdf", "image/png", "image/jpeg", "image/gif", "image/webp"]),
        (status = 401, description = "No valid view token. Also for a code that does not exist and a link that is not Secure View"),
        (status = 410, description = "The link expired or was disabled, or its file is gone"),
        (status = 502, description = "File storage is unavailable"),
        (status = 503, description = "The link could not be checked"),
    ),
    tag = "Links"
)]
pub async fn view_file(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = view_token(&headers) else {
        return session_expired();
    };
    let (link, file, files) = match secure_link(&state, &code).await {
        Ok(Some(found)) => found,
        Ok(None) => return session_expired(),
        Err(()) => return unavailable(),
    };
    if validate_view_token(token, &link, file.id).is_none() {
        return session_expired();
    }

    // The click cap is not asked again: this visit was counted when the token
    // was issued, and a one-time link is by now used up by this very viewer.
    // Everything that takes a link down for everyone still applies.
    let now = chrono::Utc::now().naive_utc();
    if link.expires_at.is_some_and(|expires_at| now > expires_at) {
        return gone("Link has expired");
    }
    match link_owner_is_active(&state.db, &link).await {
        Ok(true) => {}
        Ok(false) => return gone("This link has been disabled"),
        Err(_) => return unavailable(),
    }
    let Some((_, content_type)) = viewer_kind(&file) else {
        return gone("This file is no longer available");
    };

    let object = match open_for_viewer(&files, &file).await {
        Ok(Some(object)) => object,
        Ok(None) => return gone("This file is no longer available"),
        Err(error) => {
            tracing::error!("Failed to read a file for the viewer: {error}");
            return json_error(
                StatusCode::BAD_GATEWAY,
                "File storage is unavailable. Try again in a moment.",
            );
        }
    };

    let mut response = Response::new(Body::from_stream(object.body));
    let out = response.headers_mut();
    out.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    out.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    out.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );
    out.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(size) = object.size {
        out.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    }
    response
}
