//! Sign-in for the Claude connector (the remote MCP server in `deploy/lnp/mcp`).
//!
//! The connector is its own OAuth authorization server towards Claude and
//! signs each person in with Google's redirect flow, using the same Google
//! client as the dashboard. It keeps no accounts: these two endpoints turn
//! what it holds into an opn.onl session for the person, so tools run as them.
//!
//! * `POST /auth/connector/google` takes the Google ID token the connector
//!   received and the `nonce` it sent to Google. The token goes through
//!   `google_auth::verified_id_token`, the same check as `/auth/google`
//!   (signature, issuer, audience, expiry, verified email, allowed domains,
//!   the `hd` rule) plus the nonce, and the address through
//!   `verified_sign_in::verified_user`, the same account step. It needs
//!   nothing but a valid ID token, so it is safe to reach from anywhere.
//! * `POST /auth/connector/session` renews that session without Google, so a
//!   connector sign-in can outlive one session (24 hours). It takes a
//!   short-lived assertion the connector signs with a secret only it and this
//!   server know, naming the user and the `token_version` seen at sign-in. A
//!   new session is made only while that user is live, enabled, unchanged
//!   (`token_version`) and still on an allowed domain, so someone who has left
//!   is cut off at the next renewal. Whoever holds the secret can ask for a
//!   session as any live user: the deployment must not route this path from
//!   the internet (`deploy/lnp/nginx/`), and the secret is the second lock.
//!
//! Off (`404`) unless Google sign-in is configured; the renewal endpoint is
//! also off until `CONNECTOR_SIGNIN_SECRET` is set to at least 32 bytes that
//! are not `JWT_SECRET`.
//!
//! Tokens, assertions and the secret are never logged.

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::entity::users;
use crate::handlers::google_auth::{self, GoogleAuthConfig};
use crate::handlers::json_error;
use crate::handlers::verified_sign_in::verified_user;
use crate::utils::email_domain_policy::ensure_sign_in_domain_allowed;
use crate::utils::env::env_nonempty;
use crate::utils::jwt::create_jwt;

/// The `aud` of a renewal assertion. Fixed, so nothing else signed with the
/// same secret can be replayed here.
pub const ASSERTION_AUDIENCE: &str = "opn-connector-session";
/// An assertion may not be valid for longer than this, whatever it claims.
pub const ASSERTION_MAX_LIFE_SECS: i64 = 60;
/// How far ahead of this server's clock an assertion's `iat` may be.
const ASSERTION_CLOCK_SKEW_SECS: i64 = 5;
const MIN_SECRET_BYTES: usize = 32;
/// Longest `nonce` accepted; the connector's is 43 characters.
const MAX_NONCE_BYTES: usize = 256;

/// No `Debug`: the struct holds the assertion secret.
#[derive(Clone)]
pub struct ConnectorAuth {
    pub google: GoogleAuthConfig,
    assertion_secret: Option<String>,
}

impl ConnectorAuth {
    /// `assertion_secret` turns the renewal endpoint on. It is dropped, and
    /// renewal stays off, when it is shorter than 32 bytes or equal to
    /// `jwt_secret`: a session token must never pass for an assertion.
    pub fn new(
        google: GoogleAuthConfig,
        assertion_secret: Option<String>,
        jwt_secret: Option<&str>,
    ) -> Self {
        let assertion_secret = assertion_secret.filter(|secret| {
            if secret.len() < MIN_SECRET_BYTES {
                tracing::error!(
                    "CONNECTOR_SIGNIN_SECRET is shorter than {MIN_SECRET_BYTES} bytes; \
                     connector session renewal is off"
                );
                return false;
            }
            if jwt_secret == Some(secret.as_str()) {
                tracing::error!(
                    "CONNECTOR_SIGNIN_SECRET equals JWT_SECRET; connector session renewal is off"
                );
                return false;
            }
            true
        });
        Self {
            google,
            assertion_secret,
        }
    }

    /// `None` unless Google sign-in is configured in this process.
    pub fn from_env() -> Option<Self> {
        let google = google_auth::config()?.clone();
        Some(Self::new(
            google,
            env_nonempty("CONNECTOR_SIGNIN_SECRET"),
            env_nonempty("JWT_SECRET").as_deref(),
        ))
    }

    pub fn renewal_enabled(&self) -> bool {
        self.assertion_secret.is_some()
    }
}

#[derive(Deserialize, ToSchema)]
pub struct ConnectorGoogleRequest {
    /// The ID token Google's token endpoint returned to the connector.
    pub id_token: String,
    /// The `nonce` the connector sent to Google for this sign-in.
    pub nonce: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ConnectorSessionRequest {
    /// An HS256 JWT signed with `CONNECTOR_SIGNIN_SECRET`: `aud`
    /// `opn-connector-session`, `sub` the user id, `tv` the token version seen
    /// at sign-in, `iat` and an `exp` at most 60 seconds later.
    pub assertion: String,
}

/// A session for the connector to run one person's tools with.
#[derive(Serialize, ToSchema)]
pub struct ConnectorSession {
    /// An opn.onl session token, as `/auth/login` answers with.
    pub token: String,
    pub user_id: i32,
    /// What a later renewal must name; it changes when the person's sessions
    /// are ended (password change, sign-out everywhere).
    pub token_version: i32,
}

fn session_for(user: &users::Model) -> Response {
    match create_jwt(user.id, &user.email, user.token_version) {
        Ok(token) => (
            StatusCode::OK,
            Json(ConnectorSession {
                token,
                user_id: user.id,
                token_version: user.token_version,
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to create JWT: {}", e);
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create session",
            )
        }
    }
}

fn not_enabled() -> Response {
    json_error(StatusCode::NOT_FOUND, "Connector sign-in is not enabled")
}

/// Sign the Claude connector's person in with their Google ID token.
#[utoipa::path(
    post,
    path = "/auth/connector/google",
    request_body = ConnectorGoogleRequest,
    responses(
        (status = 200, description = "Signed in", body = ConnectorSession),
        (status = 400, description = "Missing ID token or nonce"),
        (status = 401, description = "The ID token is invalid or is not the answer to this nonce"),
        (status = 403, description = "Account not allowed (domain, unverified email) or disabled"),
        (status = 404, description = "Connector sign-in is not enabled"),
        (status = 502, description = "Google's keys could not be fetched"),
    ),
    tag = "Authentication"
)]
pub async fn connector_google_login(State(state): State<AppState>, body: Bytes) -> Response {
    let Some(connector) = state.connector.as_deref() else {
        return not_enabled();
    };
    let Ok(request) = serde_json::from_slice::<ConnectorGoogleRequest>(&body) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Expected a JSON body with an id_token and a nonce",
        );
    };
    if request.id_token.is_empty()
        || request.nonce.is_empty()
        || request.nonce.len() > MAX_NONCE_BYTES
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Expected a JSON body with an id_token and a nonce",
        );
    }

    let email = match google_auth::verified_id_token(
        &request.id_token,
        &connector.google,
        Some(&request.nonce),
    )
    .await
    {
        Ok(email) => email,
        Err(reason) => {
            tracing::warn!("Connector Google sign-in refused: {}", reason.log_detail());
            return json_error(reason.status(), reason.public_message());
        }
    };
    match verified_user(&state, &email, "Google (connector)").await {
        Ok(user) => session_for(&user),
        Err((status, message)) => json_error(status, message),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AssertionError {
    /// Bad signature, algorithm, audience, expiry or shape.
    Invalid(String),
    /// Valid for longer than [`ASSERTION_MAX_LIFE_SECS`], or issued in the future.
    TooLongLived,
}

#[derive(Deserialize)]
struct AssertionClaims {
    sub: String,
    tv: i32,
    iat: i64,
    exp: i64,
}

/// Who a renewal assertion is for: `(user id, token version)`.
pub fn verify_assertion(
    assertion: &str,
    secret: &str,
    now: i64,
) -> Result<(i32, i32), AssertionError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[ASSERTION_AUDIENCE]);
    validation.set_required_spec_claims(&["exp", "aud", "sub"]);
    // The connector runs beside this server; no slack for a stale assertion.
    validation.leeway = 0;

    let claims = decode::<AssertionClaims>(
        assertion,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|e| AssertionError::Invalid(e.to_string()))?
    .claims;

    // Both numbers are the signer's: a difference that does not fit in an i64
    // is a life longer than any, not one that wraps round to a short one.
    let life_ok = claims
        .exp
        .checked_sub(claims.iat)
        .is_some_and(|life| life <= ASSERTION_MAX_LIFE_SECS);
    if !life_ok || claims.iat > now.saturating_add(ASSERTION_CLOCK_SKEW_SECS) {
        return Err(AssertionError::TooLongLived);
    }
    let user_id = claims
        .sub
        .parse::<i32>()
        .map_err(|_| AssertionError::Invalid("sub is not a user id".to_string()))?;
    Ok((user_id, claims.tv))
}

/// What the connector is told when a person can no longer be renewed. One
/// message for every reason: the connector only needs to end the sign-in.
fn renewal_refused(status: StatusCode, why: &str, user_id: i32) -> Response {
    tracing::warn!(user_id, "Connector session renewal refused: {why}");
    json_error(status, "This sign-in can no longer be renewed")
}

/// Renew the Claude connector's session for a person who signed in earlier.
#[utoipa::path(
    post,
    path = "/auth/connector/session",
    request_body = ConnectorSessionRequest,
    responses(
        (status = 200, description = "A new session", body = ConnectorSession),
        (status = 400, description = "Missing assertion"),
        (status = 401, description = "The assertion is invalid, or the account is gone or has changed since sign-in"),
        (status = 403, description = "The account is disabled or its domain is no longer allowed"),
        (status = 404, description = "Connector session renewal is not enabled"),
    ),
    tag = "Authentication"
)]
pub async fn connector_session(State(state): State<AppState>, body: Bytes) -> Response {
    let Some(connector) = state.connector.as_deref() else {
        return not_enabled();
    };
    let Some(secret) = connector.assertion_secret.as_deref() else {
        return not_enabled();
    };
    let Ok(request) = serde_json::from_slice::<ConnectorSessionRequest>(&body) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Expected a JSON body with an assertion",
        );
    };

    let (user_id, token_version) =
        match verify_assertion(&request.assertion, secret, chrono::Utc::now().timestamp()) {
            Ok(subject) => subject,
            Err(reason) => {
                tracing::warn!("Connector session assertion refused: {:?}", reason);
                return json_error(StatusCode::UNAUTHORIZED, "Invalid assertion");
            }
        };

    let user = match users::Entity::find_by_id(user_id).one(&state.db).await {
        Ok(user) => user,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
    };
    // A soft-deleted row keeps its id: it must not be found here.
    let Some(user) = user.filter(|user| user.deleted_at.is_none()) else {
        return renewal_refused(StatusCode::UNAUTHORIZED, "no such live account", user_id);
    };
    if user.token_version != token_version {
        return renewal_refused(
            StatusCode::UNAUTHORIZED,
            "the account's sessions were ended since sign-in",
            user_id,
        );
    }
    if user.is_disabled() {
        return renewal_refused(StatusCode::FORBIDDEN, "the account is disabled", user_id);
    }
    // The rules a fresh Google sign-in would apply to the address today.
    let on_allowed_domain = user.email.rsplit_once('@').is_some_and(|(_, domain)| {
        connector
            .google
            .allowed_domains
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(domain))
    });
    if !on_allowed_domain
        || ensure_sign_in_domain_allowed(&state.db, &user.email)
            .await
            .is_err()
    {
        return renewal_refused(
            StatusCode::FORBIDDEN,
            "the address is no longer on an allowed domain",
            user_id,
        );
    }
    session_for(&user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::{Value, json};

    const SECRET: &str = "connector-secret-0123456789abcdef0123";
    const NOW: i64 = 1_800_000_000;

    fn sign(claims: Value, secret: &str) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    /// An assertion the connector would send right now. `exp` is checked
    /// against the real clock, `iat` against the `now` handed in.
    fn claims() -> Value {
        let now = chrono::Utc::now().timestamp();
        json!({ "aud": ASSERTION_AUDIENCE, "sub": "42", "tv": 3, "iat": now, "exp": now + 30 })
    }

    fn verify(claims: Value) -> Result<(i32, i32), AssertionError> {
        verify_assertion(
            &sign(claims, SECRET),
            SECRET,
            chrono::Utc::now().timestamp(),
        )
    }

    #[test]
    fn a_good_assertion_names_the_user_and_the_token_version() {
        assert_eq!(verify(claims()), Ok((42, 3)));
    }

    #[test]
    fn an_assertion_signed_with_another_secret_is_refused() {
        let forged = sign(claims(), "another-secret-0123456789abcdef012345");
        assert!(matches!(
            verify_assertion(&forged, SECRET, chrono::Utc::now().timestamp()),
            Err(AssertionError::Invalid(_))
        ));
    }

    #[test]
    fn an_expired_assertion_is_refused_without_leeway() {
        let now = chrono::Utc::now().timestamp();
        let mut c = claims();
        c["iat"] = json!(now - 40);
        c["exp"] = json!(now - 5);
        assert!(matches!(verify(c), Err(AssertionError::Invalid(_))));
    }

    #[test]
    fn an_assertion_for_another_audience_or_none_is_refused() {
        for aud in [json!("opn-connector"), json!("opn.onl"), json!("")] {
            let mut c = claims();
            c["aud"] = aud;
            assert!(matches!(verify(c), Err(AssertionError::Invalid(_))));
        }
        let mut c = claims();
        c.as_object_mut().unwrap().remove("aud");
        assert!(matches!(verify(c), Err(AssertionError::Invalid(_))));
    }

    #[test]
    fn an_assertion_valid_for_longer_than_a_minute_is_refused() {
        let now = chrono::Utc::now().timestamp();
        let mut c = claims();
        c["exp"] = json!(now + ASSERTION_MAX_LIFE_SECS + 1);
        assert_eq!(verify(c), Err(AssertionError::TooLongLived));
        // Exactly the limit is fine.
        let mut c = claims();
        c["exp"] = json!(now + ASSERTION_MAX_LIFE_SECS);
        assert!(verify(c).is_ok());
        // Backdating `iat` does not buy a longer life; post-dating it is refused.
        let mut c = claims();
        c["iat"] = json!(now + 3600);
        c["exp"] = json!(now + 3630);
        assert_eq!(verify(c), Err(AssertionError::TooLongLived));
    }

    #[test]
    fn a_life_too_long_to_count_is_refused_not_wrapped_round() {
        let now = chrono::Utc::now().timestamp();
        // `exp - iat` does not fit in an i64 for any of these.
        for iat in [i64::MIN, i64::MIN + now, -i64::MAX] {
            let mut c = claims();
            c["iat"] = json!(iat);
            assert_eq!(verify(c), Err(AssertionError::TooLongLived), "iat {iat}");
        }
        // The largest clock reading there is does not overflow the skew either.
        assert_eq!(
            verify_assertion(&sign(claims(), SECRET), SECRET, i64::MAX),
            Ok((42, 3))
        );
    }

    #[test]
    fn an_assertion_missing_a_claim_is_refused() {
        for claim in ["sub", "tv", "iat", "exp"] {
            let mut c = claims();
            c.as_object_mut().unwrap().remove(claim);
            assert!(
                matches!(verify(c), Err(AssertionError::Invalid(_))),
                "{claim}"
            );
        }
        let mut c = claims();
        c["sub"] = json!("jane@leadershipnowproject.com");
        assert!(matches!(verify(c), Err(AssertionError::Invalid(_))));
    }

    #[test]
    fn an_unsigned_or_differently_signed_assertion_is_refused() {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let part = |v: Value| URL_SAFE_NO_PAD.encode(v.to_string());
        let unsigned = format!(
            "{}.{}.",
            part(json!({ "alg": "none", "typ": "JWT" })),
            part(claims())
        );
        assert!(verify_assertion(&unsigned, SECRET, NOW).is_err());
        let hs512 = encode(
            &Header::new(Algorithm::HS512),
            &claims(),
            &EncodingKey::from_secret(SECRET.as_bytes()),
        )
        .unwrap();
        assert!(verify_assertion(&hs512, SECRET, NOW).is_err());
    }

    fn google() -> GoogleAuthConfig {
        GoogleAuthConfig::from_values(
            Some("client-1.apps.googleusercontent.com".into()),
            Some("s".into()),
            Some("leadershipnowproject.com".into()),
            true,
        )
        .unwrap()
    }

    #[test]
    fn renewal_is_off_without_a_usable_secret() {
        let jwt = "jwt-secret-0123456789abcdef0123456789";
        let on = |secret: Option<&str>| {
            ConnectorAuth::new(google(), secret.map(str::to_string), Some(jwt)).renewal_enabled()
        };
        assert!(on(Some(SECRET)));
        assert!(!on(None));
        assert!(!on(Some("short")));
        assert!(!on(Some(&SECRET[..31])));
        assert!(!on(Some(jwt)), "the session secret must not be reused");
    }
}
