//! File sharing: upload a file, share it as a short link.
//!
//! Real router, real Postgres, and a `MemoryFileStore` in place of the bucket
//! (there is no S3 here). The store is handed to the app through `AppState`,
//! so nothing in this file touches the environment. What these tests pin:
//!
//! * an upload belongs to the account that started it;
//! * a visitor gets a signed address only after every check a link has, and
//!   nothing else ever shows one, or the storage key;
//! * a file goes when its link goes, and a bucket that fails does not undo
//!   the link's deletion;
//! * what is shown in the browser and what is downloaded.
//!
//! Sweeps over the whole database (the daily task) are in
//! `integration_serial/file_sweep_tests.rs`. Here every removal is scoped to
//! the files a test made: another test's rows are swept by nobody.

use crate::common;

use axum::http::header::{CACHE_CONTROL, LOCATION, REFERRER_POLICY};
use common::{spawn_real_app, spawn_real_app_with_files, unique_code, unique_email};
use opn_onl_backend::entity::{link_files, links, org_members, organizations, users};
use opn_onl_backend::utils::ClickBuffer;
use opn_onl_backend::utils::create_jwt;
use opn_onl_backend::utils::files::{
    FileSharing, MemoryFileStore, Sweep, SweepReport, remove_dead_files,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------- fixtures

struct Person {
    id: i32,
    token: String,
}

async fn person_with(db: &DatabaseConnection, admin: bool, verified: bool) -> Person {
    let email = unique_email();
    let user = users::ActiveModel {
        email: Set(email.clone()),
        password_hash: Set("not-a-real-hash".to_string()),
        created_at: Set(chrono::Utc::now().naive_utc()),
        email_verified: Set(verified),
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
    person_with(db, false, true).await
}

/// One test's bucket. Every app made from it shares the bucket, and each app
/// has its own rate limiters: the mock transport puts all callers in one
/// per-second bucket, so a test takes a fresh app rather than tripping it.
struct Bucket {
    db: DatabaseConnection,
    store: Arc<MemoryFileStore>,
    max_bytes: u64,
}

impl Bucket {
    async fn new() -> Self {
        Self::with_max(1_000_000).await
    }

    async fn with_max(max_bytes: u64) -> Self {
        Self {
            db: common::setup_test_db().await,
            store: Arc::new(MemoryFileStore::new()),
            max_bytes,
        }
    }

    fn sharing(&self) -> FileSharing {
        let mut sharing = FileSharing::with_store(self.store.clone());
        sharing.max_bytes = self.max_bytes;
        sharing
    }

    async fn app(&self) -> axum_test::TestServer {
        spawn_real_app_with_files(self.sharing()).await.0
    }

    async fn app_and_clicks(&self) -> (axum_test::TestServer, Arc<ClickBuffer>) {
        let (server, _, clicks) = spawn_real_app_with_files(self.sharing()).await;
        (server, clicks)
    }

    /// Call 1: start an upload. Returns the answer.
    async fn start(&self, who: &Person, filename: &str, content_type: &str, size: u64) -> Value {
        let res = self
            .app()
            .await
            .post("/files/uploads")
            .authorization_bearer(&who.token)
            .json(&json!({"filename": filename, "content_type": content_type, "size_bytes": size}))
            .await;
        assert_eq!(res.status_code(), 201, "start upload: {}", res.text());
        res.json()
    }

    /// Calls 1 and 2: start an upload and put `size` bytes where the browser
    /// would. Returns the upload id and the storage key.
    async fn uploaded(
        &self,
        who: &Person,
        filename: &str,
        content_type: &str,
        size: u64,
    ) -> (i64, String) {
        let started = self.start(who, filename, content_type, size).await;
        let key = key_of(&started);
        self.store.put_sized(&key, size);
        (started["upload_id"].as_i64().expect("upload_id"), key)
    }

    /// All three calls. Returns the link the API answered with and the key.
    async fn shared(
        &self,
        who: &Person,
        filename: &str,
        content_type: &str,
        size: u64,
        fields: Value,
    ) -> (Value, String) {
        let (upload_id, key) = self.uploaded(who, filename, content_type, size).await;
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
}

/// The storage key inside a `MemoryFileStore` upload URL.
fn key_of(started: &Value) -> String {
    let url = url::Url::parse(started["upload_url"].as_str().expect("upload_url")).unwrap();
    assert_eq!(url.host_str(), Some("uploads.test"));
    url.path().trim_start_matches('/').to_string()
}

async fn file_row(db: &DatabaseConnection, key: &str) -> link_files::Model {
    link_files::Entity::find()
        .filter(link_files::Column::StorageKey.eq(key))
        .one(db)
        .await
        .unwrap()
        .expect("link_files row")
}

async fn link_row(db: &DatabaseConnection, id: i64) -> links::Model {
    links::Entity::find_by_id(id as i32)
        .one(db)
        .await
        .unwrap()
        .expect("link row")
}

async fn links_of(db: &DatabaseConnection, user_id: i32) -> u64 {
    links::Entity::find()
        .filter(links::Column::UserId.eq(user_id))
        .count(db)
        .await
        .unwrap()
}

fn code(link: &Value) -> &str {
    link["code"].as_str().expect("code")
}

/// Nothing in `text` says where a file is stored or how to fetch it.
fn assert_no_storage_details(text: &str, key: &str, what: &str) {
    for secret in [
        key,
        "downloads.test",
        "uploads.test",
        "X-Amz-",
        "opn-file:",
        "storage_key",
    ] {
        assert!(
            !text.contains(secret),
            "{what} must not contain {secret:?}: {text}"
        );
    }
}

// ------------------------------------------------------------- the upload

#[tokio::test]
async fn three_calls_upload_a_file_and_make_a_short_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;

    // 1. Start.
    let started = bucket
        .start(&alice, "Q3 report.pdf", "application/pdf", 4_096)
        .await;
    let upload_id = started["upload_id"].as_i64().expect("upload_id");
    let key = key_of(&started);
    assert!(key.starts_with("files/"), "{key}");
    assert!(
        !key.to_lowercase().contains("report"),
        "the key is not built from the filename: {key}"
    );
    assert_eq!(
        started["required_headers"],
        json!({"content-type": "application/pdf"})
    );
    assert!(
        started["upload_url"]
            .as_str()
            .unwrap()
            .contains("content-length=4096"),
        "the URL is signed for the declared size: {started}"
    );
    let expires_at = chrono::DateTime::parse_from_rfc3339(started["expires_at"].as_str().unwrap())
        .expect("RFC 3339");
    let ttl = expires_at.signed_duration_since(chrono::Utc::now());
    assert!(ttl.num_seconds() > 800 && ttl.num_seconds() <= 900, "{ttl}");

    let row = file_row(&bucket.db, &key).await;
    assert_eq!(row.status, "pending");
    assert_eq!(row.link_id, None);
    assert_eq!(row.user_id, Some(alice.id));
    assert_eq!(row.filename, "Q3 report.pdf");
    assert_eq!(links_of(&bucket.db, alice.id).await, 0, "no link yet");

    // 2. The browser sends the file to the bucket.
    bucket.store.put_sized(&key, 4_096);

    // 3. Complete.
    let alias = format!("report-{}", unique_code());
    let res = bucket
        .app()
        .await
        .post(&format!("/files/uploads/{upload_id}/complete"))
        .authorization_bearer(&alice.token)
        .json(&json!({"alias": alias, "title": "Third quarter"}))
        .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let link: Value = res.json();
    assert_eq!(link["code"], json!(alias));
    assert_eq!(link["title"], "Third quarter");
    assert_eq!(link["original_url"], "", "a file link shows no URL");
    assert_eq!(link["expires_at"], Value::Null, "no expiry by default");
    assert_eq!(
        link["file"],
        json!({
            "filename": "Q3 report.pdf",
            "size_bytes": 4096,
            "content_type": "application/pdf",
            "secure_view": false,
            "secure_view_eligible": true,
        })
    );
    assert_no_storage_details(&res.text(), &key, "the new link");

    let row = file_row(&bucket.db, &key).await;
    assert_eq!(row.status, "ready");
    assert_eq!(row.link_id, link["id"].as_i64().map(|id| id as i32));
    let stored = link_row(&bucket.db, link["id"].as_i64().unwrap()).await;
    assert!(stored.is_file());
    assert_eq!(stored.user_id, Some(alice.id));
}

#[tokio::test]
async fn the_title_defaults_to_the_filename_and_link_fields_are_honoured() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let expires = "2099-01-02T03:04:05Z";
    let (link, _) = bucket
        .shared(
            &alice,
            "C:\\Users\\alice\\Minutes.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            10,
            json!({"expires_at": expires, "password": "hunter2", "notes": "board only"}),
        )
        .await;
    assert_eq!(link["title"], "Minutes.docx", "the path is not kept");
    assert_eq!(link["file"]["filename"], "Minutes.docx");
    assert_eq!(link["expires_at"], expires);
    assert_eq!(link["has_password"], true);
    assert_eq!(link["notes"], "board only");
}

#[tokio::test]
async fn a_file_that_was_never_uploaded_cannot_be_completed() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let started = bucket.start(&alice, "a.pdf", "application/pdf", 10).await;
    let upload_id = started["upload_id"].as_i64().unwrap();

    let res = bucket
        .app()
        .await
        .post(&format!("/files/uploads/{upload_id}/complete"))
        .authorization_bearer(&alice.token)
        .json(&json!({}))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    assert_eq!(
        res.json::<Value>()["error"],
        "The file has not been uploaded yet"
    );
    assert_eq!(links_of(&bucket.db, alice.id).await, 0);
    assert_eq!(
        file_row(&bucket.db, &key_of(&started)).await.status,
        "pending"
    );
}

#[tokio::test]
async fn a_file_over_the_limit_is_refused_before_it_is_uploaded() {
    let bucket = Bucket::with_max(1_000).await;
    let alice = person(&bucket.db).await;
    let app = bucket.app().await;

    for (size, status) in [(1_001u64, 413), (0, 400)] {
        let res = app
            .post("/files/uploads")
            .authorization_bearer(&alice.token)
            .json(&json!({"filename": "big.bin", "content_type": "", "size_bytes": size}))
            .await;
        assert_eq!(res.status_code(), status, "{size}: {}", res.text());
        assert!(res.json::<Value>()["error"].is_string());
    }
    let rows = link_files::Entity::find()
        .filter(link_files::Column::UserId.eq(alice.id))
        .count(&bucket.db)
        .await
        .unwrap();
    assert_eq!(rows, 0, "a refused upload leaves no row");

    // Exactly the limit is allowed.
    let res = app
        .post("/files/uploads")
        .authorization_bearer(&alice.token)
        .json(&json!({"filename": "ok.bin", "size_bytes": 1_000}))
        .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    assert_eq!(
        res.json::<Value>()["required_headers"]["content-type"],
        "application/octet-stream",
        "no declared type is a plain download"
    );
}

#[tokio::test]
async fn an_object_bigger_than_the_limit_is_deleted_when_the_upload_is_completed() {
    let bucket = Bucket::with_max(1_000).await;
    let alice = person(&bucket.db).await;
    // Declared small, sent large.
    let started = bucket
        .start(&alice, "sneaky.zip", "application/zip", 500)
        .await;
    let upload_id = started["upload_id"].as_i64().unwrap();
    let key = key_of(&started);
    bucket.store.put_sized(&key, 5_000);

    let app = bucket.app().await;
    let res = app
        .post(&format!("/files/uploads/{upload_id}/complete"))
        .authorization_bearer(&alice.token)
        .json(&json!({}))
        .await;
    assert_eq!(res.status_code(), 413, "{}", res.text());
    assert!(!bucket.store.contains(&key), "the object is deleted");
    assert_eq!(links_of(&bucket.db, alice.id).await, 0, "no link is made");
    let row = file_row(&bucket.db, &key).await;
    assert!(row.deleted_at.is_some());
    assert_eq!(row.link_id, None);

    // The upload is closed.
    let again = app
        .post(&format!("/files/uploads/{upload_id}/complete"))
        .authorization_bearer(&alice.token)
        .json(&json!({}))
        .await;
    assert_eq!(again.status_code(), 410, "{}", again.text());
    assert_eq!(links_of(&bucket.db, alice.id).await, 0);
}

#[tokio::test]
async fn the_size_recorded_is_the_size_that_reached_the_bucket() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let started = bucket.start(&alice, "a.bin", "", 500).await;
    let key = key_of(&started);
    bucket.store.put_sized(&key, 700);
    let res = bucket
        .app()
        .await
        .post(&format!(
            "/files/uploads/{}/complete",
            started["upload_id"].as_i64().unwrap()
        ))
        .authorization_bearer(&alice.token)
        .json(&json!({}))
        .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    assert_eq!(res.json::<Value>()["file"]["size_bytes"], 700);
    assert_eq!(file_row(&bucket.db, &key).await.size_bytes, 700);
}

#[tokio::test]
async fn completing_twice_returns_the_same_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (upload_id, key) = bucket
        .uploaded(&alice, "a.pdf", "application/pdf", 10)
        .await;
    let app = bucket.app().await;
    let complete = || {
        app.post(&format!("/files/uploads/{upload_id}/complete"))
            .authorization_bearer(&alice.token)
            .json(&json!({"title": "Once"}))
    };

    let first = complete().await;
    assert_eq!(first.status_code(), 201, "{}", first.text());
    // The retry is answered from the database. It does not ask the bucket
    // again, so it works even while the bucket is unreachable.
    bucket.store.fail_heads(true);
    let second = complete().await;
    assert_eq!(second.status_code(), 200, "{}", second.text());
    let (first, second): (Value, Value) = (first.json(), second.json());
    assert_eq!(first["id"], second["id"]);
    assert_eq!(first["code"], second["code"]);
    assert_eq!(second["file"]["filename"], "a.pdf");
    assert_eq!(
        links_of(&bucket.db, alice.id).await,
        1,
        "one upload, one link"
    );
    assert!(bucket.store.contains(&key));
}

#[tokio::test]
async fn a_complete_that_loses_the_race_makes_no_second_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (upload_id, _) = bucket
        .uploaded(&alice, "a.pdf", "application/pdf", 10)
        .await;
    let (slow_app, fast_app) = (bucket.app().await, bucket.app().await);
    let path = format!("/files/uploads/{upload_id}/complete");

    // The slow request reads the upload as pending and stops while asking the
    // bucket about it. The fast one completes the upload in that gap. When
    // the slow one goes on, its picture of the upload is out of date.
    let gate = bucket.store.hold_next_head();
    let slow = async {
        slow_app
            .post(&path)
            .authorization_bearer(&alice.token)
            .json(&json!({"title": "Slow"}))
            .await
    };
    let fast = async {
        // A slow request that is answered before it asks the bucket leaves no
        // gap to race in. Fail then, rather than wait for it forever.
        tokio::time::timeout(Duration::from_secs(20), gate.arrived())
            .await
            .expect("the slow request never asked the bucket about its upload");
        let res = fast_app
            .post(&path)
            .authorization_bearer(&alice.token)
            .json(&json!({"title": "Fast"}))
            .await;
        gate.release();
        res
    };
    let (slow, fast) = tokio::join!(slow, fast);

    assert_eq!(fast.status_code(), 201, "{}", fast.text());
    assert_eq!(
        slow.status_code(),
        200,
        "the loser is given the winner's link: {}",
        slow.text()
    );
    let (slow, fast): (Value, Value) = (slow.json(), fast.json());
    assert_eq!(slow["id"], fast["id"]);
    assert_eq!(slow["title"], "Fast");
    assert_eq!(
        links_of(&bucket.db, alice.id).await,
        1,
        "the loser's link was rolled back"
    );
}

#[tokio::test]
async fn several_completes_at_once_make_one_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (upload_id, _) = bucket
        .uploaded(&alice, "a.pdf", "application/pdf", 10)
        .await;
    // One app each: a test server answers its own requests one at a time.
    let apps = [bucket.app().await, bucket.app().await, bucket.app().await];
    let complete = |app: usize| {
        let app = &apps[app];
        let token = &alice.token;
        async move {
            app.post(&format!("/files/uploads/{upload_id}/complete"))
                .authorization_bearer(token)
                .json(&json!({}))
                .await
        }
    };

    let (a, b, c) = tokio::join!(complete(0), complete(1), complete(2));
    let mut statuses =
        [a.status_code(), b.status_code(), c.status_code()].map(|status| status.as_u16());
    statuses.sort_unstable();
    assert_eq!(
        statuses,
        [200, 200, 201],
        "{} {} {}",
        a.text(),
        b.text(),
        c.text()
    );
    let ids: Vec<Value> = [a, b, c]
        .iter()
        .map(|res| res.json::<Value>()["id"].clone())
        .collect();
    assert!(ids.iter().all(|id| *id == ids[0]), "{ids:?}");
    assert_eq!(links_of(&bucket.db, alice.id).await, 1);
}

#[tokio::test]
async fn someone_elses_upload_answers_like_one_that_does_not_exist() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let mallory = person(&bucket.db).await;
    let (upload_id, key) = bucket
        .uploaded(&alice, "secret.pdf", "application/pdf", 10)
        .await;
    let app = bucket.app().await;

    let foreign = app
        .post(&format!("/files/uploads/{upload_id}/complete"))
        .authorization_bearer(&mallory.token)
        .json(&json!({"title": "mine now"}))
        .await;
    let unknown = app
        .post(&format!("/files/uploads/{}/complete", i32::MAX))
        .authorization_bearer(&mallory.token)
        .json(&json!({}))
        .await;
    assert_eq!(foreign.status_code(), 403, "{}", foreign.text());
    assert_eq!(unknown.status_code(), 403, "{}", unknown.text());
    assert_eq!(
        foreign.text(),
        unknown.text(),
        "the answer must not say which ids exist"
    );
    assert!(!foreign.text().contains("secret"), "{}", foreign.text());

    // Nothing happened to Alice's upload, and Mallory has no link.
    let row = file_row(&bucket.db, &key).await;
    assert_eq!(row.status, "pending");
    assert_eq!(row.link_id, None);
    assert_eq!(links_of(&bucket.db, mallory.id).await, 0);
    assert!(bucket.store.contains(&key));

    // And once it is completed, Mallory still learns nothing from a retry.
    let done = app
        .post(&format!("/files/uploads/{upload_id}/complete"))
        .authorization_bearer(&alice.token)
        .json(&json!({}))
        .await;
    assert_eq!(done.status_code(), 201, "{}", done.text());
    let after = app
        .post(&format!("/files/uploads/{upload_id}/complete"))
        .authorization_bearer(&mallory.token)
        .json(&json!({}))
        .await;
    assert_eq!(after.status_code(), 403);
    assert_eq!(after.text(), unknown.text());
}

#[tokio::test]
async fn the_upload_endpoints_need_a_signed_in_verified_account() {
    let bucket = Bucket::new().await;
    let app = bucket.app().await;
    let start = json!({"filename": "a.pdf", "content_type": "application/pdf", "size_bytes": 1});

    assert_eq!(
        app.post("/files/uploads").json(&start).await.status_code(),
        401
    );
    assert_eq!(
        app.post("/files/uploads/1/complete")
            .json(&json!({}))
            .await
            .status_code(),
        401
    );
    assert_eq!(app.get("/files/usage").await.status_code(), 401);

    let unverified = person_with(&bucket.db, false, false).await;
    let res = app
        .post("/files/uploads")
        .authorization_bearer(&unverified.token)
        .json(&start)
        .await;
    assert_eq!(res.status_code(), 403, "{}", res.text());
}

#[tokio::test]
async fn completing_follows_the_alias_rules_of_an_ordinary_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let taken = format!("taken-{}", unique_code());
    links::ActiveModel {
        code: Set(taken.clone()),
        original_url: Set("https://iana.org/taken".to_string()),
        user_id: Set(Some(alice.id)),
        ..Default::default()
    }
    .insert(&bucket.db)
    .await
    .unwrap();
    let (upload_id, key) = bucket
        .uploaded(&alice, "a.pdf", "application/pdf", 10)
        .await;
    let app = bucket.app().await;
    let complete = |alias: &str| {
        app.post(&format!("/files/uploads/{upload_id}/complete"))
            .authorization_bearer(&alice.token)
            .json(&json!({"custom_alias": alias}))
    };

    // A field that cannot be read is refused, not dropped: an expiry the server
    // could not parse must not become a link that never expires.
    let unreadable = app
        .post(&format!("/files/uploads/{upload_id}/complete"))
        .authorization_bearer(&alice.token)
        .json(&json!({"expires_at": "next tuesday"}))
        .await;
    assert!(
        unreadable.status_code().is_client_error(),
        "{}",
        unreadable.text()
    );
    assert_eq!(file_row(&bucket.db, &key).await.status, "pending");

    let conflict = complete(&taken).await;
    assert_eq!(conflict.status_code(), 409, "{}", conflict.text());
    let reserved = complete("admin").await;
    assert_eq!(reserved.status_code(), 400, "{}", reserved.text());

    // A refused alias costs nothing: the file is still there to be shared.
    assert!(bucket.store.contains(&key));
    assert_eq!(file_row(&bucket.db, &key).await.status, "pending");
    let free = format!("free-{}", unique_code());
    let ok = complete(&free).await;
    assert_eq!(ok.status_code(), 201, "{}", ok.text());
    assert_eq!(ok.json::<Value>()["code"], json!(free));
}

// ---------------------------------------------------------------- serving

#[tokio::test]
async fn opening_a_file_link_redirects_to_a_short_lived_signed_address() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, key) = bucket
        .shared(&alice, "Q3 report.pdf", "application/pdf", 10, json!({}))
        .await;

    let res = bucket.app().await.get(&format!("/{}", code(&link))).await;
    assert_eq!(res.status_code(), 302, "{}", res.text());
    assert_eq!(res.header(CACHE_CONTROL), "no-store");
    assert_eq!(res.header(REFERRER_POLICY), "no-referrer");

    let signed = bucket.store.downloads();
    assert_eq!(signed.len(), 1, "one visit, one signed address");
    assert_eq!(res.header(LOCATION), signed[0].url.as_str());
    assert_eq!(signed[0].key, key);
    assert_eq!(signed[0].ttl, Duration::from_secs(300));
    assert_eq!(signed[0].content_type, "application/pdf");
    assert_eq!(
        signed[0].content_disposition,
        "inline; filename=\"Q3 report.pdf\"; filename*=UTF-8''Q3%20report.pdf"
    );
}

#[tokio::test]
async fn risky_and_mismatched_files_are_always_downloaded() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    for (filename, declared, shown_inline) in [
        ("photo.PNG", "image/png", true),
        ("page.html", "text/html", false),
        // A page dressed up as a picture: the name gives it away.
        ("page.html", "image/png", false),
        // A picture's name on something declared as a page.
        ("photo.png", "text/html", false),
        ("logo.svg", "image/svg+xml", false),
        ("app.js", "text/javascript", false),
        ("notes", "application/pdf", false),
    ] {
        let (link, _) = bucket
            .shared(&alice, filename, declared, 10, json!({}))
            .await;
        let res = bucket.app().await.get(&format!("/{}", code(&link))).await;
        assert_eq!(res.status_code(), 302, "{filename} {declared}");
        let signed = bucket.store.downloads().pop().expect("signed");
        if shown_inline {
            assert_eq!(signed.content_type, declared);
            assert!(signed.content_disposition.starts_with("inline; "));
        } else {
            assert_eq!(
                signed.content_type, "application/octet-stream",
                "{filename} declared {declared}"
            );
            assert!(
                signed.content_disposition.starts_with("attachment; "),
                "{filename} declared {declared}: {}",
                signed.content_disposition
            );
        }
    }
}

#[tokio::test]
async fn a_head_request_is_told_nothing() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, _) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;

    let (app, clicks) = bucket.app_and_clicks().await;
    let res = app
        .method(axum::http::Method::HEAD, &format!("/{}", code(&link)))
        .await;
    assert_eq!(res.status_code(), 200);
    assert!(res.maybe_header(LOCATION).is_none());
    assert!(
        bucket.store.downloads().is_empty(),
        "nothing is signed for a HEAD"
    );
    assert_eq!(clicks.pending_count(link["id"].as_i64().unwrap() as i32), 0);
}

#[tokio::test]
async fn a_visit_is_counted_like_any_other_click() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, _) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let link_id = link["id"].as_i64().unwrap() as i32;

    let (app, clicks) = bucket.app_and_clicks().await;
    for _ in 0..2 {
        assert_eq!(
            app.get(&format!("/{}", code(&link))).await.status_code(),
            302
        );
    }
    assert_eq!(clicks.pending_count(link_id), 2);
    clicks.flush(&bucket.db).await;
    assert_eq!(link_row(&bucket.db, link_id as i64).await.click_count, 2);
}

#[tokio::test]
async fn a_link_that_is_not_live_gives_no_signed_address() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let past = chrono::Utc::now().naive_utc() - chrono::Duration::minutes(1);
    let future = chrono::Utc::now().naive_utc() + chrono::Duration::days(1);

    // Expired.
    let (expired, expired_key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let mut row: links::ActiveModel = link_row(&bucket.db, expired["id"].as_i64().unwrap())
        .await
        .into();
    row.expires_at = Set(Some(past));
    row.update(&bucket.db).await.unwrap();

    // Scheduled for later.
    let (scheduled, _) = bucket
        .shared(&alice, "b.pdf", "application/pdf", 10, json!({}))
        .await;
    let mut row: links::ActiveModel = link_row(&bucket.db, scheduled["id"].as_i64().unwrap())
        .await
        .into();
    row.starts_at = Set(Some(future));
    row.update(&bucket.db).await.unwrap();

    // Deleted (straight in the database, so the object is still in the bucket).
    let (deleted, _) = bucket
        .shared(&alice, "c.pdf", "application/pdf", 10, json!({}))
        .await;
    let mut row: links::ActiveModel = link_row(&bucket.db, deleted["id"].as_i64().unwrap())
        .await
        .into();
    row.deleted_at = Set(Some(past));
    row.update(&bucket.db).await.unwrap();

    // Its uploader's account was disabled.
    let gone = person(&bucket.db).await;
    let (orphaned, _) = bucket
        .shared(&gone, "d.pdf", "application/pdf", 10, json!({}))
        .await;
    let mut user: users::ActiveModel = users::Entity::find_by_id(gone.id)
        .one(&bucket.db)
        .await
        .unwrap()
        .unwrap()
        .into();
    user.disabled_at = Set(Some(past));
    user.update(&bucket.db).await.unwrap();

    let app = bucket.app().await;
    for (link, status, what) in [
        (&expired, 410, "expired"),
        (&scheduled, 410, "scheduled"),
        (&deleted, 404, "deleted"),
        (&orphaned, 410, "disabled owner"),
    ] {
        let res = app.get(&format!("/{}", code(link))).await;
        assert_eq!(res.status_code(), status, "{what}: {}", res.text());
        assert!(res.maybe_header(LOCATION).is_none(), "{what}");
        assert_eq!(res.header(CACHE_CONTROL), "no-store", "{what}");
    }
    assert!(
        bucket.store.downloads().is_empty(),
        "no address may be signed for a link that is not live"
    );
    assert!(
        bucket.store.contains(&expired_key),
        "expiry itself deletes nothing; the sweep does"
    );
}

#[tokio::test]
async fn a_password_link_signs_nothing_until_the_password_is_given() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    // 4 rounds: the cheapest bcrypt accepts, so the test is not slow.
    let (link, key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let mut row: links::ActiveModel = link_row(&bucket.db, link["id"].as_i64().unwrap())
        .await
        .into();
    row.password_hash = Set(Some(bcrypt::hash("open sesame", 4).unwrap()));
    row.update(&bucket.db).await.unwrap();
    let path = format!("/{}", code(&link));
    let app = bucket.app().await;

    // No password: sent to the password page.
    let res = app.get(&path).await;
    assert_eq!(res.status_code(), 307, "{}", res.text());
    assert!(
        res.header(LOCATION)
            .to_str()
            .unwrap()
            .contains("/password/")
    );

    // The public preview describes no destination.
    let preview = app.get(&format!("{path}/preview")).await;
    assert_eq!(preview.status_code(), 200);
    assert_eq!(preview.json::<Value>()["original_url"], "");
    assert_no_storage_details(&preview.text(), &key, "the preview");

    // A wrong password, by header and by the verify call.
    let wrong = app
        .get(&path)
        .add_header(
            axum::http::HeaderName::from_static("x-link-password"),
            "nope",
        )
        .await;
    assert_eq!(wrong.status_code(), 401);
    let wrong = app
        .post(&format!("{path}/verify"))
        .json(&json!({"password": "nope"}))
        .await;
    assert_eq!(wrong.status_code(), 401);

    // A forged unlock.
    let forged = app
        .get(&path)
        .add_query_param("unlock", "not-a-token")
        .await;
    assert_eq!(forged.status_code(), 307);

    assert!(
        bucket.store.downloads().is_empty(),
        "nothing may be signed before the password check passes"
    );

    // The right password buys a proof, not the file's address.
    let verified = app
        .post(&format!("{path}/verify"))
        .json(&json!({"password": "open sesame"}))
        .await;
    assert_eq!(verified.status_code(), 200, "{}", verified.text());
    assert_no_storage_details(&verified.text(), &key, "the verify answer");
    assert!(bucket.store.downloads().is_empty());
    let redirect_url = verified.json::<Value>()["redirect_url"]
        .as_str()
        .unwrap()
        .to_string();
    let unlock = url::Url::parse(&redirect_url)
        .unwrap()
        .query_pairs()
        .find(|(name, _)| name == "unlock")
        .map(|(_, value)| value.to_string())
        .expect("unlock");

    // The proof opens the redirect, which signs the address.
    let opened = app.get(&path).add_query_param("unlock", &unlock).await;
    assert_eq!(opened.status_code(), 302, "{}", opened.text());
    assert_eq!(bucket.store.downloads().len(), 1);

    // An unlocked preview still describes no destination.
    let preview = app
        .get(&format!("{path}/preview"))
        .add_query_param("unlock", &unlock)
        .await;
    assert_eq!(preview.json::<Value>()["original_url"], "");
    assert_no_storage_details(&preview.text(), &key, "the unlocked preview");
}

#[tokio::test]
async fn a_one_time_file_link_serves_the_file_once() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, _) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let app = bucket.app().await;
    let res = app
        .put(&format!("/links/{}", link["id"]))
        .authorization_bearer(&alice.token)
        .json(&json!({"burn_after_reading": true}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());

    let path = format!("/{}", code(&link));
    assert_eq!(app.get(&path).await.status_code(), 302);
    let again = app.get(&path).await;
    assert_eq!(again.status_code(), 410, "{}", again.text());
    assert_eq!(
        bucket.store.downloads().len(),
        1,
        "only the one visit got an address"
    );
}

#[tokio::test]
async fn a_file_that_is_gone_from_the_bucket_is_a_dead_link_and_uses_up_nothing() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let link_id = link["id"].as_i64().unwrap();
    // A one-time link whose file was removed (a link an admin restored after
    // its file was deleted looks like this).
    let mut row: links::ActiveModel = link_row(&bucket.db, link_id).await.into();
    row.burn_after_reading = Set(true);
    row.max_clicks = Set(Some(1));
    row.update(&bucket.db).await.unwrap();
    let mut file: link_files::ActiveModel = file_row(&bucket.db, &key).await.into();
    file.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    file.update(&bucket.db).await.unwrap();

    let res = bucket.app().await.get(&format!("/{}", code(&link))).await;
    assert_eq!(res.status_code(), 410, "{}", res.text());
    assert!(res.text().contains("no longer available"), "{}", res.text());
    assert!(bucket.store.downloads().is_empty());
    let row = link_row(&bucket.db, link_id).await;
    assert_eq!(row.click_count, 0, "the one visit was not spent on nothing");
    assert_eq!(row.burned_at, None);
}

// ------------------------------------------------- what the API gives away

#[tokio::test]
async fn lists_exports_and_stats_show_the_file_and_never_where_it_is() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, key) = bucket
        .shared(&alice, "Q3 report.pdf", "application/pdf", 4_096, json!({}))
        .await;
    let id = link["id"].as_i64().unwrap();
    // A visit, flushed to the database, so there is a signed address the API
    // could wrongly repeat and a click whose destination the stats describe.
    let (visitor, clicks) = bucket.app_and_clicks().await;
    assert_eq!(
        visitor
            .get(&format!("/{}", code(&link)))
            .await
            .status_code(),
        302
    );
    clicks.flush(&bucket.db).await;
    let signed_url = bucket.store.downloads()[0].url.clone();

    let tag = bucket
        .app()
        .await
        .post("/tags")
        .authorization_bearer(&alice.token)
        .json(&json!({"name": format!("t{}", unique_code())}))
        .await;
    assert_eq!(tag.status_code(), 201, "{}", tag.text());
    let tag_id = tag.json::<Value>()["id"].as_i64().unwrap();
    let folder = bucket
        .app()
        .await
        .post("/folders")
        .authorization_bearer(&alice.token)
        .json(&json!({"name": format!("f{}", unique_code())}))
        .await;
    assert_eq!(folder.status_code(), 201, "{}", folder.text());
    let folder_id = folder.json::<Value>()["id"].as_i64().unwrap();
    let app = bucket.app().await;
    let tagged = app
        .post(&format!("/links/{id}/tags"))
        .authorization_bearer(&alice.token)
        .json(&json!({"tag_ids": [tag_id]}))
        .await;
    assert!(tagged.status_code().is_success(), "{}", tagged.text());
    let moved = app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&alice.token)
        .json(&json!({"folder_id": folder_id}))
        .await;
    assert_eq!(moved.status_code(), 200, "{}", moved.text());
    assert_eq!(moved.json::<Value>()["file"]["filename"], "Q3 report.pdf");

    let expected = json!({
        "filename": "Q3 report.pdf",
        "size_bytes": 4096,
        "content_type": "application/pdf",
        "secure_view": false,
        "secure_view_eligible": true,
    });
    for path in [
        "/links".to_string(),
        format!("/folders/{folder_id}/links"),
        format!("/tags/{tag_id}/links"),
    ] {
        let res = app.get(&path).authorization_bearer(&alice.token).await;
        assert_eq!(res.status_code(), 200, "{path}: {}", res.text());
        let items: Vec<Value> = res.json();
        let item = items
            .iter()
            .find(|item| item["id"] == json!(id))
            .expect("listed");
        assert_eq!(item["file"], expected, "{path}");
        assert_eq!(item["original_url"], "", "{path}");
        assert_no_storage_details(&res.text(), &key, &path);
        assert!(!res.text().contains(&signed_url), "{path}");
    }

    let one = app
        .get(&format!("/links/{id}"))
        .authorization_bearer(&alice.token)
        .await;
    assert_eq!(one.json::<Value>()["file"], expected);
    assert_no_storage_details(&one.text(), &key, "GET /links/{id}");

    for path in [
        "/links/export".to_string(),
        format!("/links/{id}/stats"),
        "/analytics/dashboard".to_string(),
        format!("/{}/preview", code(&link)),
        "/links?search=opn-file".to_string(),
    ] {
        let res = app.get(&path).authorization_bearer(&alice.token).await;
        assert_eq!(res.status_code(), 200, "{path}: {}", res.text());
        assert_no_storage_details(&res.text(), &key, &path);
        assert!(!res.text().contains(&signed_url), "{path}");
    }
    let stats: Value = app
        .get(&format!("/links/{id}/stats"))
        .authorization_bearer(&alice.token)
        .await
        .json();
    assert_eq!(
        stats["total_clicks"], 1,
        "the visit is in the stats: {stats}"
    );
    assert_eq!(stats["original_url"], "");
    let export = app
        .get("/links/export")
        .authorization_bearer(&alice.token)
        .await;
    assert_eq!(export.status_code(), 200);
    assert!(
        export.text().contains(code(&link)),
        "the link is in the export"
    );
}

// --------------------------------------------------------------- deleting

#[tokio::test]
async fn deleting_a_file_link_deletes_its_file() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (doomed, doomed_key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let (kept, kept_key) = bucket
        .shared(&alice, "b.pdf", "application/pdf", 10, json!({}))
        .await;

    let res = bucket
        .app()
        .await
        .delete(&format!("/links/{}", doomed["id"]))
        .authorization_bearer(&alice.token)
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());

    assert!(!bucket.store.contains(&doomed_key), "the object is gone");
    assert!(file_row(&bucket.db, &doomed_key).await.deleted_at.is_some());
    assert!(
        bucket.store.contains(&kept_key),
        "another link's file is untouched"
    );
    assert!(file_row(&bucket.db, &kept_key).await.deleted_at.is_none());
    assert_eq!(
        bucket
            .app()
            .await
            .get(&format!("/{}", code(&kept)))
            .await
            .status_code(),
        302
    );
    assert_eq!(
        bucket
            .app()
            .await
            .get(&format!("/{}", code(&doomed)))
            .await
            .status_code(),
        404
    );
}

#[tokio::test]
async fn bulk_delete_deletes_the_files_of_the_links_it_deleted_and_no_others() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let bob = person(&bucket.db).await;
    let (a, a_key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let (b, b_key) = bucket
        .shared(&alice, "b.pdf", "application/pdf", 10, json!({}))
        .await;
    let (kept, kept_key) = bucket
        .shared(&alice, "c.pdf", "application/pdf", 10, json!({}))
        .await;
    let (bobs, bobs_key) = bucket
        .shared(&bob, "bob.pdf", "application/pdf", 10, json!({}))
        .await;

    let res = bucket
        .app()
        .await
        .post("/links/bulk/delete")
        .authorization_bearer(&alice.token)
        // Bob's link is asked for too; Alice may not delete it.
        .json(&json!({"ids": [a["id"], b["id"], bobs["id"]]}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(res.json::<Value>()["deleted"], 2);

    assert!(!bucket.store.contains(&a_key));
    assert!(!bucket.store.contains(&b_key));
    assert!(bucket.store.contains(&kept_key));
    assert!(
        bucket.store.contains(&bobs_key),
        "a link the caller may not delete keeps its file"
    );
    assert!(file_row(&bucket.db, &bobs_key).await.deleted_at.is_none());
    assert!(
        link_row(&bucket.db, bobs["id"].as_i64().unwrap())
            .await
            .deleted_at
            .is_none()
    );
    assert!(
        link_row(&bucket.db, kept["id"].as_i64().unwrap())
            .await
            .deleted_at
            .is_none()
    );
}

#[tokio::test]
async fn a_bucket_that_will_not_delete_does_not_stop_the_link_being_deleted() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let id = link["id"].as_i64().unwrap();
    bucket.store.fail_deletes(true);

    let res = bucket
        .app()
        .await
        .delete(&format!("/links/{id}"))
        .authorization_bearer(&alice.token)
        .await;
    assert_eq!(
        res.status_code(),
        200,
        "the link is deleted all the same: {}",
        res.text()
    );
    assert!(link_row(&bucket.db, id).await.deleted_at.is_some());
    assert_eq!(
        bucket
            .app()
            .await
            .get(&format!("/{}", code(&link)))
            .await
            .status_code(),
        404
    );

    // The file is still owed a deletion, and the row says so.
    assert!(bucket.store.contains(&key));
    let row = file_row(&bucket.db, &key).await;
    assert!(row.deleted_at.is_none());

    // The next sweep that looks at it removes it. (The daily sweep over every
    // file is tested alone in integration_serial.)
    bucket.store.fail_deletes(false);
    let report = remove_dead_files(
        &bucket.db,
        bucket.store.as_ref(),
        Sweep::Files(vec![row.id]),
    )
    .await
    .unwrap();
    assert_eq!(
        report,
        SweepReport {
            removed: 1,
            failed: 0
        }
    );
    assert!(!bucket.store.contains(&key));
    assert!(file_row(&bucket.db, &key).await.deleted_at.is_some());
}

#[tokio::test]
async fn a_sweep_never_removes_the_file_of_a_live_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (_, live_key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let (_, pending_key) = bucket
        .uploaded(&alice, "b.pdf", "application/pdf", 10)
        .await;
    let ids = vec![
        file_row(&bucket.db, &live_key).await.id,
        file_row(&bucket.db, &pending_key).await.id,
    ];

    // Asked for by id, as a delete handler would, although neither is dead.
    let report = remove_dead_files(&bucket.db, bucket.store.as_ref(), Sweep::Files(ids))
        .await
        .unwrap();
    assert_eq!(report, SweepReport::default());
    assert!(bucket.store.contains(&live_key));
    assert!(
        bucket.store.contains(&pending_key),
        "an upload in progress is left alone"
    );
}

#[tokio::test]
async fn admin_and_account_deletion_remove_files_too() {
    let bucket = Bucket::new().await;
    let admin = person_with(&bucket.db, true, true).await;

    // An admin deletes one link.
    let alice = person(&bucket.db).await;
    let (single, single_key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let res = bucket
        .app()
        .await
        .delete(&format!("/admin/links/{}", single["id"]))
        .authorization_bearer(&admin.token)
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert!(!bucket.store.contains(&single_key));

    // An admin deletes several.
    let (bulk, bulk_key) = bucket
        .shared(&alice, "b.pdf", "application/pdf", 10, json!({}))
        .await;
    let res = bucket
        .app()
        .await
        .post("/admin/links/bulk/delete")
        .authorization_bearer(&admin.token)
        .json(&json!({"ids": [bulk["id"]]}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert!(!bucket.store.contains(&bulk_key));

    // An admin deletes the account: its remaining links go, and their files.
    let (_, account_key) = bucket
        .shared(&alice, "c.pdf", "application/pdf", 10, json!({}))
        .await;
    let bystander = person(&bucket.db).await;
    let (_, bystander_key) = bucket
        .shared(&bystander, "d.pdf", "application/pdf", 10, json!({}))
        .await;
    let res = bucket
        .app()
        .await
        .delete(&format!("/admin/users/{}", alice.id))
        .authorization_bearer(&admin.token)
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert!(!bucket.store.contains(&account_key));
    assert!(
        file_row(&bucket.db, &account_key)
            .await
            .deleted_at
            .is_some()
    );
    assert!(
        bucket.store.contains(&bystander_key),
        "someone else's file stays"
    );

    // An admin removes an account for good: the link rows go, the files too.
    let carol = person(&bucket.db).await;
    let (_, hard_key) = bucket
        .shared(&carol, "e.pdf", "application/pdf", 10, json!({}))
        .await;
    let res = bucket
        .app()
        .await
        .delete(&format!("/admin/users/{}/hard", carol.id))
        .authorization_bearer(&admin.token)
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert!(!bucket.store.contains(&hard_key));
    let row = file_row(&bucket.db, &hard_key).await;
    assert!(row.deleted_at.is_some());
    assert_eq!(
        row.link_id, None,
        "the link row is gone; the file row records the removal"
    );
    assert!(bucket.store.contains(&bystander_key));
}

#[tokio::test]
async fn deleting_your_own_account_removes_your_files() {
    let bucket = Bucket::new().await;
    let email = unique_email();
    let user = users::ActiveModel {
        email: Set(email.clone()),
        password_hash: Set(bcrypt::hash("correct horse", 4).unwrap()),
        created_at: Set(chrono::Utc::now().naive_utc()),
        email_verified: Set(true),
        ..Default::default()
    }
    .insert(&bucket.db)
    .await
    .unwrap();
    let me = Person {
        id: user.id,
        token: create_jwt(user.id, &email, user.token_version).unwrap(),
    };
    let (_, key) = bucket
        .shared(&me, "a.pdf", "application/pdf", 10, json!({}))
        .await;

    let res = bucket
        .app()
        .await
        .post("/auth/delete-account")
        .authorization_bearer(&me.token)
        .json(&json!({"password": "correct horse"}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert!(!bucket.store.contains(&key));
    assert!(file_row(&bucket.db, &key).await.deleted_at.is_some());
}

// ------------------------------------------------------------ editing

#[tokio::test]
async fn a_file_link_cannot_be_pointed_at_a_url_cloned_or_routed() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let id = link["id"].as_i64().unwrap();
    let app = bucket.app().await;

    let res = app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&alice.token)
        .json(&json!({"original_url": "https://iana.org/elsewhere", "title": "Hijacked"}))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    let row = link_row(&bucket.db, id).await;
    assert!(row.is_file(), "still a file link");
    assert_eq!(
        row.title.as_deref(),
        Some("a.pdf"),
        "nothing of the request was applied"
    );

    let clone = app
        .post(&format!("/links/{id}/clone"))
        .authorization_bearer(&alice.token)
        .await;
    assert_eq!(clone.status_code(), 400, "{}", clone.text());
    assert!(
        clone.json::<Value>()["error"]
            .as_str()
            .unwrap()
            .contains("cannot be cloned")
    );
    assert_eq!(links_of(&bucket.db, alice.id).await, 1);

    let rules = app
        .put(&format!("/links/{id}/rules"))
        .authorization_bearer(&alice.token)
        .json(&json!({"rules": [{"match_device": "Mobile", "destination_url": "https://iana.org/m"}]}))
        .await;
    assert_eq!(rules.status_code(), 400, "{}", rules.text());

    // The file is untouched and still served.
    assert!(bucket.store.contains(&key));
    assert_eq!(
        app.get(&format!("/{}", code(&link))).await.status_code(),
        302
    );

    // The dashboard echoes the empty URL the API shows; that is not a change.
    let edit = app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&alice.token)
        .json(&json!({"original_url": "", "title": "Renamed"}))
        .await;
    assert_eq!(edit.status_code(), 200, "{}", edit.text());
    let edited: Value = edit.json();
    assert_eq!(edited["title"], "Renamed");
    assert_eq!(edited["original_url"], "");
    assert_eq!(edited["file"]["filename"], "a.pdf");
    assert!(link_row(&bucket.db, id).await.is_file());
}

#[tokio::test]
async fn an_org_colleague_edits_the_link_under_the_usual_roles_and_the_file_rules_hold() {
    let bucket = Bucket::new().await;
    let owner = person(&bucket.db).await;
    let editor = person(&bucket.db).await;
    let viewer = person(&bucket.db).await;
    let outsider = person(&bucket.db).await;
    let org = organizations::ActiveModel {
        name: Set("Team".to_string()),
        slug: Set(format!("team-{}", unique_code().to_lowercase())),
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
            10,
            json!({"org_id": org.id}),
        )
        .await;
    assert_eq!(link["org_id"], json!(org.id));
    let id = link["id"].as_i64().unwrap();
    let app = bucket.app().await;

    // A colleague sees the file in the organization's list.
    let list = app
        .get(&format!("/links?org_id={}", org.id))
        .authorization_bearer(&editor.token)
        .await;
    let items: Vec<Value> = list.json();
    let item = items
        .iter()
        .find(|item| item["id"] == json!(id))
        .expect("listed");
    assert_eq!(item["file"]["filename"], "plan.pdf");
    assert_no_storage_details(&list.text(), &key, "the organization list");

    // An editor may retitle it, and still may not repoint or clone it.
    let retitle = app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&editor.token)
        .json(&json!({"title": "The plan"}))
        .await;
    assert_eq!(retitle.status_code(), 200, "{}", retitle.text());
    assert_eq!(retitle.json::<Value>()["file"]["filename"], "plan.pdf");
    let repoint = app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&editor.token)
        .json(&json!({"original_url": "https://iana.org/elsewhere"}))
        .await;
    assert_eq!(repoint.status_code(), 400, "{}", repoint.text());
    let clone = app
        .post(&format!("/links/{id}/clone"))
        .authorization_bearer(&editor.token)
        .await;
    assert_eq!(clone.status_code(), 400, "{}", clone.text());

    // An editor who did not create it may not delete it; nor a viewer; nor a
    // stranger, who cannot even read it. The file survives all three.
    for who in [&editor, &viewer, &outsider] {
        let res = app
            .delete(&format!("/links/{id}"))
            .authorization_bearer(&who.token)
            .await;
        assert_eq!(res.status_code(), 403, "{}", res.text());
    }
    let res = app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&viewer.token)
        .json(&json!({"title": "Nope"}))
        .await;
    assert_eq!(res.status_code(), 403);
    assert_eq!(
        app.get(&format!("/links/{id}"))
            .authorization_bearer(&outsider.token)
            .await
            .status_code(),
        403
    );
    assert!(bucket.store.contains(&key));
    assert!(file_row(&bucket.db, &key).await.deleted_at.is_none());

    // Deleting the organization removes its links for good, and their files.
    let res = bucket
        .app()
        .await
        .delete(&format!("/orgs/{}", org.id))
        .authorization_bearer(&owner.token)
        .await;
    assert_eq!(res.status_code(), 204, "{}", res.text());
    assert!(!bucket.store.contains(&key));
    assert!(file_row(&bucket.db, &key).await.deleted_at.is_some());
}

// ------------------------------------------------------------------ usage

#[tokio::test]
async fn usage_counts_the_callers_own_stored_files_on_live_links() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let bob = person(&bucket.db).await;
    bucket
        .shared(&alice, "a.pdf", "application/pdf", 100, json!({}))
        .await;
    bucket
        .shared(&alice, "b.pdf", "application/pdf", 250, json!({}))
        .await;
    // Not counted: an upload that was never completed,
    bucket
        .uploaded(&alice, "pending.pdf", "application/pdf", 9_999)
        .await;
    // a file whose link was deleted,
    let (deleted, _) = bucket
        .shared(&alice, "gone.pdf", "application/pdf", 7_000, json!({}))
        .await;
    let res = bucket
        .app()
        .await
        .delete(&format!("/links/{}", deleted["id"]))
        .authorization_bearer(&alice.token)
        .await;
    assert_eq!(res.status_code(), 200);
    // a file whose link was deleted while the bucket would not delete it (the
    // object is still stored, but it is on no live link),
    let (stuck, stuck_key) = bucket
        .shared(&alice, "stuck.pdf", "application/pdf", 3_000, json!({}))
        .await;
    bucket.store.fail_deletes(true);
    let res = bucket
        .app()
        .await
        .delete(&format!("/links/{}", stuck["id"]))
        .authorization_bearer(&alice.token)
        .await;
    assert_eq!(res.status_code(), 200);
    bucket.store.fail_deletes(false);
    assert!(bucket.store.contains(&stuck_key));
    // a live link whose file has been removed from the bucket,
    let (_, removed_key) = bucket
        .shared(&alice, "removed.pdf", "application/pdf", 600, json!({}))
        .await;
    let mut removed: link_files::ActiveModel = file_row(&bucket.db, &removed_key).await.into();
    removed.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    removed.update(&bucket.db).await.unwrap();
    // and someone else's file.
    bucket
        .shared(&bob, "bob.pdf", "application/pdf", 40_000, json!({}))
        .await;

    let app = bucket.app().await;
    let usage = app
        .get("/files/usage")
        .authorization_bearer(&alice.token)
        .await;
    assert_eq!(usage.status_code(), 200, "{}", usage.text());
    assert_eq!(
        usage.json::<Value>(),
        json!({"bytes_used": 350, "file_count": 2, "max_file_bytes": 1_000_000})
    );
    let usage = app
        .get("/files/usage")
        .authorization_bearer(&bob.token)
        .await;
    assert_eq!(usage.json::<Value>()["bytes_used"], 40_000);
    assert_eq!(usage.json::<Value>()["file_count"], 1);
    let nobody = person(&bucket.db).await;
    let usage = app
        .get("/files/usage")
        .authorization_bearer(&nobody.token)
        .await;
    assert_eq!(
        usage.json::<Value>(),
        json!({"bytes_used": 0, "file_count": 0, "max_file_bytes": 1_000_000})
    );
}

// ------------------------------------------------------- feature on / off

#[tokio::test]
async fn settings_announce_file_sharing_only_when_it_is_on() {
    let bucket = Bucket::with_max(12_345).await;
    let on = bucket.app().await.get("/auth/settings").await;
    assert_eq!(
        on.json::<Value>()["files"],
        json!({"enabled": true, "max_bytes": 12_345})
    );

    let (off, _) = spawn_real_app().await;
    let settings: Value = off.get("/auth/settings").await.json();
    assert!(
        settings.as_object().unwrap().get("files").is_none(),
        "no bucket, no `files` key: {settings}"
    );
}

#[tokio::test]
async fn without_a_bucket_the_endpoints_answer_404_and_links_are_unchanged() {
    let (app, db) = spawn_real_app().await;
    let alice = person(&db).await;
    // Someone else's upload, made while the feature was on. Switched off, the
    // answer for it is the same 404 as for everything else.
    let earlier = Bucket::new().await;
    let (existing_upload, _) = earlier
        .uploaded(&alice, "a.pdf", "application/pdf", 10)
        .await;

    for res in [
        app.post("/files/uploads")
            .authorization_bearer(&alice.token)
            .json(&json!({"filename": "a.pdf", "content_type": "application/pdf", "size_bytes": 1}))
            .await,
        app.post(&format!("/files/uploads/{existing_upload}/complete"))
            .authorization_bearer(&alice.token)
            .json(&json!({}))
            .await,
        app.get("/files/usage")
            .authorization_bearer(&alice.token)
            .await,
    ] {
        assert_eq!(res.status_code(), 404, "{}", res.text());
        assert_eq!(
            res.json::<Value>(),
            json!({"error": "File sharing is not set up on this server"})
        );
    }
    // Without credentials the answer is the 401 every protected route gives,
    // on or off, so it does not say whether the feature is set up.
    let off = app.get("/files/usage").await;
    let on = earlier.app().await.get("/files/usage").await;
    assert_eq!(off.status_code(), 401);
    assert_eq!(on.status_code(), 401);
    assert_eq!(off.text(), on.text());
    let rows = link_files::Entity::find()
        .filter(link_files::Column::UserId.eq(alice.id))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(rows, 1, "nothing was started while the feature was off");
    let untouched = link_files::Entity::find_by_id(existing_upload as i32)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched.status, "pending");
    assert_eq!(links_of(&db, alice.id).await, 0);

    // An ordinary link looks exactly as it did: no `file` key anywhere.
    let created = app
        .post("/links")
        .authorization_bearer(&alice.token)
        .json(&json!({"original_url": "https://iana.org/plain"}))
        .await;
    assert_eq!(created.status_code(), 201, "{}", created.text());
    let created: Value = created.json();
    assert!(
        created.as_object().unwrap().get("file").is_none(),
        "{created}"
    );
    assert_eq!(created["original_url"], "https://iana.org/plain");
    let listed: Vec<Value> = app
        .get("/links")
        .authorization_bearer(&alice.token)
        .await
        .json();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].as_object().unwrap().get("file").is_none());
    assert_eq!(listed[0]["original_url"], "https://iana.org/plain");
    let visit = app
        .get(&format!("/{}", created["code"].as_str().unwrap()))
        .await;
    assert_eq!(
        visit.status_code(),
        307,
        "an ordinary redirect is still a 307"
    );
    assert_eq!(visit.header(LOCATION), "https://iana.org/plain");
}

#[tokio::test]
async fn a_file_link_left_behind_when_the_bucket_is_unset_is_a_dead_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, _) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;

    let (off, _) = spawn_real_app().await;
    let res = off.get(&format!("/{}", code(&link))).await;
    assert_eq!(res.status_code(), 410, "{}", res.text());
    assert!(res.maybe_header(LOCATION).is_none());
}

// ------------------------------------------- the review's fixes and guards

#[tokio::test]
async fn an_expiry_in_the_past_is_refused_and_the_upload_can_be_retried() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (upload_id, key) = bucket
        .uploaded(&alice, "a.pdf", "application/pdf", 10)
        .await;
    let app = bucket.app().await;
    let path = format!("/files/uploads/{upload_id}/complete");

    let res = app
        .post(&path)
        .authorization_bearer(&alice.token)
        .json(&json!({"expires_at": "2020-01-01T23:59:00Z"}))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    let error = res.json::<Value>()["error"]
        .as_str()
        .expect("a JSON error")
        .to_string();
    assert!(error.contains("in the past"), "{error}");

    // No link was made, and the upload is as it was: pending, in the bucket.
    assert_eq!(links_of(&bucket.db, alice.id).await, 0);
    let row = file_row(&bucket.db, &key).await;
    assert_eq!(row.status, "pending");
    assert_eq!(row.link_id, None);
    assert_eq!(row.deleted_at, None);
    assert!(bucket.store.contains(&key));

    // The same upload completes with a date that is still ahead.
    let res = app
        .post(&path)
        .authorization_bearer(&alice.token)
        .json(&json!({"expires_at": "2099-01-01T23:59:00Z"}))
        .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    assert_eq!(res.json::<Value>()["is_active"], true);
    assert_eq!(links_of(&bucket.db, alice.id).await, 1);
}

/// The ids `GET {path}` lists.
async fn listed_ids(app: &axum_test::TestServer, who: &Person, path: &str) -> Vec<i64> {
    let res = app.get(path).authorization_bearer(&who.token).await;
    assert_eq!(res.status_code(), 200, "{path}: {}", res.text());
    res.json::<Vec<Value>>()
        .iter()
        .map(|item| item["id"].as_i64().expect("id"))
        .collect()
}

#[tokio::test]
async fn search_finds_a_file_link_by_its_filename_and_never_by_its_marker() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let word = format!("budget{}", unique_code().to_lowercase());
    let (link, key) = bucket
        .shared(
            &alice,
            &format!("{word}.pdf"),
            "application/pdf",
            10,
            json!({"title": "Q3 numbers"}),
        )
        .await;
    let id = link["id"].as_i64().unwrap();
    let file = file_row(&bucket.db, &key).await;
    let marker = link_row(&bucket.db, id).await.original_url;
    assert_eq!(marker, format!("opn-file:{}", file.id));
    let app = bucket.app().await;

    // By the filename, in any case, although the title says something else.
    for search in [word.clone(), word.to_uppercase(), format!("{word}.pdf")] {
        assert_eq!(
            listed_ids(&app, &alice, &format!("/links?search={search}")).await,
            vec![id],
            "search={search}"
        );
    }
    // Never by the marker, whole or in part. Alice has this one link, so any
    // hit is the marker matching.
    for search in ["opn-file", "opn", "file:", marker.as_str()] {
        assert_eq!(
            listed_ids(&app, &alice, &format!("/links?search={search}")).await,
            Vec::<i64>::new(),
            "search={search}"
        );
    }
    // The other fields are searched as before.
    assert_eq!(
        listed_ids(&app, &alice, "/links?search=q3%20numbers").await,
        vec![id]
    );
    assert_eq!(
        listed_ids(&app, &alice, &format!("/links?search={}", code(&link))).await,
        vec![id]
    );

    // The search narrows what the caller may list, and nothing more.
    let stranger = person(&bucket.db).await;
    assert_eq!(
        listed_ids(&app, &stranger, &format!("/links?search={word}")).await,
        Vec::<i64>::new()
    );

    // The admin list follows the same rule.
    let admin = person_with(&bucket.db, true, true).await;
    let admin_search = |search: String| {
        let (app, token, user_id) = (&app, &admin.token, alice.id);
        async move {
            let res = app
                .get("/admin/links")
                .add_query_param("user_id", user_id)
                .add_query_param("search", &search)
                .authorization_bearer(token)
                .await;
            assert_eq!(res.status_code(), 200, "{search}: {}", res.text());
            res.text()
        }
    };
    assert!(admin_search(word.clone()).await.contains(code(&link)));
    assert!(
        !admin_search("opn-file".to_string())
            .await
            .contains(code(&link))
    );

    // A file that was removed from the bucket is no longer searched.
    let mut removed: link_files::ActiveModel = file.into();
    removed.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    removed.update(&bucket.db).await.unwrap();
    assert_eq!(
        listed_ids(&app, &alice, &format!("/links?search={word}")).await,
        Vec::<i64>::new()
    );
}

#[tokio::test]
async fn a_one_time_file_link_is_used_up_in_the_database_by_its_one_visit() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, _) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let link_id = link["id"].as_i64().unwrap();
    let mut row: links::ActiveModel = link_row(&bucket.db, link_id).await.into();
    row.burn_after_reading = Set(true);
    row.update(&bucket.db).await.unwrap();
    let path = format!("/{}", code(&link));

    // Two apps are two processes' worth of click buffers: the second knows
    // nothing of the first one's visit except what is in the database. Only
    // the conditional UPDATE that consumes the click can refuse it.
    let (first, second) = (bucket.app().await, bucket.app().await);
    assert_eq!(first.get(&path).await.status_code(), 302);
    let row = link_row(&bucket.db, link_id).await;
    assert_eq!(row.click_count, 1, "the visit was consumed at once");
    assert!(row.burned_at.is_some());

    let again = second.get(&path).await;
    assert_eq!(again.status_code(), 410, "{}", again.text());
    assert_eq!(
        bucket.store.downloads().len(),
        1,
        "only the one visit got an address"
    );
}

#[tokio::test]
async fn a_file_link_with_the_interstitial_signs_nothing_until_the_visitor_confirms() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, _) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;
    let mut row: links::ActiveModel = link_row(&bucket.db, link["id"].as_i64().unwrap())
        .await
        .into();
    row.safe_link_interstitial = Set(true);
    row.update(&bucket.db).await.unwrap();
    let path = format!("/{}", code(&link));
    let app = bucket.app().await;

    let res = app.get(&path).await;
    assert_eq!(res.status_code(), 307, "{}", res.text());
    let location = res.header(LOCATION).to_str().unwrap().to_string();
    assert!(
        location.contains(&format!("/r/{}", code(&link))),
        "sent to the interstitial page: {location}"
    );
    assert!(
        bucket.store.downloads().is_empty(),
        "nothing is signed before the visitor confirms"
    );

    let confirmed = app.get(&path).add_query_param("confirm", "1").await;
    assert_eq!(confirmed.status_code(), 302, "{}", confirmed.text());
    assert_eq!(bucket.store.downloads().len(), 1);
}

#[tokio::test]
async fn the_preview_of_a_live_file_link_says_it_is_live() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (link, key) = bucket
        .shared(&alice, "a.pdf", "application/pdf", 10, json!({}))
        .await;

    // The password page and the interstitial read this. The marker is not a
    // URL the blocklist can judge, so it must not be asked.
    let res = bucket
        .app()
        .await
        .get(&format!("/{}/preview", code(&link)))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    let preview: Value = res.json();
    assert_eq!(preview["is_active"], true, "{preview}");
    assert_eq!(preview["inactive_reason"], Value::Null, "{preview}");
    assert_eq!(preview["original_url"], "");
    assert_no_storage_details(&res.text(), &key, "the preview");
}

#[tokio::test]
async fn an_upload_the_sweep_closed_while_it_was_being_completed_gets_no_link() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (upload_id, key) = bucket
        .uploaded(&alice, "a.pdf", "application/pdf", 10)
        .await;
    let file_id = file_row(&bucket.db, &key).await.id;
    let app = bucket.app().await;

    // The complete reads the upload as open and stops while asking the bucket.
    // In that gap the sweep closes the upload: it stamps the row as removed.
    // (The object is left in place here so that the complete goes on to the
    // claim; in production the sweep's delete may land just after the HEAD.)
    let gate = bucket.store.hold_next_head();
    let complete = async {
        app.post(&format!("/files/uploads/{upload_id}/complete"))
            .authorization_bearer(&alice.token)
            .json(&json!({}))
            .await
    };
    let sweep = async {
        tokio::time::timeout(Duration::from_secs(20), gate.arrived())
            .await
            .expect("the complete never asked the bucket about its upload");
        let mut closed: link_files::ActiveModel = link_files::Entity::find_by_id(file_id)
            .one(&bucket.db)
            .await
            .unwrap()
            .unwrap()
            .into();
        closed.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
        closed.update(&bucket.db).await.unwrap();
        gate.release();
    };
    let (res, ()) = tokio::join!(complete, sweep);

    assert_eq!(res.status_code(), 410, "{}", res.text());
    assert_eq!(
        links_of(&bucket.db, alice.id).await,
        0,
        "no link may be joined to a file the sweep removed"
    );
    let row = file_row(&bucket.db, &key).await;
    assert_eq!(row.link_id, None);
    assert_eq!(row.status, "pending");
}

#[tokio::test]
async fn deleting_an_account_removes_the_files_of_the_organization_that_goes_with_it() {
    let bucket = Bucket::new().await;
    let admin = person_with(&bucket.db, true, true).await;
    let owner = person(&bucket.db).await;
    let colleague = person(&bucket.db).await;
    let org = organizations::ActiveModel {
        name: Set("Team".to_string()),
        slug: Set(format!("team-{}", unique_code().to_lowercase())),
        owner_id: Set(owner.id),
        ..Default::default()
    }
    .insert(&bucket.db)
    .await
    .unwrap();
    for (who, role) in [(&owner, "owner"), (&colleague, "editor")] {
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
    // The colleague shares a file in the organization and then leaves it. The
    // link is the organization's; its creator is not the owner.
    let (link, key) = bucket
        .shared(
            &colleague,
            "plan.pdf",
            "application/pdf",
            10,
            json!({"org_id": org.id}),
        )
        .await;
    let link_id = link["id"].as_i64().unwrap();
    assert_eq!(
        link_row(&bucket.db, link_id).await.user_id,
        Some(colleague.id)
    );
    org_members::Entity::delete_many()
        .filter(org_members::Column::OrgId.eq(org.id))
        .filter(org_members::Column::UserId.eq(colleague.id))
        .exec(&bucket.db)
        .await
        .unwrap();

    // The owner is now alone in it, so the organization goes with the account.
    let res = bucket
        .app()
        .await
        .delete(&format!("/admin/users/{}", owner.id))
        .authorization_bearer(&admin.token)
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert!(
        link_row(&bucket.db, link_id).await.deleted_at.is_some(),
        "the organization's link went down with its owner"
    );
    assert!(
        !bucket.store.contains(&key),
        "and its file went at once, not at the next daily sweep"
    );
    assert!(file_row(&bucket.db, &key).await.deleted_at.is_some());
}

#[tokio::test]
async fn completing_again_after_the_link_was_deleted_answers_410() {
    let bucket = Bucket::new().await;
    let alice = person(&bucket.db).await;
    let (upload_id, _) = bucket
        .uploaded(&alice, "a.pdf", "application/pdf", 10)
        .await;
    let app = bucket.app().await;
    let complete = || {
        app.post(&format!("/files/uploads/{upload_id}/complete"))
            .authorization_bearer(&alice.token)
            .json(&json!({}))
    };
    let first = complete().await;
    assert_eq!(first.status_code(), 201, "{}", first.text());
    let link: Value = first.json();

    let deleted = app
        .delete(&format!("/links/{}", link["id"]))
        .authorization_bearer(&alice.token)
        .await;
    assert!(deleted.status_code().is_success(), "{}", deleted.text());

    let again = complete().await;
    assert_eq!(again.status_code(), 410, "{}", again.text());
    assert!(
        !again.text().contains(code(&link)),
        "a deleted link is not handed back: {}",
        again.text()
    );
    assert_eq!(
        links_of(&bucket.db, alice.id).await,
        1,
        "and no new link is made for a file that is gone"
    );
}
