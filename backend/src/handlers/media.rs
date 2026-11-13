//! The media page's one call: `POST /{code}/media`.
//!
//! `GET /{code}` on a media link runs every gate a link has, counts the
//! click, and sends the visitor to `{FRONTEND_URL}/m/{code}` with a ticket in
//! the fragment. The page trades that ticket here for what it shows: the
//! title, the player's address and the optional button.
//!
//! A caller without a valid ticket learns nothing: a code that does not
//! exist, a deleted link, a link that is not a media link and a bad ticket
//! all get the same 401. That is also what keeps a password-protected link's
//! video to those who gave the password, since a ticket is only issued behind
//! the password gate and names the password it was issued under.
//!
//! The player's address in the answer is built by `utils::media::embed_src`
//! from the stored provider and id. Nothing in the request can change it.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::entity::links;
use crate::entity::links::InactiveKind;
use crate::handlers::json_error;
use crate::handlers::links::destination_is_blocklisted;
use crate::utils::link_owner::link_owner_is_active;
use crate::utils::media::{cta_text, embed_src, media_of_link, start_of, validate_media_ticket};

#[derive(Deserialize, ToSchema)]
pub struct OpenMediaRequest {
    /// The ticket from the address `GET /{code}` redirected to.
    pub ticket: String,
}

#[derive(Serialize, ToSchema)]
pub struct MediaCta {
    pub label: String,
    /// An `https` address.
    pub url: String,
}

#[derive(Serialize, ToSchema)]
pub struct MediaEmbed {
    /// The player's address. Always on `www.youtube-nocookie.com`,
    /// `player.vimeo.com` or `drive.google.com`.
    pub src: String,
}

#[derive(Serialize, ToSchema)]
pub struct OpenMediaResponse {
    /// `youtube`, `vimeo` or `drive`.
    pub provider: String,
    pub media_id: String,
    /// The link's title; empty when it has none.
    pub title: String,
    /// The button under the player, when the link has one.
    pub cta: Option<MediaCta>,
    pub embed: MediaEmbed,
}

/// The one answer for a caller who has not shown a valid ticket, whatever
/// the reason: no such code, a deleted link, a link that is not a media
/// link, a ticket for another link, an expired one.
fn session_expired() -> Response {
    let mut response = json_error(
        StatusCode::UNAUTHORIZED,
        "This page has expired. Open the link again.",
    );
    no_store(&mut response);
    response
}

fn gone(reason: &str) -> Response {
    let mut response = json_error(StatusCode::GONE, reason);
    no_store(&mut response);
    response
}

fn unavailable() -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "The link could not be checked. Try again in a moment.",
    )
}

fn no_store(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

/// What a media link's page shows
///
/// The media page calls this with the ticket from the address `GET /{code}`
/// redirected to. The ticket may be used again while it lives (an hour), so
/// reloading the page works. This call counts no click and uses nothing up:
/// the visit was counted when the ticket was issued. The checks a ticket
/// cannot vouch for are asked again: the link still exists, has started, has
/// not expired, its owner is active, its destination is not blocked, and its
/// password is the one the ticket was issued under.
#[utoipa::path(
    post,
    path = "/{code}/media",
    params(("code" = String, Path, description = "Short link code")),
    request_body = OpenMediaRequest,
    responses(
        (status = 200, description = "What the page shows", body = OpenMediaResponse),
        (status = 401, description = "No valid ticket: missing, expired, for another link, or issued before the link's password changed. Also for a code that does not exist and a link that is not a media link"),
        (status = 410, description = "The link stopped being live after the ticket was issued: not started yet, expired or disabled"),
        (status = 503, description = "The link could not be checked"),
    ),
    tag = "Links"
)]
pub async fn open_media(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Json(payload): Json<OpenMediaRequest>,
) -> Response {
    let link = match links::Entity::find()
        .filter(links::Column::Code.eq(&code))
        .filter(links::Column::DeletedAt.is_null())
        .one(&state.db)
        .await
    {
        Ok(Some(link)) => link,
        Ok(None) => return session_expired(),
        Err(error) => {
            tracing::error!("Failed to load a link for the media page: {error}");
            return unavailable();
        }
    };
    let media = match media_of_link(&state.db, &link).await {
        Ok(Some(media)) => media,
        Ok(None) => return session_expired(),
        Err(error) => {
            tracing::error!("Failed to load a link's media: {error}");
            return unavailable();
        }
    };
    if !validate_media_ticket(&payload.ticket, &link) {
        return session_expired();
    }

    // The ticket says every check passed when it was issued, up to an hour
    // ago. What may have changed since is asked again. The click cap is not:
    // this visit was counted then, and a one-time link is by now used up by
    // this very visitor.
    let now = chrono::Utc::now().naive_utc();
    if link.starts_at.is_some_and(|starts_at| now < starts_at) {
        return gone(InactiveKind::NotStarted.message());
    }
    if link.expires_at.is_some_and(|expires_at| now > expires_at) {
        return gone(InactiveKind::Expired.message());
    }
    match link_owner_is_active(&state.db, &link).await {
        Ok(true) => {}
        Ok(false) => return gone("This link has been disabled"),
        Err(_) => return unavailable(),
    }
    match destination_is_blocklisted(&state.db, &link.original_url).await {
        Ok(false) => {}
        Ok(true) => return gone("This link has been disabled"),
        Err(_) => return unavailable(),
    }

    let Some(src) = embed_src(&media.provider, &media.media_id, start_of(&link, &media)) else {
        return gone("This video is no longer available");
    };
    // Checked again on the way out, as the player's address is: a row that
    // was not written through `POST /links` still cannot put a link on the
    // page that is anything but `https`, and the link goes out in the URL
    // parser's spelling (`cta_href`), so an older row holding `https:host`
    // cannot become a path on our own site.
    let cta = cta_text(media.cta_label.as_deref(), media.cta_url.as_deref())
        .ok()
        .flatten()
        .map(|(label, url)| MediaCta { label, url });
    let mut response = (
        StatusCode::OK,
        Json(OpenMediaResponse {
            provider: media.provider.clone(),
            media_id: media.media_id.clone(),
            title: link.title.clone().unwrap_or_default().trim().to_string(),
            cta,
            embed: MediaEmbed { src },
        }),
    )
        .into_response();
    no_store(&mut response);
    response
}
