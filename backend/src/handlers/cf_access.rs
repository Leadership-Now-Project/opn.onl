//! Sign-in through Cloudflare Access.
//!
//! An instance whose dashboard sits behind Cloudflare Access can let the person
//! Access already signed in through: Access adds a signed JWT to every request it
//! lets through, in the `Cf-Access-Jwt-Assertion` header. `POST /auth/cf-access`
//! checks that token against the team's published keys, finds or creates the
//! account for its email, and answers with the same [`AuthResponse`] as
//! `/auth/login`, so every other auth path works unchanged.
//!
//! Off unless both `CF_ACCESS_TEAM_DOMAIN` and `CF_ACCESS_AUD` are set; the
//! endpoint then answers `404` and the instance behaves as upstream.
//!
//! | variable | meaning |
//! |----------|---------|
//! | `CF_ACCESS_TEAM_DOMAIN` | `https://<team>.cloudflareaccess.com`; also the expected issuer |
//! | `CF_ACCESS_AUD` | the Access application's AUD tag; several may be comma-separated |
//! | `CF_ACCESS_ALLOWED_EMAIL_DOMAINS` | optional comma-separated list; when set, other addresses are refused even if Access let them through |
//!
//! Accounts created here are verified (Access vouched for the address) and get
//! a random password nobody knows. The first live account becomes admin, as in
//! `/auth/register`, under the same advisory lock.

use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use once_cell::sync::Lazy;
use sea_orm::*;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::AppState;
use crate::entity::users;
use crate::handlers::auth::{AuthResponse, FIRST_ACCOUNT_ADMIN_LOCK};
use crate::handlers::json_error;
use crate::utils::email::generate_token;
use crate::utils::email_domain_policy::{
    ensure_email_domain_allowed, ensure_sign_in_domain_allowed, live_users_by_email,
    normalize_email,
};
use crate::utils::env::env_nonempty;
use crate::utils::jwt::{create_jwt, hash_password};

pub const ASSERTION_HEADER: &str = "cf-access-jwt-assertion";

/// Keys are refetched at most this often, and at once when a token names a key
/// the cached set lacks (Cloudflare rotates them).
const JWKS_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CfAccessConfig {
    pub team_domain: String,
    pub audiences: Vec<String>,
    pub allowed_email_domains: Vec<String>,
}

impl CfAccessConfig {
    pub fn from_values(
        team_domain: Option<String>,
        aud: Option<String>,
        allowed_email_domains: Option<String>,
    ) -> Option<Self> {
        let team_domain = normalize_team_domain(&team_domain?);
        let audiences = split_list(&aud?, false);
        if team_domain.is_empty() || audiences.is_empty() {
            return None;
        }
        Some(Self {
            team_domain,
            audiences,
            allowed_email_domains: allowed_email_domains
                .map(|v| split_list(&v, true))
                .unwrap_or_default(),
        })
    }

    fn from_env() -> Option<Self> {
        Self::from_values(
            env_nonempty("CF_ACCESS_TEAM_DOMAIN"),
            env_nonempty("CF_ACCESS_AUD"),
            env_nonempty("CF_ACCESS_ALLOWED_EMAIL_DOMAINS"),
        )
    }

    fn certs_url(&self) -> String {
        format!("{}/cdn-cgi/access/certs", self.team_domain)
    }

    fn email_domain_allowed(&self, email: &str) -> bool {
        if self.allowed_email_domains.is_empty() {
            return true;
        }
        let Some((_, domain)) = email.rsplit_once('@') else {
            return false;
        };
        self.allowed_email_domains.iter().any(|d| d == domain)
    }
}

static CONFIG: Lazy<Option<CfAccessConfig>> = Lazy::new(CfAccessConfig::from_env);
static JWKS: Lazy<RwLock<Option<(Instant, JwkSet)>>> = Lazy::new(|| RwLock::new(None));

/// `lnp.cloudflareaccess.com`, with or without scheme or trailing slash, becomes
/// `https://lnp.cloudflareaccess.com`: the exact `iss` Access puts in its tokens.
fn normalize_team_domain(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() || trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

fn split_list(raw: &str, lowercase: bool) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| {
            if lowercase {
                v.to_ascii_lowercase()
            } else {
                v.to_string()
            }
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum AssertionError {
    /// The token names a key the set does not hold; worth one refetch.
    UnknownKey,
    Invalid(String),
    EmailDomainNotAllowed,
}

#[derive(Deserialize)]
struct AccessClaims {
    email: String,
}

/// Check an Access token against `jwks` and return its normalised email.
pub fn verify_assertion(
    token: &str,
    jwks: &JwkSet,
    config: &CfAccessConfig,
) -> Result<String, AssertionError> {
    let header = decode_header(token).map_err(|e| AssertionError::Invalid(e.to_string()))?;
    let kid = header
        .kid
        .ok_or_else(|| AssertionError::Invalid("token has no kid".to_string()))?;
    let jwk = jwks.find(&kid).ok_or(AssertionError::UnknownKey)?;
    let key = DecodingKey::from_jwk(jwk).map_err(|e| AssertionError::Invalid(e.to_string()))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&config.audiences);
    validation.set_issuer(&[&config.team_domain]);
    validation.set_required_spec_claims(&["exp", "iss", "aud"]);

    let claims = decode::<AccessClaims>(token, &key, &validation)
        .map_err(|e| AssertionError::Invalid(e.to_string()))?
        .claims;
    let email = normalize_email(&claims.email);
    if email.is_empty() {
        return Err(AssertionError::Invalid("token has no email".to_string()));
    }
    if !config.email_domain_allowed(&email) {
        return Err(AssertionError::EmailDomainNotAllowed);
    }
    Ok(email)
}

async fn fetch_jwks(config: &CfAccessConfig) -> Result<JwkSet, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    client
        .get(config.certs_url())
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json::<JwkSet>()
        .await
        .map_err(|e| e.to_string())
}

async fn jwks(config: &CfAccessConfig, force_refresh: bool) -> Result<JwkSet, String> {
    if !force_refresh
        && let Some((fetched, set)) = JWKS.read().await.as_ref()
        && fetched.elapsed() < JWKS_TTL
    {
        return Ok(set.clone());
    }
    let set = fetch_jwks(config).await?;
    *JWKS.write().await = Some((Instant::now(), set.clone()));
    Ok(set)
}

async fn verify_with_refresh(
    token: &str,
    config: &CfAccessConfig,
) -> Result<String, AssertionError> {
    let keys_error = |e: String| AssertionError::Invalid(format!("fetching Access keys: {e}"));
    let set = jwks(config, false).await.map_err(keys_error)?;
    match verify_assertion(token, &set, config) {
        Err(AssertionError::UnknownKey) => {
            let set = jwks(config, true).await.map_err(keys_error)?;
            verify_assertion(token, &set, config)
        }
        other => other,
    }
}

/// Sign in as the person Cloudflare Access already authenticated.
#[utoipa::path(
    post,
    path = "/auth/cf-access",
    responses(
        (status = 200, description = "Signed in", body = AuthResponse),
        (status = 401, description = "No valid Cloudflare Access token"),
        (status = 403, description = "Account disabled or email domain not allowed"),
        (status = 404, description = "Cloudflare Access sign-in is not enabled"),
    ),
    tag = "Authentication"
)]
pub async fn cf_access_login(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(config) = CONFIG.as_ref() else {
        return json_error(StatusCode::NOT_FOUND, "Cloudflare Access sign-in is not enabled");
    };
    let Some(token) = headers
        .get(ASSERTION_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    else {
        return json_error(StatusCode::UNAUTHORIZED, "No Cloudflare Access token");
    };

    let email = match verify_with_refresh(token, config).await {
        Ok(email) => email,
        Err(reason) => {
            let (status, detail) = match reason {
                AssertionError::EmailDomainNotAllowed => {
                    (StatusCode::FORBIDDEN, "email domain not allowed".to_string())
                }
                AssertionError::UnknownKey => {
                    (StatusCode::UNAUTHORIZED, "signed by an unknown key".to_string())
                }
                AssertionError::Invalid(detail) => (StatusCode::UNAUTHORIZED, detail),
            };
            tracing::warn!("Cloudflare Access token refused: {detail}");
            return json_error(status, "Cloudflare Access token refused");
        }
    };

    let user = match find_or_create_user(&state.db, &email).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if user.is_disabled() {
        return json_error(StatusCode::FORBIDDEN, "This account is disabled");
    }

    match create_jwt(user.id, &user.email, user.token_version) {
        Ok(token) => (
            StatusCode::OK,
            Json(AuthResponse {
                token,
                user_id: user.id,
                email: user.email,
                email_verified: user.email_verified,
                is_admin: user.is_admin,
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to create JWT: {}", e);
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create session")
        }
    }
}

fn database_error() -> Response {
    json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
}

async fn find_or_create_user(
    db: &DatabaseConnection,
    email: &str,
) -> Result<users::Model, Response> {
    if let Some(user) = live_users_by_email(email)
        .one(db)
        .await
        .map_err(|_| database_error())?
    {
        ensure_sign_in_domain_allowed(db, email)
            .await
            .map_err(|r| json_error(StatusCode::FORBIDDEN, r.public_message()))?;
        return Ok(user);
    }

    ensure_email_domain_allowed(db, email)
        .await
        .map_err(|r| json_error(StatusCode::FORBIDDEN, r.public_message()))?;
    // Nobody knows this password; Access is the way in.
    let password_hash = hash_password(&generate_token())
        .await
        .map_err(|_| database_error())?;

    let txn = db.begin().await.map_err(|_| database_error())?;
    // Same lock and count as `/auth/register`, so the two cannot both seat a
    // first admin.
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock($1)",
        [FIRST_ACCOUNT_ADMIN_LOCK.into()],
    ))
    .await
    .map_err(|_| database_error())?;
    // A concurrent first request may have created the account meanwhile.
    if let Some(user) = live_users_by_email(email)
        .one(&txn)
        .await
        .map_err(|_| database_error())?
    {
        let _ = txn.rollback().await;
        return Ok(user);
    }
    let live_users = users::Entity::find()
        .filter(users::Column::DeletedAt.is_null())
        .count(&txn)
        .await
        .map_err(|_| database_error())?;

    let user = users::ActiveModel {
        email: Set(email.to_string()),
        password_hash: Set(password_hash),
        email_verified: Set(true),
        is_admin: Set(live_users == 0),
        ..Default::default()
    }
    .insert(&txn)
    .await
    .map_err(|e| {
        tracing::error!("Failed to create Cloudflare Access user: {}", e);
        database_error()
    })?;
    txn.commit().await.map_err(|_| database_error())?;
    tracing::info!(user_id = user.id, "Account created through Cloudflare Access");
    Ok(user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use openssl::rsa::Rsa;
    use serde_json::json;

    const TEAM: &str = "https://lnp.cloudflareaccess.com";
    const AUD: &str = "aud-tag-1";

    struct TestKey {
        encoding: EncodingKey,
        jwks: JwkSet,
    }

    fn test_key(kid: &str) -> TestKey {
        let rsa = Rsa::generate(2048).unwrap();
        let jwks: JwkSet = serde_json::from_value(json!({ "keys": [{
            "kty": "RSA", "alg": "RS256", "use": "sig", "kid": kid,
            "n": URL_SAFE_NO_PAD.encode(rsa.n().to_vec()),
            "e": URL_SAFE_NO_PAD.encode(rsa.e().to_vec()),
        }]}))
        .unwrap();
        TestKey {
            encoding: EncodingKey::from_rsa_pem(&rsa.private_key_to_pem().unwrap()).unwrap(),
            jwks,
        }
    }

    fn config(allowed: Option<&str>) -> CfAccessConfig {
        CfAccessConfig::from_values(
            Some(TEAM.to_string()),
            Some(AUD.to_string()),
            allowed.map(str::to_string),
        )
        .unwrap()
    }

    fn token(key: &TestKey, kid: &str, claims: serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        encode(&header, &claims, &key.encoding).unwrap()
    }

    fn claims(email: &str, aud: &str, iss: &str) -> serde_json::Value {
        let exp = chrono::Utc::now().timestamp() + 300;
        json!({ "email": email, "aud": [aud], "iss": iss, "exp": exp, "type": "app" })
    }

    #[test]
    fn config_needs_team_and_aud() {
        assert!(CfAccessConfig::from_values(None, Some(AUD.into()), None).is_none());
        assert!(CfAccessConfig::from_values(Some(TEAM.into()), None, None).is_none());
        let blank_aud = CfAccessConfig::from_values(Some(TEAM.into()), Some(" , ".into()), None);
        assert!(blank_aud.is_none());
    }

    #[test]
    fn team_domain_is_normalised_to_the_issuer() {
        let c = CfAccessConfig::from_values(
            Some("lnp.cloudflareaccess.com/".into()),
            Some("a, b".into()),
            Some("LeadershipNowProject.com, leadershipnowproject.org".into()),
        )
        .unwrap();
        assert_eq!(c.team_domain, TEAM);
        assert_eq!(c.audiences, vec!["a", "b"]);
        assert_eq!(
            c.allowed_email_domains,
            vec!["leadershipnowproject.com", "leadershipnowproject.org"]
        );
        assert_eq!(c.certs_url(), format!("{TEAM}/cdn-cgi/access/certs"));
    }

    #[test]
    fn valid_token_yields_normalised_email() {
        let key = test_key("k1");
        let t = token(&key, "k1", claims("Jane@LeadershipNowProject.org", AUD, TEAM));
        assert_eq!(
            verify_assertion(&t, &key.jwks, &config(None)),
            Ok("jane@leadershipnowproject.org".to_string())
        );
    }

    #[test]
    fn wrong_audience_or_issuer_is_refused() {
        let key = test_key("k1");
        let evil = "https://evil.cloudflareaccess.com";
        for c in [claims("a@x.org", "other-aud", TEAM), claims("a@x.org", AUD, evil)] {
            let t = token(&key, "k1", c);
            assert!(matches!(
                verify_assertion(&t, &key.jwks, &config(None)),
                Err(AssertionError::Invalid(_))
            ));
        }
    }

    #[test]
    fn expired_token_is_refused() {
        let key = test_key("k1");
        let mut c = claims("a@x.org", AUD, TEAM);
        c["exp"] = json!(chrono::Utc::now().timestamp() - 600);
        let t = token(&key, "k1", c);
        assert!(matches!(
            verify_assertion(&t, &key.jwks, &config(None)),
            Err(AssertionError::Invalid(_))
        ));
    }

    #[test]
    fn token_signed_by_another_key_is_refused() {
        let real = test_key("k1");
        let forged = test_key("k1");
        let t = token(&forged, "k1", claims("a@x.org", AUD, TEAM));
        assert!(matches!(
            verify_assertion(&t, &real.jwks, &config(None)),
            Err(AssertionError::Invalid(_))
        ));
    }

    #[test]
    fn unknown_kid_asks_for_a_refetch() {
        let key = test_key("k2");
        let other = test_key("k1");
        let t = token(&key, "k2", claims("a@x.org", AUD, TEAM));
        assert_eq!(
            verify_assertion(&t, &other.jwks, &config(None)),
            Err(AssertionError::UnknownKey)
        );
    }

    #[test]
    fn email_outside_allowed_domains_is_refused() {
        let key = test_key("k1");
        let allowed = config(Some("leadershipnowproject.com,leadershipnowproject.org"));
        let ok = token(&key, "k1", claims("a@leadershipnowproject.com", AUD, TEAM));
        assert!(verify_assertion(&ok, &key.jwks, &allowed).is_ok());
        let other = token(&key, "k1", claims("a@gmail.com", AUD, TEAM));
        assert_eq!(
            verify_assertion(&other, &key.jwks, &allowed),
            Err(AssertionError::EmailDomainNotAllowed)
        );
    }
}
