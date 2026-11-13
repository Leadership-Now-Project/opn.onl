//! Sign-in for an email address an identity provider has already verified.
//!
//! `POST /auth/cf-access` and `POST /auth/google` each check their provider's
//! token and then call [`sign_in_verified_email`], so the two cannot drift on
//! what happens next: which account the address maps to, when one is created,
//! who becomes the first admin, and who is refused.
//!
//! Accounts created here are verified (the provider vouched for the address)
//! and get a random password nobody knows. The first live account becomes
//! admin, as in `/auth/register`, under the same advisory lock. A soft-deleted
//! account is never revived: `live_users_by_email` does not see it.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::*;

use crate::AppState;
use crate::entity::users;
use crate::handlers::auth::{AuthResponse, FIRST_ACCOUNT_ADMIN_LOCK};
use crate::handlers::json_error;
use crate::utils::email::generate_token;
use crate::utils::email_domain_policy::{
    ensure_email_domain_allowed, ensure_sign_in_domain_allowed, live_users_by_email,
};
use crate::utils::jwt::{create_jwt, hash_password};

/// Answer with a session for `email`, creating the account on first sign-in.
///
/// `email` must already be normalised (`normalize_email`) and verified by the
/// caller's provider. `source` names that provider in the logs.
pub async fn sign_in_verified_email(
    state: &AppState,
    email: &str,
    source: &'static str,
) -> Response {
    let user = match find_or_create_user(&state.db, email, source).await {
        Ok(user) => user,
        Err((status, message)) => return json_error(status, message),
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
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create session",
            )
        }
    }
}

/// A refusal from [`find_or_create_user`]: status and public message.
type Refusal = (StatusCode, &'static str);

fn database_error() -> Refusal {
    (StatusCode::INTERNAL_SERVER_ERROR, "Database error")
}

async fn find_or_create_user(
    db: &DatabaseConnection,
    email: &str,
    source: &'static str,
) -> Result<users::Model, Refusal> {
    if let Some(user) = live_users_by_email(email)
        .one(db)
        .await
        .map_err(|_| database_error())?
    {
        ensure_sign_in_domain_allowed(db, email)
            .await
            .map_err(|r| (StatusCode::FORBIDDEN, r.public_message()))?;
        return Ok(user);
    }

    ensure_email_domain_allowed(db, email)
        .await
        .map_err(|r| (StatusCode::FORBIDDEN, r.public_message()))?;
    // Nobody knows this password; the provider is the way in.
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
        tracing::error!("Failed to create {source} user: {}", e);
        database_error()
    })?;
    txn.commit().await.map_err(|_| database_error())?;
    tracing::info!(user_id = user.id, "Account created through {source}");
    Ok(user)
}
