//! `POST /auth/connector/google` and `POST /auth/connector/session`: the Claude
//! connector's sign-in (`src/handlers/connector_auth.rs`).
//!
//! The connector's settings are given through `AppState`
//! (`common::spawn_real_app_with_connector`), so these run against the real
//! router and database beside every other test. Google is a local stand-in
//! that serves one RSA key; the ID tokens are signed here with that key, which
//! is what Google's token endpoint would hand the connector.

use crate::common;

use std::sync::OnceLock;

use axum::{Json, Router, routing::get};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, jwk::JwkSet};
use openssl::rsa::Rsa;
use opn_onl_backend::AppState;
use opn_onl_backend::entity::{blocked_email_domains, users};
use opn_onl_backend::handlers::connector_auth::{ASSERTION_AUDIENCE, ConnectorAuth};
use opn_onl_backend::handlers::google_auth::GoogleAuthConfig;
use opn_onl_backend::handlers::verified_sign_in::sign_in_verified_email;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
use serde_json::{Value, json};

const CLIENT_ID: &str = "connector-test.apps.googleusercontent.com";
/// The domain of `common::unique_email()`.
const DOMAIN: &str = "users.opn.onl";
const KID: &str = "k1";
const NONCE: &str = "nonce-from-the-connector-0123456789";
const SECRET: &str = "connector-signin-secret-0123456789abcdef";

struct TestKey {
    encoding: EncodingKey,
    jwks: JwkSet,
}

fn generate_key() -> TestKey {
    let rsa = Rsa::generate(2048).unwrap();
    let jwks: JwkSet = serde_json::from_value(json!({ "keys": [{
        "kty": "RSA", "alg": "RS256", "use": "sig", "kid": KID,
        "n": URL_SAFE_NO_PAD.encode(rsa.n().to_vec()),
        "e": URL_SAFE_NO_PAD.encode(rsa.e().to_vec()),
    }]}))
    .unwrap();
    TestKey {
        encoding: EncodingKey::from_rsa_pem(&rsa.private_key_to_pem().unwrap()).unwrap(),
        jwks,
    }
}

/// "Google's" key for every test here.
fn google_key() -> &'static TestKey {
    static KEY: OnceLock<TestKey> = OnceLock::new();
    KEY.get_or_init(generate_key)
}

/// Serve Google's keys locally and return settings pointed at them.
async fn google_config() -> GoogleAuthConfig {
    let jwks = google_key().jwks.clone();
    let app = Router::new().route("/certs", get(move || async move { Json(jwks) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = GoogleAuthConfig::from_values(
        Some(CLIENT_ID.into()),
        Some("unused-client-secret".into()),
        Some(DOMAIN.into()),
        true,
    )
    .unwrap();
    config.jwks_url = format!("http://{addr}/certs");
    config
}

async fn app_with_renewal() -> (axum_test::TestServer, DatabaseConnection) {
    let connector = ConnectorAuth::new(google_config().await, Some(SECRET.into()), None);
    common::spawn_real_app_with_connector(connector).await
}

/// What Google would put in an ID token for a Workspace user at `email`,
/// answering a request that carried [`NONCE`].
fn claims(email: &str) -> Value {
    json!({
        "iss": "https://accounts.google.com",
        "aud": CLIENT_ID,
        "exp": chrono::Utc::now().timestamp() + 300,
        "sub": "1234567890",
        "email": email,
        "email_verified": true,
        "hd": DOMAIN,
        "nonce": NONCE,
    })
}

fn id_token_signed_by(key: &TestKey, claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.to_string());
    encode(&header, claims, &key.encoding).unwrap()
}

fn id_token(claims: &Value) -> String {
    id_token_signed_by(google_key(), claims)
}

async fn google_sign_in(server: &axum_test::TestServer, claims: &Value) -> (u16, Value) {
    let res = server
        .post("/auth/connector/google")
        .json(&json!({ "id_token": id_token(claims), "nonce": NONCE }))
        .await;
    (res.status_code().as_u16(), res.json())
}

/// A renewal assertion as the connector signs it.
fn assertion_with(secret: &str, claims: Value) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap()
}

fn assertion_claims(user_id: i64, token_version: i64) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "aud": ASSERTION_AUDIENCE,
        "sub": user_id.to_string(),
        "tv": token_version,
        "iat": now,
        "exp": now + 30,
    })
}

async fn renew(server: &axum_test::TestServer, assertion: &str) -> (u16, Value) {
    let res = server
        .post("/auth/connector/session")
        .json(&json!({ "assertion": assertion }))
        .await;
    (res.status_code().as_u16(), res.json())
}

/// Sign a fresh person in and return `(email, user id, token version)`.
async fn signed_in(server: &axum_test::TestServer) -> (String, i64, i64) {
    let email = common::unique_email();
    let (status, body) = google_sign_in(server, &claims(&email)).await;
    assert_eq!(status, 200, "{body}");
    (
        email,
        body["user_id"].as_i64().unwrap(),
        body["token_version"].as_i64().unwrap(),
    )
}

async fn user(db: &DatabaseConnection, id: i64) -> users::Model {
    users::Entity::find_by_id(i32::try_from(id).unwrap())
        .one(db)
        .await
        .expect("query")
        .expect("user")
}

async fn assert_session_is_for(server: &axum_test::TestServer, token: &str, email: &str) {
    let me = server.get("/auth/me").authorization_bearer(token).await;
    assert_eq!(me.status_code(), 200, "{}", me.text());
    assert_eq!(me.json::<Value>()["email"], email);
}

// --- switched off ---------------------------------------------------------

#[tokio::test]
async fn both_endpoints_are_404_while_google_sign_in_is_not_configured() {
    let (server, _) = common::spawn_real_app().await;

    let google = server
        .post("/auth/connector/google")
        .json(&json!({ "id_token": id_token(&claims(&common::unique_email())), "nonce": NONCE }))
        .await;
    assert_eq!(google.status_code(), 404, "{}", google.text());
    assert_eq!(
        google.json::<Value>()["error"],
        "Connector sign-in is not enabled"
    );

    let session = server
        .post("/auth/connector/session")
        .json(&json!({ "assertion": assertion_with(SECRET, assertion_claims(1, 0)) }))
        .await;
    assert_eq!(session.status_code(), 404, "{}", session.text());
}

#[tokio::test]
async fn renewal_is_404_without_a_shared_secret_and_sign_in_still_works() {
    for unusable in [None, Some("too-short".to_string())] {
        let connector = ConnectorAuth::new(google_config().await, unusable, None);
        let (server, _) = common::spawn_real_app_with_connector(connector).await;

        let (_, user_id, token_version) = signed_in(&server).await;
        // Signed with the secret the other tests use, and with the unusable one.
        for secret in [SECRET, "too-short"] {
            let assertion = assertion_with(secret, assertion_claims(user_id, token_version));
            let (status, body) = renew(&server, &assertion).await;
            assert_eq!(status, 404, "{body}");
            assert!(body.get("token").is_none());
        }
    }
}

#[tokio::test]
async fn renewal_stays_off_when_the_shared_secret_is_the_session_secret() {
    common::baseline_env();
    let jwt_secret = std::env::var("JWT_SECRET").expect("baseline JWT_SECRET");
    let connector = ConnectorAuth::new(
        google_config().await,
        Some(jwt_secret.clone()),
        Some(&jwt_secret),
    );
    let (server, _) = common::spawn_real_app_with_connector(connector).await;
    let (_, user_id, token_version) = signed_in(&server).await;

    let assertion = assertion_with(&jwt_secret, assertion_claims(user_id, token_version));
    let (status, body) = renew(&server, &assertion).await;
    assert_eq!(status, 404, "{body}");
}

// --- POST /auth/connector/google ------------------------------------------

#[tokio::test]
async fn a_good_id_token_signs_in_and_the_session_is_that_persons() {
    let (server, db) = app_with_renewal().await;
    let email = common::unique_email();

    let (status, body) = google_sign_in(&server, &claims(&email)).await;
    assert_eq!(status, 200, "{body}");
    let created = user(&db, body["user_id"].as_i64().unwrap()).await;
    assert_eq!(created.email, email);
    assert!(created.email_verified);
    assert_eq!(body["token_version"], created.token_version);
    assert_session_is_for(&server, body["token"].as_str().unwrap(), &email).await;
}

#[tokio::test]
async fn it_is_the_same_account_as_the_dashboards_sign_in_for_that_address() {
    let (server, db) = app_with_renewal().await;
    let state = AppState::for_tests(db.clone()).await;
    let email = common::unique_email();

    // The dashboard's `/auth/google` ends in this call once its token passed.
    let dashboard = sign_in_verified_email(&state, &email, "Google").await;
    assert_eq!(dashboard.status().as_u16(), 200);
    let bytes = axum::body::to_bytes(dashboard.into_body(), usize::MAX)
        .await
        .unwrap();
    let dashboard: Value = serde_json::from_slice(&bytes).unwrap();

    // Google reports the address in another case; it is still that account.
    let (status, connector) = google_sign_in(&server, &claims(&email.to_uppercase())).await;
    assert_eq!(status, 200, "{connector}");
    assert_eq!(connector["user_id"], dashboard["user_id"]);

    // And the other way round, for an account the connector created.
    let (email, user_id, _) = signed_in(&server).await;
    let dashboard = sign_in_verified_email(&state, &email, "Google").await;
    let bytes = axum::body::to_bytes(dashboard.into_body(), usize::MAX)
        .await
        .unwrap();
    let dashboard: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(dashboard["user_id"], user_id);
}

#[tokio::test]
async fn it_refuses_what_the_dashboards_google_sign_in_refuses() {
    let email = common::unique_email();
    let with = |claim: &str, value: Value| {
        let mut c = claims(&email);
        c[claim] = value;
        c
    };
    let without = |claim: &str| {
        let mut c = claims(&email);
        c.as_object_mut().unwrap().remove(claim);
        c
    };

    let cases: Vec<(&str, Value, u16)> = vec![
        (
            "another client's token",
            with("aud", json!("someone-else.apps.googleusercontent.com")),
            401,
        ),
        (
            "another issuer",
            with("iss", json!("https://accounts.example.com")),
            401,
        ),
        (
            "expired",
            with("exp", json!(chrono::Utc::now().timestamp() - 600)),
            401,
        ),
        (
            "unverified email",
            with("email_verified", json!(false)),
            403,
        ),
        ("no email_verified", without("email_verified"), 403),
        ("a consumer account (no hd)", without("hd"), 403),
        ("another Workspace", with("hd", json!("example.org")), 403),
        ("an outside address", claims("someone@gmail.com"), 403),
        (
            "a look-alike domain",
            claims(&format!("someone@evil-{DOMAIN}")),
            403,
        ),
        (
            "another request's nonce",
            with("nonce", json!("other")),
            401,
        ),
        ("no nonce in the token", without("nonce"), 401),
    ];
    for (what, claims, expected) in cases {
        // An app of its own per case: these POSTs share one address, and the
        // sign-in rate limit (ten a minute) is per app.
        let (server, _) = app_with_renewal().await;
        let (status, body) = google_sign_in(&server, &claims).await;
        assert_eq!(status, expected, "{what}: {body}");
        assert!(body.get("token").is_none(), "{what}: {body}");
        // The public message never says which check failed.
        let message = body["error"].as_str().unwrap();
        assert!(
            message == "Google sign-in failed" || message == "This Google account is not allowed",
            "{what}: {message}"
        );
    }

    // Signed by a key Google does not publish under that id.
    let (server, _) = app_with_renewal().await;
    let forged = id_token_signed_by(&generate_key(), &claims(&email));
    let res = server
        .post("/auth/connector/google")
        .json(&json!({ "id_token": forged, "nonce": NONCE }))
        .await;
    assert_eq!(res.status_code(), 401, "{}", res.text());

    // Nothing above created the account.
    let (status, body) = google_sign_in(&server, &claims(&email)).await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn the_request_must_name_the_nonce_it_sent_to_google() {
    let (server, _) = app_with_renewal().await;
    let token = id_token(&claims(&common::unique_email()));

    for body in [
        json!({ "id_token": token }),
        json!({ "id_token": token, "nonce": "" }),
        json!({ "id_token": token, "nonce": "n".repeat(257) }),
        json!({ "nonce": NONCE }),
        json!({ "id_token": "", "nonce": NONCE }),
        json!("not an object"),
    ] {
        let res = server.post("/auth/connector/google").json(&body).await;
        assert_eq!(res.status_code(), 400, "{body}: {}", res.text());
    }
    let res = server.post("/auth/connector/google").text("not json").await;
    assert_eq!(res.status_code(), 400, "{}", res.text());

    // A different nonce than the token's is not a bad request, it is a refusal.
    let res = server
        .post("/auth/connector/google")
        .json(&json!({ "id_token": token, "nonce": "another-nonce" }))
        .await;
    assert_eq!(res.status_code(), 401, "{}", res.text());
    assert!(res.json::<Value>().get("token").is_none());
}

#[tokio::test]
async fn a_disabled_account_is_refused_and_a_deleted_one_is_not_revived() {
    let (server, db) = app_with_renewal().await;

    let (email, id, _) = signed_in(&server).await;
    let mut disabled: users::ActiveModel = user(&db, id).await.into();
    disabled.disabled_at = Set(Some(chrono::Utc::now().naive_utc()));
    disabled.update(&db).await.unwrap();
    let (status, body) = google_sign_in(&server, &claims(&email)).await;
    assert_eq!(status, 403, "{body}");
    assert!(body.get("token").is_none());

    // As on the dashboard: the deleted account stays deleted, and the session
    // is for a new, empty account, never for the old one.
    let (email, old_id, _) = signed_in(&server).await;
    let mut gone: users::ActiveModel = user(&db, old_id).await.into();
    gone.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    gone.update(&db).await.unwrap();
    let (status, body) = google_sign_in(&server, &claims(&email)).await;
    assert_eq!(status, 200, "{body}");
    assert_ne!(body["user_id"], old_id);
    assert!(user(&db, old_id).await.deleted_at.is_some());
}

#[tokio::test]
async fn both_endpoints_spend_the_sign_in_rate_limit() {
    let (server, _) = app_with_renewal().await;
    let token = id_token(&claims(&common::unique_email()));

    // Ten sign-in POSTs a minute per address, shared with /auth/login.
    for _ in 0..5 {
        let res = server
            .post("/auth/connector/google")
            .json(&json!({ "id_token": token, "nonce": "not-this-one" }))
            .await;
        assert_eq!(res.status_code(), 401, "{}", res.text());
        let res = server
            .post("/auth/connector/session")
            .json(&json!({ "assertion": "x" }))
            .await;
        assert_eq!(res.status_code(), 401, "{}", res.text());
    }
    for path in ["/auth/connector/google", "/auth/connector/session"] {
        let res = server.post(path).json(&json!({})).await;
        assert_eq!(res.status_code(), 429, "{path}: {}", res.text());
    }
}

// --- POST /auth/connector/session -----------------------------------------

#[tokio::test]
async fn a_good_assertion_renews_the_session_for_that_person_only() {
    let (server, _) = app_with_renewal().await;
    let (email, user_id, token_version) = signed_in(&server).await;
    let (other_email, other_id, _) = signed_in(&server).await;

    let assertion = assertion_with(SECRET, assertion_claims(user_id, token_version));
    let (status, body) = renew(&server, &assertion).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["user_id"], user_id);
    assert_eq!(body["token_version"], token_version);
    assert_session_is_for(&server, body["token"].as_str().unwrap(), &email).await;
    assert_ne!(email, other_email);
    assert_ne!(user_id, other_id);
}

#[tokio::test]
async fn an_assertion_that_does_not_verify_is_refused() {
    let (server, _) = app_with_renewal().await;
    let (_, user_id, token_version) = signed_in(&server).await;
    common::baseline_env();
    let good = || assertion_claims(user_id, token_version);
    let now = chrono::Utc::now().timestamp();
    let with = |claim: &str, value: Value| {
        let mut c = good();
        c[claim] = value;
        c
    };
    let jwt_secret = std::env::var("JWT_SECRET").expect("baseline JWT_SECRET");

    let mut expired = good();
    expired["iat"] = json!(now - 90);
    expired["exp"] = json!(now - 30);
    let mut no_audience = good();
    no_audience.as_object_mut().unwrap().remove("aud");

    let cases: Vec<(&str, String)> = vec![
        (
            "another secret",
            assertion_with("another-secret-0123456789abcdef012345", good()),
        ),
        ("the session secret", assertion_with(&jwt_secret, good())),
        ("expired", assertion_with(SECRET, expired)),
        (
            "another audience",
            assertion_with(SECRET, with("aud", json!("opn.onl"))),
        ),
        ("no audience", assertion_with(SECRET, no_audience)),
        (
            "valid for an hour",
            assertion_with(SECRET, with("exp", json!(now + 3600))),
        ),
        (
            "an email as subject",
            assertion_with(SECRET, with("sub", json!("a@users.opn.onl"))),
        ),
        ("not a token", "not-a-token".to_string()),
    ];
    for (what, assertion) in cases {
        let (status, body) = renew(&server, &assertion).await;
        assert_eq!(status, 401, "{what}: {body}");
        assert!(body.get("token").is_none(), "{what}: {body}");
    }

    // A second app on the same database: the sign-in rate limit is per app.
    let (server, _) = app_with_renewal().await;
    // A session token is not an assertion, whoever it belongs to.
    let (_, signed) = google_sign_in(&server, &claims(&common::unique_email())).await;
    let (status, body) = renew(&server, signed["token"].as_str().unwrap()).await;
    assert_eq!(status, 401, "{body}");

    for body in [json!({}), json!({ "assertion": 7 }), json!("x")] {
        let res = server.post("/auth/connector/session").json(&body).await;
        assert_eq!(res.status_code(), 400, "{body}: {}", res.text());
    }

    // The same person still renews with a proper one.
    let (status, body) = renew(&server, &assertion_with(SECRET, good())).await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn someone_whose_sessions_were_ended_is_not_renewed() {
    let (server, db) = app_with_renewal().await;
    let (_, user_id, token_version) = signed_in(&server).await;

    // What a password change or "sign out everywhere" does.
    let mut changed: users::ActiveModel = user(&db, user_id).await.into();
    changed.token_version = Set(i32::try_from(token_version).unwrap() + 1);
    changed.update(&db).await.unwrap();

    let stale = assertion_with(SECRET, assertion_claims(user_id, token_version));
    let (status, body) = renew(&server, &stale).await;
    assert_eq!(status, 401, "{body}");
    assert!(body.get("token").is_none());
    assert_eq!(body["error"], "This sign-in can no longer be renewed");

    // Only a new Google sign-in learns the new version.
    let current = assertion_with(SECRET, assertion_claims(user_id, token_version + 1));
    assert_eq!(renew(&server, &current).await.0, 200);
}

#[tokio::test]
async fn a_deleted_disabled_or_unknown_account_is_not_renewed() {
    let (server, db) = app_with_renewal().await;

    let (_, deleted_id, tv) = signed_in(&server).await;
    let mut gone: users::ActiveModel = user(&db, deleted_id).await.into();
    gone.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    gone.update(&db).await.unwrap();
    let (status, body) = renew(
        &server,
        &assertion_with(SECRET, assertion_claims(deleted_id, tv)),
    )
    .await;
    assert_eq!(status, 401, "{body}");
    assert!(body.get("token").is_none());

    let (_, disabled_id, tv) = signed_in(&server).await;
    let mut disabled: users::ActiveModel = user(&db, disabled_id).await.into();
    disabled.disabled_at = Set(Some(chrono::Utc::now().naive_utc()));
    disabled.update(&db).await.unwrap();
    let (status, body) = renew(
        &server,
        &assertion_with(SECRET, assertion_claims(disabled_id, tv)),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(body.get("token").is_none());

    let (status, body) = renew(
        &server,
        &assertion_with(SECRET, assertion_claims(i64::from(i32::MAX), 0)),
    )
    .await;
    assert_eq!(status, 401, "{body}");
}

#[tokio::test]
async fn an_account_whose_domain_is_no_longer_allowed_is_not_renewed() {
    let (server, db) = app_with_renewal().await;
    let (_, user_id, tv) = signed_in(&server).await;

    // The same database behind an app whose allowed domains no longer
    // include this person's.
    let mut google = google_config().await;
    google.allowed_domains = vec!["elsewhere.example".to_string()];
    let narrowed = ConnectorAuth::new(google, Some(SECRET.into()), None);
    let mut state = AppState::for_tests(db.clone()).await;
    state.connector = Some(std::sync::Arc::new(narrowed));
    let narrowed_server = axum_test::TestServer::new(opn_onl_backend::build_router(state)).unwrap();

    let assertion = assertion_with(SECRET, assertion_claims(user_id, tv));
    let (status, body) = renew(&narrowed_server, &assertion).await;
    assert_eq!(status, 403, "{body}");
    assert!(body.get("token").is_none());
    // Control: the app that still allows the domain renews the same assertion.
    assert_eq!(renew(&server, &assertion).await.0, 200);
}

#[tokio::test]
async fn an_account_whose_domain_an_admin_has_blocked_is_not_renewed() {
    // A domain of this test's own: a block is seen by every test on the
    // shared database, and `DOMAIN` is everybody's.
    let domain = format!("conn-{}.opn.dev", uuid::Uuid::new_v4().simple());
    let mut google = google_config().await;
    google.allowed_domains = vec![domain.clone()];
    let connector = ConnectorAuth::new(google, Some(SECRET.into()), None);
    let (server, db) = common::spawn_real_app_with_connector(connector).await;

    let mut c = claims(&format!("jane@{domain}"));
    c["hd"] = json!(domain);
    let (status, body) = google_sign_in(&server, &c).await;
    assert_eq!(status, 200, "{body}");
    let assertion = assertion_with(
        SECRET,
        assertion_claims(
            body["user_id"].as_i64().unwrap(),
            body["token_version"].as_i64().unwrap(),
        ),
    );
    assert_eq!(renew(&server, &assertion).await.0, 200, "control");

    // The row alone, as if the block had gone in without the account being
    // disabled with it (the admin endpoint does both): the domain is still
    // on the connector's own list, and the account is live and enabled.
    let block = blocked_email_domains::ActiveModel {
        domain: Set(domain.clone()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    let account = user(&db, body["user_id"].as_i64().unwrap()).await;
    assert!(!account.is_disabled() && !account.is_admin);

    let (status, body) = renew(&server, &assertion).await;
    assert_eq!(status, 403, "{body}");
    assert!(body.get("token").is_none());

    // Unblocked, the same person is renewed again.
    blocked_email_domains::Entity::delete_by_id(block.id)
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(renew(&server, &assertion).await.0, 200);
}
