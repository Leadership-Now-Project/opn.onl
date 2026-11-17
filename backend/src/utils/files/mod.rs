//! File sharing: an uploaded file served by a short link.
//!
//! The bytes never pass through this process. The browser uploads straight to
//! a private bucket with a signed URL, and a visitor who opens the short link
//! is redirected to a signed URL that lives for a few minutes. Everything the
//! rest of the app needs from the bucket is the [`FileStore`] trait, so later
//! features (a viewer, other upload sources) reuse it and tests run on
//! [`MemoryFileStore`].
//!
//! The feature is off until `FILES_S3_BUCKET` is set. Off means the `/files`
//! routes answer 404 and nothing else behaves differently.

mod memory;
mod s3;

pub use memory::{HeadGate, MemoryFileStore, SignedDownload};
pub use s3::S3FileStore;

use axum::body::Bytes;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait,
    JoinType, QueryFilter, QuerySelect, RelationTrait, Statement,
};
use std::sync::Arc;
use std::time::Duration;

use crate::AppState;
use crate::entity::{link_files, links};

/// 500 MB.
pub const DEFAULT_MAX_BYTES: u64 = 524_288_000;
pub const DEFAULT_UPLOAD_URL_TTL_SECS: u64 = 900;
pub const DEFAULT_DOWNLOAD_URL_TTL_SECS: u64 = 300;
pub const DEFAULT_REGION: &str = "us-east-2";
/// The longest a signed URL may live. S3 refuses anything longer.
const MAX_URL_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// An upload that was started and never completed is removed after this long.
pub const PENDING_UPLOAD_HOURS: i64 = 24;

/// What a served file is called when nothing usable is left of its name.
const FALLBACK_FILENAME: &str = "file";
/// In UTF-8 bytes, not characters. The name travels percent-encoded inside
/// the signed address a visitor is redirected to, where one non-ASCII
/// character costs up to fifteen bytes; this keeps that `Location` header
/// near 1 kB for the name, well inside a proxy's 4 kB header buffer.
const MAX_FILENAME_BYTES: usize = 180;
const MAX_CONTENT_TYPE_CHARS: usize = 100;
pub const OCTET_STREAM: &str = "application/octet-stream";

/// The settings read from `FILES_*`. `None` from [`FilesConfig::from_lookup`]
/// means the feature is off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    /// Both keys, or neither. Neither means the AWS default credential chain
    /// (environment, shared profile, container or EC2 instance role).
    pub static_keys: Option<(String, String)>,
    pub max_bytes: u64,
    pub upload_url_ttl: Duration,
    pub download_url_ttl: Duration,
}

impl FilesConfig {
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Compose passes an option left alone as an empty string, so blank is
    /// unset everywhere here, and a blank bucket is the feature switched off.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let get = |name: &str| lookup(name).filter(|value| !value.trim().is_empty());
        let number = |name: &str, default: u64| {
            get(name)
                .and_then(|raw| raw.trim().parse::<u64>().ok())
                .filter(|&n| n > 0)
                .unwrap_or(default)
        };

        let bucket = get("FILES_S3_BUCKET")?.trim().to_string();
        let static_keys = match (get("FILES_S3_ACCESS_KEY"), get("FILES_S3_SECRET_KEY")) {
            (Some(access), Some(secret)) => Some((access, secret)),
            _ => None,
        };
        Some(Self {
            bucket,
            region: get("FILES_S3_REGION")
                .map(|region| region.trim().to_string())
                .unwrap_or_else(|| DEFAULT_REGION.to_string()),
            endpoint: get("FILES_S3_ENDPOINT").map(|endpoint| endpoint.trim().to_string()),
            static_keys,
            max_bytes: number("FILES_MAX_BYTES", DEFAULT_MAX_BYTES),
            upload_url_ttl: Duration::from_secs(
                number("FILES_UPLOAD_URL_TTL_SECS", DEFAULT_UPLOAD_URL_TTL_SECS)
                    .min(MAX_URL_TTL_SECS),
            ),
            download_url_ttl: Duration::from_secs(
                number("FILES_DOWNLOAD_URL_TTL_SECS", DEFAULT_DOWNLOAD_URL_TTL_SECS)
                    .min(MAX_URL_TTL_SECS),
            ),
        })
    }
}

/// File sharing as the handlers see it: where the objects are, and the limits.
/// `AppState::files` is `None` while the feature is off.
#[derive(Clone)]
pub struct FileSharing {
    pub store: Arc<dyn FileStore>,
    pub max_bytes: u64,
    pub upload_url_ttl: Duration,
    pub download_url_ttl: Duration,
    /// Secure View: the largest PDF a protected copy is made of
    /// (`SECURE_VIEW_PROTECT_MAX_BYTES`). A larger one is shown from the
    /// original.
    pub protect_max_bytes: u64,
    /// Secure View: the memory budget of the protected-copy step
    /// (`SECURE_VIEW_PROTECT_BUDGET_BYTES`, see
    /// `secure_view::ProtectLimits::budget_bytes`). A PDF that needs more is
    /// shown from the original.
    pub protect_budget_bytes: u64,
}

impl FileSharing {
    /// The real bucket, or `None` when `FILES_S3_BUCKET` is not set.
    pub async fn from_env() -> Option<Self> {
        let config = FilesConfig::from_env()?;
        let store = S3FileStore::connect(&config).await;
        Some(Self {
            store: Arc::new(store),
            max_bytes: config.max_bytes,
            upload_url_ttl: config.upload_url_ttl,
            download_url_ttl: config.download_url_ttl,
            protect_max_bytes: crate::utils::env::env_parse(
                "SECURE_VIEW_PROTECT_MAX_BYTES",
                crate::utils::secure_view::DEFAULT_PROTECT_MAX_BYTES,
            ),
            protect_budget_bytes: crate::utils::env::env_parse(
                "SECURE_VIEW_PROTECT_BUDGET_BYTES",
                crate::utils::secure_view::DEFAULT_PROTECT_BUDGET_BYTES,
            ),
        })
    }

    /// File sharing on `store` with the default limits. Tests build theirs
    /// from this and change the fields they need.
    pub fn with_store(store: Arc<dyn FileStore>) -> Self {
        Self {
            store,
            max_bytes: DEFAULT_MAX_BYTES,
            upload_url_ttl: Duration::from_secs(DEFAULT_UPLOAD_URL_TTL_SECS),
            download_url_ttl: Duration::from_secs(DEFAULT_DOWNLOAD_URL_TTL_SECS),
            protect_max_bytes: crate::utils::secure_view::DEFAULT_PROTECT_MAX_BYTES,
            protect_budget_bytes: crate::utils::secure_view::DEFAULT_PROTECT_BUDGET_BYTES,
        }
    }
}

/// A storage call that did not get an answer it could use. The text is for the
/// log, never for a client.
#[derive(Debug, thiserror::Error)]
#[error("file storage: {0}")]
pub struct StoreError(pub String);

/// A signed upload: the URL to `PUT` the bytes to, and the headers that
/// request must carry for the signature to match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresignedUpload {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// An object being read from the bucket: its size when the bucket said, and
/// its bytes as they arrive.
pub struct StoredObject {
    pub size: Option<u64>,
    pub body: BoxStream<'static, Result<Bytes, StoreError>>,
}

/// The bucket, as much of it as the app uses.
pub trait FileStore: Send + Sync {
    /// A URL the browser can `PUT` exactly `content_length` bytes of
    /// `content_type` to, for `ttl`.
    fn presign_put<'a>(
        &'a self,
        key: &'a str,
        content_type: &'a str,
        content_length: u64,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<PresignedUpload, StoreError>>;

    /// A URL that returns the object for `ttl`, with the response's
    /// `Content-Type` and `Content-Disposition` replaced by the ones given.
    fn presign_get<'a>(
        &'a self,
        key: &'a str,
        response_content_type: &'a str,
        response_content_disposition: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<String, StoreError>>;

    /// The object's size in bytes, or `None` when there is no such object.
    fn head<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<u64>, StoreError>>;

    /// Remove the object. Removing one that is not there succeeds.
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), StoreError>>;

    /// The object's bytes as a stream, or `None` when there is no such
    /// object. Only the Secure View viewer reads a file through this process;
    /// every other visitor is redirected to a signed address.
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<StoredObject>, StoreError>>;

    /// Store `bytes` at `key`. Used for the protected copy of a PDF; uploads
    /// go straight from the browser to the bucket.
    fn put<'a>(
        &'a self,
        key: &'a str,
        bytes: Bytes,
        content_type: &'a str,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
}

/// A new object key: random, and nothing of the filename in it.
pub fn new_storage_key() -> String {
    format!("files/{}", uuid::Uuid::new_v4().simple())
}

/// The uploader's filename as it is stored: the last path segment, without
/// control characters, at most 180 bytes with the extension kept.
pub fn clean_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    if cleaned.is_empty() {
        return FALLBACK_FILENAME.to_string();
    }
    if cleaned.len() <= MAX_FILENAME_BYTES {
        return cleaned.to_string();
    }
    // Too long: keep the extension, since it is what decides how the file is
    // served and what the visitor's computer opens it with.
    let extension = cleaned
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .filter(|ext| !ext.is_empty() && ext.len() <= 20);
    match extension {
        Some(ext) => {
            let stem = first_bytes(cleaned, MAX_FILENAME_BYTES - ext.len() - 1);
            format!("{}.{ext}", stem.trim_end())
        }
        None => first_bytes(cleaned, MAX_FILENAME_BYTES)
            .trim_end()
            .to_string(),
    }
}

/// The longest prefix of `text` that fits in `max` bytes without cutting a
/// character in half.
fn first_bytes(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The browser-declared content type as it is stored and signed into the
/// upload URL: lower case, parameters dropped, and `application/octet-stream`
/// for anything that is not a plain `type/subtype`.
pub fn clean_content_type(raw: &str) -> String {
    let essence = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let token = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "!#$&-^_.+".contains(c))
    };
    match essence.split_once('/') {
        Some((kind, subtype))
            if essence.len() <= MAX_CONTENT_TYPE_CHARS && token(kind) && token(subtype) =>
        {
            essence
        }
        _ => OCTET_STREAM.to_string(),
    }
}

/// The types a browser is allowed to show in place, each with the filename
/// extensions that go with it. Everything else is downloaded.
const INLINE_TYPES: &[(&str, &[&str])] = &[
    ("application/pdf", &["pdf"]),
    ("image/png", &["png"]),
    ("image/jpeg", &["jpg", "jpeg"]),
    ("image/gif", &["gif"]),
    ("image/webp", &["webp"]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Inline,
    Attachment,
}

/// How a stored file is sent to a visitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServedAs {
    pub content_type: &'static str,
    pub disposition: Disposition,
}

/// Decide how a file is served from what was stored about it.
///
/// Shown in the browser only when the stored content type is on the short
/// list above AND the filename's last extension belongs to that type. The
/// content type came from the uploader's browser, so on its own it proves
/// nothing: a `.html` file declared as `image/png` is downloaded. Everything
/// that is downloaded goes out as `application/octet-stream`, so no browser is
/// handed HTML, SVG, XML or script to run under a name on this site's link.
pub fn served_as(stored_content_type: &str, filename: &str) -> ServedAs {
    let declared = stored_content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let extension = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.trim().to_ascii_lowercase())
        .unwrap_or_default();
    for (content_type, extensions) in INLINE_TYPES {
        if declared == *content_type && extensions.contains(&extension.as_str()) {
            return ServedAs {
                content_type,
                disposition: Disposition::Inline,
            };
        }
    }
    ServedAs {
        content_type: OCTET_STREAM,
        disposition: Disposition::Attachment,
    }
}

/// The `Content-Disposition` value for `filename`.
///
/// Two spellings, as RFC 6266 recommends: `filename=` holds an ASCII
/// approximation in quotes, and `filename*=` the real name percent-encoded as
/// UTF-8 (RFC 5987). Nothing of the name is written raw apart from ASCII
/// letters, digits and a few punctuation marks, so a quote, a backslash, a
/// semicolon or a line break in it cannot end the value or start a header.
pub fn content_disposition(disposition: Disposition, filename: &str) -> String {
    let kind = match disposition {
        Disposition::Inline => "inline",
        Disposition::Attachment => "attachment",
    };
    let name = clean_filename(filename);
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " !#$&'()+,-.=@[]^_`{}~".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// The signed URL a visitor is redirected to for `file`.
pub async fn download_url(
    files: &FileSharing,
    file: &link_files::Model,
) -> Result<String, StoreError> {
    let served = served_as(&file.content_type, &file.filename);
    let disposition = content_disposition(served.disposition, &file.filename);
    files
        .store
        .presign_get(
            &file.storage_key,
            served.content_type,
            &disposition,
            files.download_url_ttl,
        )
        .await
}

/// Which files a sweep looks at.
#[derive(Clone, Debug)]
pub enum Sweep {
    /// Every file. The daily task.
    All,
    /// Only these `link_files` ids. A handler that has just deleted links.
    Files(Vec<i32>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Objects removed from the bucket, their rows marked deleted.
    pub removed: u64,
    /// Objects the bucket would not remove. Their rows are unchanged, so the
    /// next sweep tries again.
    pub failed: u64,
}

const SWEEP_BATCH: i64 = 500;

/// Remove from the bucket every file in `scope` that nothing can serve any
/// more, and mark its row deleted. A file is dead when:
///
/// * its link is soft-deleted, has expired, or no longer exists; or
/// * its upload was started more than [`PENDING_UPLOAD_HOURS`] ago and never
///   completed.
///
/// The rule is checked here, in one statement, whatever the caller thinks it
/// has just deleted: a file whose link is live is never touched. An object the
/// bucket fails to remove keeps its row as it was, which is what makes the
/// next sweep find it again.
pub async fn remove_dead_files(
    db: &DatabaseConnection,
    store: &dyn FileStore,
    scope: Sweep,
) -> Result<SweepReport, DbErr> {
    let ids = match scope {
        Sweep::All => None,
        Sweep::Files(ids) if ids.is_empty() => return Ok(SweepReport::default()),
        Sweep::Files(ids) => Some(ids),
    };
    let now = chrono::Utc::now().naive_utc();
    let stale = now - chrono::Duration::hours(PENDING_UPLOAD_HOURS);

    let mut report = SweepReport::default();
    let mut after_id = 0;
    loop {
        let scope_sql = if ids.is_some() {
            "AND lf.id = ANY($5)"
        } else {
            ""
        };
        let sql = format!(
            r#"SELECT lf.id, lf.storage_key, lf.protected_key
               FROM link_files lf
               LEFT JOIN links l ON l.id = lf.link_id
               WHERE lf.deleted_at IS NULL
                 AND lf.id > $1
                 AND (
                      (lf.status = 'ready'
                       AND (l.id IS NULL OR l.deleted_at IS NOT NULL OR l.expires_at < $2))
                   OR (lf.status = 'pending' AND lf.created_at < $3)
                 )
                 {scope_sql}
               ORDER BY lf.id
               LIMIT $4"#
        );
        let mut values: Vec<sea_orm::Value> = vec![
            after_id.into(),
            now.into(),
            stale.into(),
            SWEEP_BATCH.into(),
        ];
        if let Some(ids) = &ids {
            values.push(ids.clone().into());
        }
        let rows = db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?;
        let batch_len = rows.len() as i64;
        for row in rows {
            let id: i32 = row.try_get("", "id")?;
            let key: String = row.try_get("", "storage_key")?;
            let protected_key: Option<String> = row.try_get("", "protected_key")?;
            after_id = id;
            // The protected copy of a Secure View PDF goes with the original,
            // and first: the row is marked deleted only when both are gone.
            let mut removed = Ok(());
            if let Some(protected_key) = &protected_key {
                removed = store.delete(protected_key).await;
            }
            if removed.is_ok() {
                removed = store.delete(&key).await;
            }
            match removed {
                Ok(()) => {
                    // Only if the row still names the copy that was just
                    // removed. A copy recorded since the row was read is
                    // still in the bucket; leaving the row as it is makes the
                    // next sweep remove it.
                    let marked = db
                        .execute(Statement::from_sql_and_values(
                            DbBackend::Postgres,
                            "UPDATE link_files SET deleted_at = $1 \
                             WHERE id = $2 AND protected_key IS NOT DISTINCT FROM $3",
                            [now.into(), id.into(), protected_key.into()],
                        ))
                        .await?;
                    if marked.rows_affected() == 1 {
                        report.removed += 1;
                    } else {
                        report.failed += 1;
                    }
                }
                Err(error) => {
                    tracing::error!("Failed to remove a stored file, will retry: {error}");
                    report.failed += 1;
                }
            }
        }
        if batch_len < SWEEP_BATCH {
            return Ok(report);
        }
    }
}

/// The files of a set of links, noted BEFORE those links are deleted, so the
/// objects can be removed once the delete has committed. A hard delete clears
/// `link_files.link_id`, after which the rows could not be found by link.
#[derive(Debug, Default)]
pub struct FilesOfLinks(Vec<i32>);

impl FilesOfLinks {
    /// The files of the links matching `links_filter`. Nothing while file
    /// sharing is off, and nothing on a database error (the daily sweep then
    /// removes whatever this would have).
    pub async fn find<C: ConnectionTrait>(
        state: &AppState,
        db: &C,
        links_filter: Condition,
    ) -> Self {
        if state.files.is_none() {
            return Self::default();
        }
        let ids = link_files::Entity::find()
            .select_only()
            .column(link_files::Column::Id)
            .join(JoinType::InnerJoin, link_files::Relation::Link.def())
            .filter(link_files::Column::DeletedAt.is_null())
            .filter(links_filter)
            .into_tuple::<i32>()
            .all(db)
            .await;
        match ids {
            Ok(ids) => Self(ids),
            Err(error) => {
                tracing::error!("Failed to look up the files of deleted links: {error}");
                Self::default()
            }
        }
    }

    /// The files of the links with these ids.
    pub async fn of_links(state: &AppState, link_ids: &[i32]) -> Self {
        if link_ids.is_empty() {
            return Self::default();
        }
        Self::find(
            state,
            &state.db,
            Condition::all().add(links::Column::Id.is_in(link_ids.to_vec())),
        )
        .await
    }

    /// The files of every link a user's deletion can take down: the links they
    /// created, and the links of organizations they own.
    pub async fn of_account(state: &AppState, user_id: i32) -> Self {
        use crate::entity::organizations;
        use sea_orm::sea_query::Query;

        let owned_orgs = Query::select()
            .column(organizations::Column::Id)
            .from(organizations::Entity)
            .and_where(organizations::Column::OwnerId.eq(user_id))
            .to_owned();
        Self::find(
            state,
            &state.db,
            Condition::any()
                .add(links::Column::UserId.eq(user_id))
                .add(links::Column::OrgId.in_subquery(owned_orgs)),
        )
        .await
    }

    /// Remove the objects of those files whose link is now gone. Call it after
    /// the delete has committed. It never fails the request: an object that
    /// could not be removed is logged and left for the daily sweep.
    pub async fn remove(self, state: &AppState) {
        let Some(files) = &state.files else {
            return;
        };
        if let Err(error) =
            remove_dead_files(&state.db, files.store.as_ref(), Sweep::Files(self.0)).await
        {
            tracing::error!("Failed to remove the files of deleted links: {error}");
        }
    }
}

/// Once at start and then daily: remove the objects of deleted and expired
/// links, of links that were purged, and of uploads that were never completed.
pub fn spawn_cleanup_task(db: DatabaseConnection, store: Arc<dyn FileStore>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        loop {
            interval.tick().await;
            match remove_dead_files(&db, store.as_ref(), Sweep::All).await {
                Ok(SweepReport {
                    removed: 0,
                    failed: 0,
                }) => {}
                Ok(report) => tracing::info!(
                    "File sweep removed {} stored files ({} could not be removed and will be retried)",
                    report.removed,
                    report.failed
                ),
                Err(error) => tracing::error!("File sweep failed: {error}"),
            }
            // Secure View: the ids of tickets that opened a view are only
            // needed until the ticket would have expired.
            if let Err(error) = crate::utils::secure_view::forget_expired_tickets(&db).await {
                tracing::error!("Could not forget used Secure View tickets: {error}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Option<FilesConfig> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        FilesConfig::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn no_bucket_or_a_blank_bucket_is_the_feature_off() {
        assert_eq!(config(&[]), None);
        assert_eq!(config(&[("FILES_S3_BUCKET", "")]), None);
        assert_eq!(config(&[("FILES_S3_BUCKET", "   ")]), None);
        // The other settings do not switch it on.
        assert_eq!(
            config(&[
                ("FILES_S3_REGION", "us-east-2"),
                ("FILES_S3_ACCESS_KEY", "k"),
                ("FILES_S3_SECRET_KEY", "s"),
                ("FILES_MAX_BYTES", "10"),
            ]),
            None
        );
    }

    #[test]
    fn a_bucket_alone_gets_the_defaults() {
        let config = config(&[("FILES_S3_BUCKET", "lnp-files")]).expect("on");
        assert_eq!(config.bucket, "lnp-files");
        assert_eq!(config.region, "us-east-2");
        assert_eq!(config.endpoint, None);
        assert_eq!(config.static_keys, None);
        assert_eq!(config.max_bytes, 524_288_000);
        assert_eq!(config.upload_url_ttl, Duration::from_secs(900));
        assert_eq!(config.download_url_ttl, Duration::from_secs(300));
    }

    #[test]
    fn blank_options_are_unset_and_bad_numbers_fall_back() {
        let config = config(&[
            ("FILES_S3_BUCKET", "b"),
            ("FILES_S3_REGION", ""),
            ("FILES_S3_ENDPOINT", " "),
            ("FILES_S3_ACCESS_KEY", "only-one-key"),
            ("FILES_S3_SECRET_KEY", ""),
            ("FILES_MAX_BYTES", "lots"),
            ("FILES_UPLOAD_URL_TTL_SECS", "0"),
            ("FILES_DOWNLOAD_URL_TTL_SECS", ""),
        ])
        .expect("on");
        assert_eq!(config.region, "us-east-2");
        assert_eq!(config.endpoint, None);
        assert_eq!(
            config.static_keys, None,
            "one key without the other is not a credential"
        );
        assert_eq!(config.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(config.upload_url_ttl, Duration::from_secs(900));
        assert_eq!(config.download_url_ttl, Duration::from_secs(300));
    }

    #[test]
    fn set_options_are_read() {
        let config = config(&[
            ("FILES_S3_BUCKET", "b"),
            ("FILES_S3_REGION", "eu-west-3"),
            ("FILES_S3_ENDPOINT", "https://s3.example.test"),
            ("FILES_S3_ACCESS_KEY", "access"),
            ("FILES_S3_SECRET_KEY", "secret"),
            ("FILES_MAX_BYTES", "1048576"),
            ("FILES_UPLOAD_URL_TTL_SECS", "60"),
            ("FILES_DOWNLOAD_URL_TTL_SECS", "999999999"),
        ])
        .expect("on");
        assert_eq!(config.region, "eu-west-3");
        assert_eq!(config.endpoint.as_deref(), Some("https://s3.example.test"));
        assert_eq!(
            config.static_keys,
            Some(("access".to_string(), "secret".to_string()))
        );
        assert_eq!(config.max_bytes, 1_048_576);
        assert_eq!(config.upload_url_ttl, Duration::from_secs(60));
        assert_eq!(
            config.download_url_ttl,
            Duration::from_secs(MAX_URL_TTL_SECS),
            "a signed URL cannot outlive what S3 allows"
        );
    }

    #[test]
    fn storage_keys_are_random_and_say_nothing_about_the_file() {
        let a = new_storage_key();
        let b = new_storage_key();
        assert_ne!(a, b);
        for key in [&a, &b] {
            let id = key.strip_prefix("files/").expect("under files/");
            assert_eq!(id.len(), 32);
            assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    fn inline(content_type: &'static str) -> ServedAs {
        ServedAs {
            content_type,
            disposition: Disposition::Inline,
        }
    }

    const DOWNLOAD: ServedAs = ServedAs {
        content_type: OCTET_STREAM,
        disposition: Disposition::Attachment,
    };

    #[test]
    fn the_five_safe_types_are_shown_when_the_extension_agrees() {
        assert_eq!(
            served_as("application/pdf", "report.pdf"),
            inline("application/pdf")
        );
        assert_eq!(served_as("image/png", "a.png"), inline("image/png"));
        assert_eq!(served_as("image/jpeg", "a.jpg"), inline("image/jpeg"));
        assert_eq!(served_as("image/jpeg", "a.jpeg"), inline("image/jpeg"));
        assert_eq!(served_as("image/gif", "a.gif"), inline("image/gif"));
        assert_eq!(served_as("image/webp", "a.webp"), inline("image/webp"));
    }

    #[test]
    fn case_and_parameters_do_not_change_the_answer() {
        assert_eq!(
            served_as("Application/PDF", "REPORT.PDF"),
            inline("application/pdf")
        );
        assert_eq!(served_as("IMAGE/PNG", "Photo.PnG"), inline("image/png"));
        assert_eq!(
            served_as("image/png; charset=binary", "a.png"),
            inline("image/png")
        );
    }

    #[test]
    fn a_type_and_an_extension_that_disagree_are_downloaded() {
        // The declared type is the uploader's word.
        assert_eq!(served_as("image/png", "page.html"), DOWNLOAD);
        assert_eq!(served_as("image/png", "page.HTML"), DOWNLOAD);
        assert_eq!(served_as("application/pdf", "page.htm"), DOWNLOAD);
        // And so is the name.
        assert_eq!(served_as("text/html", "report.pdf"), DOWNLOAD);
        assert_eq!(served_as("image/svg+xml", "picture.png"), DOWNLOAD);
        // Two safe types that do not go together.
        assert_eq!(served_as("image/png", "a.jpg"), DOWNLOAD);
        assert_eq!(served_as("application/pdf", "a.png"), DOWNLOAD);
    }

    #[test]
    fn active_and_unknown_types_are_always_downloaded() {
        for (content_type, filename) in [
            ("text/html", "page.html"),
            ("application/xhtml+xml", "page.xhtml"),
            ("image/svg+xml", "logo.svg"),
            ("text/xml", "feed.xml"),
            ("application/xml", "feed.xml"),
            ("text/javascript", "app.js"),
            ("application/javascript", "app.js"),
            ("text/plain", "notes.txt"),
            ("text/css", "site.css"),
            ("application/zip", "bundle.zip"),
            ("video/mp4", "clip.mp4"),
            ("application/octet-stream", "report.pdf"),
            ("", "report.pdf"),
            ("nonsense", "a.png"),
        ] {
            assert_eq!(
                served_as(content_type, filename),
                DOWNLOAD,
                "{content_type:?} {filename:?}"
            );
        }
    }

    #[test]
    fn only_the_last_extension_counts() {
        assert_eq!(served_as("application/pdf", "report.pdf.html"), DOWNLOAD);
        assert_eq!(served_as("image/png", "photo.png.svg"), DOWNLOAD);
        assert_eq!(
            served_as("image/png", "page.html.png"),
            inline("image/png"),
            "sent as image/png, so the browser draws it or shows a broken image"
        );
    }

    #[test]
    fn no_extension_is_downloaded() {
        assert_eq!(served_as("application/pdf", "report"), DOWNLOAD);
        assert_eq!(served_as("image/png", "png"), DOWNLOAD);
        assert_eq!(served_as("image/png", "photo."), DOWNLOAD);
        assert_eq!(served_as("image/png", ""), DOWNLOAD);
    }

    #[test]
    fn filenames_lose_their_path_and_control_characters() {
        assert_eq!(clean_filename("report.pdf"), "report.pdf");
        assert_eq!(clean_filename("C:\\Users\\me\\report.pdf"), "report.pdf");
        assert_eq!(clean_filename("../../etc/passwd"), "passwd");
        assert_eq!(clean_filename("a\r\nb\t.txt"), "ab.txt");
        assert_eq!(clean_filename("  spaced name.docx  "), "spaced name.docx");
        assert_eq!(clean_filename(""), "file");
        assert_eq!(clean_filename("   "), "file");
        assert_eq!(clean_filename(".."), "file");
        assert_eq!(clean_filename("dir/"), "file");
        assert_eq!(clean_filename("résumé.pdf"), "résumé.pdf");
    }

    #[test]
    fn very_long_filenames_are_cut_and_keep_their_extension() {
        let long = format!("{}.pdf", "a".repeat(500));
        let cleaned = clean_filename(&long);
        assert_eq!(cleaned.len(), 180);
        assert!(cleaned.ends_with(".pdf"));

        // The limit is in bytes, and a character is never cut in half.
        let accents = format!("{}.pdf", "é".repeat(500));
        let cleaned = clean_filename(&accents);
        assert_eq!(cleaned.len(), 180);
        assert_eq!(cleaned, format!("{}.pdf", "é".repeat(88)));
        let kanji = "日".repeat(500);
        assert_eq!(clean_filename(&kanji), "日".repeat(60));
        let odd_width = format!("a{}", "日".repeat(500));
        assert_eq!(clean_filename(&odd_width), format!("a{}", "日".repeat(59)));

        // An "extension" that is itself enormous is not kept as one.
        let odd = format!("a.{}", "b".repeat(500));
        assert_eq!(clean_filename(&odd).len(), 180);

        // A name exactly at the limit is left alone.
        let exact = format!("{}.pdf", "a".repeat(176));
        assert_eq!(clean_filename(&exact), exact);
    }

    #[test]
    fn content_types_are_normalised() {
        assert_eq!(clean_content_type("application/pdf"), "application/pdf");
        assert_eq!(clean_content_type(" Image/PNG "), "image/png");
        assert_eq!(
            clean_content_type("text/html; charset=utf-8"),
            "text/html",
            "parameters are dropped"
        );
        assert_eq!(clean_content_type("image/svg+xml"), "image/svg+xml");
        for bad in [
            "",
            "pdf",
            "/",
            "a/",
            "/b",
            "a/b/c",
            "text/html\r\nX-Evil: 1",
            "te xt/html",
            "text/\"html\"",
        ] {
            assert_eq!(clean_content_type(bad), OCTET_STREAM, "{bad:?}");
        }
        let long = format!("application/{}", "x".repeat(200));
        assert_eq!(clean_content_type(&long), OCTET_STREAM);
    }

    #[test]
    fn disposition_names_the_kind_and_both_filename_spellings() {
        assert_eq!(
            content_disposition(Disposition::Inline, "report.pdf"),
            "inline; filename=\"report.pdf\"; filename*=UTF-8''report.pdf"
        );
        assert_eq!(
            content_disposition(Disposition::Attachment, "Q3 report (final).xlsx"),
            "attachment; filename=\"Q3 report (final).xlsx\"; \
             filename*=UTF-8''Q3%20report%20%28final%29.xlsx"
        );
    }

    #[test]
    fn quotes_backslashes_and_semicolons_cannot_break_out_of_the_value() {
        let value = content_disposition(Disposition::Attachment, "a\"; filename=\"evil.html\".pdf");
        assert_eq!(
            value,
            "attachment; filename=\"a__ filename=_evil.html_.pdf\"; \
             filename*=UTF-8''a%22%3B%20filename%3D%22evil.html%22.pdf"
        );
        // Exactly the two quotes that delimit the ASCII name.
        assert_eq!(value.matches('"').count(), 2);

        // A backslash is a path separator to `clean_filename`, so it never
        // gets as far as escaping the closing quote.
        let value = content_disposition(Disposition::Attachment, "evil\\\".pdf");
        assert_eq!(
            value,
            "attachment; filename=\"_.pdf\"; filename*=UTF-8''%22.pdf"
        );
        assert!(!value.contains('\\'));
    }

    #[test]
    fn line_breaks_never_reach_the_header() {
        for name in [
            "a\r\nSet-Cookie: x=1.pdf",
            "a\nb.pdf",
            "a\rb.pdf",
            "a\u{0}b.pdf",
            "a\u{7f}b.pdf",
        ] {
            let value = content_disposition(Disposition::Attachment, name);
            assert!(
                value.chars().all(|c| !c.is_control()),
                "{name:?} gave {value:?}"
            );
            assert!(value.is_ascii());
        }
        // Stored names are already clean; this is the same rule applied again.
        assert_eq!(
            content_disposition(Disposition::Attachment, "a\r\nb.pdf"),
            "attachment; filename=\"ab.pdf\"; filename*=UTF-8''ab.pdf"
        );
    }

    #[test]
    fn unicode_names_are_percent_encoded_and_approximated() {
        let value = content_disposition(Disposition::Inline, "résumé 日本.pdf");
        assert!(value.is_ascii());
        assert_eq!(
            value,
            "inline; filename=\"r_sum_ __.pdf\"; \
             filename*=UTF-8''r%C3%A9sum%C3%A9%20%E6%97%A5%E6%9C%AC.pdf"
        );
    }

    #[test]
    fn percent_signs_are_encoded_so_they_cannot_forge_an_escape() {
        let value = content_disposition(Disposition::Attachment, "100%0d%0a.txt");
        assert!(value.ends_with("filename*=UTF-8''100%250d%250a.txt"));
        assert!(value.contains("filename=\"100_0d_0a.txt\""));
    }

    #[test]
    fn very_long_names_stay_bounded() {
        // The worst a name can cost: three-byte characters, every byte of
        // which is percent-encoded here and again when the value goes into
        // the signed URL's query.
        let value = content_disposition(Disposition::Attachment, &"日".repeat(5_000));
        assert!(value.len() < 60 + 180 * 3 + 64, "{}", value.len());
        let in_a_url = urlencoding::encode(&value);
        assert!(in_a_url.len() < 1_300, "{}", in_a_url.len());
    }
}
