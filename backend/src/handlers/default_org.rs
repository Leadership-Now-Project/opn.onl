//! The shared default organization: one team every staff account joins.
//!
//! Off unless `DEFAULT_ORG_NAME` and `DEFAULT_ORG_SLUG` are both set: without
//! them an instance behaves as upstream. With them set:
//!
//! - the organization with that slug is created the first time someone signs
//!   in through a verified-email provider (`verified_sign_in.rs`);
//! - every account that signs in that way becomes a member: `editor`, or
//!   `admin` when its address is listed in `DEFAULT_ORG_ADMINS`. A listed
//!   member below admin is promoted. Nobody is ever demoted, so a role an
//!   admin set by hand in the Teams page stays;
//! - the member's personal links, folders and tags move into the
//!   organization at that same moment, so links made before this existed are
//!   in the shared list;
//! - for a member who may edit, the organization takes the place of the
//!   personal scope ([`home_org`]): a link, folder or tag created without an
//!   `org_id` lands in it, and `GET /folders` and `GET /tags` without one list
//!   its folders and tags. A client that never sends `org_id` (an API key, the
//!   Claude connector) therefore keeps working after its links have moved.
//!
//! Who may then read, change or delete a link is not decided here: it is the
//! ordinary organization rule in `link_access.rs`.
//!
//! The settings are read on every call, not once per process. They are three
//! environment lookups at sign-in, and the tests switch them per test.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr,
    EntityTrait, QueryFilter, Set, Statement, TransactionTrait,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::AppState;
use crate::entity::{folders, org_members, organizations, tags, users};
use crate::utils::email_domain_policy::{live_users_by_email, normalize_email};
use crate::utils::env::env_nonempty;
use crate::utils::validation::{check_name, check_slug};

/// Serializes creating the default organization, so two first sign-ins at the
/// same moment make one organization with one owner.
const DEFAULT_ORG_LOCK: i64 = 1_557_021_030;

/// `DEFAULT_ORG_*` as configured.
pub struct DefaultOrgConfig {
    pub name: String,
    pub slug: String,
    /// Normalised addresses (`normalize_email`), in the order written.
    pub admins: Vec<String>,
}

impl DefaultOrgConfig {
    fn lists_as_admin(&self, email: &str) -> bool {
        let email = normalize_email(email);
        self.admins.contains(&email)
    }
}

/// The configured default organization, or `None` when the feature is off:
/// name or slug unset or blank, or not a name and slug the Teams page would
/// accept (reported once; a typo must not half-enable the feature).
pub fn configured() -> Option<DefaultOrgConfig> {
    let name = env_nonempty("DEFAULT_ORG_NAME")?.trim().to_string();
    let slug = env_nonempty("DEFAULT_ORG_SLUG")?.trim().to_string();
    if let Err(problem) = check_name("DEFAULT_ORG_NAME", &name).and_then(|()| check_slug(&slug)) {
        static REPORTED: std::sync::Once = std::sync::Once::new();
        REPORTED.call_once(|| {
            tracing::warn!(
                "DEFAULT_ORG_NAME / DEFAULT_ORG_SLUG ignored, the default organization is off: {problem}"
            );
        });
        return None;
    }
    let admins = env_nonempty("DEFAULT_ORG_ADMINS")
        .map(|list| {
            list.split(',')
                .map(normalize_email)
                .filter(|email| !email.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(DefaultOrgConfig { name, slug, admins })
}

async fn find_by_slug<C: ConnectionTrait>(
    db: &C,
    slug: &str,
) -> Result<Option<organizations::Model>, DbErr> {
    organizations::Entity::find()
        .filter(organizations::Column::Slug.eq(slug))
        .one(db)
        .await
}

/// What `GET /auth/settings` says about the default organization.
#[derive(Serialize, ToSchema)]
pub struct DefaultOrgInfo {
    pub id: i32,
    pub name: String,
}

/// The default organization for `GET /auth/settings`: `None` when the feature
/// is off, or on but nobody has signed in yet (it does not exist until then).
pub async fn settings_info(db: &DatabaseConnection) -> Option<DefaultOrgInfo> {
    let config = configured()?;
    match find_by_slug(db, &config.slug).await {
        Ok(org) => org.map(|org| DefaultOrgInfo {
            id: org.id,
            name: org.name,
        }),
        Err(error) => {
            tracing::error!("Failed to read the default organization: {}", error);
            None
        }
    }
}

/// The organization that stands in for `user_id`'s personal scope when a
/// request names no `org_id`: the default organization when they are a member
/// who may edit there, else `None` (personal, as upstream).
pub async fn home_org<C: ConnectionTrait>(db: &C, user_id: i32) -> Option<i32> {
    let config = configured()?;
    let org = find_by_slug(db, &config.slug).await.ok().flatten()?;
    let member = org_members::Entity::find()
        .filter(org_members::Column::OrgId.eq(org.id))
        .filter(org_members::Column::UserId.eq(user_id))
        .one(db)
        .await
        .ok()
        .flatten()?;
    member.can_edit().then_some(org.id)
}

/// Make `user` a member of the default organization and bring their personal
/// links into it. Called after every verified-email sign-in; does nothing when
/// the feature is off. Safe to repeat: a second call changes nothing.
pub async fn join_at_sign_in(state: &AppState, user: &users::Model) -> Result<(), DbErr> {
    let Some(config) = configured() else {
        return Ok(());
    };
    let org = match find_by_slug(&state.db, &config.slug).await? {
        Some(org) => org,
        None => create(&state.db, &config, user).await?,
    };
    let member = ensure_member(
        &state.db,
        org.id,
        user.id,
        config.lists_as_admin(&user.email),
    )
    .await?;
    // A member who may not edit (a viewer, set by hand) keeps their personal
    // links: moving them would take away the right to change their own.
    if member.can_edit() {
        let codes = move_personal_resources(&state.db, org.id, user.id).await?;
        // The redirect cache holds each link's `org_id`.
        crate::handlers::links::invalidate_cached_codes(state, &codes).await;
    }
    Ok(())
}

/// Create the organization, unless a concurrent sign-in just did.
///
/// An organization must have an owner, and its links answer to that account
/// (`utils/link_owner.rs`). The owner is the first address in
/// `DEFAULT_ORG_ADMINS` that already has a usable account; only when none does
/// is it the account signing in.
async fn create(
    db: &DatabaseConnection,
    config: &DefaultOrgConfig,
    signing_in: &users::Model,
) -> Result<organizations::Model, DbErr> {
    let txn = db.begin().await?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock($1)",
        [DEFAULT_ORG_LOCK.into()],
    ))
    .await?;
    if let Some(org) = find_by_slug(&txn, &config.slug).await? {
        txn.commit().await?;
        return Ok(org);
    }

    let mut owner_id = signing_in.id;
    for admin in &config.admins {
        let account = live_users_by_email(admin)
            .filter(users::Column::DisabledAt.is_null())
            .one(&txn)
            .await?;
        if let Some(account) = account {
            owner_id = account.id;
            break;
        }
    }

    let org = organizations::ActiveModel {
        name: Set(config.name.clone()),
        slug: Set(config.slug.clone()),
        owner_id: Set(owner_id),
        ..Default::default()
    }
    .insert(&txn)
    .await?;
    org_members::ActiveModel {
        org_id: Set(org.id),
        user_id: Set(owner_id),
        role: Set("owner".to_string()),
        ..Default::default()
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    tracing::info!(org_id = org.id, owner_id, "Default organization created");
    Ok(org)
}

/// The membership of `user_id`, created as an editor if missing. A member
/// below admin who is `listed_admin` is then promoted, new or not; every other
/// existing role is left as it is.
async fn ensure_member(
    db: &DatabaseConnection,
    org_id: i32,
    user_id: i32,
    listed_admin: bool,
) -> Result<org_members::Model, DbErr> {
    let membership = || {
        org_members::Entity::find()
            .filter(org_members::Column::OrgId.eq(org_id))
            .filter(org_members::Column::UserId.eq(user_id))
    };
    let member = match membership().one(db).await? {
        Some(member) => member,
        None => {
            // The unique (org_id, user_id) index settles two sign-ins of one
            // account racing: the second insert is skipped.
            org_members::Entity::insert(org_members::ActiveModel {
                org_id: Set(org_id),
                user_id: Set(user_id),
                role: Set("editor".to_string()),
                ..Default::default()
            })
            .on_conflict(
                OnConflict::columns([org_members::Column::OrgId, org_members::Column::UserId])
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
            membership().one(db).await?.ok_or_else(|| {
                DbErr::RecordNotFound("default organization membership".to_string())
            })?
        }
    };
    if listed_admin && !member.is_admin() {
        let mut promoted: org_members::ActiveModel = member.into();
        promoted.role = Set("admin".to_string());
        return promoted.update(db).await;
    }
    Ok(member)
}

/// Move the member's personal folders, tags and live links into the
/// organization, in one transaction. Returns the codes of the links moved.
///
/// Folders and tags go too because a link may only use those of its own scope
/// (`links::validate_link_resource_scope`): left behind, a moved link would
/// point at a folder nobody can pick any more. A soft-deleted link is not
/// moved; it stays where the retention task expects it.
async fn move_personal_resources(
    db: &DatabaseConnection,
    org_id: i32,
    user_id: i32,
) -> Result<Vec<String>, DbErr> {
    let txn = db.begin().await?;
    folders::Entity::update_many()
        .col_expr(folders::Column::OrgId, Expr::value(org_id))
        .col_expr(folders::Column::UserId, Expr::value(Option::<i32>::None))
        .filter(folders::Column::UserId.eq(user_id))
        .filter(folders::Column::OrgId.is_null())
        .exec(&txn)
        .await?;
    tags::Entity::update_many()
        .col_expr(tags::Column::OrgId, Expr::value(org_id))
        .col_expr(tags::Column::UserId, Expr::value(Option::<i32>::None))
        .filter(tags::Column::UserId.eq(user_id))
        .filter(tags::Column::OrgId.is_null())
        .exec(&txn)
        .await?;
    let moved = txn
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE links SET org_id = $1 \
             WHERE user_id = $2 AND org_id IS NULL AND deleted_at IS NULL \
             RETURNING code",
            [org_id.into(), user_id.into()],
        ))
        .await?;
    txn.commit().await?;
    moved
        .iter()
        .map(|row| row.try_get::<String>("", "code"))
        .collect()
}
