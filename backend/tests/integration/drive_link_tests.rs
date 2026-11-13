//! Google Drive links: a short link made for a file picked in Google Drive.
//!
//! Real router, real Postgres. Nothing here talks to Google: the browser does
//! that, and the server only stores which Drive file a link was made for. The
//! picker settings are handed to the app through `AppState`, so nothing in
//! this file touches the environment. What these tests pin:
//!
//! * `GET /auth/settings` names the picker only when it is configured;
//! * a `drive_file` is stored only with a destination in Google Drive and a
//!   well-formed file id, and a refused request creates no link;
//! * every answer that shows the link shows `drive`, to colleagues too, and
//!   never the file id;
//! * the row goes when the destination changes or the link is deleted;
//! * a link created without `drive_file` is what it always was.

use crate::common;

use axum::http::header::LOCATION;
use common::{spawn_real_app, unique_code, unique_email};
use opn_onl_backend::entity::{link_drive_files, links, org_members, organizations, users};
use opn_onl_backend::utils::create_jwt;
use opn_onl_backend::utils::drive::DrivePicker;
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

/// A Drive address no other test uses, so the per-account "same URL" throttle
/// and lookups by URL never meet another test's rows.
fn doc_url() -> (String, String) {
    let file_id = format!("1{}_-x", unique_code());
    (
        format!("https://docs.google.com/document/d/{file_id}/edit?usp=drivesdk"),
        file_id,
    )
}

fn drive_file(file_id: &str) -> Value {
    json!({
        "file_id": file_id,
        "name": "Board plan.gdoc",
        "mime_type": "application/vnd.google-apps.document",
    })
}

async fn create(app: &axum_test::TestServer, who: &Person, body: Value) -> axum_test::TestResponse {
    app.post("/links")
        .authorization_bearer(&who.token)
        .json(&body)
        .await
}

/// Create a Drive link and return the API's answer and the file id.
async fn drive_link(app: &axum_test::TestServer, who: &Person, extra: Value) -> (Value, String) {
    let (url, file_id) = doc_url();
    let mut body = json!({"original_url": url, "drive_file": drive_file(&file_id)});
    for (key, value) in extra.as_object().cloned().unwrap_or_default() {
        body[key] = value;
    }
    let res = create(app, who, body).await;
    assert_eq!(res.status_code(), 201, "create: {}", res.text());
    (res.json(), file_id)
}

async fn rows_for(db: &DatabaseConnection, link_id: i64) -> Vec<link_drive_files::Model> {
    link_drive_files::Entity::find()
        .filter(link_drive_files::Column::LinkId.eq(link_id as i32))
        .all(db)
        .await
        .unwrap()
}

async fn links_of(db: &DatabaseConnection, who: &Person) -> u64 {
    links::Entity::find()
        .filter(links::Column::UserId.eq(who.id))
        .count(db)
        .await
        .unwrap()
}

// ---------------------------------------------------------------- settings

#[tokio::test]
async fn settings_name_the_picker_only_when_it_is_configured() {
    let (plain, db) = spawn_real_app().await;
    let settings: Value = plain.get("/auth/settings").await.json();
    assert!(
        settings.get("drive").is_none(),
        "an instance without the picker settings says nothing about Drive: {settings}"
    );

    let mut state = opn_onl_backend::AppState::for_tests(db).await;
    state.drive = Some(DrivePicker {
        api_key: "browser-key".to_string(),
        app_id: "1234567890".to_string(),
    });
    let configured =
        axum_test::TestServer::new(opn_onl_backend::build_router(state)).expect("test server");
    let settings: Value = configured.get("/auth/settings").await.json();
    assert_eq!(
        settings["drive"],
        json!({"api_key": "browser-key", "app_id": "1234567890"})
    );
}

// ------------------------------------------------------------------ create

#[tokio::test]
async fn a_drive_link_stores_its_file_and_every_answer_shows_it() {
    let (app, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let (created, file_id) = drive_link(&app, &owner, json!({"title": "Board plan"})).await;
    let id = created["id"].as_i64().unwrap();
    let shown = json!({
        "name": "Board plan.gdoc",
        "mime_type": "application/vnd.google-apps.document",
    });
    assert_eq!(created["drive"], shown);
    assert!(created.get("file").is_none());

    let rows = rows_for(&db, id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].file_id, file_id);
    assert_eq!(rows[0].name, "Board plan.gdoc");
    assert_eq!(rows[0].created_by, Some(owner.id));

    let got: Value = app
        .get(&format!("/links/{id}"))
        .authorization_bearer(&owner.token)
        .await
        .json();
    assert_eq!(got["drive"], shown);
    assert!(
        got["original_url"]
            .as_str()
            .unwrap()
            .starts_with("https://docs.google.com/document/d/"),
        "the destination is the Drive address, shown as for any link"
    );

    let listed: Vec<Value> = app
        .get("/links")
        .authorization_bearer(&owner.token)
        .await
        .json();
    let item = listed.iter().find(|item| item["id"] == json!(id)).unwrap();
    assert_eq!(item["drive"], shown);
    // The file id is for the server; no answer carries it apart from the URL.
    assert!(item["drive"].get("file_id").is_none());

    // An ordinary redirect to where the file lives.
    let visit = app
        .get(&format!("/{}", created["code"].as_str().unwrap()))
        .await;
    assert!(visit.status_code().is_redirection());
    assert_eq!(
        visit.header(LOCATION).to_str().unwrap(),
        got["original_url"].as_str().unwrap()
    );
}

#[tokio::test]
async fn the_folder_and_tag_lists_show_the_drive_file() {
    let (app, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let (created, _) = drive_link(&app, &owner, json!({})).await;
    let id = created["id"].as_i64().unwrap();

    let tag: Value = app
        .post("/tags")
        .authorization_bearer(&owner.token)
        .json(&json!({"name": format!("t-{}", unique_code())}))
        .await
        .json();
    let folder: Value = app
        .post("/folders")
        .authorization_bearer(&owner.token)
        .json(&json!({"name": format!("f-{}", unique_code())}))
        .await
        .json();
    let (tag_id, folder_id) = (tag["id"].as_i64().unwrap(), folder["id"].as_i64().unwrap());
    let tagged = app
        .post(&format!("/links/{id}/tags"))
        .authorization_bearer(&owner.token)
        .json(&json!({"tag_ids": [tag_id]}))
        .await;
    assert!(tagged.status_code().is_success(), "{}", tagged.text());
    // Moving the link changes no destination, so the row stays.
    let moved = app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&owner.token)
        .json(&json!({"folder_id": folder_id}))
        .await;
    assert_eq!(moved.status_code(), 200, "{}", moved.text());
    assert_eq!(moved.json::<Value>()["drive"]["name"], "Board plan.gdoc");

    for path in [
        format!("/folders/{folder_id}/links"),
        format!("/tags/{tag_id}/links"),
    ] {
        let items: Vec<Value> = app
            .get(&path)
            .authorization_bearer(&owner.token)
            .await
            .json();
        let item = items
            .iter()
            .find(|item| item["id"] == json!(id))
            .unwrap_or_else(|| panic!("{path} lists the link"));
        assert_eq!(item["drive"]["name"], "Board plan.gdoc", "{path}");
    }
}

#[tokio::test]
async fn a_drive_file_needs_a_destination_in_google_drive() {
    let (app, db) = spawn_real_app().await;
    let owner = person(&db).await;
    for url in [
        "https://iana.org/not-drive",
        "http://drive.google.com/file/d/abc/view",
        "https://drive.google.com.iana.org/file/d/abc/view",
        "https://sites.google.com/view/abc",
    ] {
        let res = create(
            &app,
            &owner,
            json!({"original_url": url, "drive_file": drive_file("abc123")}),
        )
        .await;
        assert_eq!(res.status_code(), 400, "{url}: {}", res.text());
        assert!(
            res.json::<Value>()["error"]
                .as_str()
                .unwrap()
                .contains("Google Drive"),
            "{url}"
        );
    }
    assert_eq!(
        links_of(&db, &owner).await,
        0,
        "a refused request makes no link"
    );
    assert_eq!(
        link_drive_files::Entity::find()
            .filter(link_drive_files::Column::CreatedBy.eq(owner.id))
            .count(&db)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn a_bad_file_reference_is_refused_and_makes_no_link() {
    let (app, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let (url, _) = doc_url();
    let long_id = "a".repeat(201);
    let long_name = "n".repeat(501);
    for (file_id, name) in [
        ("", "Plan"),
        ("abc/../def", "Plan"),
        ("abc def", "Plan"),
        ("abc'--", "Plan"),
        (long_id.as_str(), "Plan"),
        ("abc123", ""),
        ("abc123", long_name.as_str()),
    ] {
        let res = create(
            &app,
            &owner,
            json!({
                "original_url": url,
                "drive_file": {"file_id": file_id, "name": name, "mime_type": "application/pdf"},
            }),
        )
        .await;
        assert_eq!(
            res.status_code(),
            400,
            "{file_id:?} {name:?}: {}",
            res.text()
        );
        assert!(res.json::<Value>()["error"].is_string());
    }
    assert_eq!(links_of(&db, &owner).await, 0);
}

// ------------------------------------------------------------ organization

#[tokio::test]
async fn an_org_colleague_sees_the_drive_file_and_an_outsider_sees_nothing() {
    let (app, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let viewer = person(&db).await;
    let outsider = person(&db).await;
    let org = organizations::ActiveModel {
        name: Set("Team".to_string()),
        slug: Set(format!("team-{}", unique_code().to_lowercase())),
        owner_id: Set(owner.id),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    for (who, role) in [(&owner, "owner"), (&viewer, "viewer")] {
        org_members::ActiveModel {
            org_id: Set(org.id),
            user_id: Set(who.id),
            role: Set(role.to_string()),
            ..Default::default()
        }
        .insert(&db)
        .await
        .unwrap();
    }
    let (created, _) = drive_link(&app, &owner, json!({"org_id": org.id})).await;
    let id = created["id"].as_i64().unwrap();

    let items: Vec<Value> = app
        .get(&format!("/links?org_id={}", org.id))
        .authorization_bearer(&viewer.token)
        .await
        .json();
    let item = items.iter().find(|item| item["id"] == json!(id)).unwrap();
    assert_eq!(item["drive"]["name"], "Board plan.gdoc");
    let got = app
        .get(&format!("/links/{id}"))
        .authorization_bearer(&viewer.token)
        .await;
    assert_eq!(got.status_code(), 200);
    assert_eq!(got.json::<Value>()["drive"]["name"], "Board plan.gdoc");

    let denied = app
        .get(&format!("/links/{id}"))
        .authorization_bearer(&outsider.token)
        .await;
    assert_eq!(denied.status_code(), 403);
    assert!(!denied.text().contains("Board plan"));
}

// ------------------------------------------------------ edit and delete

#[tokio::test]
async fn a_new_destination_forgets_the_drive_file_and_the_same_one_keeps_it() {
    let (app, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let (created, _) = drive_link(&app, &owner, json!({})).await;
    let id = created["id"].as_i64().unwrap();
    let url = created["original_url"].as_str().unwrap().to_string();

    // The edit dialog sends the destination back with every save.
    let same = app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&owner.token)
        .json(&json!({"original_url": url, "title": "Renamed"}))
        .await;
    assert_eq!(same.status_code(), 200, "{}", same.text());
    assert_eq!(same.json::<Value>()["drive"]["name"], "Board plan.gdoc");
    assert_eq!(rows_for(&db, id).await.len(), 1);

    // Another file in Drive is still another destination.
    let (other_url, _) = doc_url();
    let changed = app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&owner.token)
        .json(&json!({"original_url": other_url}))
        .await;
    assert_eq!(changed.status_code(), 200, "{}", changed.text());
    assert!(changed.json::<Value>().get("drive").is_none());
    assert!(rows_for(&db, id).await.is_empty());
    let got: Value = app
        .get(&format!("/links/{id}"))
        .authorization_bearer(&owner.token)
        .await
        .json();
    assert!(got.get("drive").is_none());
}

#[tokio::test]
async fn deleting_the_link_leaves_nothing_behind() {
    let (app, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let (one, _) = drive_link(&app, &owner, json!({})).await;
    let (two, _) = drive_link(&app, &owner, json!({})).await;
    let (kept, _) = drive_link(&app, &owner, json!({})).await;
    let (one, two, kept) = (
        one["id"].as_i64().unwrap(),
        two["id"].as_i64().unwrap(),
        kept["id"].as_i64().unwrap(),
    );

    let deleted = app
        .delete(&format!("/links/{one}"))
        .authorization_bearer(&owner.token)
        .await;
    assert_eq!(deleted.status_code(), 200, "{}", deleted.text());
    assert!(rows_for(&db, one).await.is_empty());

    let bulk = app
        .post("/links/bulk/delete")
        .authorization_bearer(&owner.token)
        .json(&json!({"ids": [two]}))
        .await;
    assert_eq!(bulk.status_code(), 200, "{}", bulk.text());
    assert!(rows_for(&db, two).await.is_empty());

    // Only the deleted links' rows went.
    assert_eq!(rows_for(&db, kept).await.len(), 1);

    // A link row that is removed outright takes its row with it.
    links::Entity::delete_by_id(kept as i32)
        .exec(&db)
        .await
        .unwrap();
    assert!(rows_for(&db, kept).await.is_empty());
}

// ------------------------------------------------------------- unchanged

#[tokio::test]
async fn a_link_without_a_drive_file_is_what_it_always_was() {
    let (app, db) = spawn_real_app().await;
    let owner = person(&db).await;

    // A Drive address typed into the ordinary form is an ordinary link.
    let (url, _) = doc_url();
    let res = create(&app, &owner, json!({"original_url": url})).await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let typed: Value = res.json();
    assert!(typed.get("drive").is_none());
    assert!(
        rows_for(&db, typed["id"].as_i64().unwrap())
            .await
            .is_empty()
    );

    let res = create(
        &app,
        &owner,
        json!({"original_url": format!("https://iana.org/{}", unique_code())}),
    )
    .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let plain: Value = res.json();
    assert!(plain.get("drive").is_none());
    assert!(plain.get("file").is_none());

    let listed: Vec<Value> = app
        .get("/links")
        .authorization_bearer(&owner.token)
        .await
        .json();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|item| item.get("drive").is_none()));
}
