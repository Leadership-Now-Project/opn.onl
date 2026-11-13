//! `POST /auth/google` while the feature is off, and the account step it shares
//! with `POST /auth/cf-access`.
//!
//! Google sign-in is configured from the environment once per process, and
//! this binary runs without it, so the endpoint itself can only be seen
//! switched off here. Its checks (code exchange, ID token, domains) are unit
//! tests in `src/handlers/google_auth.rs` against a local fake Google. What
//! both sign-in paths do once an address is verified is one function,
//! `sign_in_verified_email`, exercised below against the real database.

use crate::common;

use axum::response::Response;
use opn_onl_backend::AppState;
use opn_onl_backend::entity::users;
use opn_onl_backend::handlers::verified_sign_in::sign_in_verified_email;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::{Value, json};

async fn body_of(response: Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("JSON body"))
}

async fn sign_in(state: &AppState, email: &str) -> (u16, Value) {
    body_of(sign_in_verified_email(state, email, "test provider").await).await
}

async fn user(db: &sea_orm::DatabaseConnection, id: i32) -> users::Model {
    users::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("query")
        .expect("user")
}

#[tokio::test]
async fn google_sign_in_is_404_while_it_is_not_configured() {
    let (server, _) = common::spawn_real_app().await;

    let res = server
        .post("/auth/google")
        .add_header("X-Requested-With", "XmlHttpRequest")
        .json(&json!({ "code": "4/anything" }))
        .await;
    assert_eq!(res.status_code(), 404, "{}", res.text());
    assert_eq!(
        res.json::<Value>()["error"],
        "Google sign-in is not enabled"
    );

    // Off is off whatever the request looks like.
    let bare = server.post("/auth/google").await;
    assert_eq!(bare.status_code(), 404, "{}", bare.text());
}

#[tokio::test]
async fn settings_carry_no_google_client_id_while_it_is_not_configured() {
    let (server, _) = common::spawn_real_app().await;

    let settings: Value = server.get("/auth/settings").await.json();
    assert!(
        settings.get("google_client_id").is_none(),
        "the login page would offer a Google button: {settings}"
    );
    // The rest of the answer is still there.
    assert!(settings.get("passkeys_enabled").is_some());
}

#[tokio::test]
async fn a_verified_address_gets_an_account_once_and_the_same_one_after() {
    let (server, db) = common::spawn_real_app().await;
    let state = AppState::for_tests(db.clone()).await;
    let email = common::unique_email();

    let (status, first) = sign_in(&state, &email).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["email"], email);
    assert_eq!(first["email_verified"], true);
    let id = i32::try_from(first["user_id"].as_i64().unwrap()).unwrap();
    let created = user(&db, id).await;
    assert!(created.email_verified);
    assert!(created.deleted_at.is_none());

    // The session it answers with is a real one.
    let me = server
        .get("/auth/me")
        .authorization_bearer(first["token"].as_str().unwrap())
        .await;
    assert_eq!(me.status_code(), 200, "{}", me.text());
    assert_eq!(me.json::<Value>()["email"], email);

    let (status, second) = sign_in(&state, &email).await;
    assert_eq!(status, 200, "{second}");
    assert_eq!(second["user_id"], first["user_id"]);
    let accounts = users::Entity::find()
        .filter(users::Column::Email.eq(email.as_str()))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(accounts.len(), 1);
}

#[tokio::test]
async fn an_account_that_registered_with_a_password_is_matched_not_duplicated() {
    let (server, db) = common::spawn_real_app().await;
    let state = AppState::for_tests(db.clone()).await;
    let email = common::unique_email();

    let registered = server
        .post("/auth/register")
        .json(&json!({ "email": email, "password": "correct horse battery" }))
        .await;
    assert_eq!(registered.status_code(), 201, "{}", registered.text());
    let registered: Value = registered.json();

    let (status, signed_in) = sign_in(&state, &email).await;
    assert_eq!(status, 200, "{signed_in}");
    // Same account, so its links stay with it; and signing in this way does
    // not quietly mark an unverified address as verified.
    assert_eq!(signed_in["user_id"], registered["user_id"]);
    assert_eq!(signed_in["email_verified"], registered["email_verified"]);
}

#[tokio::test]
async fn a_deleted_account_is_not_revived() {
    let (_server, db) = common::spawn_real_app().await;
    let state = AppState::for_tests(db.clone()).await;
    let email = common::unique_email();

    let (_, first) = sign_in(&state, &email).await;
    let old_id = i32::try_from(first["user_id"].as_i64().unwrap()).unwrap();
    let mut gone: users::ActiveModel = user(&db, old_id).await.into();
    // What account deletion does: stamp the row; the address stays on it.
    gone.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    gone.update(&db).await.unwrap();

    let (status, again) = sign_in(&state, &email).await;
    assert_eq!(status, 200, "{again}");
    assert_ne!(again["user_id"], first["user_id"]);
    assert!(user(&db, old_id).await.deleted_at.is_some());
}

#[tokio::test]
async fn a_disabled_account_is_refused() {
    let (_server, db) = common::spawn_real_app().await;
    let state = AppState::for_tests(db.clone()).await;
    let email = common::unique_email();

    let (_, first) = sign_in(&state, &email).await;
    let id = i32::try_from(first["user_id"].as_i64().unwrap()).unwrap();
    let mut disabled: users::ActiveModel = user(&db, id).await.into();
    disabled.disabled_at = Set(Some(chrono::Utc::now().naive_utc()));
    disabled.update(&db).await.unwrap();

    let (status, refused) = sign_in(&state, &email).await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["error"], "This account is disabled");
    assert!(refused.get("token").is_none());
}
