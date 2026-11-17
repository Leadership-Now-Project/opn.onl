//! Sign-in with a Google account, limited to chosen email domains.
//!
//! The login page runs Google Identity Services' OAuth code flow in a popup and
//! posts the one-time code to `POST /auth/google`. The handler exchanges it at
//! Google's token endpoint (which proves the code was issued to this client),
//! checks the returned ID token against Google's published keys, and hands the
//! verified address to `verified_sign_in::sign_in_verified_email`, the same
//! step `/auth/cf-access` ends with.
//!
//! Off unless the client id, the client secret and at least one allowed domain
//! are set; the endpoint then answers `404` and `/auth/settings` carries no
//! `google_client_id`, so the login page shows no Google button.
//!
//! | variable | meaning |
//! |----------|---------|
//! | `GOOGLE_CLIENT_ID` | OAuth 2.0 Web client id; also the expected `aud` |
//! | `GOOGLE_CLIENT_SECRET` | that client's secret |
//! | `GOOGLE_ALLOWED_DOMAINS` | comma-separated email domains that may sign in |
//! | `GOOGLE_REQUIRE_HD` | default true: the token's `hd` (Workspace domain) must be present and be one of the allowed domains, which keeps out consumer accounts that merely use such an address |
//!
//! The Claude connector signs people in with the same Google client, through
//! Google's redirect flow, and posts the ID token it gets to
//! `POST /auth/connector/google` (`connector_auth.rs`). That handler and this
//! one check the token with the one function here, [`verified_id_token`].
//!
//! The code, the tokens and the secret are never logged.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use once_cell::sync::Lazy;
use serde::Deserialize;
use tokio::sync::RwLock;
use utoipa::ToSchema;

use crate::AppState;
use crate::handlers::json_error;
use crate::handlers::verified_sign_in::sign_in_verified_email;
use crate::utils::email_domain_policy::normalize_email;
use crate::utils::env::{env_flag, env_nonempty};

const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";
/// Both spellings appear in Google's ID tokens.
const GOOGLE_ISSUERS: [&str; 2] = ["accounts.google.com", "https://accounts.google.com"];
/// What a popup-mode code client redeems its code with instead of a real
/// redirect address.
const POPUP_REDIRECT_URI: &str = "postmessage";
/// Google's popup flow sends this header with the code; a cross-site form post
/// cannot, so requiring it keeps another site from submitting a code here.
const AJAX_HEADER: &str = "x-requested-with";

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Keys are refetched at most this often, and at once when a token names a key
/// the cached set lacks (Google rotates them).
const JWKS_TTL: Duration = Duration::from_secs(60 * 60);

/// No `Debug`: the struct holds the client secret.
#[derive(Clone)]
pub struct GoogleAuthConfig {
    pub client_id: String,
    client_secret: String,
    pub allowed_domains: Vec<String>,
    pub require_hd: bool,
    /// Google's endpoints; fields so tests can point them at a local fake.
    pub token_url: String,
    pub jwks_url: String,
}

impl GoogleAuthConfig {
    pub fn from_values(
        client_id: Option<String>,
        client_secret: Option<String>,
        allowed_domains: Option<String>,
        require_hd: bool,
    ) -> Option<Self> {
        let client_id = client_id?.trim().to_string();
        let client_secret = client_secret?.trim().to_string();
        let allowed_domains: Vec<String> = allowed_domains?
            .split(',')
            .map(|d| d.trim().to_ascii_lowercase())
            .filter(|d| !d.is_empty())
            .collect();
        if client_id.is_empty() || client_secret.is_empty() || allowed_domains.is_empty() {
            return None;
        }
        Some(Self {
            client_id,
            client_secret,
            allowed_domains,
            require_hd,
            token_url: GOOGLE_TOKEN_URL.to_string(),
            jwks_url: GOOGLE_JWKS_URL.to_string(),
        })
    }

    fn from_env() -> Option<Self> {
        Self::from_values(
            env_nonempty("GOOGLE_CLIENT_ID"),
            env_nonempty("GOOGLE_CLIENT_SECRET"),
            env_nonempty("GOOGLE_ALLOWED_DOMAINS"),
            env_flag("GOOGLE_REQUIRE_HD", true),
        )
    }
}

static CONFIG: Lazy<Option<GoogleAuthConfig>> = Lazy::new(GoogleAuthConfig::from_env);

/// This process's Google sign-in settings, when it is fully configured.
pub fn config() -> Option<&'static GoogleAuthConfig> {
    CONFIG.as_ref()
}
/// Keyed by JWKS address, so a test's fake never meets another's keys.
static JWKS: Lazy<RwLock<HashMap<String, (Instant, JwkSet)>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// The client id the login page needs, when Google sign-in is fully configured.
pub fn configured_client_id() -> Option<String> {
    CONFIG.as_ref().map(|c| c.client_id.clone())
}

#[derive(Deserialize, ToSchema)]
pub struct GoogleLoginRequest {
    /// The one-time authorization code from Google's popup.
    pub code: String,
}

/// Everything Google's token endpoint answers with. Sign-in reads only
/// `id_token`; the rest is here for callers that ask for more scopes.
#[derive(Deserialize)]
pub struct GoogleTokenResponse {
    pub id_token: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub scope: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum IdTokenError {
    /// The token names a key the set does not hold; worth one refetch.
    UnknownKey,
    Invalid(String),
    EmailNotVerified,
    DomainNotAllowed,
    /// `hd` is missing or is not one of the allowed domains.
    HostedDomainMismatch,
    /// The caller named the `nonce` it sent to Google, and the token carries
    /// none or another: the token is not the answer to that request.
    NonceMismatch,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SignInError {
    /// The request lacks `X-Requested-With`.
    NotAjax,
    BadRequest,
    /// Google refused the code (used, expired, or issued to another client).
    CodeRefused(String),
    /// Google could not be reached or answered something unusable.
    GoogleUnavailable(String),
    Token(IdTokenError),
}

impl SignInError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotAjax | Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::GoogleUnavailable(_) => StatusCode::BAD_GATEWAY,
            Self::CodeRefused(_)
            | Self::Token(
                IdTokenError::UnknownKey | IdTokenError::Invalid(_) | IdTokenError::NonceMismatch,
            ) => StatusCode::UNAUTHORIZED,
            Self::Token(
                IdTokenError::EmailNotVerified
                | IdTokenError::DomainNotAllowed
                | IdTokenError::HostedDomainMismatch,
            ) => StatusCode::FORBIDDEN,
        }
    }

    /// What the caller is told. Never the address, the code or a token.
    pub(crate) fn public_message(&self) -> &'static str {
        match self {
            Self::NotAjax => "Missing X-Requested-With header",
            Self::BadRequest => "Expected a JSON body with a code",
            Self::GoogleUnavailable(_) => "Could not reach Google; try again",
            Self::CodeRefused(_) | Self::Token(_) if self.status() == StatusCode::FORBIDDEN => {
                "This Google account is not allowed"
            }
            Self::CodeRefused(_) | Self::Token(_) => "Google sign-in failed",
        }
    }

    /// What the log says. Built from fixed text, Google's error code and
    /// library errors only.
    pub(crate) fn log_detail(&self) -> String {
        match self {
            Self::NotAjax => "no X-Requested-With header".to_string(),
            Self::BadRequest => "unreadable request body".to_string(),
            Self::CodeRefused(d) => format!("code refused: {d}"),
            Self::GoogleUnavailable(d) => format!("Google unavailable: {d}"),
            Self::Token(IdTokenError::UnknownKey) => "signed by an unknown key".to_string(),
            Self::Token(IdTokenError::Invalid(d)) => format!("invalid ID token: {d}"),
            Self::Token(IdTokenError::EmailNotVerified) => "email not verified".to_string(),
            Self::Token(IdTokenError::DomainNotAllowed) => "email domain not allowed".to_string(),
            Self::Token(IdTokenError::HostedDomainMismatch) => {
                "hd claim missing or not an allowed domain".to_string()
            }
            Self::Token(IdTokenError::NonceMismatch) => {
                "nonce missing or not the one sent to Google".to_string()
            }
        }
    }
}

#[derive(Deserialize)]
struct IdClaims {
    email: Option<String>,
    /// A boolean, or in some tokens the string "true".
    email_verified: Option<serde_json::Value>,
    hd: Option<String>,
    nonce: Option<String>,
}

/// Check a Google ID token against `jwks` and return its normalised email.
pub fn verify_id_token(
    token: &str,
    jwks: &JwkSet,
    config: &GoogleAuthConfig,
) -> Result<String, IdTokenError> {
    verify_id_token_for(token, jwks, config, None)
}

/// Compare two strings in time that depends only on their lengths.
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// [`verify_id_token`], and when `expected_nonce` is given the token must also
/// carry exactly that `nonce`. Every sign-in path's token goes through here.
pub fn verify_id_token_for(
    token: &str,
    jwks: &JwkSet,
    config: &GoogleAuthConfig,
    expected_nonce: Option<&str>,
) -> Result<String, IdTokenError> {
    let header = decode_header(token).map_err(|e| IdTokenError::Invalid(e.to_string()))?;
    let kid = header
        .kid
        .ok_or_else(|| IdTokenError::Invalid("token has no kid".to_string()))?;
    let jwk = jwks.find(&kid).ok_or(IdTokenError::UnknownKey)?;
    let key = DecodingKey::from_jwk(jwk).map_err(|e| IdTokenError::Invalid(e.to_string()))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[&config.client_id]);
    validation.set_issuer(&GOOGLE_ISSUERS);
    validation.set_required_spec_claims(&["exp", "iss", "aud"]);

    let claims = decode::<IdClaims>(token, &key, &validation)
        .map_err(|e| IdTokenError::Invalid(e.to_string()))?
        .claims;

    let verified = match &claims.email_verified {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s == "true",
        _ => false,
    };
    if !verified {
        return Err(IdTokenError::EmailNotVerified);
    }

    let email = normalize_email(claims.email.as_deref().unwrap_or_default());
    let Some((local, domain)) = email.rsplit_once('@') else {
        return Err(IdTokenError::Invalid("token has no email".to_string()));
    };
    if local.is_empty() || !config.allowed_domains.iter().any(|d| d == domain) {
        return Err(IdTokenError::DomainNotAllowed);
    }
    if config.require_hd {
        // `hd` is the Workspace the account belongs to. It need not be the
        // email's own domain: for a user on a secondary domain Google may
        // report the organisation's primary one. A consumer account has none.
        let hd = claims
            .hd
            .as_deref()
            .map(|hd| hd.trim().to_ascii_lowercase());
        let hd_allowed = |hd: &str| config.allowed_domains.iter().any(|d| d == hd);
        if !hd.as_deref().is_some_and(hd_allowed) {
            return Err(IdTokenError::HostedDomainMismatch);
        }
    }
    if let Some(expected) = expected_nonce {
        let carried = claims.nonce.as_deref().unwrap_or_default();
        if expected.is_empty() || !constant_time_eq(carried, expected) {
            return Err(IdTokenError::NonceMismatch);
        }
    }
    Ok(email)
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())
}

/// Redeem a popup-flow authorization code at Google's token endpoint and
/// return everything it answers with. Sign-in asks only for an ID token; a
/// caller that requested more scopes or offline access finds the access and
/// refresh tokens in the same response.
pub async fn exchange_code(
    config: &GoogleAuthConfig,
    code: &str,
) -> Result<GoogleTokenResponse, SignInError> {
    let unavailable = |e: String| SignInError::GoogleUnavailable(e);
    let response = http_client()
        .map_err(unavailable)?
        .post(&config.token_url)
        .form(&[
            ("code", code),
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("redirect_uri", POPUP_REDIRECT_URI),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| unavailable(e.without_url().to_string()))?;

    let status = response.status();
    if status.is_client_error() {
        // Google's body is `{"error": "invalid_grant", ...}`; only that short
        // code goes to the log.
        #[derive(Deserialize)]
        struct GoogleError {
            error: Option<String>,
        }
        let reason = response
            .json::<GoogleError>()
            .await
            .ok()
            .and_then(|e| e.error)
            .map(|e| e.chars().take(60).collect::<String>())
            .unwrap_or_else(|| "no error code".to_string());
        return Err(SignInError::CodeRefused(format!("{status}: {reason}")));
    }
    if !status.is_success() {
        return Err(unavailable(format!("token endpoint answered {status}")));
    }
    response
        .json::<GoogleTokenResponse>()
        .await
        .map_err(|e| unavailable(e.without_url().to_string()))
}

async fn fetch_jwks(config: &GoogleAuthConfig) -> Result<JwkSet, String> {
    http_client()?
        .get(&config.jwks_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.without_url().to_string())?
        .json::<JwkSet>()
        .await
        .map_err(|e| e.without_url().to_string())
}

async fn jwks(config: &GoogleAuthConfig, force_refresh: bool) -> Result<JwkSet, String> {
    if !force_refresh
        && let Some((fetched, set)) = JWKS.read().await.get(&config.jwks_url)
        && fetched.elapsed() < JWKS_TTL
    {
        return Ok(set.clone());
    }
    let set = fetch_jwks(config).await?;
    JWKS.write()
        .await
        .insert(config.jwks_url.clone(), (Instant::now(), set.clone()));
    Ok(set)
}

/// Check a Google ID token against Google's published keys (cached, refetched
/// once when the token names a key the cache lacks) and return its normalised
/// email. `expected_nonce` is the nonce the caller sent to Google, when its
/// flow has one. The one check behind `/auth/google` and
/// `/auth/connector/google`.
pub async fn verified_id_token(
    token: &str,
    config: &GoogleAuthConfig,
    expected_nonce: Option<&str>,
) -> Result<String, SignInError> {
    let keys_error = |e: String| SignInError::GoogleUnavailable(format!("fetching keys: {e}"));
    let set = jwks(config, false).await.map_err(keys_error)?;
    match verify_id_token_for(token, &set, config, expected_nonce) {
        Err(IdTokenError::UnknownKey) => {
            let set = jwks(config, true).await.map_err(keys_error)?;
            verify_id_token_for(token, &set, config, expected_nonce).map_err(SignInError::Token)
        }
        other => other.map_err(SignInError::Token),
    }
}

/// The whole check, short of touching accounts: the request is the popup
/// flow's, the code redeems, and the ID token names an allowed address.
async fn verified_email(
    config: &GoogleAuthConfig,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<String, SignInError> {
    if !headers.contains_key(AJAX_HEADER) {
        return Err(SignInError::NotAjax);
    }
    let request: GoogleLoginRequest =
        serde_json::from_slice(body).map_err(|_| SignInError::BadRequest)?;
    let code = request.code.trim();
    if code.is_empty() {
        return Err(SignInError::BadRequest);
    }

    let tokens = exchange_code(config, code).await?;
    let id_token = tokens
        .id_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SignInError::CodeRefused("Google returned no ID token".to_string()))?;
    verified_id_token(&id_token, config, None).await
}

/// Sign in with a Google account from an allowed domain.
#[utoipa::path(
    post,
    path = "/auth/google",
    request_body = GoogleLoginRequest,
    responses(
        (status = 200, description = "Signed in", body = AuthResponse),
        (status = 400, description = "Missing X-Requested-With header or code"),
        (status = 401, description = "Google refused the code or the ID token is invalid"),
        (status = 403, description = "Account not allowed (domain, unverified email) or disabled"),
        (status = 404, description = "Google sign-in is not enabled"),
        (status = 502, description = "Google could not be reached"),
    ),
    tag = "Authentication"
)]
pub async fn google_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(config) = CONFIG.as_ref() else {
        return json_error(StatusCode::NOT_FOUND, "Google sign-in is not enabled");
    };
    match verified_email(config, &headers, &body).await {
        Ok(email) => sign_in_verified_email(&state, &email, "Google").await,
        Err(reason) => {
            tracing::warn!("Google sign-in refused: {}", reason.log_detail());
            json_error(reason.status(), reason.public_message())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::{
        Form, Json, Router,
        routing::{get, post},
    };
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use openssl::rsa::Rsa;
    use serde_json::{Value, json};

    const CLIENT_ID: &str = "client-1.apps.googleusercontent.com";
    const SECRET: &str = "test-secret";
    const DOMAINS: &str = "leadershipnowproject.com,leadershipnowproject.org";
    const ISS: &str = "https://accounts.google.com";

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

    fn config_with(require_hd: bool) -> GoogleAuthConfig {
        GoogleAuthConfig::from_values(
            Some(CLIENT_ID.into()),
            Some(SECRET.into()),
            Some(DOMAINS.into()),
            require_hd,
        )
        .unwrap()
    }

    fn config() -> GoogleAuthConfig {
        config_with(true)
    }

    fn token(key: &TestKey, kid: &str, claims: Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        encode(&header, &claims, &key.encoding).unwrap()
    }

    /// A token Google would issue to a Workspace user at `email`'s domain.
    fn claims(email: &str) -> Value {
        let domain = email.rsplit_once('@').unwrap().1.to_ascii_lowercase();
        json!({
            "iss": ISS,
            "aud": CLIENT_ID,
            "exp": chrono::Utc::now().timestamp() + 300,
            "sub": "1234567890",
            "email": email,
            "email_verified": true,
            "hd": domain,
        })
    }

    fn verify(claims: Value, config: &GoogleAuthConfig) -> Result<String, IdTokenError> {
        let key = test_key("k1");
        verify_id_token(&token(&key, "k1", claims), &key.jwks, config)
    }

    // --- configuration ---------------------------------------------------

    #[test]
    fn config_is_off_unless_id_secret_and_a_domain_are_all_set() {
        let v = |s: &str| Some(s.to_string());
        let on = |id, secret, domains| GoogleAuthConfig::from_values(id, secret, domains, true);
        assert!(on(v(CLIENT_ID), v(SECRET), v(DOMAINS)).is_some());
        assert!(on(None, v(SECRET), v(DOMAINS)).is_none());
        assert!(on(v(CLIENT_ID), None, v(DOMAINS)).is_none());
        assert!(on(v(CLIENT_ID), v(SECRET), None).is_none());
        assert!(on(v("  "), v(SECRET), v(DOMAINS)).is_none());
        assert!(on(v(CLIENT_ID), v(" "), v(DOMAINS)).is_none());
        assert!(on(v(CLIENT_ID), v(SECRET), v(" , ")).is_none());
    }

    #[test]
    fn config_lowercases_domains_and_defaults_to_googles_endpoints() {
        let c = GoogleAuthConfig::from_values(
            Some(CLIENT_ID.into()),
            Some(SECRET.into()),
            Some("LeadershipNowProject.com , leadershipnowproject.ORG,".into()),
            true,
        )
        .unwrap();
        assert_eq!(
            c.allowed_domains,
            vec!["leadershipnowproject.com", "leadershipnowproject.org"]
        );
        assert_eq!(c.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(c.jwks_url, "https://www.googleapis.com/oauth2/v3/certs");
    }

    // --- ID token --------------------------------------------------------

    #[test]
    fn valid_token_yields_normalised_email_for_both_domains() {
        assert_eq!(
            verify(claims("Jane@LeadershipNowProject.org"), &config()),
            Ok("jane@leadershipnowproject.org".to_string())
        );
        assert_eq!(
            verify(claims("sam@leadershipnowproject.com"), &config()),
            Ok("sam@leadershipnowproject.com".to_string())
        );
    }

    #[test]
    fn both_issuer_spellings_are_accepted() {
        let mut c = claims("a@leadershipnowproject.com");
        c["iss"] = json!("accounts.google.com");
        assert!(verify(c, &config()).is_ok());
    }

    #[test]
    fn wrong_audience_is_refused() {
        let mut c = claims("a@leadershipnowproject.com");
        c["aud"] = json!("someone-else.apps.googleusercontent.com");
        assert!(matches!(
            verify(c, &config()),
            Err(IdTokenError::Invalid(_))
        ));
    }

    #[test]
    fn wrong_issuer_is_refused() {
        let mut c = claims("a@leadershipnowproject.com");
        c["iss"] = json!("https://accounts.example.com");
        assert!(matches!(
            verify(c, &config()),
            Err(IdTokenError::Invalid(_))
        ));
    }

    #[test]
    fn expired_token_is_refused() {
        let mut c = claims("a@leadershipnowproject.com");
        c["exp"] = json!(chrono::Utc::now().timestamp() - 600);
        assert!(matches!(
            verify(c, &config()),
            Err(IdTokenError::Invalid(_))
        ));
    }

    #[test]
    fn token_signed_by_another_key_is_refused() {
        let real = test_key("k1");
        let forged = test_key("k1");
        let t = token(&forged, "k1", claims("a@leadershipnowproject.com"));
        assert!(matches!(
            verify_id_token(&t, &real.jwks, &config()),
            Err(IdTokenError::Invalid(_))
        ));
    }

    #[test]
    fn unknown_kid_asks_for_a_refetch() {
        let key = test_key("k2");
        let other = test_key("k1");
        let t = token(&key, "k2", claims("a@leadershipnowproject.com"));
        assert_eq!(
            verify_id_token(&t, &other.jwks, &config()),
            Err(IdTokenError::UnknownKey)
        );
    }

    #[test]
    fn unverified_email_is_refused() {
        for value in [json!(false), json!("false"), json!(null), json!(1)] {
            let mut c = claims("a@leadershipnowproject.com");
            c["email_verified"] = value;
            assert_eq!(verify(c, &config()), Err(IdTokenError::EmailNotVerified));
        }
        let mut c = claims("a@leadershipnowproject.com");
        c.as_object_mut().unwrap().remove("email_verified");
        assert_eq!(verify(c, &config()), Err(IdTokenError::EmailNotVerified));
    }

    #[test]
    fn email_verified_as_the_string_true_is_accepted() {
        let mut c = claims("a@leadershipnowproject.com");
        c["email_verified"] = json!("true");
        assert!(verify(c, &config()).is_ok());
    }

    #[test]
    fn email_outside_the_allowed_domains_is_refused() {
        // A Workspace account of another organisation: hd matches its own
        // domain, so only the allow-list stands in its way.
        for email in [
            "a@gmail.com",
            "a@example.org",
            "a@evil-leadershipnowproject.com",
            "a@leadershipnowproject.com.evil.test",
        ] {
            assert_eq!(
                verify(claims(email), &config()),
                Err(IdTokenError::DomainNotAllowed),
                "{email}"
            );
            assert_eq!(
                verify(claims(email), &config_with(false)),
                Err(IdTokenError::DomainNotAllowed),
                "{email}"
            );
        }
    }

    #[test]
    fn missing_hd_is_refused() {
        // A consumer Google account registered with a staff-looking address.
        let mut c = claims("a@leadershipnowproject.com");
        c.as_object_mut().unwrap().remove("hd");
        assert_eq!(
            verify(c, &config()),
            Err(IdTokenError::HostedDomainMismatch)
        );
    }

    #[test]
    fn hd_outside_the_allowed_domains_is_refused() {
        for hd in [
            "example.org",
            "gmail.com",
            "evil-leadershipnowproject.com",
            "",
        ] {
            let mut c = claims("a@leadershipnowproject.com");
            c["hd"] = json!(hd);
            assert_eq!(
                verify(c, &config()),
                Err(IdTokenError::HostedDomainMismatch),
                "{hd}"
            );
        }
    }

    #[test]
    fn hd_may_be_another_allowed_domain_than_the_emails() {
        // Google may report the Workspace's primary domain for a user whose
        // address is on a secondary one.
        let mut c = claims("jane@leadershipnowproject.org");
        c["hd"] = json!("leadershipnowproject.com");
        assert_eq!(
            verify(c, &config()),
            Ok("jane@leadershipnowproject.org".to_string())
        );
        let mut c = claims("sam@leadershipnowproject.com");
        c["hd"] = json!("LeadershipNowProject.ORG");
        assert!(verify(c, &config()).is_ok());
    }

    #[test]
    fn an_allowed_hd_does_not_admit_an_email_outside_the_allowed_domains() {
        let mut c = claims("a@gmail.com");
        c["hd"] = json!("leadershipnowproject.com");
        assert_eq!(verify(c, &config()), Err(IdTokenError::DomainNotAllowed));
    }

    #[test]
    fn hd_is_not_checked_when_the_requirement_is_switched_off() {
        let relaxed = config_with(false);
        let mut c = claims("a@leadershipnowproject.com");
        c.as_object_mut().unwrap().remove("hd");
        assert_eq!(
            verify(c, &relaxed),
            Ok("a@leadershipnowproject.com".to_string())
        );
        let mut c = claims("a@leadershipnowproject.com");
        c["hd"] = json!("example.org");
        assert!(verify(c, &relaxed).is_ok());
    }

    #[test]
    fn a_nonce_is_checked_only_when_the_caller_expects_one() {
        let key = test_key("k1");
        let check = |claims: Value, expected: Option<&str>| {
            verify_id_token_for(&token(&key, "k1", claims), &key.jwks, &config(), expected)
        };
        let mut with_nonce = claims("a@leadershipnowproject.com");
        with_nonce["nonce"] = json!("n-123");
        let without = claims("a@leadershipnowproject.com");

        assert!(check(with_nonce.clone(), Some("n-123")).is_ok());
        // The popup flow sends no nonce and expects none.
        assert!(check(without.clone(), None).is_ok());
        assert!(check(with_nonce.clone(), None).is_ok());
        for wrong in ["n-124", "N-123", "n-12", "n-1234", ""] {
            assert_eq!(
                check(with_nonce.clone(), Some(wrong)),
                Err(IdTokenError::NonceMismatch),
                "{wrong:?}"
            );
        }
        assert_eq!(
            check(without.clone(), Some("n-123")),
            Err(IdTokenError::NonceMismatch)
        );
        // An empty expectation never matches a token without a nonce.
        assert_eq!(check(without, Some("")), Err(IdTokenError::NonceMismatch));
        // The nonce is the last check: a refused account is still refused.
        let mut outsider = claims("a@gmail.com");
        outsider["nonce"] = json!("n-123");
        assert_eq!(
            check(outsider, Some("n-123")),
            Err(IdTokenError::DomainNotAllowed)
        );
        assert_eq!(
            SignInError::Token(IdTokenError::NonceMismatch).status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn refusals_map_to_statuses_and_never_say_why_in_public() {
        let forbidden = [
            IdTokenError::EmailNotVerified,
            IdTokenError::DomainNotAllowed,
            IdTokenError::HostedDomainMismatch,
        ];
        for e in forbidden {
            let e = SignInError::Token(e);
            assert_eq!(e.status(), StatusCode::FORBIDDEN);
            assert_eq!(e.public_message(), "This Google account is not allowed");
        }
        for e in [
            SignInError::Token(IdTokenError::UnknownKey),
            SignInError::Token(IdTokenError::Invalid("x".into())),
            SignInError::CodeRefused("x".into()),
        ] {
            assert_eq!(e.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(e.public_message(), "Google sign-in failed");
        }
        assert_eq!(SignInError::NotAjax.status(), StatusCode::BAD_REQUEST);
        assert_eq!(SignInError::BadRequest.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            SignInError::GoogleUnavailable("x".into()).status(),
            StatusCode::BAD_GATEWAY
        );
    }

    // --- the code exchange, against a local fake Google --------------------

    /// What the fake token endpoint answers, and what it was sent.
    #[derive(Clone)]
    struct Fake {
        token_status: Arc<Mutex<StatusCode>>,
        token_body: Arc<Mutex<Value>>,
        jwks: Arc<Mutex<JwkSet>>,
        token_forms: Arc<Mutex<Vec<HashMap<String, String>>>>,
        jwks_fetches: Arc<Mutex<usize>>,
    }

    impl Fake {
        fn forms(&self) -> Vec<HashMap<String, String>> {
            self.token_forms.lock().unwrap().clone()
        }
    }

    /// Start a fake Google answering the token endpoint with `id_token` and
    /// serving `jwks`; returns it with a config pointed at it.
    async fn fake_google(id_token: Option<String>, jwks: JwkSet) -> (Fake, GoogleAuthConfig) {
        let body = match id_token {
            Some(t) => json!({ "id_token": t, "access_token": "at", "expires_in": 3599 }),
            None => json!({ "access_token": "at", "expires_in": 3599 }),
        };
        let fake = Fake {
            token_status: Arc::new(Mutex::new(StatusCode::OK)),
            token_body: Arc::new(Mutex::new(body)),
            jwks: Arc::new(Mutex::new(jwks)),
            token_forms: Arc::new(Mutex::new(Vec::new())),
            jwks_fetches: Arc::new(Mutex::new(0)),
        };
        let for_token = fake.clone();
        let for_certs = fake.clone();
        let app = Router::new()
            .route(
                "/token",
                post(move |Form(form): Form<HashMap<String, String>>| {
                    let fake = for_token.clone();
                    async move {
                        fake.token_forms.lock().unwrap().push(form);
                        let status = *fake.token_status.lock().unwrap();
                        let body = fake.token_body.lock().unwrap().clone();
                        (status, Json(body))
                    }
                }),
            )
            .route(
                "/certs",
                get(move || {
                    let fake = for_certs.clone();
                    async move {
                        *fake.jwks_fetches.lock().unwrap() += 1;
                        let set = fake.jwks.lock().unwrap().clone();
                        Json(set)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut config = config();
        config.token_url = format!("http://{addr}/token");
        config.jwks_url = format!("http://{addr}/certs");
        (fake, config)
    }

    fn ajax() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("X-Requested-With", "XmlHttpRequest".parse().unwrap());
        headers
    }

    const BODY: &[u8] = br#"{"code":"4/the-code"}"#;

    #[tokio::test]
    async fn a_good_code_signs_in_and_is_redeemed_the_popup_way() {
        let key = test_key("k1");
        let t = token(&key, "k1", claims("Jane@leadershipnowproject.org"));
        let (fake, config) = fake_google(Some(t), key.jwks.clone()).await;

        assert_eq!(
            verified_email(&config, &ajax(), BODY).await,
            Ok("jane@leadershipnowproject.org".to_string())
        );

        let forms = fake.forms();
        assert_eq!(forms.len(), 1);
        let form = &forms[0];
        assert_eq!(form["redirect_uri"], "postmessage");
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "4/the-code");
        assert_eq!(form["client_id"], CLIENT_ID);
        assert_eq!(form["client_secret"], SECRET);
    }

    #[tokio::test]
    async fn a_request_without_x_requested_with_is_refused_before_google_is_asked() {
        let key = test_key("k1");
        let t = token(&key, "k1", claims("a@leadershipnowproject.com"));
        let (fake, config) = fake_google(Some(t), key.jwks.clone()).await;

        let refused = verified_email(&config, &HeaderMap::new(), BODY).await;
        assert_eq!(refused, Err(SignInError::NotAjax));
        assert!(fake.forms().is_empty());
    }

    #[tokio::test]
    async fn a_body_without_a_code_is_a_bad_request() {
        let key = test_key("k1");
        let (fake, config) = fake_google(None, key.jwks.clone()).await;
        for body in [&b"not json"[..], br#"{}"#, br#"{"code":"  "}"#] {
            assert_eq!(
                verified_email(&config, &ajax(), body).await,
                Err(SignInError::BadRequest)
            );
        }
        assert!(fake.forms().is_empty());
    }

    #[tokio::test]
    async fn a_code_google_refuses_is_unauthorized_and_logs_only_the_error_code() {
        let key = test_key("k1");
        let (fake, config) = fake_google(None, key.jwks.clone()).await;
        *fake.token_status.lock().unwrap() = StatusCode::BAD_REQUEST;
        *fake.token_body.lock().unwrap() =
            json!({ "error": "invalid_grant", "error_description": "Bad Request" });

        let refused = verified_email(&config, &ajax(), BODY).await.unwrap_err();
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            refused,
            SignInError::CodeRefused("400 Bad Request: invalid_grant".to_string())
        );
        assert!(!refused.log_detail().contains("the-code"));
        assert!(!refused.log_detail().contains(SECRET));
    }

    #[tokio::test]
    async fn a_failing_token_endpoint_is_a_bad_gateway() {
        let key = test_key("k1");
        let (fake, config) = fake_google(None, key.jwks.clone()).await;
        *fake.token_status.lock().unwrap() = StatusCode::INTERNAL_SERVER_ERROR;

        let refused = verified_email(&config, &ajax(), BODY).await.unwrap_err();
        assert!(matches!(refused, SignInError::GoogleUnavailable(_)));
        assert_eq!(refused.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn an_unreachable_google_is_a_bad_gateway() {
        // Bind and drop, so the port is closed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut config = config();
        config.token_url = format!("http://{addr}/token");
        config.jwks_url = format!("http://{addr}/certs");

        let refused = verified_email(&config, &ajax(), BODY).await.unwrap_err();
        assert_eq!(refused.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn an_answer_without_an_id_token_is_refused() {
        let key = test_key("k1");
        let (_fake, config) = fake_google(None, key.jwks.clone()).await;

        let refused = verified_email(&config, &ajax(), BODY).await.unwrap_err();
        assert!(matches!(refused, SignInError::CodeRefused(_)));
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_id_token_for_a_refused_account_is_forbidden_through_the_whole_flow() {
        let key = test_key("k1");
        let t = token(&key, "k1", claims("a@gmail.com"));
        let (_fake, config) = fake_google(Some(t), key.jwks.clone()).await;

        let refused = verified_email(&config, &ajax(), BODY).await.unwrap_err();
        assert_eq!(refused, SignInError::Token(IdTokenError::DomainNotAllowed));
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_forged_id_token_is_unauthorized_through_the_whole_flow() {
        let real = test_key("k1");
        let forged = test_key("k1");
        let t = token(&forged, "k1", claims("a@leadershipnowproject.com"));
        let (_fake, config) = fake_google(Some(t), real.jwks.clone()).await;

        let refused = verified_email(&config, &ajax(), BODY).await.unwrap_err();
        assert!(matches!(
            refused,
            SignInError::Token(IdTokenError::Invalid(_))
        ));
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn keys_are_cached_and_refetched_once_when_google_rotates_them() {
        let old = test_key("k1");
        let new = test_key("k2");
        let first = token(&old, "k1", claims("a@leadershipnowproject.com"));
        let (fake, config) = fake_google(Some(first), old.jwks.clone()).await;

        assert!(verified_email(&config, &ajax(), BODY).await.is_ok());
        assert!(verified_email(&config, &ajax(), BODY).await.is_ok());
        assert_eq!(
            *fake.jwks_fetches.lock().unwrap(),
            1,
            "second use is cached"
        );

        // Google rotates: the next token names a key the cache lacks.
        *fake.jwks.lock().unwrap() = new.jwks.clone();
        let second = token(&new, "k2", claims("a@leadershipnowproject.com"));
        *fake.token_body.lock().unwrap() = json!({ "id_token": second });
        assert!(verified_email(&config, &ajax(), BODY).await.is_ok());
        assert_eq!(*fake.jwks_fetches.lock().unwrap(), 2);

        // A token naming a key Google does not publish costs one refetch and
        // is refused.
        let stranger = test_key("k9");
        let third = token(&stranger, "k9", claims("a@leadershipnowproject.com"));
        *fake.token_body.lock().unwrap() = json!({ "id_token": third });
        assert_eq!(
            verified_email(&config, &ajax(), BODY).await,
            Err(SignInError::Token(IdTokenError::UnknownKey))
        );
        assert_eq!(*fake.jwks_fetches.lock().unwrap(), 3);
    }
}
