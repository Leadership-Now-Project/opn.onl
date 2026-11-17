//! Secure View: a file link that opens a viewer instead of handing over the
//! file.
//!
//! Real router, real Postgres, a `MemoryFileStore` for the bucket. What these
//! tests pin:
//!
//! * `GET /{code}` on a Secure View link signs nothing, counts nothing and
//!   sends the visitor to the viewer with a ticket in the fragment;
//! * the bytes are reached only with a ticket, an email and a token in a
//!   header, and every check of the link is asked again on the way;
//! * a one-time link is used up by the first email, not by the redirect;
//! * no answer here contains an address of the bucket;
//! * the email is shown to the link's creator and the organization's admins,
//!   and to nobody else, the realtime stream included;
//! * the protected copy of a PDF is made, served, and removed with the link.
//!
//! Sweeps over the whole database, and the retention scrub, are in
//! `integration_serial/secure_view_sweep_tests.rs`.

use crate::common;

use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, REFERRER_POLICY,
};
use axum_test::{TestResponse, TestServer};
use common::{spawn_real_app, spawn_real_app_with_files, unique_email};
use opn_onl_backend::entity::{click_events, link_files, links, org_members, organizations, users};
use opn_onl_backend::utils::ClickBuffer;
use opn_onl_backend::utils::create_jwt;
use opn_onl_backend::utils::files::{FileSharing, MemoryFileStore};
use opn_onl_backend::utils::secure_view::{Protected, make_protected_copy};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use serde_json::{Value, json};
use std::future::IntoFuture;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------- fixtures

struct Person {
    id: i32,
    token: String,
}

async fn person_with(db: &DatabaseConnection, admin: bool) -> Person {
    let email = unique_email();
    let user = users::ActiveModel {
        email: Set(email.clone()),
        password_hash: Set("not-a-real-hash".to_string()),
        created_at: Set(chrono::Utc::now().naive_utc()),
        email_verified: Set(true),
        is_admin: Set(admin),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert user");
    let token = create_jwt(user.id, &email, user.token_version).expect("mint token");
    Person { id: user.id, token }
}

async fn person(db: &DatabaseConnection) -> Person {
    person_with(db, false).await
}

/// A one-page PDF, written by hand and valid: four objects, a cross-reference
/// table with correct offsets, and a trailer without an ID.
fn small_pdf() -> Vec<u8> {
    let content = "BT /F1 18 Tf 20 100 Td (Secure View fixture) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R \
         /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
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

/// Bytes that are not a PDF, whatever the name says. Nothing can make a
/// protected copy of them, so the viewer is given exactly these.
fn not_a_pdf(tag: &str) -> Vec<u8> {
    format!("not really a pdf: {tag} {}", uuid::Uuid::new_v4()).into_bytes()
}

/// One test's bucket. Every app made from it shares the bucket, and each app
/// has its own rate limiters: the mock transport puts all callers in one
/// bucket, so a test takes a fresh app per call rather than tripping one.
struct Bucket {
    db: DatabaseConnection,
    store: Arc<MemoryFileStore>,
    protect_max_bytes: u64,
}

impl Bucket {
    async fn new() -> Self {
        Self {
            db: common::setup_test_db().await,
            store: Arc::new(MemoryFileStore::new()),
            protect_max_bytes: 1_000_000,
        }
    }

    fn sharing(&self) -> FileSharing {
        let mut sharing = FileSharing::with_store(self.store.clone());
        sharing.max_bytes = 5_000_000;
        sharing.protect_max_bytes = self.protect_max_bytes;
        sharing
    }

    async fn app(&self) -> TestServer {
        spawn_real_app_with_files(self.sharing()).await.0
    }

    async fn app_and_clicks(&self) -> (TestServer, Arc<ClickBuffer>) {
        let (server, _, clicks) = spawn_real_app_with_files(self.sharing()).await;
        (server, clicks)
    }

    /// Start an upload and put `bytes` where the browser would. Returns the
    /// upload id and the storage key.
    async fn uploaded(
        &self,
        who: &Person,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> (i64, String) {
        let res = self
            .app()
            .await
            .post("/files/uploads")
            .authorization_bearer(&who.token)
            .json(&json!({
                "filename": filename,
                "content_type": content_type,
                "size_bytes": bytes.len(),
            }))
            .await;
        assert_eq!(res.status_code(), 201, "start upload: {}", res.text());
        let started: Value = res.json();
        let url = url::Url::parse(started["upload_url"].as_str().unwrap()).unwrap();
        let key = url.path().trim_start_matches('/').to_string();
        self.store.put_bytes(&key, bytes.to_vec());
        (started["upload_id"].as_i64().unwrap(), key)
    }

    /// Upload `bytes` and complete the upload with `fields`. Returns the link
    /// the API answered with and the storage key.
    async fn shared(
        &self,
        who: &Person,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
        fields: Value,
    ) -> (Value, String) {
        let (upload_id, key) = self.uploaded(who, filename, content_type, bytes).await;
        let res = self
            .app()
            .await
            .post(&format!("/files/uploads/{upload_id}/complete"))
            .authorization_bearer(&who.token)
            .json(&fields)
            .await;
        assert_eq!(res.status_code(), 201, "complete upload: {}", res.text());
        (res.json(), key)
    }

    /// A Secure View link to an image. No protected copy is made of an image
    /// and the bucket is not read for one, so the bytes the viewer gets are
    /// the ones returned here and every read of the bucket is the viewer's.
    async fn secure(&self, who: &Person, fields: Value) -> Secure {
        let bytes = not_a_pdf("secure");
        let mut fields = fields;
        fields["secure_view"] = json!(true);
        let (link, key) = self
            .shared(who, "plan.png", "image/png", &bytes, fields)
            .await;
        Secure {
            id: link["id"].as_i64().unwrap() as i32,
            code: link["code"].as_str().unwrap().to_string(),
            key,
            bytes,
        }
    }

    /// `GET /{code}` as a visitor's browser.
    async fn visit(&self, code: &str) -> TestResponse {
        self.app().await.get(&format!("/{code}")).await
    }

    /// The ticket a visitor of `code` is given.
    async fn ticket(&self, code: &str) -> String {
        ticket_in(&self.visit(code).await)
    }

    /// `POST /{code}/view`.
    async fn open(&self, code: &str, ticket: &str, email: &str) -> TestResponse {
        self.app()
            .await
            .post(&format!("/{code}/view"))
            .json(&json!({"ticket": ticket, "email": email}))
            .await
    }

    /// A view token for `code`, through the whole visit.
    async fn view_token(&self, code: &str, email: &str) -> String {
        let ticket = self.ticket(code).await;
        let res = self.open(code, &ticket, email).await;
        assert_eq!(res.status_code(), 200, "open view: {}", res.text());
        res.json::<Value>()["view_token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// `GET /{code}/view/file` with the token in the header.
    async fn file(&self, code: &str, token: &str) -> TestResponse {
        self.app()
            .await
            .get(&format!("/{code}/view/file"))
            .add_header("x-view-token", token)
            .await
    }
}

struct Secure {
    id: i32,
    code: String,
    key: String,
    bytes: Vec<u8>,
}

/// The ticket in the fragment of a redirect to the viewer.
fn ticket_in(res: &TestResponse) -> String {
    assert_eq!(res.status_code(), 302, "{}", res.text());
    let location = res.header(LOCATION).to_str().unwrap().to_string();
    let (_, ticket) = location
        .split_once("#t=")
        .unwrap_or_else(|| panic!("no ticket in {location}"));
    ticket.to_string()
}

async fn file_row(db: &DatabaseConnection, key: &str) -> link_files::Model {
    link_files::Entity::find()
        .filter(link_files::Column::StorageKey.eq(key))
        .one(db)
        .await
        .unwrap()
        .expect("link_files row")
}

async fn link_row(db: &DatabaseConnection, id: i32) -> links::Model {
    links::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("link row")
}

async fn change_link(
    db: &DatabaseConnection,
    id: i32,
    change: impl FnOnce(&mut links::ActiveModel),
) {
    let mut active: links::ActiveModel = link_row(db, id).await.into();
    change(&mut active);
    active.update(db).await.expect("update link");
}

/// Nothing in an answer says where a file is stored or how to fetch it.
fn assert_no_storage_details(res: &TestResponse, key: &str, what: &str) {
    let mut text = res.text();
    for (name, value) in res.headers() {
        text.push_str(&format!("\n{name}: {}", value.to_str().unwrap_or("")));
    }
    for secret in [
        key,
        "downloads.test",
        "uploads.test",
        "X-Amz-",
        "amazonaws",
        "opn-file:",
        "storage_key",
        "protected_key",
    ] {
        assert!(
            !text.contains(secret),
            "{what} must not contain {secret:?}: {text}"
        );
    }
}

/// A token of the same shape the server signs, with a lifetime the test
/// chooses, so an expired one can be made without waiting.
fn mint(purpose: &str, link: &links::Model, file_id: i32, email: Option<&str>, ttl: i64) -> String {
    sign_claims(&claims(purpose, link, file_id, email, ttl))
}

/// The claims of [`mint`], for a test that needs to change one.
fn claims(
    purpose: &str,
    link: &links::Model,
    file_id: i32,
    email: Option<&str>,
    ttl: i64,
) -> Value {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};

    let fingerprint = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(
        link.password_hash.as_deref().unwrap_or("").as_bytes(),
    ));
    let mut claims = json!({
        "sub": purpose,
        "exp": chrono::Utc::now().timestamp() + ttl,
        "link_id": link.id,
        "file_id": file_id,
        "code": link.code,
        "password_fingerprint": fingerprint,
        // A file's generation is 0 until its switch is turned.
        "generation": 0,
    });
    if purpose == "secure_view_ticket" {
        claims["jti"] = json!(uuid::Uuid::new_v4().simple().to_string());
    }
    if let Some(email) = email {
        claims["email"] = json!(email);
    }
    claims
}

fn sign_claims(claims: &Value) -> String {
    let secret = std::env::var("JWT_SECRET").expect("JWT_SECRET");
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap()
}

const SESSION_EXPIRED: &str = "This viewing session has expired. Open the link again.";

fn assert_session_expired(res: &TestResponse, what: &str) {
    assert_eq!(res.status_code(), 401, "{what}: {}", res.text());
    assert_eq!(
        res.json::<Value>(),
        json!({"error": SESSION_EXPIRED}),
        "{what}"
    );
}

// ------------------------------------------------------------ the redirect

#[tokio::test]
async fn a_secure_link_sends_the_visitor_to_the_viewer_and_signs_nothing() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;

    let (app, clicks) = bucket.app_and_clicks().await;
    let res = app.get(&format!("/{}", secure.code)).await;
    assert_eq!(res.status_code(), 302, "{}", res.text());
    let location = res.header(LOCATION).to_str().unwrap().to_string();
    let frontend = std::env::var("FRONTEND_URL").unwrap();
    let (page, ticket) = location.split_once("#t=").expect("ticket in the fragment");
    assert_eq!(
        page,
        format!("{}/v/{}", frontend.trim_end_matches('/'), secure.code),
        "the viewer page, with the ticket only in the fragment"
    );
    assert!(!page.contains('?'), "nothing in the query: {location}");
    assert_eq!(ticket.matches('.').count(), 2, "a signed token: {ticket}");
    assert_eq!(res.header(CACHE_CONTROL), "no-store");
    assert_eq!(res.header(REFERRER_POLICY), "no-referrer");
    assert_no_storage_details(&res, &secure.key, "the redirect");

    // No address was signed, nothing was read from the bucket, no click.
    assert!(bucket.store.downloads().is_empty(), "nothing is signed");
    assert!(bucket.store.reads().is_empty());
    assert_eq!(clicks.pending_count(secure.id), 0);
    assert_eq!(clicks.queued_event_count(), 0);
    assert_eq!(link_row(&bucket.db, secure.id).await.click_count, 0);

    // A HEAD gets what any file link gets: no destination.
    let head = bucket
        .app()
        .await
        .method(axum::http::Method::HEAD, &format!("/{}", secure.code))
        .await;
    assert_eq!(head.status_code(), 200);
    assert!(head.maybe_header(LOCATION).is_none());
}

#[tokio::test]
async fn a_file_link_without_secure_view_is_served_as_before() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, key) = bucket
        .shared(
            &alice,
            "plan.pdf",
            "application/pdf",
            &not_a_pdf("plain"),
            json!({}),
        )
        .await;
    assert_eq!(link["file"]["secure_view"], json!(false));
    assert_eq!(link["file"]["secure_view_eligible"], json!(true));
    assert!(!file_row(&bucket.db, &key).await.secure_view);

    let res = bucket.visit(link["code"].as_str().unwrap()).await;
    assert_eq!(res.status_code(), 302);
    let location = res.header(LOCATION).to_str().unwrap().to_string();
    assert!(
        location.starts_with(&format!("https://downloads.test/{key}?")),
        "the signed address, as before: {location}"
    );
    assert_eq!(bucket.store.downloads().len(), 1);

    // And the viewer's calls say nothing about it.
    let code = link["code"].as_str().unwrap();
    assert_session_expired(
        &bucket.open(code, "x.y.z", "pat@example.org").await,
        "open view",
    );
    assert_session_expired(&bucket.file(code, "x.y.z").await, "view file");
}

// ---------------------------------------------------------------- the view

#[tokio::test]
async fn an_email_and_the_ticket_open_the_view_and_the_token_fetches_the_exact_bytes() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({"title": "Board pack"})).await;
    let ticket = bucket.ticket(&secure.code).await;

    let (app, clicks) = bucket.app_and_clicks().await;
    let res = app
        .post(&format!("/{}/view", secure.code))
        .json(&json!({"ticket": ticket, "email": "  Pat.Lee@Example.ORG "}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(res.header(CACHE_CONTROL), "no-store");
    assert_no_storage_details(&res, &secure.key, "the view");
    let view: Value = res.json();
    assert_eq!(view["expires_in"], json!(300));
    assert_eq!(view["title"], json!("Board pack"));
    assert_eq!(
        view["file"],
        json!({
            "kind": "image",
            "content_type": "image/png",
            "filename": "plan.png",
            "size_bytes": secure.bytes.len(),
        })
    );
    assert_eq!(view["watermark"]["email"], json!("pat.lee@example.org"));
    chrono::DateTime::parse_from_rfc3339(view["watermark"]["at"].as_str().unwrap())
        .expect("RFC 3339");
    // The visit is counted here, with the email, once.
    assert_eq!(clicks.pending_count(secure.id), 1);
    clicks.flush(&bucket.db).await;
    let events = click_events::Entity::find()
        .filter(click_events::Column::LinkId.eq(secure.id))
        .all(&bucket.db)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].viewer_email.as_deref(),
        Some("pat.lee@example.org")
    );

    let token = view["view_token"].as_str().unwrap();
    let file = bucket.file(&secure.code, token).await;
    assert_eq!(file.status_code(), 200, "{}", file.text());
    assert_eq!(file.as_bytes().as_ref(), secure.bytes.as_slice());
    assert_eq!(file.header(CONTENT_TYPE), "image/png");
    assert_eq!(file.header(CONTENT_DISPOSITION), "inline");
    assert_eq!(file.header(CACHE_CONTROL), "no-store, private");
    assert_eq!(file.header("x-content-type-options"), "nosniff");
    assert_eq!(
        file.header(CONTENT_LENGTH),
        secure.bytes.len().to_string().as_str()
    );
    assert_no_storage_details(
        &bucket.file(&secure.code, "x.y.z").await,
        &secure.key,
        "a refusal",
    );

    // `Authorization: Bearer` works too, and fetching again counts nothing.
    let again = bucket
        .app()
        .await
        .get(&format!("/{}/view/file", secure.code))
        .authorization_bearer(token)
        .await;
    assert_eq!(again.status_code(), 200);
    assert_eq!(again.as_bytes().as_ref(), secure.bytes.as_slice());
    assert_eq!(link_row(&bucket.db, secure.id).await.click_count, 1);
    assert_eq!(
        click_events::Entity::find()
            .filter(click_events::Column::LinkId.eq(secure.id))
            .all(&bucket.db)
            .await
            .unwrap()
            .len(),
        1,
        "the file request is not a click"
    );
    // The bucket never signed an address for this link.
    assert!(bucket.store.downloads().is_empty());
}

#[tokio::test]
async fn an_image_is_shown_as_an_image() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let bytes = b"\x89PNG\r\n\x1a\nnot much of a picture".to_vec();
    let (link, _) = bucket
        .shared(
            &alice,
            "chart.png",
            "image/png",
            &bytes,
            json!({"secure_view": true}),
        )
        .await;
    let code = link["code"].as_str().unwrap();
    let ticket = bucket.ticket(code).await;
    let view: Value = bucket.open(code, &ticket, "pat@example.org").await.json();
    assert_eq!(view["file"]["kind"], json!("image"));
    assert_eq!(view["file"]["content_type"], json!("image/png"));
    assert_eq!(view["title"], json!("chart.png"), "the file's name");
    let file = bucket
        .file(code, view["view_token"].as_str().unwrap())
        .await;
    assert_eq!(file.header(CONTENT_TYPE), "image/png");
    assert_eq!(file.as_bytes().as_ref(), bytes.as_slice());
}

#[tokio::test]
async fn without_a_valid_ticket_every_refusal_is_the_same() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    let other = bucket.secure(&alice, json!({})).await;
    let link = link_row(&bucket.db, secure.id).await;
    let file_id = file_row(&bucket.db, &secure.key).await.id;
    let email = "pat@example.org";

    // The control: a ticket minted here is accepted, so the refusals below
    // are about the ticket and not about this test's way of signing.
    let good = mint("secure_view_ticket", &link, file_id, None, 600);
    assert_eq!(
        bucket.open(&secure.code, &good, email).await.status_code(),
        200
    );

    let others_ticket = bucket.ticket(&other.code).await;
    let a_view_token = bucket.view_token(&secure.code, email).await;
    let mut without_an_id = claims("secure_view_ticket", &link, file_id, None, 600);
    without_an_id.as_object_mut().unwrap().remove("jti");
    let mut another_generation = claims("secure_view_ticket", &link, file_id, None, 600);
    another_generation["generation"] = json!(1);
    for (what, ticket) in [
        ("a ticket without an id", sign_claims(&without_an_id)),
        (
            "a ticket from another turn of the switch",
            sign_claims(&another_generation),
        ),
        ("no ticket", String::new()),
        ("not a token", "garbage".to_string()),
        ("an unsigned token", "a.b.c".to_string()),
        (
            "an expired ticket",
            mint("secure_view_ticket", &link, file_id, None, -1),
        ),
        ("another link's ticket", others_ticket),
        ("a view token used as a ticket", a_view_token),
        (
            "a ticket for another file",
            mint("secure_view_ticket", &link, file_id + 1, None, 600),
        ),
    ] {
        let res = bucket.open(&secure.code, &ticket, email).await;
        assert_session_expired(&res, what);
        assert_no_storage_details(&res, &secure.key, what);
    }

    // A code that does not exist, a deleted link, a file link without Secure
    // View and an ordinary link: the same answer as a bad ticket.
    assert_session_expired(
        &bucket.open("no-such-code-here", &good, email).await,
        "a missing code",
    );
    let (plain, _) = bucket
        .shared(&alice, "a.pdf", "application/pdf", b"x", json!({}))
        .await;
    assert_session_expired(
        &bucket
            .open(plain["code"].as_str().unwrap(), &good, email)
            .await,
        "a file link without Secure View",
    );
    let url_link: Value = bucket
        .app()
        .await
        .post("/links")
        .authorization_bearer(&alice.token)
        .json(&json!({"original_url": "https://iana.org/somewhere"}))
        .await
        .json();
    assert_session_expired(
        &bucket
            .open(url_link["code"].as_str().unwrap(), &good, email)
            .await,
        "an ordinary link",
    );
    change_link(&bucket.db, secure.id, |link| {
        link.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    })
    .await;
    assert_session_expired(
        &bucket.open(&secure.code, &good, email).await,
        "a deleted link",
    );
    assert_session_expired(
        &bucket
            .file(
                &secure.code,
                &mint("secure_view_token", &link, file_id, Some(email), 600),
            )
            .await,
        "a deleted link's file",
    );
}

#[tokio::test]
async fn an_email_that_is_not_one_is_refused_and_not_repeated() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;

    for bad in [
        "",
        "   ",
        "pat-at-example",
        "pat@",
        "a@b",
        "pat@[127.0.0.1]",
        "<img src=x onerror=alert(1)>@example.org",
        "pat@example.org\r\nX-Injected: 1",
    ] {
        let (app, clicks) = bucket.app_and_clicks().await;
        let res = app
            .post(&format!("/{}/view", secure.code))
            .json(&json!({"ticket": ticket, "email": bad}))
            .await;
        assert_eq!(res.status_code(), 400, "{bad:?}: {}", res.text());
        assert_eq!(
            res.json::<Value>(),
            json!({"error": "Enter a valid email address"}),
            "the value is not repeated"
        );
        assert_eq!(clicks.pending_count(secure.id), 0, "nothing is counted");
    }
    // The ticket is still good for a real address.
    assert_eq!(
        bucket
            .open(&secure.code, &ticket, "pat@example.org")
            .await
            .status_code(),
        200
    );
}

#[tokio::test]
async fn every_check_of_the_link_is_asked_again_when_the_view_is_opened() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let email = "pat@example.org";

    // Expired after the ticket was issued.
    let secure = bucket.secure(&alice, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;
    change_link(&bucket.db, secure.id, |link| {
        link.expires_at = Set(Some(
            chrono::Utc::now().naive_utc() - chrono::Duration::minutes(1),
        ));
    })
    .await;
    let res = bucket.open(&secure.code, &ticket, email).await;
    assert_eq!(res.status_code(), 410, "{}", res.text());
    assert_eq!(res.json::<Value>(), json!({"error": "Link has expired"}));

    // Its owner's account disabled.
    let secure = bucket.secure(&alice, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;
    let token = bucket.view_token(&secure.code, email).await;
    let mut owner: users::ActiveModel = users::Entity::find_by_id(alice.id)
        .one(&bucket.db)
        .await
        .unwrap()
        .unwrap()
        .into();
    owner.disabled_at = Set(Some(chrono::Utc::now().naive_utc()));
    owner.update(&bucket.db).await.unwrap();
    let res = bucket.open(&secure.code, &ticket, email).await;
    assert_eq!(res.status_code(), 410, "{}", res.text());
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "This link has been disabled"})
    );
    let res = bucket.file(&secure.code, &token).await;
    assert_eq!(res.status_code(), 410, "the file too: {}", res.text());

    let bob = person(&bucket.db).await;

    // The password changed: the ticket was issued under the old one.
    let secure = bucket.secure(&bob, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;
    let token = bucket.view_token(&secure.code, email).await;
    change_link(&bucket.db, secure.id, |link| {
        link.password_hash = Set(Some("a-new-password-hash".to_string()));
    })
    .await;
    assert_session_expired(
        &bucket.open(&secure.code, &ticket, email).await,
        "after a password change",
    );
    assert_session_expired(
        &bucket.file(&secure.code, &token).await,
        "the file after a password change",
    );

    // Secure View switched off.
    let secure = bucket.secure(&bob, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;
    let token = bucket.view_token(&secure.code, email).await;
    let res = bucket
        .app()
        .await
        .put(&format!("/links/{}/secure-view", secure.id))
        .authorization_bearer(&bob.token)
        .json(&json!({"enabled": false}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_session_expired(
        &bucket.open(&secure.code, &ticket, email).await,
        "after Secure View is switched off",
    );
    assert_session_expired(
        &bucket.file(&secure.code, &token).await,
        "the file after Secure View is switched off",
    );

    // The file removed from the bucket by a sweep.
    let secure = bucket.secure(&bob, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;
    let token = bucket.view_token(&secure.code, email).await;
    let mut swept: link_files::ActiveModel = file_row(&bucket.db, &secure.key).await.into();
    swept.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    swept.update(&bucket.db).await.unwrap();
    assert_session_expired(
        &bucket.open(&secure.code, &ticket, email).await,
        "after the file was swept",
    );
    assert_session_expired(
        &bucket.file(&secure.code, &token).await,
        "the file after it was swept",
    );

    // The object gone from the bucket while the row still says it is there.
    let secure = bucket.secure(&bob, json!({})).await;
    let token = bucket.view_token(&secure.code, email).await;
    use opn_onl_backend::utils::files::FileStore;
    bucket.store.delete(&secure.key).await.unwrap();
    let res = bucket.file(&secure.code, &token).await;
    assert_eq!(res.status_code(), 410, "{}", res.text());
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "This file is no longer available"})
    );

    // Expired while the viewer was open: the token stops fetching.
    let secure = bucket.secure(&bob, json!({})).await;
    let token = bucket.view_token(&secure.code, email).await;
    change_link(&bucket.db, secure.id, |link| {
        link.expires_at = Set(Some(
            chrono::Utc::now().naive_utc() - chrono::Duration::minutes(1),
        ));
    })
    .await;
    let res = bucket.file(&secure.code, &token).await;
    assert_eq!(res.status_code(), 410, "{}", res.text());
    assert_eq!(res.json::<Value>(), json!({"error": "Link has expired"}));

    // Rescheduled to start later: the open call and the file request are
    // asked the same question, and the link itself agrees with both.
    let secure = bucket.secure(&bob, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;
    let token = bucket.view_token(&secure.code, email).await;
    change_link(&bucket.db, secure.id, |link| {
        link.starts_at = Set(Some(
            chrono::Utc::now().naive_utc() + chrono::Duration::hours(1),
        ));
    })
    .await;
    let not_yet = json!({"error": "Link is scheduled to activate later"});
    assert_eq!(bucket.visit(&secure.code).await.status_code(), 410);
    let res = bucket.open(&secure.code, &ticket, email).await;
    assert_eq!(res.status_code(), 410, "{}", res.text());
    assert_eq!(res.json::<Value>(), not_yet);
    let res = bucket.file(&secure.code, &token).await;
    assert_eq!(res.status_code(), 410, "the file too: {}", res.text());
    assert_eq!(res.json::<Value>(), not_yet);
    assert_eq!(res.header(CACHE_CONTROL), "no-store");
}

#[tokio::test]
async fn switching_secure_view_off_ends_tickets_and_tokens_even_if_it_is_switched_on_again() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    let email = "pat@example.org";
    let ticket = bucket.ticket(&secure.code).await;
    let token = bucket.view_token(&secure.code, email).await;
    assert_eq!(bucket.file(&secure.code, &token).await.status_code(), 200);
    assert_eq!(
        file_row(&bucket.db, &secure.key)
            .await
            .secure_view_generation,
        0
    );

    for (enabled, generation) in [(false, 1), (true, 2)] {
        let res = bucket
            .app()
            .await
            .put(&format!("/links/{}/secure-view", secure.id))
            .authorization_bearer(&alice.token)
            .json(&json!({"enabled": enabled}))
            .await;
        assert_eq!(res.status_code(), 200, "{}", res.text());
        let row = file_row(&bucket.db, &secure.key).await;
        assert_eq!(row.secure_view, enabled);
        assert_eq!(row.secure_view_generation, generation, "one turn, one step");
    }
    // Asking for what it already is turns nothing.
    let res = bucket
        .app()
        .await
        .put(&format!("/links/{}/secure-view", secure.id))
        .authorization_bearer(&alice.token)
        .json(&json!({"enabled": true}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(
        file_row(&bucket.db, &secure.key)
            .await
            .secure_view_generation,
        2
    );

    // Secure View is on, the link is live, the ticket and the token are
    // minutes old: neither works, because the switch was turned in between.
    assert_session_expired(
        &bucket.open(&secure.code, &ticket, email).await,
        "a ticket from before the switch was turned",
    );
    assert_session_expired(
        &bucket.file(&secure.code, &token).await,
        "a token from before the switch was turned",
    );

    // A visitor who opens the link now is served as usual.
    let token = bucket.view_token(&secure.code, email).await;
    assert_eq!(bucket.file(&secure.code, &token).await.status_code(), 200);
}

#[tokio::test]
async fn a_ticket_opens_one_view() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    change_link(&bucket.db, secure.id, |link| {
        link.max_clicks = Set(Some(3));
    })
    .await;
    let ticket = bucket.ticket(&secure.code).await;

    // A mistyped address does not use the ticket up.
    assert_eq!(
        bucket
            .open(&secure.code, &ticket, "not-an-address")
            .await
            .status_code(),
        400
    );
    let first = bucket.open(&secure.code, &ticket, "pat@example.org").await;
    assert_eq!(first.status_code(), 200, "{}", first.text());

    // The same ticket again, whatever the address: refused like any ticket
    // that is not valid, and the capped link is not run down by it.
    for n in 0..4 {
        let again = bucket
            .open(&secure.code, &ticket, &format!("p{n}@example.org"))
            .await;
        assert_session_expired(&again, "a ticket that has opened a view");
    }
    assert_eq!(link_row(&bucket.db, secure.id).await.click_count, 1);

    // Another visit is another ticket.
    let second = bucket.ticket(&secure.code).await;
    assert_eq!(
        bucket
            .open(&secure.code, &second, "sam@example.org")
            .await
            .status_code(),
        200
    );
    assert_eq!(link_row(&bucket.db, secure.id).await.click_count, 2);
}

#[tokio::test]
async fn one_ticket_sent_many_times_at_once_opens_one_view() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    // No cap: nothing but the ticket itself stops the others.
    let secure = bucket.secure(&alice, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;

    let (app, clicks) = bucket.app_and_clicks().await;
    let path = format!("/{}/view", secure.code);
    let attempts = (0..8).map(|n| {
        app.post(&path)
            .json(&json!({"ticket": ticket, "email": format!("p{n}@example.org")}))
            .into_future()
    });
    let answers = futures::future::join_all(attempts).await;
    let count = |status: u16| {
        answers
            .iter()
            .filter(|res| res.status_code() == status)
            .count()
    };
    assert_eq!((count(200), count(401)), (1, 7), "exactly one view");
    assert_eq!(clicks.pending_count(secure.id), 1, "and one click");
}

#[tokio::test]
async fn used_tickets_are_forgotten_once_they_would_have_expired() {
    use opn_onl_backend::utils::secure_view::forget_expired_tickets;
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let db = common::setup_test_db().await;
    let ticket_row = |id: &str, minutes: i64| {
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO secure_view_tickets (id, expires_at) VALUES ($1, $2)",
            [
                id.to_string().into(),
                (chrono::Utc::now().naive_utc() + chrono::Duration::minutes(minutes)).into(),
            ],
        )
    };
    let old = format!("old-{}", uuid::Uuid::new_v4().simple());
    let live = format!("live-{}", uuid::Uuid::new_v4().simple());
    db.execute(ticket_row(&old, -1)).await.unwrap();
    db.execute(ticket_row(&live, 10)).await.unwrap();

    assert!(forget_expired_tickets(&db).await.unwrap() >= 1);

    let remaining = |id: String| {
        let db = db.clone();
        async move {
            db.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 AS found FROM secure_view_tickets WHERE id = $1",
                [id.into()],
            ))
            .await
            .unwrap()
            .is_some()
        }
    };
    assert!(!remaining(old).await, "expired a minute ago");
    assert!(remaining(live).await, "still needed for ten minutes");
}

#[tokio::test]
async fn a_one_time_link_is_used_up_by_the_first_email_and_by_nothing_before_it() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    change_link(&bucket.db, secure.id, |link| {
        link.burn_after_reading = Set(true);
        link.max_clicks = Set(Some(1));
    })
    .await;
    let path = format!("/{}", secure.code);

    // A HEAD, a link unfurler and any number of redirects use up nothing.
    let head = bucket
        .app()
        .await
        .method(axum::http::Method::HEAD, &path)
        .await;
    assert_eq!(head.status_code(), 200);
    assert!(head.maybe_header(LOCATION).is_none());
    let unfurl = bucket
        .app()
        .await
        .get(&path)
        .add_header(
            "user-agent",
            "Slackbot-LinkExpanding 1.0 (+https://api.slack.com/robots)",
        )
        .await;
    assert_eq!(unfurl.status_code(), 200, "{}", unfurl.text());
    assert!(unfurl.maybe_header(LOCATION).is_none());
    let first_ticket = bucket.ticket(&secure.code).await;
    let second_ticket = bucket.ticket(&secure.code).await;
    let row = link_row(&bucket.db, secure.id).await;
    assert_eq!(row.click_count, 0, "nothing is used up before the email");
    assert_eq!(row.burned_at, None);

    // The first email opens it and uses it up.
    let res = bucket
        .open(&secure.code, &first_ticket, "first@example.org")
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    let token = res.json::<Value>()["view_token"]
        .as_str()
        .unwrap()
        .to_string();
    let row = link_row(&bucket.db, secure.id).await;
    assert_eq!(row.click_count, 1);
    assert!(row.burned_at.is_some(), "burned by the view");

    // A second email is refused, with the same ticket or a fresh-looking one.
    for ticket in [&first_ticket, &second_ticket] {
        let res = bucket
            .open(&secure.code, ticket, "second@example.org")
            .await;
        assert_eq!(res.status_code(), 410, "{}", res.text());
        assert_eq!(
            res.json::<Value>(),
            json!({"error": "This one-time link has already been opened"})
        );
    }
    assert_eq!(bucket.visit(&secure.code).await.status_code(), 410);
    assert_eq!(link_row(&bucket.db, secure.id).await.click_count, 1);

    // The viewer that opened it can still fetch the file, more than once.
    for _ in 0..2 {
        let file = bucket.file(&secure.code, &token).await;
        assert_eq!(file.status_code(), 200, "{}", file.text());
        assert_eq!(file.as_bytes().as_ref(), secure.bytes.as_slice());
    }
    assert_eq!(link_row(&bucket.db, secure.id).await.click_count, 1);
}

#[tokio::test]
async fn two_emails_at_once_cannot_both_open_a_one_time_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    change_link(&bucket.db, secure.id, |link| {
        link.burn_after_reading = Set(true);
        link.max_clicks = Set(Some(1));
    })
    .await;
    // Eight visitors, each with a ticket of their own: what lets one of
    // them in and no more is the link's cap, not the ticket.
    let mut tickets = Vec::new();
    for _ in 0..8 {
        tickets.push(bucket.ticket(&secure.code).await);
    }

    let app = bucket.app().await;
    let path = format!("/{}/view", secure.code);
    let attempts = tickets.iter().enumerate().map(|(n, ticket)| {
        app.post(&path)
            .json(&json!({"ticket": ticket, "email": format!("p{n}@example.org")}))
            .into_future()
    });
    let answers = futures::future::join_all(attempts).await;
    let opened = answers
        .iter()
        .filter(|res| res.status_code() == 200)
        .count();
    let refused = answers
        .iter()
        .filter(|res| res.status_code() == 410)
        .count();
    assert_eq!((opened, refused), (1, 7), "exactly one view");
    assert_eq!(link_row(&bucket.db, secure.id).await.click_count, 1);
}

#[tokio::test]
async fn the_file_is_reached_only_with_a_view_token_in_a_header() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    let other = bucket.secure(&alice, json!({})).await;
    let link = link_row(&bucket.db, secure.id).await;
    let file_id = file_row(&bucket.db, &secure.key).await.id;
    let email = "pat@example.org";
    let path = format!("/{}/view/file", secure.code);

    // The control: a token minted here fetches the file.
    let good = mint("secure_view_token", &link, file_id, Some(email), 200);
    assert_eq!(bucket.file(&secure.code, &good).await.status_code(), 200);

    let none = bucket.app().await.get(&path).await;
    assert_session_expired(&none, "no token");
    assert_no_storage_details(&none, &secure.key, "no token");

    let others_token = bucket.view_token(&other.code, email).await;
    let a_ticket = bucket.ticket(&secure.code).await;
    for (what, token) in [
        ("not a token", "garbage".to_string()),
        (
            "an expired token",
            mint("secure_view_token", &link, file_id, Some(email), -1),
        ),
        ("another link's token", others_token),
        ("a ticket used as a token", a_ticket),
        (
            "a token without an email",
            mint("secure_view_token", &link, file_id, None, 200),
        ),
        (
            "a token for another file",
            mint("secure_view_token", &link, file_id + 1, Some(email), 200),
        ),
    ] {
        assert_session_expired(&bucket.file(&secure.code, &token).await, what);
    }

    // Never from the address: a token there would be in logs and history.
    for name in ["token", "view_token", "t", "x-view-token", "access_token"] {
        let res = bucket
            .app()
            .await
            .get(&path)
            .add_query_param(name, &good)
            .await;
        assert_session_expired(&res, &format!("?{name}="));
    }
    // A missing code is the same answer.
    assert_session_expired(
        &bucket.file("no-such-code-here", &good).await,
        "a missing code",
    );
    assert!(
        bucket.store.reads().len() == 1,
        "the bucket was read once, for the control: {:?}",
        bucket.store.reads()
    );
}

#[tokio::test]
async fn a_password_link_asks_for_the_password_before_the_viewer() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket
        .secure(&alice, json!({"password": "open sesame"}))
        .await;

    let res = bucket.visit(&secure.code).await;
    assert_eq!(res.status_code(), 307, "{}", res.text());
    assert!(
        res.header(LOCATION)
            .to_str()
            .unwrap()
            .ends_with(&format!("/password/{}", secure.code)),
        "the password page first"
    );

    let verified = bucket
        .app()
        .await
        .post(&format!("/{}/verify", secure.code))
        .json(&json!({"password": "open sesame"}))
        .await;
    assert_eq!(verified.status_code(), 200, "{}", verified.text());
    assert_no_storage_details(&verified, &secure.key, "the unlock");
    let continue_at = verified.json::<Value>()["redirect_url"]
        .as_str()
        .unwrap()
        .to_string();
    let unlock = url::Url::parse(&continue_at)
        .unwrap()
        .query_pairs()
        .find(|(name, _)| name == "unlock")
        .map(|(_, value)| value.to_string())
        .expect("unlock token");

    let res = bucket
        .app()
        .await
        .get(&format!("/{}", secure.code))
        .add_query_param("unlock", &unlock)
        .await;
    let ticket = ticket_in(&res);
    assert!(
        !res.header(LOCATION).to_str().unwrap().contains("unlock="),
        "the unlock token does not travel on to the viewer"
    );
    let view = bucket.open(&secure.code, &ticket, "pat@example.org").await;
    assert_eq!(view.status_code(), 200, "{}", view.text());
    let token = view.json::<Value>()["view_token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(bucket.file(&secure.code, &token).await.status_code(), 200);

    // Removing the password ends the session too: it was opened under it.
    change_link(&bucket.db, secure.id, |link| {
        link.password_hash = Set(None);
    })
    .await;
    assert_session_expired(
        &bucket.file(&secure.code, &token).await,
        "after the password was removed",
    );
}

#[tokio::test]
async fn opening_a_view_has_its_own_small_budget() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    let other = bucket.secure(&alice, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;

    // One app, so one set of limiters: ten a minute for one link.
    let app = bucket.app().await;
    let path = format!("/{}/view", secure.code);
    let body = json!({"ticket": ticket, "email": "pat@example.org"});
    let first = app.post(&path).json(&body).await;
    assert_eq!(first.status_code(), 200, "{}", first.text());
    assert_eq!(
        first.header("x-ratelimit-limit"),
        "10",
        "its own budget, not the redirect one"
    );
    let mut limited = None;
    for _ in 0..10 {
        let res = app.post(&path).json(&body).await;
        if res.status_code() == 429 {
            limited = Some(res);
            break;
        }
    }
    let limited = limited.expect("the eleventh attempt within a minute is refused");
    assert_eq!(limited.header("x-ratelimit-limit"), "10");
    assert!(limited.maybe_header("retry-after").is_some());
    assert_eq!(limited.json::<Value>()["error"], json!("Too many requests"));

    // Another link is a separate budget, and fetching the file is not in it.
    let others_ticket = bucket.ticket(&other.code).await;
    let res = app
        .post(&format!("/{}/view", other.code))
        .json(&json!({"ticket": others_ticket, "email": "pat@example.org"}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    let token = first.json::<Value>()["view_token"]
        .as_str()
        .unwrap()
        .to_string();
    let file = app
        .get(&format!("/{}/view/file", secure.code))
        .add_header("x-view-token", &token)
        .await;
    assert_eq!(file.status_code(), 200);
    assert_eq!(file.header("x-ratelimit-limit"), "100");
}

#[tokio::test]
async fn rotating_codes_does_not_multiply_the_budget() {
    // One app, so one set of limiters. Thirty attempts a minute for one
    // address across every code, of which no single code sees more than
    // eight, so the per-link budget of ten is never what refuses. The codes
    // need not exist: the budget is spent before anything is looked up.
    let bucket = Bucket::new().await;
    let app = bucket.app().await;
    let body = json!({"ticket": "x.y.z", "email": "pat@example.org"});
    let mut answers = Vec::new();
    for attempt in 0..32 {
        let code = format!("rotating-code-{}", attempt % 4);
        answers.push(app.post(&format!("/{code}/view")).json(&body).await);
        // Stay under the general 20-a-second gate, which is not the subject.
        tokio::time::sleep(Duration::from_millis(70)).await;
    }
    for (attempt, res) in answers.iter().enumerate().take(30) {
        assert_eq!(res.status_code(), 401, "attempt {}", attempt + 1);
        assert_eq!(res.header("x-ratelimit-limit"), "10", "the per-link budget");
    }
    for res in &answers[30..] {
        assert_eq!(res.status_code(), 429, "{}", res.text());
        assert_eq!(
            res.header("x-ratelimit-limit"),
            "30",
            "refused by the budget across codes"
        );
        assert!(res.maybe_header("retry-after").is_some());
    }
}

// ------------------------------------------------------- switching it on

#[tokio::test]
async fn only_a_file_a_browser_can_show_may_be_secure_view() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let not_eligible =
        json!({"error": "Secure View works for PDF, PNG, JPEG, GIF and WebP files only"});

    // At completion: refused, and the upload is still there to complete.
    let (upload_id, key) = bucket
        .uploaded(&alice, "bundle.zip", "application/zip", b"PK-zip-bytes")
        .await;
    let complete = format!("/files/uploads/{upload_id}/complete");
    let res = bucket
        .app()
        .await
        .post(&complete)
        .authorization_bearer(&alice.token)
        .json(&json!({"secure_view": true}))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    assert_eq!(res.json::<Value>(), not_eligible);
    let row = file_row(&bucket.db, &key).await;
    assert_eq!(row.status, "pending");
    assert_eq!(row.link_id, None, "no link was made");
    assert!(!row.secure_view);

    let res = bucket
        .app()
        .await
        .post(&complete)
        .authorization_bearer(&alice.token)
        .json(&json!({}))
        .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let zip: Value = res.json();
    assert_eq!(zip["file"]["secure_view"], json!(false));
    assert_eq!(zip["file"]["secure_view_eligible"], json!(false));

    // By edit: refused for the zip.
    let res = bucket
        .app()
        .await
        .put(&format!("/links/{}/secure-view", zip["id"]))
        .authorization_bearer(&alice.token)
        .json(&json!({"enabled": true}))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    assert_eq!(res.json::<Value>(), not_eligible);
    assert!(!file_row(&bucket.db, &key).await.secure_view);
    // Its link still hands the file over.
    let res = bucket.visit(zip["code"].as_str().unwrap()).await;
    assert!(
        res.header(LOCATION)
            .to_str()
            .unwrap()
            .starts_with("https://downloads.test/")
    );

    // A PDF whose name says HTML is not shown in place, so not eligible.
    let (upload_id, _) = bucket
        .uploaded(&alice, "page.html", "application/pdf", b"<html>")
        .await;
    let res = bucket
        .app()
        .await
        .post(&format!("/files/uploads/{upload_id}/complete"))
        .authorization_bearer(&alice.token)
        .json(&json!({"secure_view": true}))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());

    // A link to a URL has no file.
    let url_link: Value = bucket
        .app()
        .await
        .post("/links")
        .authorization_bearer(&alice.token)
        .json(&json!({"original_url": "https://iana.org/somewhere"}))
        .await
        .json();
    let res = bucket
        .app()
        .await
        .put(&format!("/links/{}/secure-view", url_link["id"]))
        .authorization_bearer(&alice.token)
        .json(&json!({"enabled": true}))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "Secure View is only for links that share an uploaded file"})
    );
}

#[tokio::test]
async fn secure_view_is_switched_on_and_off_by_those_who_may_edit_the_link() {
    let bucket = Bucket::new().await;
    let owner = person(&bucket.db).await;
    let editor = person(&bucket.db).await;
    let viewer = person(&bucket.db).await;
    let stranger = person(&bucket.db).await;
    let org = organizations::ActiveModel {
        name: Set("Team".to_string()),
        slug: Set(format!("sv-{}", uuid::Uuid::new_v4().simple())),
        owner_id: Set(owner.id),
        ..Default::default()
    }
    .insert(&bucket.db)
    .await
    .unwrap();
    for (who, role) in [(&owner, "owner"), (&editor, "editor"), (&viewer, "viewer")] {
        org_members::ActiveModel {
            org_id: Set(org.id),
            user_id: Set(who.id),
            role: Set(role.to_string()),
            ..Default::default()
        }
        .insert(&bucket.db)
        .await
        .unwrap();
    }
    let (link, key) = bucket
        .shared(
            &owner,
            "plan.pdf",
            "application/pdf",
            &not_a_pdf("org"),
            json!({"org_id": org.id}),
        )
        .await;
    let id = link["id"].as_i64().unwrap();
    let path = format!("/links/{id}/secure-view");
    let set = |who: &Person, enabled: bool| {
        let (path, token) = (path.clone(), who.token.clone());
        let bucket = &bucket;
        async move {
            bucket
                .app()
                .await
                .put(&path)
                .authorization_bearer(&token)
                .json(&json!({"enabled": enabled}))
                .await
        }
    };

    // Not signed in, a stranger, a viewer: refused, nothing changes.
    let res = bucket
        .app()
        .await
        .put(&path)
        .json(&json!({"enabled": true}))
        .await;
    assert_eq!(res.status_code(), 401);
    assert_eq!(set(&stranger, true).await.status_code(), 403);
    assert_eq!(set(&viewer, true).await.status_code(), 403);
    assert!(!file_row(&bucket.db, &key).await.secure_view);
    // An id that does not exist is the same 403.
    let res = bucket
        .app()
        .await
        .put("/links/2147483000/secure-view")
        .authorization_bearer(&owner.token)
        .json(&json!({"enabled": true}))
        .await;
    assert_eq!(res.status_code(), 403);

    // An editor who did not create the link may: it is the organization's.
    let res = set(&editor, true).await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    let body: Value = res.json();
    assert_eq!(body["id"], json!(id));
    assert_eq!(body["file"]["secure_view"], json!(true));
    assert_eq!(body["original_url"], json!(""));
    assert_no_storage_details(&res, &key, "the answer");
    assert!(file_row(&bucket.db, &key).await.secure_view);
    let code = link["code"].as_str().unwrap();
    assert!(
        bucket
            .visit(code)
            .await
            .header(LOCATION)
            .to_str()
            .unwrap()
            .contains(&format!("/v/{code}#t="))
    );
    // The link list says so.
    let list: Value = bucket
        .app()
        .await
        .get("/links")
        .add_query_param("org_id", org.id)
        .authorization_bearer(&viewer.token)
        .await
        .json();
    let listed = list
        .as_array()
        .expect("a list of links")
        .iter()
        .find(|row| row["id"] == json!(id))
        .expect("the link is listed")
        .clone();
    assert_eq!(listed["file"]["secure_view"], json!(true));

    // Off again: the link hands the file over as before.
    let res = set(&owner, false).await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(res.json::<Value>()["file"]["secure_view"], json!(false));
    assert!(
        bucket
            .visit(code)
            .await
            .header(LOCATION)
            .to_str()
            .unwrap()
            .starts_with("https://downloads.test/")
    );
}

#[tokio::test]
async fn without_file_sharing_the_feature_is_inert() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;
    let token = bucket.view_token(&secure.code, "pat@example.org").await;

    // The same database, on a server with no bucket configured.
    let (off, _) = spawn_real_app().await;
    let res = off.get(&format!("/{}", secure.code)).await;
    assert_eq!(res.status_code(), 410, "{}", res.text());
    let res = off
        .post(&format!("/{}/view", secure.code))
        .json(&json!({"ticket": ticket, "email": "pat@example.org"}))
        .await;
    assert_session_expired(&res, "open view with file sharing off");
    let res = off
        .get(&format!("/{}/view/file", secure.code))
        .add_header("x-view-token", &token)
        .await;
    assert_session_expired(&res, "view file with file sharing off");
    let res = off
        .put(&format!("/links/{}/secure-view", secure.id))
        .authorization_bearer(&alice.token)
        .json(&json!({"enabled": false}))
        .await;
    assert_eq!(res.status_code(), 404, "{}", res.text());
}

// ------------------------------------------------------------- the emails

struct Team {
    org_id: i32,
    owner: Person,
    admin: Person,
    creator: Person,
    colleague: Person,
    viewer: Person,
}

async fn team(db: &DatabaseConnection) -> Team {
    let owner = person(db).await;
    let admin = person(db).await;
    let creator = person(db).await;
    let colleague = person(db).await;
    let viewer = person(db).await;
    let org = organizations::ActiveModel {
        name: Set("Team".to_string()),
        slug: Set(format!("sv-{}", uuid::Uuid::new_v4().simple())),
        owner_id: Set(owner.id),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    for (who, role) in [
        (&owner, "owner"),
        (&admin, "admin"),
        (&creator, "editor"),
        (&colleague, "editor"),
        (&viewer, "viewer"),
    ] {
        org_members::ActiveModel {
            org_id: Set(org.id),
            user_id: Set(who.id),
            role: Set(role.to_string()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
    }
    Team {
        org_id: org.id,
        owner,
        admin,
        creator,
        colleague,
        viewer,
    }
}

#[tokio::test]
async fn the_email_is_shown_to_the_creator_and_the_admins_and_to_no_other_member() {
    let bucket = Bucket::new().await;
    let team = team(&bucket.db).await;
    let secure = bucket
        .secure(&team.creator, json!({"org_id": team.org_id}))
        .await;
    let email = format!("reader-{}@example.org", uuid::Uuid::new_v4().simple());

    let ticket = bucket.ticket(&secure.code).await;
    let (app, clicks) = bucket.app_and_clicks().await;
    let res = app
        .post(&format!("/{}/view", secure.code))
        .json(&json!({"ticket": ticket, "email": email}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    clicks.flush(&bucket.db).await;

    let stats = |who: &Person| {
        let (token, id) = (who.token.clone(), secure.id);
        let bucket = &bucket;
        async move {
            let res = bucket
                .app()
                .await
                .get(&format!("/links/{id}/stats"))
                .authorization_bearer(&token)
                .await;
            assert_eq!(res.status_code(), 200, "{}", res.text());
            res
        }
    };

    for (who, role) in [
        (&team.creator, "the creator"),
        (&team.owner, "the organization's owner"),
        (&team.admin, "an organization admin"),
    ] {
        let res = stats(who).await;
        let body: Value = res.json();
        assert_eq!(body["total_clicks"], json!(1), "{role}");
        assert_eq!(
            body["recent_clicks"][0]["viewer_email"],
            json!(email),
            "{role} sees who viewed it"
        );
    }
    for (who, role) in [
        (&team.colleague, "another editor"),
        (&team.viewer, "a viewer"),
    ] {
        let res = stats(who).await;
        let body: Value = res.json();
        assert_eq!(body["total_clicks"], json!(1), "{role} still sees the view");
        assert_eq!(body["recent_clicks"].as_array().unwrap().len(), 1);
        assert!(
            body["recent_clicks"][0].get("viewer_email").is_none(),
            "{role} is not sent the field"
        );
        assert!(
            !res.text().contains(&email),
            "{role} is not sent the address anywhere: {}",
            res.text()
        );
    }

    // An instance admin who can read the link sees it too.
    let instance_admin = person_with(&bucket.db, true).await;
    org_members::ActiveModel {
        org_id: Set(team.org_id),
        user_id: Set(instance_admin.id),
        role: Set("viewer".to_string()),
        ..Default::default()
    }
    .insert(&bucket.db)
    .await
    .unwrap();
    let body: Value = stats(&instance_admin).await.json();
    assert_eq!(body["recent_clicks"][0]["viewer_email"], json!(email));

    // Nothing else the dashboard reads carries the address.
    for path in [
        format!("/links/{}", secure.id),
        format!("/links/{}/clicks/realtime", secure.id),
        format!("/links?org_id={}", team.org_id),
        "/analytics/dashboard".to_string(),
        format!("/links/export?org_id={}", team.org_id),
    ] {
        let res = bucket
            .app()
            .await
            .get(&path)
            .authorization_bearer(&team.owner.token)
            .await;
        assert!(
            !res.text().contains(&email),
            "{path} must not carry the address: {}",
            res.text()
        );
    }
}

#[tokio::test]
async fn the_creator_of_a_personal_link_sees_the_email() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;
    let (app, clicks) = bucket.app_and_clicks().await;
    app.post(&format!("/{}/view", secure.code))
        .json(&json!({"ticket": ticket, "email": "reader@example.org"}))
        .await;
    clicks.flush(&bucket.db).await;
    let body: Value = bucket
        .app()
        .await
        .get(&format!("/links/{}/stats", secure.id))
        .authorization_bearer(&alice.token)
        .await
        .json();
    assert_eq!(
        body["recent_clicks"][0]["viewer_email"],
        json!("reader@example.org")
    );
}

#[tokio::test]
async fn the_realtime_stream_never_carries_the_email() {
    use opn_onl_backend::handlers::websocket::WsState;

    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let secure = bucket.secure(&alice, json!({})).await;
    let ticket = bucket.ticket(&secure.code).await;

    // The real router over HTTP (a WebSocket cannot upgrade on the mock
    // transport), with file sharing and a real realtime state.
    let ws = Arc::new(WsState::new());
    let mut state = opn_onl_backend::AppState::for_tests(bucket.db.clone()).await;
    state.files = Some(bucket.sharing());
    state.ws_state = Some(ws.clone());
    let server = TestServer::builder()
        .http_transport()
        .build(opn_onl_backend::build_router(state))
        .expect("ws test server");

    let mut socket = server
        .get_websocket("/ws")
        .add_query_param("token", &alice.token)
        .await
        .into_websocket()
        .await;
    for _ in 0..200 {
        if ws.click_tx.receiver_count() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut observer = ws.click_tx.subscribe();

    let email = "watched-reader@example.org";
    let res = server
        .post(&format!("/{}/view", secure.code))
        .json(&json!({"ticket": ticket, "email": email}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());

    let frame: Value = socket.receive_json().await;
    assert_eq!(frame["type"], "click", "{frame}");
    assert_eq!(frame["link_code"], json!(secure.code));
    let text = frame.to_string();
    assert!(!text.contains(email), "{text}");
    assert!(!text.contains("example.org"), "{text}");
    assert!(frame.get("viewer_email").is_none(), "{frame}");
    // And what was broadcast, before any per-subscriber filtering.
    let broadcast = observer.recv().await.expect("the broadcast event");
    let broadcast = serde_json::to_string(&broadcast).unwrap();
    assert!(!broadcast.contains(email), "{broadcast}");
    socket.close().await;
}

// ------------------------------------------------------ the protected copy

/// Wait for the background task that makes the protected copy.
async fn protected_key(db: &DatabaseConnection, key: &str) -> String {
    for _ in 0..400 {
        if let Some(protected) = file_row(db, key).await.protected_key {
            return protected;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no protected copy was recorded");
}

#[tokio::test]
async fn a_pdf_gets_a_protected_copy_that_the_viewer_is_served_and_that_goes_with_the_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let original = small_pdf();
    let (link, key) = bucket
        .shared(
            &alice,
            "plan.pdf",
            "application/pdf",
            &original,
            json!({"secure_view": true}),
        )
        .await;
    let code = link["code"].as_str().unwrap();
    assert_eq!(link["file"]["secure_view"], json!(true));
    assert_no_storage_details_text(&link.to_string(), &key);

    let protected = protected_key(&bucket.db, &key).await;
    assert_ne!(protected, key);
    assert!(protected.starts_with("files/"));
    let copy = bucket.store.bytes(&protected).expect("the copy is stored");
    assert!(copy.starts_with(b"%PDF-"));
    assert_ne!(copy.as_ref(), original.as_slice());
    assert_eq!(
        bucket.store.bytes(&key).unwrap().as_ref(),
        original.as_slice(),
        "the original is untouched"
    );
    // It opens with no password, and says no printing, no copying.
    let reopened = lopdf::Document::load_mem(&copy).expect("opens with no password");
    let permissions = reopened.encryption_state.as_ref().unwrap().permissions();
    assert!(!permissions.contains(lopdf::Permissions::PRINTABLE));
    assert!(!permissions.contains(lopdf::Permissions::COPYABLE));
    assert!(!permissions.contains(lopdf::Permissions::MODIFIABLE));

    // The viewer is served the copy, not the original.
    let token = bucket.view_token(code, "pat@example.org").await;
    let file = bucket.file(code, &token).await;
    assert_eq!(file.status_code(), 200);
    assert_eq!(file.as_bytes().as_ref(), copy.as_ref());
    assert_eq!(file.header(CONTENT_LENGTH), copy.len().to_string().as_str());
    assert_eq!(bucket.store.reads().last(), Some(&protected));
    // The link list and the stats never name either key.
    let listed = bucket
        .app()
        .await
        .get("/links")
        .authorization_bearer(&alice.token)
        .await;
    assert_no_storage_details_text(&listed.text(), &key);
    assert_no_storage_details_text(&listed.text(), &protected);

    // A copy that has gone missing falls back to the original.
    use opn_onl_backend::utils::files::FileStore;
    bucket.store.delete(&protected).await.unwrap();
    let file = bucket.file(code, &token).await;
    assert_eq!(file.status_code(), 200);
    assert_eq!(file.as_bytes().as_ref(), original.as_slice());
    bucket.store.put_bytes(&protected, copy.clone());

    // Deleting the link removes both objects.
    let res = bucket
        .app()
        .await
        .delete(&format!("/links/{}", link["id"]))
        .authorization_bearer(&alice.token)
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert!(!bucket.store.contains(&key), "the original is gone");
    assert!(!bucket.store.contains(&protected), "the copy is gone");
    assert!(bucket.store.keys().is_empty(), "{:?}", bucket.store.keys());
    assert!(file_row(&bucket.db, &key).await.deleted_at.is_some());
}

fn assert_no_storage_details_text(text: &str, key: &str) {
    for secret in [
        key,
        "downloads.test",
        "X-Amz-",
        "storage_key",
        "protected_key",
    ] {
        assert!(
            !text.contains(secret),
            "must not contain {secret:?}: {text}"
        );
    }
}

#[tokio::test]
async fn switching_secure_view_on_later_makes_the_copy_and_switching_it_off_removes_it() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, key) = bucket
        .shared(
            &alice,
            "plan.pdf",
            "application/pdf",
            &small_pdf(),
            json!({}),
        )
        .await;
    assert_eq!(file_row(&bucket.db, &key).await.protected_key, None);
    let path = format!("/links/{}/secure-view", link["id"]);
    let set = |enabled: bool| {
        let (path, token) = (path.clone(), alice.token.clone());
        let bucket = &bucket;
        async move {
            let res = bucket
                .app()
                .await
                .put(&path)
                .authorization_bearer(&token)
                .json(&json!({"enabled": enabled}))
                .await;
            assert_eq!(res.status_code(), 200, "{}", res.text());
        }
    };

    set(true).await;
    let protected = protected_key(&bucket.db, &key).await;
    assert!(bucket.store.contains(&protected));
    // Switching it on when it is on already makes no second copy.
    set(true).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(bucket.store.keys().len(), 2, "{:?}", bucket.store.keys());

    set(false).await;
    assert!(!bucket.store.contains(&protected), "the copy is removed");
    assert!(bucket.store.contains(&key), "the original stays");
    assert_eq!(file_row(&bucket.db, &key).await.protected_key, None);

    set(true).await;
    let again = protected_key(&bucket.db, &key).await;
    assert_ne!(again, protected, "a new copy under a new key");
    assert_eq!(bucket.store.keys().len(), 2);
}

#[tokio::test]
async fn a_file_that_cannot_be_protected_is_shown_from_the_original() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let sharing = bucket.sharing();

    // Not a PDF inside: no copy, no error, the original is served.
    let garbage = not_a_pdf("garbage");
    let (link, key) = bucket
        .shared(
            &alice,
            "plan.pdf",
            "application/pdf",
            &garbage,
            json!({"secure_view": true}),
        )
        .await;
    let code = link["code"].as_str().unwrap();
    let row = file_row(&bucket.db, &key).await;
    assert_eq!(
        make_protected_copy(&bucket.db, &sharing, &row).await,
        Protected::Failed
    );
    assert_eq!(file_row(&bucket.db, &key).await.protected_key, None);
    assert_eq!(bucket.store.keys().len(), 1, "nothing was left behind");
    let ticket = bucket.ticket(code).await;
    let view = bucket.open(code, &ticket, "pat@example.org").await;
    assert_eq!(view.status_code(), 200, "{}", view.text());
    let view: Value = view.json();
    assert_eq!(view["file"]["kind"], json!("pdf"));
    let file = bucket
        .file(code, view["view_token"].as_str().unwrap())
        .await;
    assert_eq!(file.status_code(), 200);
    assert_eq!(file.header(CONTENT_TYPE), "application/pdf");
    assert_eq!(file.as_bytes().as_ref(), garbage.as_slice());

    // An image is never rewritten.
    let (image, image_key) = bucket
        .shared(
            &alice,
            "chart.png",
            "image/png",
            b"\x89PNG",
            json!({"secure_view": true}),
        )
        .await;
    assert_eq!(image["file"]["secure_view"], json!(true));
    let row = file_row(&bucket.db, &image_key).await;
    assert_eq!(
        make_protected_copy(&bucket.db, &sharing, &row).await,
        Protected::NotNeeded
    );

    // A bucket that will not take the copy: the original is served.
    let (_, key) = bucket
        .shared(&alice, "ok.pdf", "application/pdf", &small_pdf(), json!({}))
        .await;
    let mut row: link_files::ActiveModel = file_row(&bucket.db, &key).await.into();
    row.secure_view = Set(true);
    let row = row.update(&bucket.db).await.unwrap();
    bucket.store.fail_puts(true);
    assert_eq!(
        make_protected_copy(&bucket.db, &sharing, &row).await,
        Protected::Failed
    );
    bucket.store.fail_puts(false);
    assert_eq!(file_row(&bucket.db, &key).await.protected_key, None);
}

#[tokio::test]
async fn a_pdf_over_the_limit_gets_no_copy() {
    let mut bucket = Bucket::new().await;
    let pdf = small_pdf();
    // One byte under the file: too large to copy.
    bucket.protect_max_bytes = pdf.len() as u64 - 1;
    let alice = person(&bucket.db).await;
    let (link, key) = bucket
        .shared(
            &alice,
            "big.pdf",
            "application/pdf",
            &pdf,
            json!({"secure_view": true}),
        )
        .await;
    let row = file_row(&bucket.db, &key).await;
    assert_eq!(
        make_protected_copy(&bucket.db, &bucket.sharing(), &row).await,
        Protected::NotNeeded
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(file_row(&bucket.db, &key).await.protected_key, None);
    assert_eq!(bucket.store.keys(), vec![key.clone()]);
    assert!(bucket.store.reads().is_empty(), "it was not even read");

    // The viewer shows the original.
    let code = link["code"].as_str().unwrap();
    let token = bucket.view_token(code, "pat@example.org").await;
    let file = bucket.file(code, &token).await;
    assert_eq!(file.as_bytes().as_ref(), pdf.as_slice());

    // At exactly the limit a copy is made.
    bucket.protect_max_bytes = pdf.len() as u64;
    assert_eq!(
        make_protected_copy(&bucket.db, &bucket.sharing(), &row).await,
        Protected::Made
    );
    assert!(file_row(&bucket.db, &key).await.protected_key.is_some());
}

#[tokio::test]
async fn a_copy_finished_after_the_link_was_deleted_is_not_left_in_the_bucket() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let sharing = bucket.sharing();

    // The row as it was read when the copy was started...
    let (link, key) = bucket
        .shared(
            &alice,
            "plan.pdf",
            "application/pdf",
            &small_pdf(),
            json!({}),
        )
        .await;
    let id = link["id"].as_i64().unwrap() as i32;
    let mut row: link_files::ActiveModel = file_row(&bucket.db, &key).await.into();
    row.secure_view = Set(true);
    let row = row.update(&bucket.db).await.unwrap();

    // ...and the link deleted (not yet swept) before the copy is recorded.
    change_link(&bucket.db, id, |link| {
        link.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    })
    .await;
    assert_eq!(
        make_protected_copy(&bucket.db, &sharing, &row).await,
        Protected::Abandoned
    );
    assert_eq!(file_row(&bucket.db, &key).await.protected_key, None);
    assert_eq!(
        bucket.store.keys(),
        vec![key.clone()],
        "the copy was removed"
    );

    // The same for a link that expired, and for Secure View switched off.
    for case in ["expired", "switched off"] {
        let (link, key) = bucket
            .shared(
                &alice,
                "plan.pdf",
                "application/pdf",
                &small_pdf(),
                json!({}),
            )
            .await;
        let id = link["id"].as_i64().unwrap() as i32;
        let mut row: link_files::ActiveModel = file_row(&bucket.db, &key).await.into();
        row.secure_view = Set(true);
        let row = row.update(&bucket.db).await.unwrap();
        if case == "expired" {
            change_link(&bucket.db, id, |link| {
                link.expires_at = Set(Some(
                    chrono::Utc::now().naive_utc() - chrono::Duration::minutes(1),
                ));
            })
            .await;
        } else {
            let mut off: link_files::ActiveModel = row.clone().into();
            off.secure_view = Set(false);
            off.update(&bucket.db).await.unwrap();
        }
        assert_eq!(
            make_protected_copy(&bucket.db, &sharing, &row).await,
            Protected::Abandoned,
            "{case}"
        );
        assert_eq!(file_row(&bucket.db, &key).await.protected_key, None);
        assert!(bucket.store.contains(&key));
    }
    assert_eq!(bucket.store.keys().len(), 3, "three originals, no copies");
}
