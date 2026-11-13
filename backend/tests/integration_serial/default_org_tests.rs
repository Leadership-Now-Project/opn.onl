//! The shared default organization (`DEFAULT_ORG_NAME`, `DEFAULT_ORG_SLUG`,
//! `DEFAULT_ORG_ADMINS`; `src/handlers/default_org.rs`).
//!
//! Every account that signs in through a verified-email provider joins it, its
//! personal links move in, and new links are created there. Who may then read,
//! change or delete a link is the ordinary organization rule, checked here
//! through the real router for two colleagues, an admin and an outsider.
//!
//! The settings are process environment, so each test takes the serial guard.
//! Each also uses a slug of its own: its organization is one no other test has
//! touched, whatever else is in the shared database.

use crate::common;

use axum::response::Response;
use common::{set_env, spawn_real_app, unique_code, unique_email};
use opn_onl_backend::AppState;
use opn_onl_backend::entity::{folders, link_tags, links, org_members, organizations, tags, users};
use opn_onl_backend::handlers::verified_sign_in::sign_in_verified_email;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, PaginatorTrait, QueryFilter,
};
use serde_json::{Value, json};

const ORG_NAME: &str = "Default Org Under Test";

// ---------------------------------------------------------------- fixtures

/// Switch the feature on for this test, with a slug nobody else uses.
fn configure(admins: &str) -> String {
    let slug = format!("dflt-{}", uuid::Uuid::new_v4().simple());
    set_env("DEFAULT_ORG_NAME", ORG_NAME);
    set_env("DEFAULT_ORG_SLUG", &slug);
    set_env("DEFAULT_ORG_ADMINS", admins);
    slug
}

struct Session {
    id: i32,
    email: String,
    token: String,
}

async fn body_of(response: Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("JSON body"))
}

/// What `/auth/google` and `/auth/cf-access` do once the provider has vouched
/// for `email`.
async fn sign_in(db: &DatabaseConnection, email: &str) -> Session {
    let state = AppState::for_tests(db.clone()).await;
    sign_in_with(&state, email).await
}

/// [`sign_in`] on an app state the caller already has.
async fn sign_in_with(state: &AppState, email: &str) -> Session {
    let (status, body) = body_of(sign_in_verified_email(state, email, "test provider").await).await;
    assert_eq!(status, 200, "sign in {email}: {body}");
    Session {
        id: i32::try_from(body["user_id"].as_i64().unwrap()).unwrap(),
        email: email.to_string(),
        token: body["token"].as_str().unwrap().to_string(),
    }
}

/// An account that exists but has not signed in since the feature came on.
async fn account(db: &DatabaseConnection, email: &str) -> i32 {
    users::ActiveModel {
        email: Set(email.to_string()),
        password_hash: Set("not-a-real-hash".to_string()),
        created_at: Set(chrono::Utc::now().naive_utc()),
        email_verified: Set(true),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert user")
    .id
}

async fn org_by_slug(db: &DatabaseConnection, slug: &str) -> Option<organizations::Model> {
    organizations::Entity::find()
        .filter(organizations::Column::Slug.eq(slug))
        .one(db)
        .await
        .expect("query org")
}

async fn the_org(db: &DatabaseConnection, slug: &str) -> organizations::Model {
    org_by_slug(db, slug)
        .await
        .expect("the default organization exists")
}

async fn memberships(db: &DatabaseConnection, user_id: i32) -> Vec<org_members::Model> {
    org_members::Entity::find()
        .filter(org_members::Column::UserId.eq(user_id))
        .all(db)
        .await
        .expect("query memberships")
}

async fn role_in(db: &DatabaseConnection, org_id: i32, user_id: i32) -> Option<String> {
    org_members::Entity::find()
        .filter(org_members::Column::OrgId.eq(org_id))
        .filter(org_members::Column::UserId.eq(user_id))
        .one(db)
        .await
        .expect("query membership")
        .map(|member| member.role)
}

async fn set_role(db: &DatabaseConnection, org_id: i32, user_id: i32, role: &str) {
    let member = org_members::Entity::find()
        .filter(org_members::Column::OrgId.eq(org_id))
        .filter(org_members::Column::UserId.eq(user_id))
        .one(db)
        .await
        .unwrap()
        .expect("membership to change");
    let mut active: org_members::ActiveModel = member.into();
    active.role = Set(role.to_string());
    active.update(db).await.expect("set role");
}

async fn insert_link(db: &DatabaseConnection, creator: Option<i32>) -> links::Model {
    links::ActiveModel {
        code: Set(unique_code()),
        original_url: Set("https://iana.org/default-org".to_string()),
        user_id: Set(creator),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert link")
}

async fn link_row(db: &DatabaseConnection, id: i32) -> links::Model {
    links::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("link row exists")
}

/// A fresh app for each request: every app has its own rate limiters, and the
/// mock transport puts all callers in one per-second bucket.
async fn app() -> axum_test::TestServer {
    spawn_real_app().await.0
}

async fn create_link(token: &str, body: Value) -> Value {
    let res = app()
        .await
        .post("/links")
        .authorization_bearer(token)
        .json(&body)
        .await;
    assert_eq!(res.status_code(), 201, "create link: {}", res.text());
    res.json()
}

fn destination(tag: &str) -> String {
    format!(
        "https://iana.org/default-org-{tag}-{}",
        uuid::Uuid::new_v4().simple()
    )
}

/// `GET /links` with this query string: the ids listed, and the whole body.
async fn list(token: &str, query: &str) -> (Vec<i64>, Value) {
    let res = app()
        .await
        .get(&format!("/links?{query}"))
        .authorization_bearer(token)
        .await;
    assert_eq!(res.status_code(), 200, "list {query}: {}", res.text());
    let body: Value = res.json();
    let ids = body
        .as_array()
        .expect("a list")
        .iter()
        .map(|link| link["id"].as_i64().unwrap())
        .collect();
    (ids, body)
}

fn item(body: &Value, id: i64) -> &Value {
    body.as_array()
        .unwrap()
        .iter()
        .find(|link| link["id"] == id)
        .expect("link in the list")
}

// ------------------------------------------------- organization and roles

#[tokio::test]
async fn first_sign_in_creates_the_organization_and_the_member_joins_as_editor() {
    let _serial = common::serial_guard().await;
    let (server, db) = spawn_real_app().await;
    // The configured admin has an account but is not the one signing in.
    let boss_email = unique_email();
    let boss = account(&db, &boss_email).await;
    let slug = configure(&boss_email);
    assert!(org_by_slug(&db, &slug).await.is_none());
    let before: Value = server.get("/auth/settings").await.json();
    assert!(
        before.get("default_org").is_none(),
        "no organization to name yet: {before}"
    );

    let staff = sign_in(&db, &unique_email()).await;

    let org = the_org(&db, &slug).await;
    assert_eq!(org.name, ORG_NAME);
    assert_eq!(
        role_in(&db, org.id, staff.id).await.as_deref(),
        Some("editor")
    );
    // The organization answers to the configured admin, not to whoever
    // happened to sign in first.
    assert_eq!(org.owner_id, boss);
    assert_eq!(role_in(&db, org.id, boss).await.as_deref(), Some("owner"));

    // A colleague signing in later joins the same organization.
    let colleague = sign_in(&db, &unique_email()).await;
    assert_eq!(
        role_in(&db, org.id, colleague.id).await.as_deref(),
        Some("editor")
    );

    // The dashboard learns which organization to show.
    let settings: Value = server.get("/auth/settings").await.json();
    assert_eq!(
        settings["default_org"],
        json!({ "id": org.id, "name": ORG_NAME }),
        "{settings}"
    );
}

#[tokio::test]
async fn with_no_configured_admin_account_the_first_to_sign_in_owns_the_organization() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    // One listed address has no account, the other's account is disabled.
    let disabled_email = unique_email();
    let disabled = account(&db, &disabled_email).await;
    let mut row: users::ActiveModel = users::Entity::find_by_id(disabled)
        .one(&db)
        .await
        .unwrap()
        .unwrap()
        .into();
    row.disabled_at = Set(Some(chrono::Utc::now().naive_utc()));
    row.update(&db).await.unwrap();
    let slug = configure(&format!("{},{disabled_email}", unique_email()));

    let first = sign_in(&db, &unique_email()).await;

    let org = the_org(&db, &slug).await;
    assert_eq!(org.owner_id, first.id);
    assert_eq!(
        role_in(&db, org.id, first.id).await.as_deref(),
        Some("owner")
    );
    assert_eq!(role_in(&db, org.id, disabled).await, None);
}

#[tokio::test]
async fn an_address_listed_as_admin_becomes_admin_at_its_first_sign_in() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let admin_email = unique_email();
    // Written the way an operator might: other case, spaces, a stray comma.
    let slug = configure(&format!(
        " {} , {} ,",
        unique_email(),
        admin_email.to_uppercase()
    ));
    // Someone else creates the organization first, so the admin is not its owner.
    let first = sign_in(&db, &unique_email()).await;
    let org = the_org(&db, &slug).await;
    assert_eq!(org.owner_id, first.id);

    let admin = sign_in(&db, &admin_email).await;

    assert_eq!(
        role_in(&db, org.id, admin.id).await.as_deref(),
        Some("admin")
    );
}

#[tokio::test]
async fn an_existing_editor_listed_as_admin_is_promoted_at_the_next_sign_in() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let slug = configure("");
    sign_in(&db, &unique_email()).await;
    let org = the_org(&db, &slug).await;
    let email = unique_email();
    let staff = sign_in(&db, &email).await;
    assert_eq!(
        role_in(&db, org.id, staff.id).await.as_deref(),
        Some("editor")
    );

    set_env("DEFAULT_ORG_ADMINS", &email);
    // Being listed changes nothing until the account signs in again.
    assert_eq!(
        role_in(&db, org.id, staff.id).await.as_deref(),
        Some("editor")
    );
    sign_in(&db, &email).await;

    assert_eq!(
        role_in(&db, org.id, staff.id).await.as_deref(),
        Some("admin")
    );
}

#[tokio::test]
async fn signing_in_never_demotes_anyone() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let owner_email = unique_email();
    let admin_email = unique_email();
    let slug = configure(&format!("{owner_email},{admin_email}"));
    // The listed address that signs in first owns the organization.
    let owner = sign_in(&db, &owner_email).await;
    let org = the_org(&db, &slug).await;
    let admin = sign_in(&db, &admin_email).await;
    assert_eq!(
        role_in(&db, org.id, owner.id).await.as_deref(),
        Some("owner")
    );
    assert_eq!(
        role_in(&db, org.id, admin.id).await.as_deref(),
        Some("admin")
    );

    // An owner who is listed as admin stays owner.
    sign_in(&db, &owner_email).await;
    assert_eq!(
        role_in(&db, org.id, owner.id).await.as_deref(),
        Some("owner")
    );

    // Taken off the list, an admin stays admin: the list only ever promotes.
    set_env("DEFAULT_ORG_ADMINS", &owner_email);
    sign_in(&db, &admin_email).await;
    assert_eq!(
        role_in(&db, org.id, admin.id).await.as_deref(),
        Some("admin")
    );

    // An admin made by hand in the Teams page, never listed, stays admin too.
    let by_hand_email = unique_email();
    let by_hand = sign_in(&db, &by_hand_email).await;
    set_role(&db, org.id, by_hand.id, "admin").await;
    sign_in(&db, &by_hand_email).await;
    assert_eq!(
        role_in(&db, org.id, by_hand.id).await.as_deref(),
        Some("admin")
    );

    // And a viewer who is not listed is not quietly made an editor.
    let viewer_email = unique_email();
    let viewer = sign_in(&db, &viewer_email).await;
    set_role(&db, org.id, viewer.id, "viewer").await;
    sign_in(&db, &viewer_email).await;
    assert_eq!(
        role_in(&db, org.id, viewer.id).await.as_deref(),
        Some("viewer")
    );
}

#[tokio::test]
async fn a_second_sign_in_changes_nothing() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let slug = configure("");
    sign_in(&db, &unique_email()).await;
    let email = unique_email();
    let staff = sign_in(&db, &email).await;
    let org = the_org(&db, &slug).await;
    let link = create_link(
        &staff.token,
        json!({ "original_url": destination("again") }),
    )
    .await;
    let link_id = i32::try_from(link["id"].as_i64().unwrap()).unwrap();
    let member_before = memberships(&db, staff.id).await;
    let link_before = link_row(&db, link_id).await;

    let again = sign_in(&db, &email).await;

    assert_eq!(again.id, staff.id);
    assert_eq!(
        memberships(&db, staff.id).await,
        member_before,
        "one membership, same role"
    );
    assert_eq!(member_before.len(), 1);
    assert_eq!(member_before[0].org_id, org.id);
    assert_eq!(link_row(&db, link_id).await, link_before);
    let orgs_with_slug = organizations::Entity::find()
        .filter(organizations::Column::Slug.eq(slug.as_str()))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(orgs_with_slug, 1);
}

#[tokio::test]
async fn first_sign_ins_at_once_make_one_organization_with_everyone_in_it() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let slug = configure("");
    // Accounts that already exist, so each sign-in goes straight to the
    // organization step and they reach it together.
    let mut emails = Vec::new();
    for _ in 0..8 {
        let email = unique_email();
        account(&db, &email).await;
        emails.push(email);
    }

    // One app state for all of them (building one is slow enough to space the
    // sign-ins out), on a pool that already holds a connection for each: a
    // pool opens connections one at a time, and the sign-ins would otherwise
    // queue behind the first instead of racing it.
    let state = AppState::for_tests(db.clone()).await;
    let opened =
        futures::future::join_all((0..8).map(|_| db.execute_unprepared("SELECT pg_sleep(0.2)")))
            .await;
    assert!(opened.iter().all(Result::is_ok), "open the connections");
    let sessions =
        futures::future::join_all(emails.iter().map(|email| sign_in_with(&state, email))).await;

    let orgs = organizations::Entity::find()
        .filter(organizations::Column::Slug.eq(slug.as_str()))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(orgs.len(), 1);
    let mut roles = Vec::new();
    for session in &sessions {
        // A sign-in that lost the race to create it still joined.
        roles.push(
            role_in(&db, orgs[0].id, session.id)
                .await
                .expect("every account that signed in is a member"),
        );
    }
    roles.sort();
    let mut expected = vec!["editor"; 7];
    expected.push("owner");
    assert_eq!(roles, expected);
}

// ------------------------------------------------------- personal links

#[tokio::test]
async fn personal_links_move_into_the_organization_once_and_a_deleted_one_does_not() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let slug = configure("");
    sign_in(&db, &unique_email()).await;
    let org = the_org(&db, &slug).await;

    // An account from before the feature, with links, a folder and a tag.
    let email = unique_email();
    let staff = account(&db, &email).await;
    let live = insert_link(&db, Some(staff)).await;
    let filed = insert_link(&db, Some(staff)).await;
    let deleted = insert_link(&db, Some(staff)).await;
    let mut gone: links::ActiveModel = deleted.clone().into();
    gone.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    gone.update(&db).await.unwrap();
    let folder = folders::ActiveModel {
        name: Set("Campaigns".to_string()),
        user_id: Set(Some(staff)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    let mut in_folder: links::ActiveModel = filed.clone().into();
    in_folder.folder_id = Set(Some(folder.id));
    in_folder.update(&db).await.unwrap();
    let tag = tags::ActiveModel {
        name: Set("press".to_string()),
        user_id: Set(Some(staff)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    link_tags::ActiveModel {
        link_id: Set(filed.id),
        tag_id: Set(tag.id),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    // Not theirs: a stranger's personal link and an anonymous one.
    let stranger = account(&db, &unique_email()).await;
    let strangers_link = insert_link(&db, Some(stranger)).await;
    let anonymous = insert_link(&db, None).await;

    let session = sign_in(&db, &email).await;
    assert_eq!(session.id, staff);

    assert_eq!(link_row(&db, live.id).await.org_id, Some(org.id));
    let moved = link_row(&db, filed.id).await;
    assert_eq!(moved.org_id, Some(org.id));
    // Still theirs, still in its folder, still tagged.
    assert_eq!(moved.user_id, Some(staff));
    assert_eq!(moved.folder_id, Some(folder.id));
    let folder_now = folders::Entity::find_by_id(folder.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (folder_now.org_id, folder_now.user_id),
        (Some(org.id), None)
    );
    let tag_now = tags::Entity::find_by_id(tag.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((tag_now.org_id, tag_now.user_id), (Some(org.id), None));
    // A soft-deleted link is left where it was.
    let still_deleted = link_row(&db, deleted.id).await;
    assert_eq!(still_deleted.org_id, None);
    assert!(still_deleted.deleted_at.is_some());
    // Nobody else's links moved.
    assert_eq!(link_row(&db, strangers_link.id).await.org_id, None);
    assert_eq!(link_row(&db, anonymous.id).await.org_id, None);

    // The moved link is usable as an organization link: its folder filter
    // and its tag still find it, for a colleague too.
    let colleague = sign_in(&db, &unique_email()).await;
    let (ids, body) = list(
        &colleague.token,
        &format!("org_id={}&folder_id={}", org.id, folder.id),
    )
    .await;
    assert_eq!(ids, [i64::from(filed.id)]);
    assert_eq!(item(&body, i64::from(filed.id))["tags"][0]["name"], "press");

    // A later sign-in finds nothing left to move and changes nothing.
    let before = link_row(&db, live.id).await;
    sign_in(&db, &email).await;
    assert_eq!(link_row(&db, live.id).await, before);
    assert_eq!(link_row(&db, deleted.id).await.org_id, None);
}

#[tokio::test]
async fn a_viewer_keeps_their_personal_links() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let slug = configure("");
    sign_in(&db, &unique_email()).await;
    let org = the_org(&db, &slug).await;
    let email = unique_email();
    let viewer = sign_in(&db, &email).await;
    set_role(&db, org.id, viewer.id, "viewer").await;
    let personal = insert_link(&db, Some(viewer.id)).await;

    sign_in(&db, &email).await;

    // In the organization a viewer could no longer change it.
    assert_eq!(link_row(&db, personal.id).await.org_id, None);
    let created = create_link(
        &viewer.token,
        json!({ "original_url": destination("viewer") }),
    )
    .await;
    assert_eq!(created["org_id"], Value::Null);
}

// ------------------------------------------------------------ new links

#[tokio::test]
async fn a_members_new_links_land_in_the_organization() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let slug = configure("");
    let staff = sign_in(&db, &unique_email()).await;
    let org = the_org(&db, &slug).await;

    // No org_id in the request: what the connector and an API key send.
    let created = create_link(
        &staff.token,
        json!({ "original_url": destination("single") }),
    )
    .await;
    assert_eq!(created["org_id"], org.id);
    let id = i32::try_from(created["id"].as_i64().unwrap()).unwrap();
    let row = link_row(&db, id).await;
    assert_eq!((row.org_id, row.user_id), (Some(org.id), Some(staff.id)));

    let bulk = app()
        .await
        .post("/links/bulk")
        .authorization_bearer(&staff.token)
        .json(&json!({ "urls": [destination("bulk-a"), destination("bulk-b")] }))
        .await;
    assert_eq!(bulk.status_code(), 200, "bulk create: {}", bulk.text());
    let bulk: Value = bulk.json();
    let codes: Vec<&str> = bulk["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| link["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes.len(), 2, "{bulk}");
    for code in codes {
        let row = links::Entity::find()
            .filter(links::Column::Code.eq(code))
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.org_id, Some(org.id), "bulk link {code}");
    }

    // An account that is not a member (it never signed in this way) still
    // makes personal links.
    let outsider = account(&db, &unique_email()).await;
    let outsider_email = users::Entity::find_by_id(outsider)
        .one(&db)
        .await
        .unwrap()
        .unwrap()
        .email;
    let token = opn_onl_backend::utils::create_jwt(outsider, &outsider_email, 0).unwrap();
    let personal = create_link(&token, json!({ "original_url": destination("outsider") })).await;
    assert_eq!(personal["org_id"], Value::Null);
}

/// POST `path` with `body` and return the JSON of the 201.
async fn create(token: &str, path: &str, body: Value) -> Value {
    let res = app()
        .await
        .post(path)
        .authorization_bearer(token)
        .json(&body)
        .await;
    assert_eq!(res.status_code(), 201, "POST {path}: {}", res.text());
    res.json()
}

/// The names `GET path` lists.
async fn names(token: &str, path: &str) -> Vec<String> {
    let res = app().await.get(path).authorization_bearer(token).await;
    assert_eq!(res.status_code(), 200, "GET {path}: {}", res.text());
    res.json::<Value>()
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_client_that_never_names_the_organization_keeps_its_folders_and_tags() {
    // The Claude connector and API keys send no org_id. A member's links are
    // in the organization, so their folders and tags have to be found and
    // made there too, or a new link could not be filed or tagged.
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let slug = configure("");
    let staff = sign_in(&db, &unique_email()).await;
    let org = the_org(&db, &slug).await;
    let colleague = sign_in(&db, &unique_email()).await;

    let folder = create(&staff.token, "/folders", json!({ "name": "Events" })).await;
    assert_eq!(folder["org_id"], org.id, "{folder}");
    assert_eq!(folder["user_id"], Value::Null);
    let tag = create(&staff.token, "/tags", json!({ "name": "gala" })).await;
    assert_eq!(tag["org_id"], org.id, "{tag}");
    assert_eq!(tag["user_id"], Value::Null);

    // Listed without an org_id, by the member and by a colleague alike.
    for who in [&staff, &colleague] {
        assert_eq!(names(&who.token, "/folders").await, ["Events"]);
        assert_eq!(names(&who.token, "/tags").await, ["gala"]);
    }

    // And a link made the same way can use them.
    let link = create_link(
        &staff.token,
        json!({
            "original_url": destination("filed"),
            "folder_id": folder["id"],
            "tag_ids": [tag["id"]],
        }),
    )
    .await;
    assert_eq!(link["org_id"], org.id);
    assert_eq!(link["folder_id"], folder["id"]);
    assert_eq!(link["tags"][0]["name"], "gala");

    // An account outside the organization is as it was: personal folders and
    // tags, and none of the organization's.
    let outsider_email = unique_email();
    let outsider_id = account(&db, &outsider_email).await;
    let outsider = opn_onl_backend::utils::create_jwt(outsider_id, &outsider_email, 0).unwrap();
    let own_folder = create(&outsider, "/folders", json!({ "name": "Mine alone" })).await;
    assert_eq!(own_folder["org_id"], Value::Null);
    assert_eq!(own_folder["user_id"], outsider_id);
    let own_tag = create(&outsider, "/tags", json!({ "name": "solo" })).await;
    assert_eq!(own_tag["org_id"], Value::Null);
    assert_eq!(names(&outsider, "/folders").await, ["Mine alone"]);
    assert_eq!(names(&outsider, "/tags").await, ["solo"]);
}

// ------------------------------------------- colleagues, admins, outsiders

#[tokio::test]
async fn colleagues_see_and_edit_each_others_links_but_only_the_creator_and_admins_delete() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let admin_email = unique_email();
    let slug = configure(&admin_email);
    sign_in(&db, &admin_email).await;
    let org = the_org(&db, &slug).await;
    // A second listed admin, so the delete below is by an admin, not the owner.
    let second_admin_email = unique_email();
    set_env(
        "DEFAULT_ORG_ADMINS",
        format!("{admin_email},{second_admin_email}"),
    );
    let second_admin = sign_in(&db, &second_admin_email).await;
    assert_eq!(
        role_in(&db, org.id, second_admin.id).await.as_deref(),
        Some("admin")
    );
    let a = sign_in(&db, &unique_email()).await;
    let b = sign_in(&db, &unique_email()).await;
    // An account outside the organization.
    let outsider_email = unique_email();
    let outsider_id = account(&db, &outsider_email).await;
    let outsider = opn_onl_backend::utils::create_jwt(outsider_id, &outsider_email, 0).unwrap();

    let a_link = create_link(&a.token, json!({ "original_url": destination("a") })).await;
    let a_id = a_link["id"].as_i64().unwrap();
    let b_link = create_link(&b.token, json!({ "original_url": destination("b") })).await;
    let b_id = b_link["id"].as_i64().unwrap();
    let all = format!("org_id={}", org.id);
    let mine = format!("org_id={}&mine=true", org.id);

    // All LNP Links: B sees A's link, who made it, and that it is not B's to delete.
    let (ids, body) = list(&b.token, &all).await;
    assert!(ids.contains(&a_id) && ids.contains(&b_id), "{ids:?}");
    let seen = item(&body, a_id);
    assert_eq!(
        seen["created_by"],
        json!({ "name": null, "email": a.email })
    );
    assert_eq!(seen["can_delete"], false);
    assert_eq!(item(&body, b_id)["can_delete"], true);
    // My Links: only B's own.
    let (ids, _) = list(&b.token, &mine).await;
    assert_eq!(ids, [b_id]);
    let (ids, body) = list(&a.token, &mine).await;
    assert_eq!(ids, [a_id]);
    assert_eq!(item(&body, a_id)["can_delete"], true);
    // An admin may delete either.
    let (_, body) = list(&second_admin.token, &all).await;
    assert_eq!(item(&body, a_id)["can_delete"], true);
    assert_eq!(item(&body, b_id)["can_delete"], true);

    // B edits A's link.
    let edited = app()
        .await
        .put(&format!("/links/{a_id}"))
        .authorization_bearer(&b.token)
        .json(&json!({ "title": "Edited by a colleague" }))
        .await;
    assert_eq!(edited.status_code(), 200, "{}", edited.text());
    let row = link_row(&db, a_id as i32).await;
    assert_eq!(row.title.as_deref(), Some("Edited by a colleague"));
    // Editing does not take the link over.
    assert_eq!(row.user_id, Some(a.id));

    // B cannot delete it.
    let refused = app()
        .await
        .delete(&format!("/links/{a_id}"))
        .authorization_bearer(&b.token)
        .await;
    assert_eq!(refused.status_code(), 403, "{}", refused.text());
    assert_eq!(
        refused.json::<Value>()["error"],
        "You don't have permission to delete this link"
    );
    assert!(link_row(&db, a_id as i32).await.deleted_at.is_none());

    // An outsider gets the same 403 for the id as for one that does not exist.
    for (method, expected) in [
        ("GET", "You don't have permission to access this link"),
        ("PUT", "You don't have permission to modify this link"),
        ("DELETE", "You don't have permission to delete this link"),
    ] {
        let server = app().await;
        let path = format!("/links/{a_id}");
        let request = match method {
            "GET" => server.get(&path),
            "PUT" => server.put(&path).json(&json!({ "title": "taken" })),
            _ => server.delete(&path),
        };
        let res = request.authorization_bearer(&outsider).await;
        assert_eq!(res.status_code(), 403, "{method}: {}", res.text());
        assert_eq!(res.json::<Value>()["error"], expected, "{method}");
    }
    let (ids, _) = list(&outsider, &all).await;
    assert!(ids.is_empty(), "an outsider lists nothing: {ids:?}");
    assert!(link_row(&db, a_id as i32).await.deleted_at.is_none());
    assert_eq!(
        link_row(&db, a_id as i32).await.title.as_deref(),
        Some("Edited by a colleague")
    );

    // An organization admin deletes it.
    let deleted = app()
        .await
        .delete(&format!("/links/{a_id}"))
        .authorization_bearer(&second_admin.token)
        .await;
    assert_eq!(deleted.status_code(), 200, "{}", deleted.text());
    assert!(link_row(&db, a_id as i32).await.deleted_at.is_some());

    // And a creator deletes their own.
    let own = app()
        .await
        .delete(&format!("/links/{b_id}"))
        .authorization_bearer(&b.token)
        .await;
    assert_eq!(own.status_code(), 200, "{}", own.text());
}

#[tokio::test]
async fn the_list_names_the_creator_by_display_name_when_there_is_one() {
    let _serial = common::serial_guard().await;
    let (_server, db) = spawn_real_app().await;
    let slug = configure("");
    let a = sign_in(&db, &unique_email()).await;
    let org = the_org(&db, &slug).await;
    let b = sign_in(&db, &unique_email()).await;
    let named = app()
        .await
        .put("/auth/profile")
        .authorization_bearer(&a.token)
        .json(&json!({ "display_name": "Ada Lovelace" }))
        .await;
    assert_eq!(named.status_code(), 200, "{}", named.text());
    let link = create_link(&a.token, json!({ "original_url": destination("named") })).await;
    let id = link["id"].as_i64().unwrap();

    let (_, body) = list(&b.token, &format!("org_id={}", org.id)).await;

    let creator = &item(&body, id)["created_by"];
    assert_eq!(
        *creator,
        json!({ "name": "Ada Lovelace", "email": a.email })
    );
    // Name and email, nothing else about the account.
    assert_eq!(creator.as_object().unwrap().len(), 2);
}

// ------------------------------------------------------------ switched off

#[tokio::test]
async fn a_blank_or_unusable_setting_leaves_the_feature_off() {
    let _serial = common::serial_guard().await;
    let (server, db) = spawn_real_app().await;

    for (name, slug) in [
        ("", "some-slug"),
        ("   ", "some-slug"),
        ("A Name", ""),
        ("A Name", "   "),
        // Not a slug the Teams page would accept.
        ("A Name", "Not A Slug"),
    ] {
        set_env("DEFAULT_ORG_NAME", name);
        set_env("DEFAULT_ORG_SLUG", slug);
        set_env("DEFAULT_ORG_ADMINS", "someone@example.org");

        let staff = sign_in(&db, &unique_email()).await;

        assert!(
            memberships(&db, staff.id).await.is_empty(),
            "name {name:?}, slug {slug:?}"
        );
        let settings: Value = server.get("/auth/settings").await.json();
        assert!(settings.get("default_org").is_none(), "{settings}");
    }
}
