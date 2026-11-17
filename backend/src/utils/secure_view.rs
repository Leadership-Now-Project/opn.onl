//! Secure View: a file link that opens a viewer instead of handing over the
//! file.
//!
//! `GET /{code}` on such a link gives the visitor a **view ticket** and sends
//! them to the viewer page. The viewer trades the ticket and an email address
//! for a **view token** (`POST /{code}/view`, which is where the visit is
//! counted and a one-time link is used up), and reads the bytes with that
//! token (`GET /{code}/view/file`). Both are short-lived signed tokens bound
//! to the link, its file, its password and the turn of the Secure View switch
//! they were issued under, like the password unlock in `link_unlock.rs`. The
//! only thing stored is the id of each ticket that opened a view, so that a
//! ticket opens one view and no more (`secure_view_tickets`).
//!
//! These are deterrents, not locks: the bytes do reach the visitor's browser.
//! What the server guarantees is that they reach it only after every check the
//! link has and an email, and that no address of the bucket is ever sent.

use axum::body::Bytes;
use chrono::Utc;
use futures::StreamExt;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use validator::ValidateEmail;

use crate::entity::{link_files, links};
use crate::utils::files::{
    Disposition, FileSharing, FileStore, StoreError, StoredObject, new_storage_key, served_as,
};
use crate::utils::link_unlock::password_fingerprint;

/// How long a visitor has to type an email after opening the link.
pub const VIEW_TICKET_TTL_SECONDS: i64 = 15 * 60;
/// How long the viewer may fetch the file after the email was given.
pub const VIEW_TOKEN_TTL_SECONDS: i64 = 5 * 60;
const TICKET_PURPOSE: &str = "secure_view_ticket";
const TOKEN_PURPOSE: &str = "secure_view_token";

/// The longest address RFC 5321 allows.
pub const MAX_VIEWER_EMAIL_CHARS: usize = 254;

/// The largest PDF a protected copy is made of. Larger ones are shown from
/// the original. Overridden by `SECURE_VIEW_PROTECT_MAX_BYTES`
/// (`FileSharing::protect_max_bytes`).
pub const DEFAULT_PROTECT_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// The most stream bytes the protected-copy step will look at for one PDF,
/// and the most any one stream may inflate to while the file is read.
/// Overridden by `SECURE_VIEW_PROTECT_BUDGET_BYTES`
/// (`FileSharing::protect_budget_bytes`). A PDF that needs more gets no copy.
pub const DEFAULT_PROTECT_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
/// A PDF with more pages than this gets no protected copy.
pub const MAX_PROTECT_PAGES: usize = 20_000;
/// A PDF with more objects than this gets no protected copy.
pub const MAX_PROTECT_OBJECTS: usize = 1_000_000;

const PDF: &str = "application/pdf";

// ------------------------------------------------------------- eligibility

/// Whether Secure View can show this file: exactly the types a browser is
/// allowed to show in place (`files::served_as`), so a PDF or one of the four
/// image types whose name and declared type agree.
pub fn eligible(file: &link_files::Model) -> bool {
    served_as(&file.content_type, &file.filename).disposition == Disposition::Inline
}

/// What the viewer draws: `"pdf"` or `"image"`, with the content type the
/// bytes are sent as. `None` for a file Secure View cannot show.
pub fn viewer_kind(file: &link_files::Model) -> Option<(&'static str, &'static str)> {
    let served = served_as(&file.content_type, &file.filename);
    if served.disposition != Disposition::Inline {
        return None;
    }
    let kind = if served.content_type == PDF {
        "pdf"
    } else {
        "image"
    };
    Some((kind, served.content_type))
}

// ------------------------------------------------------------------- email

/// The email a visitor typed, as it is stored and drawn on the document:
/// trimmed and lower case. `None` when it is not shaped like an address. The
/// address is not verified; it is a deterrent and a record of what was typed.
pub fn clean_viewer_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_lowercase();
    if email.is_empty() || email.chars().count() > MAX_VIEWER_EMAIL_CHARS {
        return None;
    }
    // No control characters or spaces: the value is drawn on the page and
    // listed in the dashboard.
    if email.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    // An address someone could be written to: a host with a dot in it, not
    // `pat@localhost` and not an address literal (`pat@[10.0.0.1]`).
    let (_, domain) = email.rsplit_once('@')?;
    if !domain.contains('.') || domain.starts_with('[') {
        return None;
    }
    email.validate_email().then_some(email)
}

// ------------------------------------------------------------------ tokens

#[derive(Debug, Serialize, Deserialize)]
struct ViewClaims {
    sub: String,
    exp: usize,
    link_id: i32,
    file_id: i32,
    code: String,
    /// Fingerprint of the link's password hash (of nothing when it has none),
    /// so adding, changing or removing the password ends every open session.
    password_fingerprint: String,
    /// `link_files.secure_view_generation` when the token was issued. The
    /// number goes up each time the switch is turned, so switching Secure
    /// View off ends every ticket and token for good, even if it is switched
    /// on again before they would have expired.
    generation: i32,
    /// A ticket's own id. A ticket opens one view: this is what is recorded
    /// when it does. Only on a ticket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jti: Option<String>,
    /// The email the token was issued to. Only on a view token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

fn configured_secret() -> Option<String> {
    std::env::var("JWT_SECRET")
        .ok()
        .filter(|secret| secret.len() >= 32)
}

fn link_fingerprint(link: &links::Model) -> String {
    password_fingerprint(link.password_hash.as_deref().unwrap_or(""))
}

fn sign(
    secret: &str,
    purpose: &str,
    ttl_seconds: i64,
    link: &links::Model,
    file: &link_files::Model,
    email: Option<&str>,
) -> Option<String> {
    let exp = Utc::now()
        .checked_add_signed(chrono::Duration::seconds(ttl_seconds))?
        .timestamp();
    let claims = ViewClaims {
        sub: purpose.to_string(),
        exp: usize::try_from(exp).ok()?,
        link_id: link.id,
        file_id: file.id,
        code: link.code.clone(),
        password_fingerprint: link_fingerprint(link),
        generation: file.secure_view_generation,
        jti: (purpose == TICKET_PURPOSE).then(|| uuid::Uuid::new_v4().simple().to_string()),
        email: email.map(str::to_string),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .ok()
}

/// The claims of `token` when it is a live token of `purpose` for exactly
/// this link, this file, the link's current password and the current turn of
/// the Secure View switch.
fn verify(
    secret: &str,
    purpose: &str,
    token: &str,
    link: &links::Model,
    file: &link_files::Model,
) -> Option<ViewClaims> {
    let mut validation = Validation::new(Algorithm::HS256);
    // The lifetimes are minutes; the library's default minute of grace would
    // be a fifth of one of them.
    validation.leeway = 0;
    let claims = decode::<ViewClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()?
    .claims;
    (claims.sub == purpose
        && claims.link_id == link.id
        && claims.file_id == file.id
        && claims.code == link.code
        && claims.password_fingerprint == link_fingerprint(link)
        && claims.generation == file.secure_view_generation)
        .then_some(claims)
}

/// The ticket `GET /{code}` gives a visitor who passed every check of a
/// Secure View link. It proves that, and nothing else: no click is counted
/// and nothing is used up until it is traded for a view token.
pub fn create_view_ticket(link: &links::Model, file: &link_files::Model) -> Option<String> {
    sign(
        &configured_secret()?,
        TICKET_PURPOSE,
        VIEW_TICKET_TTL_SECONDS,
        link,
        file,
        None,
    )
}

/// A valid ticket, as [`spend_ticket`] needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidTicket {
    id: String,
    expires_at: chrono::NaiveDateTime,
}

/// The ticket, when it is a live one for this link and file. A ticket without
/// an id is not one this server signs.
pub fn validate_view_ticket(
    token: &str,
    link: &links::Model,
    file: &link_files::Model,
) -> Option<ValidTicket> {
    let secret = configured_secret()?;
    let claims = verify(&secret, TICKET_PURPOSE, token, link, file)?;
    Some(ValidTicket {
        id: claims.jti.filter(|id| !id.is_empty())?,
        expires_at: chrono::DateTime::from_timestamp(i64::try_from(claims.exp).ok()?, 0)?
            .naive_utc(),
    })
}

/// Use `ticket` up. `Ok(true)` the first time, `Ok(false)` when it has
/// already opened a view: one statement, so two requests with the same
/// ticket cannot both be first.
pub async fn spend_ticket(
    db: &impl ConnectionTrait,
    ticket: &ValidTicket,
) -> Result<bool, sea_orm::DbErr> {
    let spent = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO secure_view_tickets (id, expires_at) VALUES ($1, $2) \
             ON CONFLICT (id) DO NOTHING",
            [ticket.id.clone().into(), ticket.expires_at.into()],
        ))
        .await?;
    Ok(spent.rows_affected() == 1)
}

/// Forget the tickets that would have expired by now anyway. Run by the
/// daily file sweep.
pub async fn forget_expired_tickets(db: &impl ConnectionTrait) -> Result<u64, sea_orm::DbErr> {
    let forgotten = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM secure_view_tickets WHERE expires_at < $1",
            [Utc::now().naive_utc().into()],
        ))
        .await?;
    Ok(forgotten.rows_affected())
}

/// The token the viewer fetches the file with, issued once the email was
/// given and the visit counted.
pub fn create_view_token(
    link: &links::Model,
    file: &link_files::Model,
    email: &str,
) -> Option<String> {
    sign(
        &configured_secret()?,
        TOKEN_PURPOSE,
        VIEW_TOKEN_TTL_SECONDS,
        link,
        file,
        Some(email),
    )
}

/// The email a live view token for this link and file was issued to.
pub fn validate_view_token(
    token: &str,
    link: &links::Model,
    file: &link_files::Model,
) -> Option<String> {
    let secret = configured_secret()?;
    verify(&secret, TOKEN_PURPOSE, token, link, file)?.email
}

// ------------------------------------------------------------- file source

/// Where the bytes the viewer shows come from. One kind today, an object in
/// the bucket; the viewer endpoint only ever sees this type, so another
/// source (a file fetched live from elsewhere) is a new variant here and
/// nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileSource {
    Stored { key: String },
}

impl FileSource {
    /// The sources to try for `file`, best first: the protected copy when one
    /// was made, then the original.
    pub fn for_viewer(file: &link_files::Model) -> Vec<FileSource> {
        let mut sources = Vec::with_capacity(2);
        if let Some(key) = &file.protected_key {
            sources.push(FileSource::Stored { key: key.clone() });
        }
        sources.push(FileSource::Stored {
            key: file.storage_key.clone(),
        });
        sources
    }

    /// The bytes as a stream, or `None` when this source has nothing.
    pub async fn open(&self, files: &FileSharing) -> Result<Option<StoredObject>, StoreError> {
        match self {
            FileSource::Stored { key } => files.store.get(key).await,
        }
    }
}

/// Open the first source of `file` that has something. A protected copy that
/// is missing or cannot be read falls back to the original.
pub async fn open_for_viewer(
    files: &FileSharing,
    file: &link_files::Model,
) -> Result<Option<StoredObject>, StoreError> {
    let sources = FileSource::for_viewer(file);
    let last = sources.len() - 1;
    for (index, source) in sources.iter().enumerate() {
        match source.open(files).await {
            Ok(Some(object)) => return Ok(Some(object)),
            Ok(None) if index == last => return Ok(None),
            Err(error) if index == last => return Err(error),
            Ok(None) => tracing::warn!("Secure View: the protected copy is missing"),
            Err(error) => tracing::warn!("Secure View: the protected copy is unreadable: {error}"),
        }
    }
    Ok(None)
}

// ---------------------------------------------------------- protected copy

/// The limits the protected-copy step works within. They do NOT bound the
/// memory the PDF library takes while it reads a file: an object stream's
/// inflated bytes are held to the budget, but what they parse into is not
/// (measured: a 33 KB file, 1.9 GB). The deployment's container memory ceiling
/// is the backstop for that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtectLimits {
    /// The most stream bytes compared between the original and the copy, and
    /// the most any one stream may inflate to while the file is read.
    pub budget_bytes: u64,
    pub max_pages: usize,
    pub max_objects: usize,
}

impl Default for ProtectLimits {
    fn default() -> Self {
        Self {
            budget_bytes: DEFAULT_PROTECT_BUDGET_BYTES,
            max_pages: MAX_PROTECT_PAGES,
            max_objects: MAX_PROTECT_OBJECTS,
        }
    }
}

/// Why a PDF got no protected copy. The original is shown in every case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoCopy {
    /// Not a PDF, damaged, or not quite to the standard.
    Unreadable,
    /// It came with its own protection, which it keeps.
    AlreadyEncrypted,
    /// It would take more than [`ProtectLimits::budget_bytes`].
    OverBudget,
    TooManyPages,
    TooManyObjects,
    /// The copy could not be written, or does not read back as the original.
    NotFaithful,
}

impl std::fmt::Display for NoCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unreadable => "not a PDF this can rewrite",
            Self::AlreadyEncrypted => "already encrypted",
            Self::OverBudget => "over the memory budget",
            Self::TooManyPages => "too many pages",
            Self::TooManyObjects => "too many objects",
            Self::NotFaithful => "the copy did not read back as the original",
        })
    }
}

/// A copy of `pdf` that asks a PDF reader not to print, copy or change it,
/// and opens without a password. `Err` when the file is not a PDF this can
/// rewrite faithfully (damaged, not quite to the standard, already encrypted,
/// or the result does not read back as the original) or would take more than
/// `limits`; the original is then shown as it is.
///
/// AES-256 with an empty user password and a random owner password that is
/// thrown away. A reader that honours the flags greys out Print and Copy; one
/// that does not is not stopped, which is why this is "best effort".
///
/// **No stream is ever inflated here.** Encrypting works on a stream's bytes
/// as they are in the file, and so does the check that the copy reads back
/// as the original, so the memory this takes follows the size of the file
/// (which the caller caps), not what a crafted file could inflate to. The one
/// place the library inflates anything is while it reads the file (object and
/// cross-reference streams). Each of those is held to the budget in bytes, but
/// the objects they parse into are not bounded, so a crafted file can still
/// cost gigabytes here; see `ProtectLimits`.
///
/// Blocking and CPU-bound: call it on the blocking pool.
pub fn protect_pdf(pdf: &[u8], limits: &ProtectLimits) -> Result<Vec<u8>, NoCopy> {
    use lopdf::{DecompressError, Document, Error, LoadOptions};

    // Strict: a file the library would have to repair or guess at is left
    // alone. A copy must never show less than the original does.
    let per_stream = usize::try_from(limits.budget_bytes)
        .unwrap_or(usize::MAX)
        .min(lopdf::DEFAULT_MAX_DECOMPRESSED_SIZE);
    let load = |bytes: &[u8]| {
        let options = LoadOptions {
            strict: true,
            max_decompressed_size: Some(per_stream),
            ..LoadOptions::default()
        };
        Document::load_mem_with_options(bytes, options).map_err(|error| match error {
            Error::Decompress(DecompressError::MemoryLimitExceeded { .. }) => NoCopy::OverBudget,
            _ => NoCopy::Unreadable,
        })
    };

    let mut document = load(pdf)?;
    // A file that came with its own protection keeps it.
    if document.is_encrypted() || document.was_encrypted() {
        return Err(NoCopy::AlreadyEncrypted);
    }
    if document.objects.len() > limits.max_objects {
        return Err(NoCopy::TooManyObjects);
    }
    let before = fingerprint(&document, limits)?;

    let owner_password = uuid::Uuid::new_v4().simple().to_string();
    encrypt_document(&mut document, &owner_password).ok_or(NoCopy::NotFaithful)?;
    let mut protected = Vec::with_capacity(pdf.len() + 4096);
    document
        .save_to(&mut protected)
        .map_err(|_| NoCopy::NotFaithful)?;
    drop(document);

    // Read it back the way a viewer will, with no password: every page must
    // be there with the same content streams, and every stream must decrypt
    // to exactly what it was.
    let check = load(&protected).map_err(|_| NoCopy::NotFaithful)?;
    if !check.was_encrypted() || fingerprint(&check, limits) != Ok(before) {
        return Err(NoCopy::NotFaithful);
    }
    Ok(protected)
}

/// One digest of what `document` shows: its pages in order, which streams
/// each page draws, and every stream's bytes as they are stored, neither
/// decoded nor inflated. Equal for a document and a copy of it exactly when
/// every page and every stream came through unchanged.
///
/// Bounded by `limits`: it counts no more pages and content references than
/// allowed, reads no more stream bytes than the budget, and keeps nothing
/// but the digest. `Err(Unreadable)` for a document without a page.
fn fingerprint(document: &lopdf::Document, limits: &ProtectLimits) -> Result<[u8; 32], NoCopy> {
    use lopdf::Object;
    use sha2::{Digest, Sha256};

    fn id_bytes(id: lopdf::ObjectId) -> [u8; 6] {
        let mut bytes = [0u8; 6];
        bytes[..4].copy_from_slice(&id.0.to_be_bytes());
        bytes[4..].copy_from_slice(&id.1.to_be_bytes());
        bytes
    }

    let mut digest = Sha256::new();

    // The pages, and the content streams each one names. A page may name any
    // number of them and pages may share them, so the references are counted
    // against the object limit rather than trusted to be few.
    let (mut pages, mut references) = (0usize, 0usize);
    for page in document.page_iter() {
        pages += 1;
        if pages > limits.max_pages {
            return Err(NoCopy::TooManyPages);
        }
        let contents = document.get_page_contents(page);
        references = references.saturating_add(contents.len());
        if references > limits.max_objects {
            return Err(NoCopy::TooManyObjects);
        }
        digest.update(b"page");
        digest.update(id_bytes(page));
        for id in contents {
            let is_stream = matches!(document.objects.get(&id), Some(Object::Stream(_)));
            digest.update(id_bytes(id));
            digest.update([u8::from(is_stream)]);
        }
    }
    if pages == 0 {
        return Err(NoCopy::Unreadable);
    }

    // Every stream of the document. The streams a file is packed in (object
    // streams, cross-reference streams) are not part of it: saving writes
    // new ones.
    let mut left = limits.budget_bytes;
    for (id, object) in &document.objects {
        let Object::Stream(stream) = object else {
            continue;
        };
        if stream.dict.has_type(b"XRef") || stream.dict.has_type(b"ObjStm") {
            continue;
        }
        let bytes: &[u8] = &stream.content;
        left = left
            .checked_sub(bytes.len() as u64)
            .ok_or(NoCopy::OverBudget)?;
        digest.update(b"stream");
        digest.update(id_bytes(*id));
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    Ok(digest.finalize().into())
}

/// Encrypt `document` in place: opens with no password, and the permission
/// flags allow nothing but reading aloud for assistive technology (which
/// PDF 2.0 says must stay on). `owner_password` is the one that would lift
/// the restrictions; the caller throws it away.
fn encrypt_document(document: &mut lopdf::Document, owner_password: &str) -> Option<()> {
    use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
    use lopdf::{EncryptionState, EncryptionVersion, Object, Permissions, StringFormat};
    use rand::RngCore;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let mut rng = rand::thread_rng();
    // An encrypted file must carry an ID; most have one already.
    if document.trailer.get(b"ID").is_err() {
        let mut id = [0u8; 16];
        rng.fill_bytes(&mut id);
        let id = Object::String(id.to_vec(), StringFormat::Hexadecimal);
        document
            .trailer
            .set("ID", Object::Array(vec![id.clone(), id]));
    }

    let mut file_encryption_key = [0u8; 32];
    rng.fill_bytes(&mut file_encryption_key);
    let crypt_filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
    let state = EncryptionState::try_from(EncryptionVersion::V5 {
        encrypt_metadata: true,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), crypt_filter)]),
        file_encryption_key: &file_encryption_key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password,
        user_password: "",
        permissions: Permissions::COPYABLE_FOR_ACCESSIBILITY,
    })
    .ok()?;
    document.encrypt(&state).ok()
}

/// What became of an attempt to make a protected copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protected {
    /// A copy was made and recorded.
    Made,
    /// Nothing to do: not a PDF, too large, Secure View off, or already made.
    NotNeeded,
    /// The file could not be rewritten, read or stored. The original is shown.
    Failed,
    /// The link was deleted, expired or switched off meanwhile; the copy that
    /// was made has been removed again.
    Abandoned,
}

/// Read at most `limit` bytes of `object`. `None` when it is larger.
async fn collect(mut object: StoredObject, limit: u64) -> Result<Option<Vec<u8>>, StoreError> {
    let mut bytes = Vec::with_capacity(object.size.unwrap_or(0).min(limit) as usize);
    while let Some(chunk) = object.body.next().await {
        let chunk = chunk?;
        if bytes.len() as u64 + chunk.len() as u64 > limit {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Some(bytes))
}

/// Make the protected copy of `file` and record it. Never an error for the
/// caller to show: whatever goes wrong, the viewer falls back to the original.
///
/// The copy is recorded only while the link is still live and still Secure
/// View, in one statement, so a link deleted while this ran cannot be left
/// with an object nobody will remove.
pub async fn make_protected_copy(
    db: &DatabaseConnection,
    files: &FileSharing,
    file: &link_files::Model,
) -> Protected {
    let limit = files.protect_max_bytes;
    let store = files.store.as_ref();
    let is_pdf = viewer_kind(file).is_some_and(|(kind, _)| kind == "pdf");
    if !file.secure_view || !is_pdf || file.protected_key.is_some() {
        return Protected::NotNeeded;
    }
    if u64::try_from(file.size_bytes).unwrap_or(u64::MAX) > limit {
        tracing::info!(
            "Secure View: file {} is larger than the protected-copy limit; showing the original",
            file.id
        );
        return Protected::NotNeeded;
    }

    // One at a time: a copy holds the whole PDF in memory, a few times over
    // (the file, the parsed document, the copy, the copy parsed again).
    static ONE_AT_A_TIME: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
    let Ok(_turn) = ONE_AT_A_TIME.acquire().await else {
        return Protected::Failed;
    };

    let original = match store.get(&file.storage_key).await {
        Ok(Some(object)) => match collect(object, limit).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Protected::NotNeeded,
            Err(error) => {
                tracing::warn!("Secure View: could not read file {}: {error}", file.id);
                return Protected::Failed;
            }
        },
        Ok(None) => return Protected::Failed,
        Err(error) => {
            tracing::warn!("Secure View: could not read file {}: {error}", file.id);
            return Protected::Failed;
        }
    };

    // A panic inside the PDF library ends the blocking task, not the server.
    let limits = ProtectLimits {
        budget_bytes: files.protect_budget_bytes,
        ..ProtectLimits::default()
    };
    let protected = tokio::task::spawn_blocking(move || protect_pdf(&original, &limits))
        .await
        .unwrap_or(Err(NoCopy::Unreadable));
    let protected = match protected {
        Ok(protected) => protected,
        Err(reason) => {
            tracing::info!(
                "Secure View: file {} could not be given a protected copy ({reason}); showing the original",
                file.id
            );
            return Protected::Failed;
        }
    };

    let key = new_storage_key();
    if let Err(error) = store.put(&key, Bytes::from(protected), PDF).await {
        tracing::warn!(
            "Secure View: could not store the protected copy of file {}: {error}",
            file.id
        );
        // The bucket may have kept it all the same.
        let _ = store.delete(&key).await;
        return Protected::Failed;
    }

    let recorded = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE link_files AS lf
               SET protected_key = $1
               FROM links l
               WHERE lf.id = $2
                 AND l.id = lf.link_id
                 AND lf.secure_view
                 AND lf.protected_key IS NULL
                 AND lf.deleted_at IS NULL
                 AND lf.status = 'ready'
                 AND l.deleted_at IS NULL
                 AND (l.expires_at IS NULL OR l.expires_at > $3)"#,
            [
                key.clone().into(),
                file.id.into(),
                Utc::now().naive_utc().into(),
            ],
        ))
        .await;
    match recorded {
        Ok(result) if result.rows_affected() == 1 => Protected::Made,
        other => {
            if let Err(error) = other {
                tracing::error!("Secure View: could not record a protected copy: {error}");
            }
            if let Err(error) = store.delete(&key).await {
                // Nothing points at this object, so no sweep will find it.
                tracing::error!(
                    "Secure View: an unused protected copy could not be removed from the bucket: {error}"
                );
            }
            Protected::Abandoned
        }
    }
}

/// Make the protected copy in the background, so switching Secure View on
/// does not wait for a large PDF. Until it is there the original is shown.
pub fn spawn_protected_copy(state: &crate::AppState, file: &link_files::Model) {
    let Some(files) = &state.files else {
        return;
    };
    let (db, files, file) = (state.db.clone(), files.clone(), file.clone());
    tokio::spawn(async move {
        make_protected_copy(&db, &files, &file).await;
    });
}

/// Remove the protected copy of `file`, when Secure View is switched off.
/// A bucket that refuses leaves the key on the row, so the object still goes
/// when the link does.
pub async fn remove_protected_copy(
    db: &DatabaseConnection,
    store: &dyn FileStore,
    file: &link_files::Model,
) {
    let Some(key) = &file.protected_key else {
        return;
    };
    if let Err(error) = store.delete(key).await {
        tracing::error!("Secure View: could not remove a protected copy, will retry: {error}");
        return;
    }
    let cleared = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE link_files SET protected_key = NULL WHERE id = $1 AND protected_key = $2",
            [file.id.into(), key.clone().into()],
        ))
        .await;
    if let Err(error) = cleared {
        tracing::error!("Secure View: could not clear a protected copy's key: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "unit-test-secure-view-secret-long-enough";

    fn link(id: i32, code: &str, password_hash: Option<&str>) -> links::Model {
        links::Model {
            id,
            code: code.to_string(),
            original_url: links::file_link_marker(1),
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
            bio_position: None,
            bio_label: None,
            expiry_notice_sent_at: None,
        }
    }

    fn file(filename: &str, content_type: &str) -> link_files::Model {
        link_files::Model {
            id: 1,
            link_id: Some(1),
            user_id: Some(1),
            storage_key: "files/original".to_string(),
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            size_bytes: 10,
            status: link_files::STATUS_READY.to_string(),
            created_at: Utc::now().naive_utc(),
            deleted_at: None,
            secure_view: true,
            protected_key: None,
            secure_view_generation: 0,
        }
    }

    /// The file row `id`, at Secure View generation 0.
    fn file_row(id: i32) -> link_files::Model {
        link_files::Model {
            id,
            ..file("a.pdf", "application/pdf")
        }
    }

    #[test]
    fn a_token_is_for_one_link_one_file_one_password_and_one_purpose() {
        let a = link(7, "doc", Some("hash-v1"));
        let f = file_row(3);
        let ticket = sign(SECRET, TICKET_PURPOSE, 60, &a, &f, None).unwrap();
        assert!(verify(SECRET, TICKET_PURPOSE, &ticket, &a, &f).is_some());

        let refused = |link: &links::Model, file: &link_files::Model| {
            verify(SECRET, TICKET_PURPOSE, &ticket, link, file).is_none()
        };
        assert!(refused(&link(8, "doc", Some("hash-v1")), &f));
        assert!(refused(&link(7, "other", Some("hash-v1")), &f));
        assert!(refused(&a, &file_row(4)));
        // The password changed, or was removed.
        assert!(refused(&link(7, "doc", Some("hash-v2")), &f));
        assert!(refused(&link(7, "doc", None), &f));
        // A ticket is not a view token, and the other way round.
        assert!(verify(SECRET, TOKEN_PURPOSE, &ticket, &a, &f).is_none());
        let token = sign(SECRET, TOKEN_PURPOSE, 60, &a, &f, Some("a@b.org")).unwrap();
        assert!(verify(SECRET, TICKET_PURPOSE, &token, &a, &f).is_none());
        assert_eq!(
            verify(SECRET, TOKEN_PURPOSE, &token, &a, &f)
                .unwrap()
                .email
                .as_deref(),
            Some("a@b.org")
        );
        // Another server's secret.
        assert!(
            verify(
                "another-secret-that-is-long-enough-too",
                TICKET_PURPOSE,
                &ticket,
                &a,
                &f
            )
            .is_none()
        );
    }

    #[test]
    fn turning_the_switch_ends_every_ticket_and_token_for_good() {
        let a = link(7, "doc", None);
        let issued_under = file_row(3);
        let ticket = sign(SECRET, TICKET_PURPOSE, 60, &a, &issued_under, None).unwrap();
        let token = sign(
            SECRET,
            TOKEN_PURPOSE,
            60,
            &a,
            &issued_under,
            Some("a@b.org"),
        )
        .unwrap();
        assert!(verify(SECRET, TICKET_PURPOSE, &ticket, &a, &issued_under).is_some());
        assert!(verify(SECRET, TOKEN_PURPOSE, &token, &a, &issued_under).is_some());

        // Switched off (generation 1) and on again (generation 2): the same
        // file, Secure View on as before, and neither works.
        for generation in [1, 2] {
            let now = link_files::Model {
                secure_view_generation: generation,
                ..issued_under.clone()
            };
            assert!(verify(SECRET, TICKET_PURPOSE, &ticket, &a, &now).is_none());
            assert!(verify(SECRET, TOKEN_PURPOSE, &token, &a, &now).is_none());
        }
    }

    #[test]
    fn every_ticket_has_its_own_id_and_a_view_token_has_none() {
        let a = link(7, "doc", None);
        let f = file_row(3);
        let id_of = |purpose: &str| {
            let token = sign(SECRET, purpose, 60, &a, &f, None).unwrap();
            verify(SECRET, purpose, &token, &a, &f).unwrap().jti
        };
        let (first, second) = (id_of(TICKET_PURPOSE), id_of(TICKET_PURPOSE));
        assert!(first.as_deref().is_some_and(|id| id.len() == 32));
        assert_ne!(first, second, "two tickets for one link are two tickets");
        assert_eq!(id_of(TOKEN_PURPOSE), None);
    }

    #[test]
    fn a_link_without_a_password_gets_tokens_that_die_when_one_is_added() {
        let open = link(7, "doc", None);
        let f = file_row(3);
        let ticket = sign(SECRET, TICKET_PURPOSE, 60, &open, &f, None).unwrap();
        assert!(verify(SECRET, TICKET_PURPOSE, &ticket, &open, &f).is_some());
        assert!(
            verify(
                SECRET,
                TICKET_PURPOSE,
                &ticket,
                &link(7, "doc", Some("hash")),
                &f
            )
            .is_none()
        );
    }

    #[test]
    fn an_expired_token_is_refused_with_no_grace() {
        let a = link(7, "doc", None);
        let f = file_row(3);
        let expired = sign(SECRET, TICKET_PURPOSE, -5, &a, &f, None).unwrap();
        assert!(verify(SECRET, TICKET_PURPOSE, &expired, &a, &f).is_none());
    }

    #[test]
    fn emails_are_trimmed_lowercased_and_checked_for_shape() {
        assert_eq!(
            clean_viewer_email("  Pat.Lee@Example.ORG "),
            Some("pat.lee@example.org".to_string())
        );
        for bad in [
            "",
            "   ",
            "pat",
            "pat@",
            "@example.org",
            "pat lee@example.org",
            "pat@example.org\r\nBcc: x@y.z",
            "pat@exa\tmple.org",
            "<script>@example.org",
            // Nowhere a person could be written to.
            "a@b",
            "pat@localhost",
            "pat@[127.0.0.1]",
            "pat@[ipv6:::1]",
        ] {
            assert_eq!(clean_viewer_email(bad), None, "{bad:?}");
        }
        let long = format!("{}@example.org", "a".repeat(250));
        assert_eq!(clean_viewer_email(&long), None);
    }

    #[test]
    fn only_what_a_browser_may_show_in_place_is_eligible() {
        assert!(eligible(&file("a.pdf", "application/pdf")));
        assert_eq!(
            viewer_kind(&file("a.pdf", "application/pdf")),
            Some(("pdf", "application/pdf"))
        );
        for (name, content_type) in [
            ("a.png", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
        ] {
            assert_eq!(
                viewer_kind(&file(name, content_type)),
                Some(("image", content_type))
            );
        }
        for (name, content_type) in [
            ("a.zip", "application/zip"),
            ("a.html", "text/html"),
            ("a.svg", "image/svg+xml"),
            ("a.docx", "application/octet-stream"),
            // The name and the declared type must agree.
            ("a.html", "application/pdf"),
            ("a.pdf", "text/html"),
        ] {
            assert!(
                !eligible(&file(name, content_type)),
                "{name} {content_type}"
            );
            assert_eq!(viewer_kind(&file(name, content_type)), None);
        }
    }

    #[test]
    fn the_viewer_is_given_the_protected_copy_first() {
        let mut stored = file("a.pdf", "application/pdf");
        assert_eq!(
            FileSource::for_viewer(&stored),
            vec![FileSource::Stored {
                key: "files/original".to_string()
            }]
        );
        stored.protected_key = Some("files/protected".to_string());
        assert_eq!(
            FileSource::for_viewer(&stored),
            vec![
                FileSource::Stored {
                    key: "files/protected".to_string()
                },
                FileSource::Stored {
                    key: "files/original".to_string()
                },
            ]
        );
    }

    /// [`protect_pdf`] within the default limits.
    fn protect(pdf: &[u8]) -> Result<Vec<u8>, NoCopy> {
        protect_pdf(pdf, &ProtectLimits::default())
    }

    /// A one-page PDF, written by hand and valid: header, four objects, a
    /// cross-reference table with correct offsets, and a trailer without an
    /// ID (the least a real file can have).
    pub(crate) fn small_pdf() -> Vec<u8> {
        pdf_with(&[])
    }

    /// [`small_pdf`] with more objects after its four, numbered from 5.
    fn pdf_with(extra: &[&str]) -> Vec<u8> {
        let extra: Vec<Vec<u8>> = extra.iter().map(|body| body.as_bytes().to_vec()).collect();
        pdf_with_bytes(&extra)
    }

    /// [`pdf_with`] for objects that are not text (a compressed stream).
    fn pdf_with_bytes(extra: &[Vec<u8>]) -> Vec<u8> {
        let mut objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R \
             /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>"
                .to_vec(),
            {
                let content = "BT /F1 18 Tf 20 100 Td (Secure View fixture) Tj ET";
                format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len())
                    .into_bytes()
            },
        ];
        objects.extend(extra.iter().cloned());
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn a_real_pdf_gets_a_copy_that_opens_without_a_password_and_forbids_printing_and_copying() {
        use lopdf::{Document, Permissions};

        let original = small_pdf();
        let protected = protect(&original).expect("a protected copy");
        assert_ne!(protected, original);
        // The text is no longer readable in the file itself.
        assert!(!String::from_utf8_lossy(&protected).contains("Secure View fixture"));

        // Opens with the empty user password, which is what a viewer tries.
        let reopened = Document::load_mem(&protected).expect("opens with no password");
        assert!(reopened.was_encrypted());
        assert_eq!(reopened.get_pages().len(), 1);
        let page = *reopened.get_pages().get(&1).unwrap();
        let content = reopened.get_page_content(page);
        assert!(
            String::from_utf8_lossy(&content).contains("Secure View fixture"),
            "the page decrypts to what it was"
        );

        // The permission flags the copy carries.
        let state = reopened
            .encryption_state
            .as_ref()
            .expect("encryption state");
        assert_eq!(state.version(), 5);
        assert_eq!(state.revision(), 6);
        let permissions = state.permissions();
        for (flag, name) in [
            (Permissions::PRINTABLE, "print"),
            (Permissions::PRINTABLE_IN_HIGH_QUALITY, "high-quality print"),
            (Permissions::COPYABLE, "copy"),
            (Permissions::MODIFIABLE, "modify"),
            (Permissions::ANNOTABLE, "annotate"),
            (Permissions::FILLABLE, "fill forms"),
            (Permissions::ASSEMBLABLE, "assemble"),
        ] {
            assert!(!permissions.contains(flag), "{name} must be off");
        }
        assert!(permissions.contains(Permissions::COPYABLE_FOR_ACCESSIBILITY));
    }

    #[test]
    fn the_empty_password_opens_the_copy_but_is_not_the_owner_password() {
        // "Opens without a password" must not mean the owner password is
        // empty too, or every reader would grant everything.
        let mut document = lopdf::Document::load_mem(&small_pdf()).unwrap();
        encrypt_document(&mut document, "the-owner-password").expect("encrypted");
        assert!(document.is_encrypted());
        assert!(document.authenticate_user_password("").is_ok());
        assert!(document.authenticate_owner_password("").is_err());
        assert!(
            document
                .authenticate_owner_password("the-owner-password")
                .is_ok()
        );
        assert!(
            document
                .authenticate_user_password("the-owner-password")
                .is_err()
        );
    }

    #[test]
    fn a_file_that_is_not_a_pdf_gets_no_copy() {
        assert_eq!(protect(b""), Err(NoCopy::Unreadable));
        assert_eq!(protect(b"not a pdf at all"), Err(NoCopy::Unreadable));
        assert_eq!(
            protect(b"%PDF-1.4\nthis is garbage\n%%EOF\n"),
            Err(NoCopy::Unreadable)
        );
        // A PDF cut off halfway.
        let whole = small_pdf();
        assert_eq!(protect(&whole[..whole.len() / 2]), Err(NoCopy::Unreadable));
        // Random bytes behind a PDF header.
        let mut noise = b"%PDF-1.7\n".to_vec();
        noise.extend((0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8));
        assert_eq!(protect(&noise), Err(NoCopy::Unreadable));
    }

    #[test]
    fn a_pdf_with_an_object_that_cannot_be_read_gets_no_copy() {
        // The pages are fine and the file has one object that is not valid
        // PDF. A lenient reader drops that object and carries on; a copy made
        // from what is left would be missing something the original has.
        let sound = pdf_with(&["<< /Type /XObject /Subtype /Image /Width 1 /Height 1 >>"]);
        assert!(
            protect(&sound).is_ok(),
            "the control: a fifth object is fine"
        );
        let damaged = pdf_with(&["<< /Type /XObject /Subtype [ /Width >>"]);
        assert!(
            lopdf::Document::load_mem(&damaged).is_ok(),
            "a lenient reader does open it"
        );
        assert_eq!(protect(&damaged), Err(NoCopy::Unreadable));
    }

    #[test]
    fn a_pdf_that_is_already_protected_keeps_its_own_protection() {
        let protected = protect(&small_pdf()).expect("a protected copy");
        assert_eq!(protect(&protected), Err(NoCopy::AlreadyEncrypted));
    }

    // ------------------------------------------------ bounded memory

    const MIB: usize = 1024 * 1024;

    /// A small file that inflates to a great deal: one content stream of
    /// `inflated` bytes, compressed to a few kilobytes, shared by `pages`
    /// pages. Reading every page's content, which the first version of this
    /// step did, holds `inflated * pages` bytes at once.
    fn high_ratio_pdf(inflated: usize, pages: usize) -> Vec<u8> {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let mut content = Stream::new(dictionary! {}, vec![b' '; inflated]);
        content.compress().unwrap();
        let content_id = document.add_object(content);
        let kids: Vec<Object> = (0..pages)
            .map(|_| {
                Object::Reference(document.add_object(dictionary! {
                    "Type" => "Page",
                    "Parent" => pages_id,
                    "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
                    "Contents" => content_id,
                }))
            })
            .collect();
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => pages as i64,
            }),
        );
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    /// The bytes all of `pdf`'s streams take in the file, as they are stored.
    fn stored_stream_bytes(pdf: &[u8]) -> u64 {
        lopdf::Document::load_mem(pdf)
            .unwrap()
            .objects
            .values()
            .filter_map(|object| object.as_stream().ok())
            .filter(|stream| !stream.dict.has_type(b"XRef"))
            .map(|stream| stream.content.len() as u64)
            .sum()
    }

    #[test]
    fn a_small_pdf_that_inflates_enormously_is_never_inflated() {
        // 4 MiB behind every one of 20 pages: 80 MiB to anything that reads
        // page contents, in a file of a few kilobytes.
        let pdf = high_ratio_pdf(4 * MIB, 20);
        assert!(pdf.len() < 64 * 1024, "a small file: {} bytes", pdf.len());
        let loaded = lopdf::Document::load_mem(&pdf).unwrap();
        let page = *loaded.get_pages().get(&1).unwrap();
        assert_eq!(
            loaded
                .get_page_content_with_limit(page, 8 * MIB)
                .unwrap()
                .len(),
            4 * MIB + 1,
            "the fixture does inflate to 4 MiB a page"
        );

        // The budget is far below what one page inflates to. The copy is made
        // all the same, because nothing is inflated to make or to check it:
        // had a single stream been inflated, it would not have fitted.
        let tight = ProtectLimits {
            budget_bytes: 64 * 1024,
            ..ProtectLimits::default()
        };
        let protected = protect_pdf(&pdf, &tight).expect("a copy, within a 64 KiB budget");
        assert!(protected.len() < 64 * 1024, "{} bytes", protected.len());
        let reopened = lopdf::Document::load_mem(&protected).unwrap();
        assert!(reopened.was_encrypted());
        assert_eq!(reopened.get_pages().len(), 20);
    }

    #[test]
    fn a_pdf_over_the_budget_gets_no_copy() {
        // The budget counts stream bytes as they are stored. One byte short
        // of what this file's streams take: refused. Exactly enough: made.
        let pdf = high_ratio_pdf(4 * MIB, 2);
        let stored = stored_stream_bytes(&pdf);
        assert!(stored > 1024, "{stored}");
        let limits = |budget_bytes| ProtectLimits {
            budget_bytes,
            ..ProtectLimits::default()
        };
        assert_eq!(
            protect_pdf(&pdf, &limits(stored - 1)),
            Err(NoCopy::OverBudget)
        );
        // The copy's streams are the same bytes, so the same budget covers
        // reading it back.
        assert!(protect_pdf(&pdf, &limits(stored)).is_ok());
    }

    #[test]
    fn a_stream_the_reader_must_inflate_is_held_to_the_budget() {
        // An object stream is the one kind the library inflates while it
        // reads a file. This one holds a single `null` followed by 4 MiB of
        // spaces, in about 4 KiB of file.
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;

        let mut inflated = b"9 0 null".to_vec();
        inflated.resize(4 * MIB, b' ');
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&inflated).unwrap();
        let packed = encoder.finish().unwrap();
        assert!(packed.len() < 16 * 1024);

        let mut bomb = format!(
            "<< /Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /Length {} >>\nstream\n",
            packed.len()
        )
        .into_bytes();
        bomb.extend_from_slice(&packed);
        bomb.extend_from_slice(b"\nendstream");
        let pdf = pdf_with_bytes(&[bomb]);

        let limits = |budget_bytes| ProtectLimits {
            budget_bytes,
            ..ProtectLimits::default()
        };
        assert_eq!(
            protect_pdf(&pdf, &limits(MIB as u64)),
            Err(NoCopy::OverBudget),
            "4 MiB does not fit a 1 MiB budget"
        );
        assert!(
            protect_pdf(&pdf, &limits(8 * MIB as u64)).is_ok(),
            "the control: the same file within a budget it fits"
        );
    }

    #[test]
    fn a_pdf_with_too_many_pages_or_objects_gets_no_copy() {
        let pdf = high_ratio_pdf(16, 12);
        let limits = |max_pages, max_objects| ProtectLimits {
            max_pages,
            max_objects,
            ..ProtectLimits::default()
        };
        assert_eq!(
            protect_pdf(&pdf, &limits(11, 1_000)),
            Err(NoCopy::TooManyPages)
        );
        assert!(protect_pdf(&pdf, &limits(12, 1_000)).is_ok());
        // 12 pages, the page tree, the catalog, the content stream, and
        // whatever the file is packed in.
        let objects = lopdf::Document::load_mem(&pdf).unwrap().objects.len();
        assert!(objects >= 15, "{objects}");
        assert_eq!(
            protect_pdf(&pdf, &limits(12, objects - 1)),
            Err(NoCopy::TooManyObjects)
        );
        assert!(protect_pdf(&pdf, &limits(12, objects)).is_ok());

        // One page may name a great many content streams, and pages may share
        // them: the references are held to the object limit too.
        let mut document = lopdf::Document::load_mem(&pdf).unwrap();
        let content = document.get_page_contents(*document.get_pages().get(&1).unwrap())[0];
        let many: Vec<lopdf::Object> = vec![lopdf::Object::Reference(content); 50];
        for page in document.get_pages().into_values() {
            document
                .get_dictionary_mut(page)
                .unwrap()
                .set("Contents", many.clone());
        }
        let mut crowded = Vec::new();
        document.save_to(&mut crowded).unwrap();
        assert_eq!(
            protect_pdf(&crowded, &limits(12, 12 * 50 - 1)),
            Err(NoCopy::TooManyObjects)
        );
        assert!(protect_pdf(&crowded, &limits(12, 12 * 50)).is_ok());
    }

    #[test]
    fn a_pdf_with_object_streams_and_a_cross_reference_stream_gets_a_copy() {
        // What most writers produce today. The streams the file is packed in
        // are not part of the document and must not upset the comparison.
        let mut document = lopdf::Document::load_mem(&high_ratio_pdf(2048, 3)).unwrap();
        let mut modern = Vec::new();
        document.save_modern(&mut modern).unwrap();
        assert!(
            String::from_utf8_lossy(&modern).contains("/ObjStm"),
            "the fixture is packed in object streams"
        );
        let protected = protect(&modern).expect("a protected copy");
        let reopened = lopdf::Document::load_mem(&protected).unwrap();
        assert!(reopened.was_encrypted());
        assert_eq!(reopened.get_pages().len(), 3);
    }

    #[test]
    fn a_copy_whose_streams_changed_would_not_pass_the_check() {
        // The comparison the copy is checked with: the same document gives
        // the same digest, one changed byte in one stream gives another.
        let original = lopdf::Document::load_mem(&small_pdf()).unwrap();
        let same = lopdf::Document::load_mem(&small_pdf()).unwrap();
        let limits = ProtectLimits::default();
        let before = fingerprint(&original, &limits).unwrap();
        assert_eq!(fingerprint(&same, &limits), Ok(before));

        let mut changed = same;
        for object in changed.objects.values_mut() {
            if let Ok(stream) = object.as_stream_mut() {
                stream.content[0] ^= 1;
            }
        }
        assert_ne!(fingerprint(&changed, &limits), Ok(before));

        // A page that lost its content stream, with every stream still there.
        let mut emptied = lopdf::Document::load_mem(&small_pdf()).unwrap();
        let page = *emptied.get_pages().get(&1).unwrap();
        emptied
            .get_dictionary_mut(page)
            .unwrap()
            .remove(b"Contents");
        assert_ne!(fingerprint(&emptied, &limits), Ok(before));

        // A page that draws another stream than it did, with every stream
        // still there and unchanged.
        let two_streams = pdf_with(&["<< /Length 3 >>
stream
q Q
endstream"]);
        let original = lopdf::Document::load_mem(&two_streams).unwrap();
        let before = fingerprint(&original, &limits).unwrap();
        let mut repointed = lopdf::Document::load_mem(&two_streams).unwrap();
        let page = *repointed.get_pages().get(&1).unwrap();
        repointed
            .get_dictionary_mut(page)
            .unwrap()
            .set("Contents", lopdf::Object::Reference((5, 0)));
        assert_eq!(repointed.get_page_contents(page), vec![(5, 0)]);
        assert_ne!(fingerprint(&repointed, &limits), Ok(before));
    }
}
