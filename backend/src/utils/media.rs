//! Media links: a short link that opens a page with a video player on it
//! instead of redirecting to the video's host.
//!
//! The link is an ordinary URL link whose `original_url` is the video's own
//! page (a YouTube watch address, a Vimeo page, a Google Drive file), plus a
//! `link_media` row that says which video it is. `GET /{code}` runs every gate
//! a link has, counts the click, and sends the visitor to the page
//! `{FRONTEND_URL}/m/{code}` with a **media ticket** in the fragment; the page
//! trades the ticket for what to show (`POST /{code}/media`).
//!
//! What must hold:
//!
//! * The address the player is framed from is built HERE, from a provider
//!   this file knows and an id this file checked ([`embed_src`]). No client
//!   ever supplies it, so the page can only ever frame one of three hosts.
//! * Which video a link is for is decided by [`parse_media_url`], on the
//!   server. The dashboard has a twin of it for its "recognised as" hint
//!   (`frontend/src/utils/media.ts`); the twin decides nothing.
//! * Only a link whose destination [`could_be_media`] can have a row. That is
//!   how every other link's redirect, list and edit run no query here.
//! * The embed check ([`EmbedChecker`]) only ever fetches two fixed addresses
//!   (YouTube's and Vimeo's oEmbed endpoints). No host in that request comes
//!   from a client.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::future::BoxFuture;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::entity::{link_media, links};
use crate::utils::drive::is_drive_url;
use crate::utils::env::env_flag;
use crate::utils::link_unlock::password_fingerprint;

// ---------------------------------------------------------------- providers

/// Where a media link's video lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    YouTube,
    Vimeo,
    Drive,
}

impl Provider {
    /// The name stored in `link_media.provider` and shown by the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::YouTube => "youtube",
            Provider::Vimeo => "vimeo",
            Provider::Drive => "drive",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "youtube" => Some(Provider::YouTube),
            "vimeo" => Some(Provider::Vimeo),
            "drive" => Some(Provider::Drive),
            _ => None,
        }
    }
}

/// The three origins a media page may frame. `embed_src` builds nothing else,
/// the page checks its answer against the same list
/// (`frontend/src/utils/media.ts`), and the public image's
/// Content-Security-Policy names the same three (`CSP_MEDIA_EMBEDS`).
pub const EMBED_ORIGINS: [&str; 3] = [
    "https://www.youtube-nocookie.com",
    "https://player.vimeo.com",
    "https://drive.google.com",
];

// ------------------------------------------------------------------- parser

/// A YouTube or Vimeo video, as read from an address someone pasted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedMedia {
    pub provider: Provider,
    /// YouTube: the 11-character video id. Vimeo: the numeric id, followed by
    /// `:` and the hash for an unlisted video.
    pub media_id: String,
    /// Seconds into the video to start at (YouTube's `t` / `start`).
    pub start: Option<u32>,
}

/// The longest address read at all.
const MAX_URL_LEN: usize = 2048;
const YOUTUBE_ID_LEN: usize = 11;
/// Words YouTube uses in the place of a video id that are exactly as long as
/// one: a playlist player and a channel's live player. Neither is a video.
const YOUTUBE_NOT_IDS: [&str; 2] = ["videoseries", "live_stream"];
const VIMEO_ID_MAX_LEN: usize = 12;
const VIMEO_HASH_MIN_LEN: usize = 6;
const VIMEO_HASH_MAX_LEN: usize = 32;
/// The latest start that is kept: 99:59:59.
const MAX_START_SECONDS: u32 = 359_999;

fn is_youtube_id(id: &str) -> bool {
    id.len() == YOUTUBE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !YOUTUBE_NOT_IDS.contains(&id)
}

fn is_vimeo_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= VIMEO_ID_MAX_LEN
        && !id.starts_with('0')
        && id.bytes().all(|b| b.is_ascii_digit())
}

/// Words Vimeo puts after a video's or a member's number that have the shape
/// of an unlisted video's hash and are pages, not videos
/// (`vimeo.com/<id>/settings`). Twin: `VIMEO_NOT_HASHES` in
/// `frontend/src/utils/media.ts`.
const VIMEO_NOT_HASHES: [&str; 14] = [
    "settings",
    "likes",
    "collections",
    "videos",
    "following",
    "followers",
    "channels",
    "groups",
    "albums",
    "portfolios",
    "appearances",
    "members",
    "comments",
    "review",
];

fn is_vimeo_hash(hash: &str) -> bool {
    (VIMEO_HASH_MIN_LEN..=VIMEO_HASH_MAX_LEN).contains(&hash.len())
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        && !VIMEO_NOT_HASHES.contains(&hash)
}

fn is_drive_file_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether a Drive file's type says it is a video. Only such a file can be a
/// Drive media link.
pub fn is_video_mime(mime_type: &str) -> bool {
    let mime_type = mime_type.trim();
    mime_type.len() > "video/".len()
        && mime_type
            .get(.."video/".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("video/"))
}

/// `90`, `90s`, `1m30s`, `1h2m3s`: the forms YouTube writes a start time in.
fn parse_start(raw: &str) -> Option<u32> {
    if raw.is_empty() || raw.len() > 12 {
        return None;
    }
    let seconds = if raw.bytes().all(|b| b.is_ascii_digit()) {
        raw.parse::<u32>().ok()?
    } else {
        let mut total: u32 = 0;
        let mut number = String::new();
        // Each unit at most once, in the order hours, minutes, seconds.
        let mut last_unit = 0u8;
        for c in raw.chars() {
            if c.is_ascii_digit() {
                number.push(c);
                continue;
            }
            let (rank, factor) = match c {
                'h' => (1, 3600),
                'm' => (2, 60),
                's' => (3, 1),
                _ => return None,
            };
            if number.is_empty() || rank <= last_unit {
                return None;
            }
            last_unit = rank;
            total = total.checked_add(number.parse::<u32>().ok()?.checked_mul(factor)?)?;
            number.clear();
        }
        if !number.is_empty() {
            return None;
        }
        total
    };
    (1..=MAX_START_SECONDS)
        .contains(&seconds)
        .then_some(seconds)
}

/// The one value of a query parameter. `None` when it is absent, and `Err`
/// when it is given twice: an address that says two things is not read.
fn one_param(parsed: &url::Url, name: &str) -> Result<Option<String>, ()> {
    let mut values = parsed
        .query_pairs()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned());
    let first = values.next();
    if values.next().is_some() {
        return Err(());
    }
    Ok(first)
}

/// Read a YouTube or Vimeo video out of an address. `None` for anything that
/// is not exactly one of the forms these hosts hand out: another host (also a
/// look-alike such as `youtube.com.evil.test`), a name and password before
/// the host, a port, a longer path, an id of the wrong length or with any
/// character outside the host's own alphabet.
///
/// Twin: `parseMediaUrl` in `frontend/src/utils/media.ts`, which only draws
/// the "recognised as" hint. Change the two together.
pub fn parse_media_url(raw: &str) -> Option<ParsedMedia> {
    // A line break or a tab anywhere, the ends included, is not part of an
    // address anyone copied: asked before the spaces around it are dropped.
    if raw.chars().any(char::is_control) {
        return None;
    }
    let raw = raw.trim_matches(' ');
    if raw.is_empty() || raw.len() > MAX_URL_LEN {
        return None;
    }
    let parsed = url::Url::parse(raw).ok()?;
    if !matches!(parsed.scheme(), "https" | "http")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
    {
        return None;
    }
    let host = parsed.host_str()?;
    let mut segments: Vec<&str> = parsed.path_segments()?.collect();
    // One trailing slash is how some of these addresses are copied.
    if segments.len() > 1 && segments.last() == Some(&"") {
        segments.pop();
    }

    match host {
        "youtube.com" | "www.youtube.com" | "m.youtube.com" => {
            let id = match segments.as_slice() {
                ["watch"] => one_param(&parsed, "v").ok()??,
                ["shorts" | "live" | "embed", id] => (*id).to_string(),
                _ => return None,
            };
            youtube(&parsed, id)
        }
        "youtu.be" => match segments.as_slice() {
            [id] => youtube(&parsed, (*id).to_string()),
            _ => None,
        },
        "www.youtube-nocookie.com" => match segments.as_slice() {
            ["embed", id] => youtube(&parsed, (*id).to_string()),
            _ => None,
        },
        "vimeo.com" | "www.vimeo.com" => match segments.as_slice() {
            [id] => vimeo(id, one_param(&parsed, "h").ok()?),
            [id, hash] => vimeo(id, Some((*hash).to_string())),
            _ => None,
        },
        "player.vimeo.com" => match segments.as_slice() {
            ["video", id] => vimeo(id, one_param(&parsed, "h").ok()?),
            _ => None,
        },
        _ => None,
    }
}

fn youtube(parsed: &url::Url, id: String) -> Option<ParsedMedia> {
    if !is_youtube_id(&id) {
        return None;
    }
    // A start time that cannot be read is dropped; the video is still the video.
    let start = one_param(parsed, "t")
        .ok()
        .flatten()
        .or_else(|| one_param(parsed, "start").ok().flatten())
        .and_then(|raw| parse_start(&raw));
    Some(ParsedMedia {
        provider: Provider::YouTube,
        media_id: id,
        start,
    })
}

fn vimeo(id: &str, hash: Option<String>) -> Option<ParsedMedia> {
    if !is_vimeo_id(id) {
        return None;
    }
    let media_id = match hash {
        None => id.to_string(),
        Some(hash) if is_vimeo_hash(&hash) => format!("{id}:{hash}"),
        Some(_) => return None,
    };
    Some(ParsedMedia {
        provider: Provider::Vimeo,
        media_id,
        start: None,
    })
}

impl ParsedMedia {
    /// The video's own page, as the link stores it in `original_url`: one
    /// spelling per video, written here and never copied from the request.
    pub fn canonical_url(&self) -> String {
        match self.provider {
            Provider::YouTube => match self.start {
                Some(start) => format!(
                    "https://www.youtube.com/watch?v={}&t={start}s",
                    self.media_id
                ),
                None => format!("https://www.youtube.com/watch?v={}", self.media_id),
            },
            Provider::Vimeo => match self.media_id.split_once(':') {
                Some((id, hash)) => format!("https://vimeo.com/{id}/{hash}"),
                None => format!("https://vimeo.com/{}", self.media_id),
            },
            // A Drive video's page is the address Drive gave the browser.
            Provider::Drive => String::new(),
        }
    }
}

/// Whether a link with this destination can be a media link at all: a YouTube
/// or Vimeo video address, or an address in Google Drive. Every lookup of
/// `link_media` asks this first, so a link to anywhere else runs no query, and
/// the redirect cache keeps no such destination (a cached destination would
/// send the visitor past the page).
pub fn could_be_media(url: &str) -> bool {
    parse_media_url(url).is_some() || is_drive_url(url)
}

/// The address the page frames the player from. Built from the provider's
/// name and an id that is checked again here, so a row that was not written
/// by this file still cannot frame anything but one of [`EMBED_ORIGINS`].
/// `None` when the row is not one this file would have written.
pub fn embed_src(provider: &str, media_id: &str, start: Option<u32>) -> Option<String> {
    match Provider::from_name(provider)? {
        Provider::YouTube => {
            if !is_youtube_id(media_id) {
                return None;
            }
            let base = format!("{}/embed/{media_id}", EMBED_ORIGINS[0]);
            Some(
                match start.filter(|s| (1..=MAX_START_SECONDS).contains(s)) {
                    Some(start) => format!("{base}?start={start}"),
                    None => base,
                },
            )
        }
        Provider::Vimeo => match media_id.split_once(':') {
            Some((id, hash)) if is_vimeo_id(id) && is_vimeo_hash(hash) => {
                Some(format!("{}/video/{id}?h={hash}", EMBED_ORIGINS[1]))
            }
            None if is_vimeo_id(media_id) => Some(format!("{}/video/{media_id}", EMBED_ORIGINS[1])),
            _ => None,
        },
        Provider::Drive => is_drive_file_id(media_id)
            .then(|| format!("{}/file/d/{media_id}/preview", EMBED_ORIGINS[2])),
    }
}

/// Where a YouTube media link starts playing: the `t` its own canonical
/// address carries, when that address is still this video's.
pub fn start_of(link: &links::Model, media: &link_media::Model) -> Option<u32> {
    let parsed = parse_media_url(&link.original_url)?;
    (parsed.provider.as_str() == media.provider && parsed.media_id == media.media_id)
        .then_some(parsed.start)
        .flatten()
}

// ----------------------------------------------------------- call to action

/// The longest label of the button under the player, in characters.
pub const MAX_CTA_LABEL_CHARS: usize = 40;
pub const CTA_BOTH_OR_NEITHER: &str =
    "Give the button both a label and a link, or leave both empty";

/// The label and link of the optional button, as text: trimmed, and both or
/// neither. `Ok(None)` when neither was given. The link's own checks (it must
/// pass everything a link's destination passes) are the caller's, which has
/// the database: `links::checked_media_cta`.
pub fn cta_text(
    label: Option<&str>,
    url: Option<&str>,
) -> Result<Option<(String, String)>, String> {
    let label = label.map(str::trim).unwrap_or_default();
    let url = url.map(str::trim).unwrap_or_default();
    if label.is_empty() && url.is_empty() {
        return Ok(None);
    }
    if label.is_empty() || url.is_empty() {
        return Err(CTA_BOTH_OR_NEITHER.to_string());
    }
    if label.chars().count() > MAX_CTA_LABEL_CHARS {
        return Err(format!(
            "The button's label must be at most {MAX_CTA_LABEL_CHARS} characters"
        ));
    }
    if label.chars().any(char::is_control) {
        return Err("The button's label contains control characters".to_string());
    }
    let Some(url) = cta_href(url) else {
        return Err("The button's link must be an https:// address".to_string());
    };
    Ok(Some((label.to_string(), url)))
}

/// The address a button's link is stored and shown as: the URL parser's own
/// spelling of it, never the text that was typed. `None` unless it is `https`
/// with a host and no name or password: no `javascript:`, no `data:`, no
/// plain http.
///
/// The parser reads `https:example.org` and `https:/example.org` as
/// `https://example.org/`, and that host is the one every check is run on. A
/// browser given the typed text inside a page would resolve it against the
/// page instead and open our own site. So what is kept is what was checked.
/// The page call runs a stored link through here again on the way out, for a
/// row written before this rule.
///
/// Twin: `httpsHref` in `frontend/src/utils/media.ts`.
pub fn cta_href(url: &str) -> Option<String> {
    // The parser drops tabs and line breaks; an address with one is refused.
    if url.len() > MAX_URL_LEN || url.chars().any(char::is_control) {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    let https = parsed.scheme() == "https"
        && parsed.host_str().is_some_and(|host| !host.is_empty())
        && parsed.username().is_empty()
        && parsed.password().is_none();
    let href = parsed.to_string();
    (https && href.len() <= MAX_URL_LEN).then_some(href)
}

// ------------------------------------------------------------------ lookups

/// The video this link shows, when it is a media link. No query at all for a
/// link whose destination could not be one.
pub async fn media_of_link<C: ConnectionTrait>(
    db: &C,
    link: &links::Model,
) -> Result<Option<link_media::Model>, DbErr> {
    if link.is_file() || !could_be_media(&link.original_url) {
        return Ok(None);
    }
    link_media::Entity::find()
        .filter(link_media::Column::LinkId.eq(link.id))
        .one(db)
        .await
}

/// The link's destination is changing: it is no longer the video its row
/// names. No query for a link that could not have a row.
pub async fn forget_for_link<C: ConnectionTrait>(db: &C, link: &links::Model) -> Result<(), DbErr> {
    if !could_be_media(&link.original_url) {
        return Ok(());
    }
    link_media::Entity::delete_many()
        .filter(link_media::Column::LinkId.eq(link.id))
        .exec(db)
        .await?;
    Ok(())
}

// ------------------------------------------------------------------ tickets

/// How long the page may be reloaded after the link was opened.
pub const MEDIA_TICKET_TTL_SECONDS: i64 = 60 * 60;
const TICKET_PURPOSE: &str = "media_ticket";

#[derive(Debug, Serialize, Deserialize)]
struct MediaClaims {
    sub: String,
    exp: usize,
    link_id: i32,
    code: String,
    /// Fingerprint of the link's password hash (of nothing when it has none),
    /// so adding, changing or removing the password ends every open page.
    password_fingerprint: String,
}

fn configured_secret() -> Option<String> {
    std::env::var("JWT_SECRET")
        .ok()
        .filter(|secret| secret.len() >= 32)
}

fn link_fingerprint(link: &links::Model) -> String {
    password_fingerprint(link.password_hash.as_deref().unwrap_or(""))
}

/// The ticket `GET /{code}` gives a visitor who passed every check of a media
/// link. It proves that and nothing else. Unlike a Secure View ticket it may
/// be used again while it lives, so that reloading the page works: the visit
/// it stands for was counted when it was issued.
pub fn create_media_ticket(link: &links::Model) -> Option<String> {
    create_media_ticket_with_ttl(link, MEDIA_TICKET_TTL_SECONDS)
}

/// [`create_media_ticket`] with the lifetime given, for a test that needs a
/// ticket that has already ended.
pub fn create_media_ticket_with_ttl(link: &links::Model, ttl_seconds: i64) -> Option<String> {
    sign_ticket(&configured_secret()?, link, ttl_seconds)
}

fn sign_ticket(secret: &str, link: &links::Model, ttl_seconds: i64) -> Option<String> {
    let exp = Utc::now()
        .checked_add_signed(chrono::Duration::seconds(ttl_seconds))?
        .timestamp();
    let claims = MediaClaims {
        sub: TICKET_PURPOSE.to_string(),
        exp: usize::try_from(exp).ok()?,
        link_id: link.id,
        code: link.code.clone(),
        password_fingerprint: link_fingerprint(link),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .ok()
}

/// Whether `ticket` is a live media ticket for exactly this link and its
/// current password.
pub fn validate_media_ticket(ticket: &str, link: &links::Model) -> bool {
    configured_secret().is_some_and(|secret| verify_ticket(&secret, ticket, link))
}

fn verify_ticket(secret: &str, ticket: &str, link: &links::Model) -> bool {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    let Ok(data) = decode::<MediaClaims>(
        ticket,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    ) else {
        return false;
    };
    let claims = data.claims;
    claims.sub == TICKET_PURPOSE
        && claims.link_id == link.id
        && claims.code == link.code
        && claims.password_fingerprint == link_fingerprint(link)
}

// -------------------------------------------------------------- embed check

/// What the video's host said when asked whether the video can be embedded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmbedVerdict {
    /// It can. `title` is the video's own title, when the host gave one.
    Embeddable { title: Option<String> },
    /// It cannot, and why, in words for the person creating the link.
    Refused(&'static str),
    /// The host could not be asked, or its answer could not be read. The link
    /// is created all the same: our own network must never block someone.
    Unknown,
}

pub const NOT_EMBEDDABLE: &str = "This video cannot be shown on a media page: its owner has turned embedding off, or it is private.";
pub const NOT_EMBEDDABLE_HERE: &str =
    "This video cannot be shown on a media page: its owner only allows it on other websites.";
pub const VIDEO_NOT_FOUND: &str = "This video was not found. It may be private or deleted, or the address may be incomplete (an unlisted Vimeo video needs its full link).";

/// Asks a video's host whether the video can be embedded. On `AppState`, so
/// tests give the app an answer instead of the network.
pub trait EmbedChecker: Send + Sync {
    fn check<'a>(&'a self, media: &'a ParsedMedia) -> BoxFuture<'a, EmbedVerdict>;
}

/// The longest oEmbed answer that is read.
const OEMBED_MAX_BYTES: usize = 64 * 1024;
const OEMBED_TIMEOUT: Duration = Duration::from_secs(5);
const YOUTUBE_OEMBED: &str = "https://www.youtube.com/oembed";
const VIMEO_OEMBED: &str = "https://vimeo.com/api/oembed.json";

/// The address asked about `media`: one of two fixed endpoints, with the
/// video's canonical address (written by this file) as a query value. Nothing
/// a client sent decides the host, the path or the scheme.
pub fn oembed_url(media: &ParsedMedia) -> Option<String> {
    let video = urlencoding::encode(&media.canonical_url()).into_owned();
    match media.provider {
        Provider::YouTube => Some(format!("{YOUTUBE_OEMBED}?url={video}&format=json")),
        Provider::Vimeo => Some(format!("{VIMEO_OEMBED}?url={video}")),
        Provider::Drive => None,
    }
}

/// Read an oEmbed answer. Observed and documented behaviour (November 2026):
/// both hosts answer 200 with JSON for a video that can be embedded; YouTube
/// answers 400 for an id that does not exist and 401 or 403 for a private
/// video or one with embedding turned off; Vimeo answers 404 for a missing or
/// private video (and for an unlisted one asked about without its hash), 403
/// when embedding is not allowed, and 200 with a `domain_status_code` other
/// than 200 when the owner allows embedding only on other websites. Anything
/// else (a 429, a 5xx, a redirect, a body that is not JSON) decides nothing.
pub fn verdict_from(status: u16, body: &[u8]) -> EmbedVerdict {
    match status {
        200 => {
            let Ok(serde_json::Value::Object(answer)) =
                serde_json::from_slice::<serde_json::Value>(body)
            else {
                return EmbedVerdict::Unknown;
            };
            if answer
                .get("domain_status_code")
                .and_then(serde_json::Value::as_i64)
                .is_some_and(|code| code != 200)
            {
                return EmbedVerdict::Refused(NOT_EMBEDDABLE_HERE);
            }
            EmbedVerdict::Embeddable {
                title: answer
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .and_then(clean_title),
            }
        }
        401 | 403 => EmbedVerdict::Refused(NOT_EMBEDDABLE),
        400 | 404 => EmbedVerdict::Refused(VIDEO_NOT_FOUND),
        _ => EmbedVerdict::Unknown,
    }
}

/// A title from a video's host, as a link's title: no control characters, and
/// within the length a link's title may have.
pub fn clean_title(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let cleaned: String = cleaned.trim().chars().take(500).collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// The real check: one GET to the host's public oEmbed endpoint. Redirects
/// are not followed, the wait is short and the answer is read up to a cap.
pub struct OEmbedChecker {
    client: reqwest::Client,
    /// Our own public address, sent as `Referer`: Vimeo uses it to say whether
    /// a video limited to certain websites may be embedded on this one.
    referer: String,
}

impl OEmbedChecker {
    /// The check, unless `MEDIA_EMBED_CHECK` turns it off (default on).
    pub fn from_env() -> Option<Arc<dyn EmbedChecker>> {
        if !env_flag("MEDIA_EMBED_CHECK", true) {
            return None;
        }
        let checker = Self::new(format!("{}/", crate::utils::env::frontend_base_url()))?;
        Some(Arc::new(checker))
    }

    fn new(referer: String) -> Option<Self> {
        let client = reqwest::Client::builder()
            .timeout(OEMBED_TIMEOUT)
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("opn.onl media embed check")
            .build()
            .ok()?;
        Some(Self { client, referer })
    }

    async fn ask(&self, media: &ParsedMedia) -> EmbedVerdict {
        let Some(url) = oembed_url(media) else {
            return EmbedVerdict::Unknown;
        };
        let request = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::REFERER, &self.referer);
        let mut response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                // Without the address asked: it carries the video's own
                // address, and an unlisted video's is a secret of its owner.
                tracing::warn!(
                    "The media embed check could not reach the host: {}",
                    error.without_url()
                );
                return EmbedVerdict::Unknown;
            }
        };
        let status = response.status().as_u16();
        let mut body: Vec<u8> = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > OEMBED_MAX_BYTES {
                        return EmbedVerdict::Unknown;
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => return EmbedVerdict::Unknown,
            }
        }
        verdict_from(status, &body)
    }
}

impl EmbedChecker for OEmbedChecker {
    fn check<'a>(&'a self, media: &'a ParsedMedia) -> BoxFuture<'a, EmbedVerdict> {
        Box::pin(self.ask(media))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real check against the real hosts. Not part of the suite, which
    /// never reaches the network; run it by hand when a host's behaviour is
    /// in doubt: `cargo test --lib -- --ignored the_real_check`.
    #[tokio::test]
    #[ignore = "asks youtube.com and vimeo.com"]
    async fn the_real_check_reads_what_the_real_hosts_answer() {
        let checker = OEmbedChecker::new("https://links.example.org/".to_string()).unwrap();
        for (url, found) in [
            ("https://youtu.be/dQw4w9WgXcQ", true),
            ("https://youtu.be/aaaaaaaaaaa", false),
            ("https://vimeo.com/76979871", true),
            ("https://vimeo.com/999999999999", false),
        ] {
            let media = parse_media_url(url).unwrap();
            let verdict = checker.check(&media).await;
            if found {
                assert!(
                    matches!(&verdict, EmbedVerdict::Embeddable { title: Some(title) } if !title.is_empty()),
                    "{url}: {verdict:?}"
                );
            } else {
                assert_eq!(verdict, EmbedVerdict::Refused(VIDEO_NOT_FOUND), "{url}");
            }
        }
    }

    fn yt(id: &str, start: Option<u32>) -> Option<ParsedMedia> {
        Some(ParsedMedia {
            provider: Provider::YouTube,
            media_id: id.to_string(),
            start,
        })
    }

    fn vm(id: &str) -> Option<ParsedMedia> {
        Some(ParsedMedia {
            provider: Provider::Vimeo,
            media_id: id.to_string(),
            start: None,
        })
    }

    #[test]
    fn every_youtube_form_is_read() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=dQw4w9WgXcQ",
            "http://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ&list=PL123",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=AbCdEf",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ/",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
            "  https://youtu.be/dQw4w9WgXcQ  ",
        ] {
            assert_eq!(parse_media_url(url), yt("dQw4w9WgXcQ", None), "{url}");
        }
        assert_eq!(
            parse_media_url("https://youtu.be/a_b-c_d-e_f"),
            yt("a_b-c_d-e_f", None)
        );
    }

    #[test]
    fn a_start_time_is_kept_and_an_unreadable_one_dropped() {
        for (query, start) in [
            ("t=90", Some(90)),
            ("t=90s", Some(90)),
            ("t=1m30s", Some(90)),
            ("t=1h2m3s", Some(3723)),
            ("start=42", Some(42)),
            ("t=0", None),
            ("t=abc", None),
            ("t=30s1m", None),
            ("t=1m1m", None),
            ("t=99999999999", None),
            ("t=-5", None),
        ] {
            assert_eq!(
                parse_media_url(&format!(
                    "https://www.youtube.com/watch?v=dQw4w9WgXcQ&{query}"
                )),
                yt("dQw4w9WgXcQ", start),
                "{query}"
            );
        }
        assert_eq!(
            parse_media_url("https://youtu.be/dQw4w9WgXcQ?t=75"),
            yt("dQw4w9WgXcQ", Some(75))
        );
    }

    #[test]
    fn every_vimeo_form_is_read() {
        for url in [
            "https://vimeo.com/76979871",
            "https://www.vimeo.com/76979871",
            "https://vimeo.com/76979871/",
            "https://vimeo.com/76979871?share=copy",
            "https://player.vimeo.com/video/76979871",
        ] {
            assert_eq!(parse_media_url(url), vm("76979871"), "{url}");
        }
        for url in [
            "https://vimeo.com/76979871/abc123def0",
            "https://vimeo.com/76979871?h=abc123def0",
            "https://player.vimeo.com/video/76979871?h=abc123def0",
        ] {
            assert_eq!(parse_media_url(url), vm("76979871:abc123def0"), "{url}");
        }
    }

    #[test]
    fn hostile_and_unknown_addresses_are_not_read() {
        for url in [
            "",
            "not a url",
            "youtube.com/watch?v=dQw4w9WgXcQ",
            // Other schemes.
            "javascript:alert(1)//youtu.be/dQw4w9WgXcQ",
            "ftp://youtu.be/dQw4w9WgXcQ",
            "data:text/html,https://youtu.be/dQw4w9WgXcQ",
            // Look-alike hosts.
            "https://youtube.com.evil.test/watch?v=dQw4w9WgXcQ",
            "https://evil.test/watch?v=dQw4w9WgXcQ",
            "https://notyoutube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be.evil.test/dQw4w9WgXcQ",
            "https://evil.test/?u=https://youtu.be/dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://yоutube.com/watch?v=dQw4w9WgXcQ",
            "https://vimeo.com.evil.test/76979871",
            "https://player.vimeo.com.evil.test/video/76979871",
            // A name and password before the host, and a port.
            "https://youtube.com@evil.test/watch?v=dQw4w9WgXcQ",
            "https://user@www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://user:pw@youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com:8443/watch?v=dQw4w9WgXcQ",
            "https://vimeo.com:444/76979871",
            // Paths that are not a video's.
            "https://www.youtube.com/",
            "https://www.youtube.com/watch",
            "https://www.youtube.com/watch/dQw4w9WgXcQ",
            "https://www.youtube.com/playlist?list=PL1234567890",
            "https://www.youtube.com/channel/dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ/extra",
            "https://www.youtube.com/embed/dQw4w9WgXcQ/../../watch",
            "https://www.youtube.com/embed/videoseries?list=PL1234567890",
            "https://www.youtube.com/embed/live_stream?channel=UC123",
            "https://youtu.be/dQw4w9WgXcQ/extra",
            "https://www.youtube-nocookie.com/watch?v=dQw4w9WgXcQ",
            "https://vimeo.com/channels/staffpicks/76979871",
            "https://vimeo.com/76979871/abc123def0/extra",
            "https://player.vimeo.com/76979871",
            "https://player.vimeo.com/video/76979871/extra",
            // Ids of the wrong shape.
            "https://youtu.be/dQw4w9WgXc",
            "https://youtu.be/dQw4w9WgXcQQ",
            "https://youtu.be/dQw4w9WgXc%51",
            "https://youtu.be/dQw4w9WgXc!",
            "https://youtu.be/dQw4w9WgXcé",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&v=aaaaaaaaaaa",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ%22%3E%3Cscript%3E",
            "https://vimeo.com/0123",
            "https://vimeo.com/1234567890123",
            "https://vimeo.com/76979871a",
            "https://vimeo.com/７６９７９８７１",
            "https://vimeo.com/76979871/ABC123DEF0",
            "https://vimeo.com/76979871/abc",
            // Pages under a video's number, not an unlisted video's hash.
            "https://vimeo.com/76979871/settings",
            "https://vimeo.com/76979871/collections",
            "https://vimeo.com/76979871/videos",
            "https://vimeo.com/76979871/likes",
            "https://vimeo.com/76979871?h=settings",
            "https://player.vimeo.com/video/76979871?h=settings",
            "https://vimeo.com/76979871?h=abc%22onload",
            "https://vimeo.com/76979871?h=abc123def0&h=ffffffffff",
            // Control characters.
            "https://youtu.be/dQw4w9WgXcQ\n",
            "https://youtu.be/dQw4\tw9WgXcQ",
        ] {
            assert_eq!(parse_media_url(url), None, "{url:?} must not be read");
        }
        let long = format!(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&x={}",
            "a".repeat(2100)
        );
        assert_eq!(parse_media_url(&long), None);
    }

    #[test]
    fn the_canonical_address_reads_back_as_the_same_video() {
        for url in [
            "https://youtu.be/dQw4w9WgXcQ?t=1m30s",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://player.vimeo.com/video/76979871?h=abc123def0",
            "https://vimeo.com/76979871",
        ] {
            let parsed = parse_media_url(url).unwrap();
            let canonical = parsed.canonical_url();
            assert_eq!(parse_media_url(&canonical), Some(parsed), "{canonical}");
            assert!(could_be_media(&canonical));
        }
        assert_eq!(
            parse_media_url("https://youtu.be/dQw4w9WgXcQ?t=1m30s")
                .unwrap()
                .canonical_url(),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=90s"
        );
        assert_eq!(
            parse_media_url("https://player.vimeo.com/video/76979871?h=abc123def0")
                .unwrap()
                .canonical_url(),
            "https://vimeo.com/76979871/abc123def0"
        );
    }

    #[test]
    fn only_video_hosts_and_drive_could_be_media() {
        assert!(could_be_media(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        ));
        assert!(could_be_media("https://vimeo.com/76979871"));
        assert!(could_be_media("https://drive.google.com/file/d/abc/view"));
        assert!(!could_be_media("https://example.org/watch?v=dQw4w9WgXcQ"));
        assert!(!could_be_media("https://www.youtube.com/"));
        assert!(!could_be_media("opn-file:12"));
    }

    #[test]
    fn the_player_address_is_only_ever_on_one_of_three_hosts() {
        assert_eq!(
            embed_src("youtube", "dQw4w9WgXcQ", None).as_deref(),
            Some("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ")
        );
        assert_eq!(
            embed_src("youtube", "dQw4w9WgXcQ", Some(90)).as_deref(),
            Some("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?start=90")
        );
        assert_eq!(
            embed_src("vimeo", "76979871", None).as_deref(),
            Some("https://player.vimeo.com/video/76979871")
        );
        assert_eq!(
            embed_src("vimeo", "76979871:abc123def0", None).as_deref(),
            Some("https://player.vimeo.com/video/76979871?h=abc123def0")
        );
        assert_eq!(
            embed_src("drive", "1AbC_d-9", None).as_deref(),
            Some("https://drive.google.com/file/d/1AbC_d-9/preview")
        );

        // A row this file would not have written frames nothing.
        for (provider, id) in [
            ("youtube", "dQw4w9WgXc"),
            ("youtube", "../../evil1"),
            ("youtube", "dQw4w9WgXc\""),
            ("youtube", "videoseries"),
            ("youtube", "dQw4w9WgXcQ?x=1"),
            ("vimeo", "76979871/../x"),
            ("vimeo", "76979871:"),
            ("vimeo", "76979871:AB\"><"),
            ("vimeo", "76979871:settings"),
            ("vimeo", "evil.test"),
            ("drive", "abc/../../x"),
            ("drive", "abc?x=1"),
            ("drive", ""),
            ("other", "dQw4w9WgXcQ"),
            ("https://evil.test", "dQw4w9WgXcQ"),
            ("", "dQw4w9WgXcQ"),
        ] {
            assert_eq!(embed_src(provider, id, None), None, "{provider} {id:?}");
        }

        // Whatever is built starts with one of the three origins and a slash.
        for src in [
            embed_src("youtube", "dQw4w9WgXcQ", Some(5)),
            embed_src("vimeo", "1:abcdef", None),
            embed_src("drive", "x", None),
        ] {
            let src = src.unwrap();
            assert!(
                EMBED_ORIGINS
                    .iter()
                    .any(|origin| src.starts_with(&format!("{origin}/"))),
                "{src}"
            );
        }
    }

    #[test]
    fn the_button_needs_both_a_label_and_an_https_link() {
        assert_eq!(cta_text(None, None), Ok(None));
        assert_eq!(cta_text(Some("  "), Some("")), Ok(None));
        assert_eq!(
            cta_text(Some(" Join us "), Some(" https://example.org/join ")),
            Ok(Some((
                "Join us".to_string(),
                "https://example.org/join".to_string()
            )))
        );
        assert_eq!(
            cta_text(Some("Join"), None),
            Err(CTA_BOTH_OR_NEITHER.to_string())
        );
        assert_eq!(
            cta_text(None, Some("https://example.org")),
            Err(CTA_BOTH_OR_NEITHER.to_string())
        );
        assert!(cta_text(Some(&"x".repeat(41)), Some("https://example.org")).is_err());
        assert!(cta_text(Some(&"x".repeat(40)), Some("https://example.org")).is_ok());
        assert!(cta_text(Some("a\u{7}b"), Some("https://example.org")).is_err());
        for url in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            " javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "http://example.org/",
            "//example.org/",
            "example.org",
            "https://",
            "https://user:pw@example.org/",
            "https://user@example.org/",
            "mailto:someone@example.org",
            // The parser would drop the tab and the line break.
            "https://exa\tmple.org/",
            "https://example.org/a\nb",
        ] {
            assert!(
                cta_text(Some("Go"), Some(url)).is_err(),
                "{url:?} must be refused"
            );
        }
    }

    /// `https:host` and `https:/host` parse as `https://host/`, and the checks
    /// run on that host. A browser handed the typed text inside a page reads
    /// it as a path on the page's own site, so the typed text is never kept.
    #[test]
    fn the_buttons_link_is_kept_in_the_parsers_spelling_never_as_typed() {
        for (typed, kept) in [
            ("https:example.org", "https://example.org/"),
            ("https:/example.org/join", "https://example.org/join"),
            ("https:\\\\example.org\\join", "https://example.org/join"),
            (
                "HTTPS://Example.ORG/Join?a=1#b",
                "https://example.org/Join?a=1#b",
            ),
            ("https://example.org", "https://example.org/"),
            ("https://example.org:443/x", "https://example.org/x"),
            ("https://example.org/join", "https://example.org/join"),
        ] {
            assert_eq!(cta_href(typed).as_deref(), Some(kept), "{typed}");
            assert_eq!(
                cta_text(Some("Go"), Some(typed)),
                Ok(Some(("Go".to_string(), kept.to_string()))),
                "{typed}"
            );
            assert!(
                kept.starts_with("https://"),
                "an absolute address wherever it is used: {kept}"
            );
            // Its own spelling reads back as itself.
            assert_eq!(cta_href(kept).as_deref(), Some(kept));
        }
        for refused in ["https:", "https:/", "https:///", "http:example.org", ""] {
            assert_eq!(cta_href(refused), None, "{refused:?}");
        }
        let long = format!("https://example.org/{}", "a".repeat(2100));
        assert_eq!(cta_href(&long), None);
    }

    #[test]
    fn the_embed_check_only_ever_asks_two_fixed_endpoints() {
        let youtube = parse_media_url("https://youtu.be/dQw4w9WgXcQ?t=30").unwrap();
        assert_eq!(
            oembed_url(&youtube).unwrap(),
            "https://www.youtube.com/oembed?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3DdQw4w9WgXcQ%26t%3D30s&format=json"
        );
        let vimeo = parse_media_url("https://vimeo.com/76979871/abc123def0").unwrap();
        assert_eq!(
            oembed_url(&vimeo).unwrap(),
            "https://vimeo.com/api/oembed.json?url=https%3A%2F%2Fvimeo.com%2F76979871%2Fabc123def0"
        );
        assert_eq!(
            oembed_url(&ParsedMedia {
                provider: Provider::Drive,
                media_id: "abc".into(),
                start: None
            }),
            None
        );
    }

    #[test]
    fn an_oembed_answer_is_read_as_documented() {
        assert_eq!(
            verdict_from(200, br#"{"title":" A talk \u0007 ","type":"video"}"#),
            EmbedVerdict::Embeddable {
                title: Some("A talk".to_string())
            }
        );
        assert_eq!(
            verdict_from(200, br#"{"type":"video"}"#),
            EmbedVerdict::Embeddable { title: None }
        );
        assert_eq!(
            verdict_from(200, br#"{"title":"T","domain_status_code":403}"#),
            EmbedVerdict::Refused(NOT_EMBEDDABLE_HERE)
        );
        assert_eq!(
            verdict_from(200, br#"{"title":"T","domain_status_code":200}"#),
            EmbedVerdict::Embeddable {
                title: Some("T".to_string())
            }
        );
        assert_eq!(
            verdict_from(401, b"Unauthorized"),
            EmbedVerdict::Refused(NOT_EMBEDDABLE)
        );
        assert_eq!(
            verdict_from(403, b"Forbidden"),
            EmbedVerdict::Refused(NOT_EMBEDDABLE)
        );
        assert_eq!(
            verdict_from(400, b"Bad Request"),
            EmbedVerdict::Refused(VIDEO_NOT_FOUND)
        );
        assert_eq!(
            verdict_from(404, b"Not Found"),
            EmbedVerdict::Refused(VIDEO_NOT_FOUND)
        );
        // Nothing our own trouble, or the host's, could be mistaken for.
        for status in [301, 302, 429, 500, 502, 503] {
            assert_eq!(
                verdict_from(status, b"{}"),
                EmbedVerdict::Unknown,
                "{status}"
            );
        }
        assert_eq!(verdict_from(200, b"<html>"), EmbedVerdict::Unknown);
        assert_eq!(verdict_from(200, b"[1,2]"), EmbedVerdict::Unknown);
    }

    fn link(password_hash: Option<&str>) -> links::Model {
        links::Model {
            id: 7,
            code: "media1".to_string(),
            original_url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
            user_id: Some(1),
            created_at: Utc::now().naive_utc(),
            click_count: 0,
            expires_at: None,
            password_hash: password_hash.map(str::to_string),
            title: None,
            notes: None,
            folder_id: None,
            org_id: None,
            starts_at: None,
            max_clicks: None,
            deleted_at: None,
            is_pinned: false,
            burn_after_reading: false,
            burned_at: None,
            safe_link_interstitial: false,
            bio_visible: false,
            bio_label: None,
            bio_position: None,
            expiry_notice_sent_at: None,
        }
    }

    #[test]
    fn a_ticket_is_for_one_link_one_password_and_one_hour() {
        const SECRET: &str = "media-ticket-test-secret-0123456789abcdef";
        let valid = |ticket: &str, link: &links::Model| verify_ticket(SECRET, ticket, link);

        let open = link(None);
        let ticket = sign_ticket(SECRET, &open, MEDIA_TICKET_TTL_SECONDS).unwrap();
        assert!(valid(&ticket, &open));
        // Reusable: nothing is used up by checking it.
        assert!(valid(&ticket, &open));

        let mut other = link(None);
        other.id = 8;
        assert!(!valid(&ticket, &other));
        let mut renamed = link(None);
        renamed.code = "media2".to_string();
        assert!(!valid(&ticket, &renamed));
        assert!(!valid(&ticket, &link(Some("$2b$hash"))));

        let locked = link(Some("$2b$hash"));
        let locked_ticket = sign_ticket(SECRET, &locked, MEDIA_TICKET_TTL_SECONDS).unwrap();
        assert!(valid(&locked_ticket, &locked));
        assert!(!valid(&locked_ticket, &link(Some("$2b$other"))));
        assert!(!valid(&locked_ticket, &open));

        let ended = sign_ticket(SECRET, &open, -5).unwrap();
        assert!(!valid(&ended, &open));
        assert!(!valid("", &open));
        assert!(!valid("not.a.ticket", &open));
        // Signed with another secret.
        let foreign = sign_ticket(
            "another-secret-another-secret-0123456789",
            &open,
            MEDIA_TICKET_TTL_SECONDS,
        )
        .unwrap();
        assert!(!valid(&foreign, &open));

        // A token with every claim of a ticket for this link, same secret,
        // but issued for something else (a password unlock, a session): the
        // purpose is the only thing that tells them apart.
        let exp = usize::try_from(Utc::now().timestamp() + MEDIA_TICKET_TTL_SECONDS).unwrap();
        let claims_for = |purpose: &str| MediaClaims {
            sub: purpose.to_string(),
            exp,
            link_id: open.id,
            code: open.code.clone(),
            password_fingerprint: link_fingerprint(&open),
        };
        let signed = |claims: &MediaClaims| {
            encode(
                &Header::new(Algorithm::HS256),
                claims,
                &EncodingKey::from_secret(SECRET.as_bytes()),
            )
            .unwrap()
        };
        assert!(valid(&signed(&claims_for(TICKET_PURPOSE)), &open));
        for purpose in ["link_unlock", "view_ticket", "", "media_ticket "] {
            assert!(
                !valid(&signed(&claims_for(purpose)), &open),
                "a token issued as {purpose:?} is not a media ticket"
            );
        }
    }

    #[test]
    fn the_ticket_lives_an_hour() {
        assert_eq!(MEDIA_TICKET_TTL_SECONDS, 3600);
    }
}
