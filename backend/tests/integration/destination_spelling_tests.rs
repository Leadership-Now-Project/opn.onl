//! How a link's destination is spelled when it is stored and when it is sent.
//!
//! The URL parser reads `https:host/x`, `https:/host/x`, `https:\\host\x` and
//! `https:///host/x` all as `https://host/x`, and every check runs on that
//! host. A browser sent the typed text in `Location` resolves it against the
//! short-link host instead and lands on a page that does not exist. Real
//! router, real Postgres. What these tests pin:
//!
//! * text that is not written out is stored, and redirected to, in the
//!   parser's spelling, through create, edit, bulk create (which is also the
//!   CSV import), clone and routing rules;
//! * an address that IS written out is stored exactly as typed, `http://`
//!   included, also where the parser would spell it differently;
//! * the refusals (internal host, raw IP, reserved name, blocklist, file
//!   type, length) apply to the no-slash spelling too;
//! * a row stored before the rule keeps its text and redirects well formed;
//! * saving a link without a new destination, however the destination is
//!   spelled in the request, leaves the row, its Drive file and its media
//!   page alone;
//! * the same-URL throttle counts one destination typed three ways as one.

use crate::common;

use axum::http::header::LOCATION;
use axum_test::{TestResponse, TestServer};
use common::{spawn_real_app, spawn_real_app_with_embed_check, unique_code, unique_email};
use opn_onl_backend::entity::{
    blocked_domains, link_drive_files, link_media, links, routing_rules, users,
};
use opn_onl_backend::utils::create_jwt;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter,
};
use serde_json::{Value, json};

struct Person {
    id: i32,
    token: String,
}

async fn person(db: &DatabaseConnection) -> Person {
    let email = unique_email();
    let user = users::ActiveModel {
        email: Set(email.clone()),
        password_hash: Set("not-a-real-hash".to_string()),
        created_at: Set(chrono::Utc::now().naive_utc()),
        email_verified: Set(true),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert user");
    let token = create_jwt(user.id, &email, user.token_version).expect("mint token");
    Person { id: user.id, token }
}

/// A fresh app over the shared database. Each has its own rate limiters, and
/// the mock transport puts every caller in one per-second bucket, so a test
/// takes a new one for each handful of requests.
async fn app() -> TestServer {
    spawn_real_app().await.0
}

/// A path no other test uses, so the same-URL throttle and lookups by URL
/// never meet another test's rows.
fn path() -> String {
    format!("p40-{}", uuid::Uuid::new_v4().simple())
}

async fn create(who: &Person, body: Value) -> TestResponse {
    app()
        .await
        .post("/links")
        .authorization_bearer(&who.token)
        .json(&body)
        .await
}

async fn created(who: &Person, url: &str) -> (i32, String) {
    let res = create(who, json!({"original_url": url})).await;
    assert_eq!(res.status_code(), 201, "create {url:?}: {}", res.text());
    let body: Value = res.json();
    (
        body["id"].as_i64().unwrap() as i32,
        body["code"].as_str().unwrap().to_string(),
    )
}

async fn update(who: &Person, id: i32, body: Value) -> TestResponse {
    app()
        .await
        .put(&format!("/links/{id}"))
        .authorization_bearer(&who.token)
        .json(&body)
        .await
}

async fn row(db: &DatabaseConnection, id: i32) -> links::Model {
    links::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("link row")
}

async fn stored(db: &DatabaseConnection, id: i32) -> String {
    row(db, id).await.original_url
}

/// Where a visitor of `/{code}` is sent.
async fn sent_to(code: &str) -> String {
    let res = app().await.get(&format!("/{code}")).await;
    assert_eq!(res.status_code(), 307, "GET /{code}: {}", res.text());
    res.header(LOCATION).to_str().unwrap().to_string()
}

/// A link as it was stored before the rule: written straight into the table.
async fn old_row(db: &DatabaseConnection, who: &Person, original_url: &str) -> (i32, String) {
    let code = format!("p40{}", unique_code());
    let link = links::ActiveModel {
        original_url: Set(original_url.to_string()),
        code: Set(code.clone()),
        user_id: Set(Some(who.id)),
        created_at: Set(chrono::Utc::now().naive_utc()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert old link");
    (link.id, code)
}

/// Each spelling the parser accepts for `{scheme}://iana.org/{path}`, with
/// the spelling that is stored.
fn not_written_out(path: &str) -> Vec<(String, String)> {
    let https = format!("https://iana.org/{path}");
    vec![
        (format!("https:iana.org/{path}"), https.clone()),
        (format!("https:/iana.org/{path}"), https.clone()),
        (format!("https:///iana.org/{path}"), https.clone()),
        (format!("https:\\\\iana.org\\{path}"), https.clone()),
        (format!("https://iana.org\\{path}"), https.clone()),
        (format!("HTTPS:IANA.org/{path}"), https.clone()),
        (format!(" https://iana.org/{path}"), https.clone()),
        (format!("https://iana.org/{path} "), https),
        (
            format!("http:iana.org/{path}"),
            format!("http://iana.org/{path}"),
        ),
        (
            format!("http:/iana.org/{path}?a=1#b"),
            format!("http://iana.org/{path}?a=1#b"),
        ),
    ]
}

// ------------------------------------------------------------------- create

#[tokio::test]
async fn a_destination_typed_without_its_slashes_is_stored_and_sent_well_formed() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;

    for (typed, well_formed) in not_written_out(&path()) {
        let (id, code) = created(&who, &typed).await;
        assert_eq!(stored(&db, id).await, well_formed, "stored for {typed:?}");
        assert_eq!(sent_to(&code).await, well_formed, "sent for {typed:?}");
    }
    // The bare host: the one case where the parser adds the slash.
    let (id, code) = created(&who, "http:iana.org").await;
    assert_eq!(stored(&db, id).await, "http://iana.org/");
    assert_eq!(sent_to(&code).await, "http://iana.org/");
}

/// Each of these is spelled differently by the parser (see the unit tests in
/// `url_policy.rs`), and each is kept as typed: nothing changes for a link
/// that was already written out.
#[tokio::test]
async fn a_written_out_destination_is_stored_exactly_as_typed() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let path = path();

    for typed in [
        format!("https://iana.org/{path}"),
        format!("http://iana.org/{path}"),
        format!("https://iana.org/{path}?x=a\\b#c\\d"),
        format!("https://IANA.org/{path}"),
        format!("HTTPS://iana.org/{path}/upper"),
        format!("https://iana.org:443/{path}"),
        format!("http://iana.org:80/{path}"),
        format!("https://iana.org/{path}/../up"),
        format!("https://iana.org/%7E{path}"),
        // The parser would add a slash to each of these.
        "https://iana.org".to_string(),
        "http://iana.org".to_string(),
    ] {
        let (id, code) = created(&who, &typed).await;
        assert_eq!(stored(&db, id).await, typed, "stored");
        assert_eq!(sent_to(&code).await, typed, "sent");
    }
}

#[tokio::test]
async fn what_is_refused_written_out_is_refused_without_the_slashes_too() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let path = path();

    let blocked_host = format!("blk-{path}.iana.org");
    blocked_domains::ActiveModel {
        domain: Set(blocked_host.clone()),
        reason: Set(Some("destination spelling test".to_string())),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("block a domain");

    // Under the limit as typed (2 bytes a letter); over it once the parser
    // has percent-encoded it (6 bytes a letter), which is what would be kept.
    let grows = format!("https:iana.org/{path}/{}", "é".repeat(500));
    assert!(grows.len() < 2048);

    for (typed, status, says) in [
        (format!("http:localhost/{path}"), 400, "local/internal"),
        (
            format!("https:/metadata.google.internal/{path}"),
            400,
            "local/internal",
        ),
        (format!("https:192.168.1.1/{path}"), 400, "raw IP"),
        (format!("https:/127.0.0.1/{path}"), 400, "raw IP"),
        (format!("https:\\\\10.0.0.1\\{path}"), 400, "raw IP"),
        (format!("https:example.com/{path}"), 400, "reserved"),
        (format!("https:iana.org/{path}.exe"), 400, ".exe"),
        (
            format!("https:iana.org/{path}?next=javascript:alert(1)"),
            400,
            "malicious",
        ),
        (grows, 400, "too long"),
        (format!("https:{blocked_host}/{path}"), 403, ""),
        (format!("https:/sub.{blocked_host}/{path}"), 403, ""),
        ("https:".to_string(), 400, ""),
        ("https:/".to_string(), 400, ""),
        ("https:///".to_string(), 400, ""),
        (format!("ftp:iana.org/{path}"), 400, "http or https"),
    ] {
        let res = create(&who, json!({"original_url": typed})).await;
        assert_eq!(res.status_code(), status, "{typed:?}: {}", res.text());
        let error = res.json::<Value>()["error"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(error.contains(says), "{typed:?} answered {error:?}");
    }
    let made = links::Entity::find()
        .filter(links::Column::UserId.eq(who.id))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(made, 0, "a refused request makes no link");

    // A name that resolves to this machine, where the resolver plays along.
    let lookup = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::lookup_host(("127.0.0.1.nip.io", 443)),
    )
    .await;
    let loopback = match lookup {
        Ok(Ok(mut addrs)) => addrs.any(|a| a.ip().is_loopback()),
        _ => false,
    };
    if loopback {
        let res = create(
            &who,
            json!({"original_url": format!("https:127.0.0.1.nip.io/{path}")}),
        )
        .await;
        assert_eq!(res.status_code(), 400, "{}", res.text());
    } else {
        eprintln!("127.0.0.1.nip.io does not resolve here; skipping the private-address case");
    }
}

// --------------------------------------------------- edit, bulk, clone, rules

#[tokio::test]
async fn an_edit_stores_a_new_destination_well_formed() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let (id, code) = created(&who, &format!("https://iana.org/{}", path())).await;

    for (typed, well_formed) in not_written_out(&path()) {
        let res = update(&who, id, json!({"original_url": typed})).await;
        assert_eq!(res.status_code(), 200, "{typed:?}: {}", res.text());
        assert_eq!(res.json::<Value>()["original_url"], well_formed.as_str());
        assert_eq!(stored(&db, id).await, well_formed, "stored for {typed:?}");
        assert_eq!(sent_to(&code).await, well_formed, "sent for {typed:?}");
        // Something else in between, so the next spelling is a real change.
        let other = format!("http://iana.org/{}", path());
        let res = update(&who, id, json!({"original_url": other})).await;
        assert_eq!(res.status_code(), 200, "{}", res.text());
        assert_eq!(stored(&db, id).await, other, "http is kept as typed");
    }

    let res = update(&who, id, json!({"original_url": "http:localhost/x"})).await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
}

async fn bulk(who: &Person, urls: &[String]) -> Value {
    let res = app()
        .await
        .post("/links/bulk")
        .authorization_bearer(&who.token)
        .json(&json!({"urls": urls}))
        .await;
    assert_eq!(res.status_code(), 200, "bulk: {}", res.text());
    res.json()
}

/// `POST /links/bulk` is also what the dashboard's import sends.
#[tokio::test]
async fn bulk_create_stores_each_destination_well_formed_and_reports_failures_as_typed() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let path = path();

    let (typed, well_formed): (Vec<String>, Vec<String>) = not_written_out(&path)
        .into_iter()
        .chain([(
            format!("http://iana.org/{path}/kept"),
            format!("http://iana.org/{path}/kept"),
        )])
        .unzip();
    let refused = [
        format!("http:localhost/{path}"),
        format!("https:192.168.1.1/{path}"),
    ];
    let mut urls = typed.clone();
    urls.extend(refused.iter().cloned());

    let body = bulk(&who, &urls).await;
    let made = body["links"].as_array().unwrap();
    assert_eq!(made.len(), typed.len(), "{body}");
    for (link, expected) in made.iter().zip(&well_formed) {
        let id = link["id"].as_i64().unwrap() as i32;
        assert_eq!(&stored(&db, id).await, expected);
        assert_eq!(&sent_to(link["code"].as_str().unwrap()).await, expected);
    }
    // The import box finds a failed line again by the text that was sent.
    let failed: Vec<&str> = body["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|failure| failure["url"].as_str().unwrap())
        .collect();
    assert_eq!(
        failed,
        refused.iter().map(String::as_str).collect::<Vec<_>>()
    );
}

/// The `Original URL` column of `GET /links/export`, one per link.
async fn exported(who: &Person) -> Vec<String> {
    let res = app()
        .await
        .get("/links/export")
        .authorization_bearer(&who.token)
        .await;
    assert_eq!(res.status_code(), 200, "export: {}", res.text());
    res.text()
        .lines()
        .skip(1)
        .map(|line| {
            // Every text cell is quoted, and no destination here has a quote
            // or a comma in it: ID,"code","url",…
            let cells: Vec<&str> = line.split(',').collect();
            cells[2].trim_matches('"').to_string()
        })
        .collect()
}

#[tokio::test]
async fn what_is_exported_imports_as_the_same_destinations() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let path = path();

    created(&who, &format!("https:iana.org/{path}/a")).await;
    created(&who, &format!("https://IANA.org:443/{path}/b")).await;
    created(&who, &format!("http://iana.org/{path}/c")).await;
    created(&who, "https://iana.org").await;
    let mut before = exported(&who).await;
    before.sort();
    assert_eq!(
        before,
        {
            let mut expected = vec![
                format!("https://iana.org/{path}/a"),
                format!("https://IANA.org:443/{path}/b"),
                format!("http://iana.org/{path}/c"),
                "https://iana.org".to_string(),
            ];
            expected.sort();
            expected
        },
        "the export shows what is stored"
    );

    let other = person(&db).await;
    let body = bulk(&other, &before).await;
    assert_eq!(body["failed"], json!([]), "{body}");
    assert_eq!(body["links"].as_array().unwrap().len(), before.len());
    let mut after = exported(&other).await;
    after.sort();
    assert_eq!(after, before, "the same text, link for link");
}

#[tokio::test]
async fn a_rule_stores_its_destination_well_formed() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let (id, code) = created(&who, &format!("https://iana.org/{}", path())).await;
    let path = path();

    let res = app()
        .await
        .put(&format!("/links/{id}/rules"))
        .authorization_bearer(&who.token)
        .json(&json!({"rules": [{"destination_url": format!("https:iana.org/{path}/rule")}]}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());

    let rules = routing_rules::Entity::find()
        .filter(routing_rules::Column::LinkId.eq(id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(rules.len(), 1);
    let well_formed = format!("https://iana.org/{path}/rule");
    assert_eq!(rules[0].destination_url, well_formed);
    assert_eq!(sent_to(&code).await, well_formed);

    // A rule written before the rule about spelling is sent well formed too.
    let mut old: routing_rules::ActiveModel = rules[0].clone().into();
    old.destination_url = Set(format!("https:/iana.org/{path}/old-rule"));
    old.update(&db).await.unwrap();
    assert_eq!(
        sent_to(&code).await,
        format!("https://iana.org/{path}/old-rule")
    );
}

// ------------------------------------------------- rows from before the rule

#[tokio::test]
async fn an_older_row_keeps_its_text_and_is_sent_well_formed() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;

    for (typed, well_formed) in not_written_out(&path()) {
        let (id, code) = old_row(&db, &who, &typed).await;

        assert_eq!(sent_to(&code).await, well_formed, "GET for {typed:?}");
        let app = app().await;
        let head = app
            .method(axum::http::Method::HEAD, &format!("/{code}"))
            .await;
        assert_eq!(head.status_code(), 307, "HEAD for {typed:?}");
        assert_eq!(head.header(LOCATION), well_formed.as_str());
        // The page that asks "continue to …?" puts this in an anchor.
        let preview: Value = app.get(&format!("/{code}/preview")).await.json();
        assert_eq!(preview["original_url"], well_formed.as_str(), "preview");
        assert_eq!(preview["domain"], "iana.org");

        assert_eq!(stored(&db, id).await, typed, "the row keeps its text");
    }
}

#[tokio::test]
async fn a_clone_of_an_older_row_is_stored_well_formed() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let path = path();
    let typed = format!("https:iana.org/{path}");
    let well_formed = format!("https://iana.org/{path}");
    let (id, _) = old_row(&db, &who, &typed).await;

    let res = app()
        .await
        .post(&format!("/links/{id}/clone"))
        .authorization_bearer(&who.token)
        .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let copy: Value = res.json();
    assert_eq!(copy["original_url"], well_formed.as_str());
    let copy_id = copy["id"].as_i64().unwrap() as i32;
    assert_eq!(stored(&db, copy_id).await, well_formed);
    assert_eq!(sent_to(copy["code"].as_str().unwrap()).await, well_formed);
    assert_eq!(stored(&db, id).await, typed, "the original keeps its text");
}

// ------------------------------------ a save that does not change where it goes

/// The most important one: an older row holds text `validate_url` would now
/// spell differently. Saving it, with or without its destination in the
/// request, in any spelling of the same address, must not rewrite it.
#[tokio::test]
async fn saving_an_older_row_without_a_new_destination_leaves_its_text_alone() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let path = path();
    let typed = format!("https:iana.org/{path}");
    let (id, code) = old_row(&db, &who, &typed).await;

    for body in [
        json!({"title": "Only the title"}),
        json!({"title": "Same text", "original_url": typed}),
        json!({"title": "Well formed", "original_url": format!("https://iana.org/{path}")}),
        json!({"title": "Another spelling", "original_url": format!("HTTPS:/IANA.org:443/{path}")}),
    ] {
        let res = update(&who, id, body.clone()).await;
        assert_eq!(res.status_code(), 200, "{body}: {}", res.text());
        assert_eq!(stored(&db, id).await, typed, "after {body}");
        assert_eq!(
            row(&db, id).await.title,
            body["title"].as_str().map(String::from)
        );
    }
    assert_eq!(sent_to(&code).await, format!("https://iana.org/{path}"));

    // A different address is a change, and is stored well formed.
    let res = update(
        &who,
        id,
        json!({"original_url": format!("https:iana.org/{path}/new")}),
    )
    .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(
        stored(&db, id).await,
        format!("https://iana.org/{path}/new")
    );
}

async fn drive_rows(db: &DatabaseConnection, link_id: i32) -> u64 {
    link_drive_files::Entity::find()
        .filter(link_drive_files::Column::LinkId.eq(link_id))
        .count(db)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_drive_link_keeps_its_file_when_saved_with_the_same_destination() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let file_id = format!("1{}_-x", unique_code());
    let url = format!("https://docs.google.com/document/d/{file_id}/edit?usp=drivesdk");

    let res = create(
        &who,
        json!({
            "original_url": url,
            "drive_file": {
                "file_id": file_id,
                "name": "Board plan.gdoc",
                "mime_type": "application/vnd.google-apps.document",
            },
        }),
    )
    .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let id = res.json::<Value>()["id"].as_i64().unwrap() as i32;
    assert_eq!(drive_rows(&db, id).await, 1);

    for body in [
        json!({"title": "Only the title"}),
        json!({"title": "Same text", "original_url": url}),
        json!({"title": "Another spelling", "original_url": url.replace("https://docs", "HTTPS://DOCS")}),
        json!({"title": "No slashes", "original_url": url.replace("https://", "https:")}),
    ] {
        let res = update(&who, id, body.clone()).await;
        assert_eq!(res.status_code(), 200, "{body}: {}", res.text());
        assert!(res.json::<Value>().get("drive").is_some(), "after {body}");
        assert_eq!(drive_rows(&db, id).await, 1, "after {body}");
        assert_eq!(stored(&db, id).await, url, "after {body}");
    }

    // Another document is a new destination: the row goes, as it always did.
    let res = update(
        &who,
        id,
        json!({"original_url": "https://docs.google.com/document/d/another/edit"}),
    )
    .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(drive_rows(&db, id).await, 0);
}

/// A Drive link made from an address without its slashes is stored written
/// out, so it is the Drive address every later check takes it for.
#[tokio::test]
async fn a_drive_link_typed_without_its_slashes_is_stored_as_a_drive_address() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let file_id = format!("1{}_-x", unique_code());

    let res = create(
        &who,
        json!({
            "original_url": format!("https:docs.google.com/document/d/{file_id}/edit"),
            "drive_file": {"file_id": file_id, "name": "Plan", "mime_type": "application/pdf"},
        }),
    )
    .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let id = res.json::<Value>()["id"].as_i64().unwrap() as i32;
    let kept = stored(&db, id).await;
    assert_eq!(
        kept,
        format!("https://docs.google.com/document/d/{file_id}/edit")
    );
    assert!(opn_onl_backend::utils::drive::is_drive_url(&kept));
    assert_eq!(drive_rows(&db, id).await, 1);
}

#[tokio::test]
async fn a_media_link_keeps_its_page_when_saved_with_the_same_destination() {
    let (app, db, _) = spawn_real_app_with_embed_check(None).await;
    let who = person(&db).await;
    let video = format!("{}Ab_-9", unique_code());

    let res = app
        .post("/links")
        .authorization_bearer(&who.token)
        .json(&json!({"original_url": format!("https://youtu.be/{video}"), "media": {}}))
        .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let id = res.json::<Value>()["id"].as_i64().unwrap() as i32;
    let url = format!("https://www.youtube.com/watch?v={video}");
    assert_eq!(stored(&db, id).await, url);
    let media_rows = || async {
        link_media::Entity::find()
            .filter(link_media::Column::LinkId.eq(id))
            .count(&db)
            .await
            .unwrap()
    };
    assert_eq!(media_rows().await, 1);

    for body in [
        json!({"title": "Only the title"}),
        json!({"title": "Same text", "original_url": url}),
        json!({"title": "Another spelling", "original_url": url.replace("https://www", "HTTPS://WWW")}),
        json!({"title": "No slashes", "original_url": url.replace("https://", "https:")}),
    ] {
        let res = update(&who, id, body.clone()).await;
        assert_eq!(res.status_code(), 200, "{body}: {}", res.text());
        assert_eq!(
            res.json::<Value>()["media"]["provider"],
            "youtube",
            "after {body}"
        );
        assert_eq!(media_rows().await, 1, "after {body}");
        assert_eq!(stored(&db, id).await, url, "after {body}");
    }

    let res = update(
        &who,
        id,
        json!({"original_url": "https://iana.org/elsewhere"}),
    )
    .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(media_rows().await, 0, "a new destination ends the page");
}

/// Saving with the same destination is not a new expiry either: the
/// reminder that was sent stays sent.
#[tokio::test]
async fn the_expiry_reminder_stamp_survives_a_save_with_the_same_destination() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let typed = format!("https:iana.org/{}", path());
    let (id, _) = old_row(&db, &who, &typed).await;
    let stamp = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);
    let mut active: links::ActiveModel = row(&db, id).await.into();
    active.expires_at = Set(Some(
        chrono::Utc::now().naive_utc() + chrono::Duration::days(2),
    ));
    active.expiry_notice_sent_at = Set(Some(stamp));
    active.update(&db).await.unwrap();

    let res = update(
        &who,
        id,
        json!({"title": "Renamed", "original_url": typed.replace("https:", "https://")}),
    )
    .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    let after = row(&db, id).await;
    assert!(after.expiry_notice_sent_at.is_some(), "the stamp is kept");
    assert_eq!(after.original_url, typed);
}

// ----------------------------------------------------------------- throttle

/// Ten of one URL in ten minutes, however it is typed: what is counted is
/// what is stored, and the three spellings are stored as one.
#[tokio::test]
async fn the_same_url_throttle_counts_one_destination_typed_three_ways_as_one() {
    let db = common::setup_test_db().await;
    let who = person(&db).await;
    let path = path();
    let spellings = [
        format!("https://iana.org/{path}"),
        format!("https:iana.org/{path}"),
        format!("https:/iana.org/{path}"),
    ];

    for n in 0..10 {
        created(&who, &spellings[n % 3]).await;
    }
    for typed in &spellings {
        let res = create(&who, json!({"original_url": typed})).await;
        assert_eq!(res.status_code(), 429, "{typed:?}: {}", res.text());
    }
    // Another address is not held back.
    created(&who, &format!("https:iana.org/{path}/other")).await;
}
