//! The preview image of a destination, fetched by the server for a signed-in
//! user, so the dashboard can show it without the user's browser connecting to
//! the destination's image host (which would hand it the user's IP address).
//!
//! `GET /links/preview-image?url=<image url>` with the user's token. The fetch
//! is the same guarded one the avatar proxy uses (no private or internal
//! addresses, every redirect re-checked), only inert raster types pass, served
//! under a fixed canonical type, and anything over 2 MiB is refused.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::Deserialize;

use crate::AppState;
use crate::handlers::json_error;
use crate::handlers::links::{
    canonical_avatar_content_type, get_user_id_from_header, ssrf_guarded_fetch, validate_url,
};

const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize, utoipa::IntoParams)]
pub struct PreviewImageQuery {
    /// External http(s) image URL, usually a page's `og:image`. Optional only
    /// so a request without it is answered 401 before 400, like the others.
    pub url: Option<String>,
}

/// Fetch a destination's preview image through this server
#[utoipa::path(
    get,
    path = "/links/preview-image",
    params(PreviewImageQuery),
    responses(
        (status = 200, description = "Proxied raster image bytes", content_type = "image/png"),
        (status = 400, description = "Invalid image URL"),
        (status = 401, description = "Unauthorized"),
        (status = 413, description = "Image larger than 2 MiB"),
        (status = 415, description = "Not a supported image type"),
        (status = 502, description = "Could not fetch the image"),
    ),
    tag = "Links",
    security(("bearer_auth" = []), ("api_key" = []))
)]
pub async fn preview_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PreviewImageQuery>,
) -> Response {
    // A server-side fetch of a user-supplied URL: signed-in users only.
    if get_user_id_from_header(&state.db, &headers).await.is_none() {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    let Some(url) = query.url.filter(|u| validate_url(u).is_ok()) else {
        return json_error(StatusCode::BAD_REQUEST, "Invalid image URL");
    };

    let response = match ssrf_guarded_fetch(
        reqwest::Method::GET,
        &url,
        Some("Mozilla/5.0 (compatible; OPN.ONL LinkPreview/1.0)"),
    )
    .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return json_error(StatusCode::BAD_GATEWAY, "Could not fetch the image"),
    };
    let raw_content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Never SVG (it can carry script) and never the upstream header itself.
    let Some(content_type) = canonical_avatar_content_type(raw_content_type) else {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Not a supported image type",
        );
    };

    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(c) => {
                buf.extend_from_slice(&c);
                if buf.len() > MAX_IMAGE_BYTES {
                    return json_error(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "Image larger than 2 MiB",
                    );
                }
            }
            Err(_) => return json_error(StatusCode::BAD_GATEWAY, "Could not fetch the image"),
        }
    }

    (
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CACHE_CONTROL, "private, max-age=3600".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox".to_string(),
            ),
        ],
        buf,
    )
        .into_response()
}
