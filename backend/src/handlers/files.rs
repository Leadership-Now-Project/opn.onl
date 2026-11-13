//! File sharing: upload a file and share it as a short link.
//!
//! Three calls. `POST /files/uploads` records the upload and answers with a
//! signed URL; the browser `PUT`s the file straight to the bucket; then
//! `POST /files/uploads/{id}/complete` checks the object and creates the link
//! through the same code as `POST /links`. The file itself never passes
//! through this process.
//!
//! While file sharing is not configured these routes answer 404 to a signed-in
//! caller (and 401 to anyone else, like every protected route, so the answer
//! does not say whether the feature is on). An upload belongs to the account
//! that started it: anyone else's id, and an id that does not exist, get the
//! same 403.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DbBackend, DbErr,
    EntityTrait, QueryFilter, Statement,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

use crate::AppState;
use crate::entity::{link_files, links, users};
use crate::handlers::links::{
    CreateLinkRequest, UploadAlreadyCompleted, get_user_id_from_header, insert_link, link_response,
};
use crate::handlers::{json_error, verification_satisfied};
use crate::utils::files::{clean_content_type, clean_filename, download_url, new_storage_key};
use crate::utils::time::utc_rfc3339;

#[derive(Deserialize, ToSchema)]
pub struct StartUploadRequest {
    /// The file's name. Stored cleaned up; shown in the link list and used as
    /// the download name.
    pub filename: String,
    /// The type the browser gives the file. Empty or missing is
    /// `application/octet-stream`.
    pub content_type: Option<String>,
    /// The exact size of the file in bytes.
    pub size_bytes: u64,
}

#[derive(Serialize, ToSchema)]
pub struct StartUploadResponse {
    pub upload_id: i32,
    /// `PUT` the file here, with `required_headers`, before `expires_at`.
    pub upload_url: String,
    /// Headers the `PUT` must send exactly as given, or the bucket refuses it.
    pub required_headers: BTreeMap<String, String>,
    /// When `upload_url` stops working.
    pub expires_at: String,
}

/// The fields of `POST /links` a file link takes. There is no `original_url`:
/// the link serves the uploaded file.
#[derive(Deserialize, ToSchema)]
pub struct CompleteUploadRequest {
    #[serde(alias = "alias")]
    pub custom_alias: Option<String>,
    /// Defaults to the file's name.
    pub title: Option<String>,
    pub notes: Option<String>,
    /// When the link stops working. The file is deleted then. Omit for a link
    /// that does not expire.
    pub expires_at: Option<DateTime<Utc>>,
    pub password: Option<String>,
    pub folder_id: Option<i32>,
    pub org_id: Option<i32>,
    pub tag_ids: Option<Vec<i32>>,
}

#[derive(Serialize, ToSchema)]
pub struct FileUsageResponse {
    /// Bytes stored for files this account uploaded whose links are not deleted.
    pub bytes_used: i64,
    pub file_count: i64,
    /// The largest file accepted, in bytes.
    pub max_file_bytes: u64,
}

/// The one answer for an upload the caller may not touch, whether it is
/// someone else's or does not exist.
fn upload_forbidden() -> Response {
    json_error(
        StatusCode::FORBIDDEN,
        "You don't have permission to access this upload",
    )
}

fn storage_unavailable() -> Response {
    json_error(
        StatusCode::BAD_GATEWAY,
        "File storage is unavailable. Try again in a moment.",
    )
}

fn too_large(max_bytes: u64) -> Response {
    json_error(
        StatusCode::PAYLOAD_TOO_LARGE,
        format!("The file is larger than the {max_bytes} byte limit"),
    )
}

fn not_set_up() -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        "File sharing is not set up on this server",
    )
}

/// Start an upload
///
/// Records the upload and returns a signed URL the browser sends the file to
/// directly. Nothing is shared until the upload is completed.
#[utoipa::path(
    post,
    path = "/files/uploads",
    request_body = StartUploadRequest,
    responses(
        (status = 201, description = "Upload started", body = StartUploadResponse),
        (status = 400, description = "The file is empty"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Email unverified"),
        (status = 404, description = "File sharing is not set up on this instance"),
        (status = 413, description = "The file is larger than the instance allows"),
        (status = 502, description = "File storage is unavailable"),
    ),
    tag = "Files",
    security(("bearer_auth" = []), ("api_key" = []))
)]
pub async fn start_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<StartUploadRequest>,
) -> Response {
    let Some(user_id) = get_user_id_from_header(&state.db, &headers).await else {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    };
    let Some(files) = &state.files else {
        return not_set_up();
    };
    // The same gate as creating a link, asked before a file is uploaded
    // rather than after.
    let user = users::Entity::find_by_id(user_id)
        .one(&state.db)
        .await
        .ok()
        .flatten();
    if let Some(user) = user
        && !verification_satisfied(&state, &user)
    {
        return json_error(
            StatusCode::FORBIDDEN,
            "Please verify your email address before creating links",
        );
    }

    if payload.size_bytes == 0 {
        return json_error(StatusCode::BAD_REQUEST, "The file is empty");
    }
    if payload.size_bytes > files.max_bytes {
        return too_large(files.max_bytes);
    }
    let Ok(size_bytes) = i64::try_from(payload.size_bytes) else {
        return too_large(files.max_bytes);
    };

    let content_type = clean_content_type(payload.content_type.as_deref().unwrap_or(""));
    let row = link_files::ActiveModel {
        user_id: Set(Some(user_id)),
        storage_key: Set(new_storage_key()),
        filename: Set(clean_filename(&payload.filename)),
        content_type: Set(content_type.clone()),
        size_bytes: Set(size_bytes),
        status: Set(link_files::STATUS_PENDING.to_string()),
        created_at: Set(Utc::now().naive_utc()),
        ..Default::default()
    };
    let row = match row.insert(&state.db).await {
        Ok(row) => row,
        Err(error) => {
            tracing::error!("Failed to record an upload: {error}");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };

    let upload = match files
        .store
        .presign_put(
            &row.storage_key,
            &content_type,
            payload.size_bytes,
            files.upload_url_ttl,
        )
        .await
    {
        Ok(upload) => upload,
        Err(error) => {
            tracing::error!("Failed to sign an upload URL: {error}");
            // No URL was handed out, so there is no object to wait for.
            let _ = link_files::Entity::delete_by_id(row.id)
                .exec(&state.db)
                .await;
            return storage_unavailable();
        }
    };

    let expires_at = Utc::now().naive_utc()
        + chrono::Duration::from_std(files.upload_url_ttl).unwrap_or(chrono::Duration::zero());
    (
        StatusCode::CREATED,
        Json(StartUploadResponse {
            upload_id: row.id,
            upload_url: upload.url,
            required_headers: upload.headers.into_iter().collect(),
            expires_at: utc_rfc3339(expires_at),
        }),
    )
        .into_response()
}

/// The caller's own upload. `None` for someone else's and for an id that does
/// not exist: the caller answers both with [`upload_forbidden`].
async fn own_upload(
    state: &AppState,
    upload_id: i32,
    user_id: i32,
) -> Result<Option<link_files::Model>, DbErr> {
    link_files::Entity::find_by_id(upload_id)
        .filter(link_files::Column::UserId.eq(user_id))
        .one(&state.db)
        .await
}

/// The link an upload was completed into, as `POST /links` would show it.
async fn completed_link(state: &AppState, upload: &link_files::Model) -> Response {
    let link = match upload.link_id {
        Some(link_id) => links::Entity::find_by_id(link_id)
            .filter(links::Column::DeletedAt.is_null())
            .one(&state.db)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let Some(link) = link else {
        return json_error(StatusCode::GONE, "This upload's link has been deleted");
    };
    match link_response(&state.db, &link).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
    }
}

/// Complete an upload
///
/// Checks that the file reached the bucket and is within the size limit, then
/// creates the short link that serves it, by the same rules as `POST /links`
/// (alias, default organization, folder, tags, password, link cap). Calling it
/// again for the same upload returns the link it already made.
#[utoipa::path(
    post,
    path = "/files/uploads/{upload_id}/complete",
    params(("upload_id" = i32, Path, description = "The `upload_id` from starting the upload")),
    request_body = CompleteUploadRequest,
    responses(
        (status = 201, description = "Link created", body = LinkResponse),
        (status = 200, description = "The upload was already completed: its link", body = LinkResponse),
        (status = 400, description = "The file has not been uploaded, `expires_at` is in the past, or an invalid link field"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not the caller's upload (also for an id that does not exist), or a link rule refused it"),
        (status = 404, description = "File sharing is not set up on this instance"),
        (status = 409, description = "Alias already exists"),
        (status = 410, description = "The upload expired, or its link was deleted"),
        (status = 413, description = "The uploaded file is larger than the instance allows; it has been deleted"),
        (status = 502, description = "File storage is unavailable"),
    ),
    tag = "Files",
    security(("bearer_auth" = []), ("api_key" = []))
)]
pub async fn complete_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<i32>,
    headers: HeaderMap,
    Json(payload): Json<CompleteUploadRequest>,
) -> Response {
    let Some(user_id) = get_user_id_from_header(&state.db, &headers).await else {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    };
    let Some(files) = &state.files else {
        return not_set_up();
    };

    let upload = match own_upload(&state, upload_id, user_id).await {
        Ok(Some(upload)) => upload,
        Ok(None) => return upload_forbidden(),
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
    };
    if upload.status == link_files::STATUS_READY {
        return completed_link(&state, &upload).await;
    }
    if upload.deleted_at.is_some() {
        return json_error(
            StatusCode::GONE,
            "This upload has expired. Upload the file again.",
        );
    }

    // A file link that is already expired would answer 410 from its first
    // visit and lose its file at the next sweep. Refused before the link is
    // made; the upload stays pending, so the same call works with a later
    // date. (A repeat of a completed upload was answered above.)
    if payload
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now())
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            "The expiry date is in the past. Choose a later date.",
        );
    }

    // What is in the bucket decides, not what the browser said it would send.
    let stored_bytes = match files.store.head(&upload.storage_key).await {
        Ok(Some(size)) => size,
        Ok(None) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "The file has not been uploaded yet",
            );
        }
        Err(error) => {
            tracing::error!("Failed to check an uploaded file: {error}");
            return storage_unavailable();
        }
    };
    let stored_size = i64::try_from(stored_bytes).ok();
    if stored_bytes > files.max_bytes || stored_size.is_none() {
        // Over the limit: the object goes now and the upload is closed. If
        // the bucket will not delete it, the row stays pending and the daily
        // sweep removes both.
        match files.store.delete(&upload.storage_key).await {
            Ok(()) => {
                let mut closed: link_files::ActiveModel = upload.into();
                closed.deleted_at = Set(Some(Utc::now().naive_utc()));
                let _ = closed.update(&state.db).await;
            }
            Err(error) => {
                tracing::error!("Failed to remove an oversized upload, will retry: {error}")
            }
        }
        return too_large(files.max_bytes);
    }
    let stored_size = stored_size.unwrap_or(upload.size_bytes);
    let upload = if stored_size == upload.size_bytes {
        upload
    } else {
        let mut measured: link_files::ActiveModel = upload.into();
        measured.size_bytes = Set(stored_size);
        match measured.update(&state.db).await {
            Ok(upload) => upload,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        }
    };

    let title = payload
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| upload.filename.clone());
    let request = CreateLinkRequest {
        original_url: String::new(),
        custom_alias: payload.custom_alias.filter(|alias| !alias.is_empty()),
        title: Some(title),
        expires_at: payload.expires_at,
        password: payload.password,
        notes: payload.notes,
        folder_id: payload.folder_id,
        org_id: payload.org_id,
        starts_at: None,
        max_clicks: None,
        burn_after_reading: None,
        safe_link_interstitial: None,
        tag_ids: payload.tag_ids,
        drive_file: None,
    };
    let response = insert_link(&state, Some(user_id), request, Some(&upload)).await;
    if response
        .extensions()
        .get::<UploadAlreadyCompleted>()
        .is_some()
    {
        // Another request completed this upload between the read above and
        // the insert. Answer with the link that one made.
        return match own_upload(&state, upload_id, user_id).await {
            Ok(Some(upload)) => completed_link(&state, &upload).await,
            Ok(None) => upload_forbidden(),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        };
    }
    response
}

/// Storage used by the caller's uploads
#[utoipa::path(
    get,
    path = "/files/usage",
    responses(
        (status = 200, description = "Storage used by files the caller uploaded", body = FileUsageResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "File sharing is not set up on this instance"),
    ),
    tag = "Files",
    security(("bearer_auth" = []), ("api_key" = []))
)]
pub async fn get_usage(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(user_id) = get_user_id_from_header(&state.db, &headers).await else {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    };
    let Some(files) = &state.files else {
        return not_set_up();
    };

    // Only what is really taking space for this account: files whose upload
    // was completed, still in the bucket, on a link that is not deleted.
    let row = state
        .db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT COALESCE(SUM(lf.size_bytes), 0)::bigint AS bytes_used,
                      COUNT(*)::bigint AS file_count
               FROM link_files lf
               JOIN links l ON l.id = lf.link_id
               WHERE lf.user_id = $1
                 AND lf.status = 'ready'
                 AND lf.deleted_at IS NULL
                 AND l.deleted_at IS NULL"#,
            [user_id.into()],
        ))
        .await;
    let totals = match row {
        Ok(Some(row)) => row
            .try_get::<i64>("", "bytes_used")
            .and_then(|bytes| Ok((bytes, row.try_get::<i64>("", "file_count")?))),
        Ok(None) => Ok((0, 0)),
        Err(error) => Err(error),
    };
    match totals {
        Ok((bytes_used, file_count)) => (
            StatusCode::OK,
            Json(FileUsageResponse {
                bytes_used,
                file_count,
                max_file_bytes: files.max_bytes,
            }),
        )
            .into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
    }
}

/// The signed address `GET /{code}` sends a visitor of file link `link_id` to.
///
/// `Ok(None)` when there is nothing to serve: the file was removed, the upload
/// was never completed, or file sharing has been switched off. `Err` when the
/// file could not be looked up or its address could not be signed. Called by
/// the redirect only after every check a link has.
pub(crate) async fn signed_destination(
    state: &AppState,
    link_id: i32,
) -> Result<Option<String>, ()> {
    let Some(files) = &state.files else {
        return Ok(None);
    };
    let file = link_files::Entity::find()
        .filter(link_files::Column::LinkId.eq(link_id))
        .filter(link_files::Column::Status.eq(link_files::STATUS_READY))
        .filter(link_files::Column::DeletedAt.is_null())
        .one(&state.db)
        .await
        .map_err(|error| tracing::error!("Failed to load the file of a link: {error}"))?;
    let Some(file) = file else {
        return Ok(None);
    };
    download_url(files, &file)
        .await
        .map(Some)
        .map_err(|error| tracing::error!("Failed to sign a download URL: {error}"))
}
