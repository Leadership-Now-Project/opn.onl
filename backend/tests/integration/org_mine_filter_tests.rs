//! `GET /links?org_id=…&mine=true`, and what an organization's list says about
//! each link: who created it and whether the caller may delete it.
//!
//! `mine` narrows the list on the server, so the count, paging and search
//! agree with it. It never widens anything: it is a filter on top of the
//! membership rule.
//!
//! Also here, because this binary runs without `DEFAULT_ORG_*`: with the
//! default organization switched off, signing in joins nothing and links stay
//! personal. The feature switched on is `integration_serial/default_org_tests.rs`.

use crate::common;

use axum::response::Response;
use common::{spawn_real_app, unique_code, unique_email};
use opn_onl_backend::AppState;
use opn_onl_backend::entity::{links, org_members, organizations, users};
use opn_onl_backend::handlers::verified_sign_in::sign_in_verified_email;
use opn_onl_backend::utils::create_jwt;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use serde_json::{Value, json};

struct Person {
    id: i32,
    email: String,
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
    Person {
        id: user.id,
        email,
        token,
    }
}

/// An organization owned by `owner`, inserted directly.
async fn organization(db: &DatabaseConnection, owner: &Person) -> i32 {
    let org = organizations::ActiveModel {
        name: Set("Mine Filter Org".to_string()),
        slug: Set(format!("mine-{}", uuid::Uuid::new_v4().simple())),
        owner_id: Set(owner.id),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert org");
    add_member(db, org.id, owner.id, "owner").await;
    org.id
}

async fn add_member(db: &DatabaseConnection, org_id: i32, user_id: i32, role: &str) {
    org_members::ActiveModel {
        org_id: Set(org_id),
        user_id: Set(user_id),
        role: Set(role.to_string()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("add org member");
}

async fn insert_link(
    db: &DatabaseConnection,
    creator: i32,
    org_id: Option<i32>,
    title: &str,
) -> i64 {
    let link = links::ActiveModel {
        code: Set(unique_code()),
        original_url: Set("https://iana.org/mine-filter".to_string()),
        user_id: Set(Some(creator)),
        org_id: Set(org_id),
        title: Set(Some(title.to_string())),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert link");
    i64::from(link.id)
}

/// `GET /links?{query}`: the ids in order, `X-Total-Count`, and the body. A
/// fresh app each time, so a test's requests never share a rate-limit bucket.
async fn list(token: &str, query: &str) -> (Vec<i64>, u64, Value) {
    let (server, _) = spawn_real_app().await;
    let res = server
        .get(&format!("/links?{query}"))
        .authorization_bearer(token)
        .await;
    assert_eq!(res.status_code(), 200, "list {query}: {}", res.text());
    let total = res
        .headers()
        .get("x-total-count")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .expect("X-Total-Count");
    let body: Value = res.json();
    let ids = body
        .as_array()
        .expect("a list")
        .iter()
        .map(|link| link["id"].as_i64().unwrap())
        .collect();
    (ids, total, body)
}

fn item(body: &Value, id: i64) -> &Value {
    body.as_array()
        .unwrap()
        .iter()
        .find(|link| link["id"] == id)
        .expect("link in the list")
}

fn sorted(mut ids: Vec<i64>) -> Vec<i64> {
    ids.sort_unstable();
    ids
}

#[tokio::test]
async fn mine_keeps_only_the_callers_links_and_the_count_paging_and_search_follow() {
    let (_server, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let a = person(&db).await;
    let b = person(&db).await;
    let org = organization(&db, &owner).await;
    add_member(&db, org, a.id, "editor").await;
    add_member(&db, org, b.id, "editor").await;
    let a1 = insert_link(&db, a.id, Some(org), "alpha report").await;
    let a2 = insert_link(&db, a.id, Some(org), "alpha budget").await;
    let b1 = insert_link(&db, b.id, Some(org), "beta report").await;
    let b2 = insert_link(&db, b.id, Some(org), "beta budget").await;
    let b3 = insert_link(&db, b.id, Some(org), "beta minutes").await;
    // B's personal link is not an organization link, mine or not.
    let b_personal = insert_link(&db, b.id, None, "beta personal").await;

    // Everyone's.
    let (ids, total, _) = list(&b.token, &format!("org_id={org}")).await;
    assert_eq!(sorted(ids), sorted(vec![a1, a2, b1, b2, b3]));
    assert_eq!(total, 5);
    // `mine=false` is the same as leaving it out.
    let (ids, total, _) = list(&b.token, &format!("org_id={org}&mine=false")).await;
    assert_eq!(sorted(ids), sorted(vec![a1, a2, b1, b2, b3]));
    assert_eq!(total, 5);

    // B's own, and the count is of those.
    let (ids, total, _) = list(&b.token, &format!("org_id={org}&mine=true")).await;
    assert_eq!(sorted(ids.clone()), sorted(vec![b1, b2, b3]));
    assert!(!ids.contains(&b_personal));
    assert_eq!(total, 3);
    // A's own.
    let (ids, total, _) = list(&a.token, &format!("org_id={org}&mine=true")).await;
    assert_eq!(sorted(ids), sorted(vec![a1, a2]));
    assert_eq!(total, 2);

    // Paging walks B's three links and nothing of A's; the total stays 3.
    let (first, total, _) = list(&b.token, &format!("org_id={org}&mine=true&limit=2")).await;
    assert_eq!((first.len(), total), (2, 3));
    let (rest, total, _) = list(
        &b.token,
        &format!("org_id={org}&mine=true&limit=2&offset=2"),
    )
    .await;
    assert_eq!((rest.len(), total), (1, 3));
    assert_eq!(
        sorted([first, rest].concat()),
        sorted(vec![b1, b2, b3]),
        "the pages together are exactly B's links"
    );

    // Search within mine, and within everyone's.
    let (ids, total, _) = list(&b.token, &format!("org_id={org}&mine=true&search=report")).await;
    assert_eq!((ids, total), (vec![b1], 1));
    let (ids, total, _) = list(&b.token, &format!("org_id={org}&search=report")).await;
    assert_eq!(sorted(ids), sorted(vec![a1, b1]));
    assert_eq!(total, 2);
}

#[tokio::test]
async fn mine_never_shows_a_non_member_anything() {
    let (_server, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let member = person(&db).await;
    let org = organization(&db, &owner).await;
    add_member(&db, org, member.id, "editor").await;
    insert_link(&db, member.id, Some(org), "members only").await;
    // Created links there, then left: the membership is gone, the rows remain.
    let former = person(&db).await;
    insert_link(&db, former.id, Some(org), "left behind").await;
    let outsider = person(&db).await;

    for who in [&former, &outsider] {
        for query in [format!("org_id={org}"), format!("org_id={org}&mine=true")] {
            let (ids, total, _) = list(&who.token, &query).await;
            assert!(ids.is_empty(), "{query}: {ids:?}");
            assert_eq!(total, 0, "{query}");
        }
    }
}

#[tokio::test]
async fn an_organization_list_names_each_creator_and_says_who_may_delete() {
    let (_server, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let admin = person(&db).await;
    let editor = person(&db).await;
    let colleague = person(&db).await;
    let viewer = person(&db).await;
    let org = organization(&db, &owner).await;
    add_member(&db, org, admin.id, "admin").await;
    add_member(&db, org, editor.id, "editor").await;
    add_member(&db, org, colleague.id, "editor").await;
    add_member(&db, org, viewer.id, "viewer").await;
    let editors = insert_link(&db, editor.id, Some(org), "by the editor").await;
    let colleagues = insert_link(&db, colleague.id, Some(org), "by a colleague").await;
    // Its creator's account has since been deleted.
    let departed = person(&db).await;
    let orphan = insert_link(&db, departed.id, Some(org), "by someone who left").await;
    let mut gone: users::ActiveModel = users::Entity::find_by_id(departed.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap()
        .into();
    gone.deleted_at = Set(Some(chrono::Utc::now().naive_utc()));
    gone.update(&db).await.unwrap();

    let (_, _, body) = list(&editor.token, &format!("org_id={org}")).await;
    assert_eq!(
        item(&body, editors)["created_by"],
        json!({ "name": null, "email": editor.email })
    );
    assert_eq!(
        item(&body, colleagues)["created_by"],
        json!({ "name": null, "email": colleague.email })
    );
    // A deleted account's address is not shown.
    assert!(item(&body, orphan).get("created_by").is_none());
    // An editor deletes only what they created.
    assert_eq!(item(&body, editors)["can_delete"], true);
    assert_eq!(item(&body, colleagues)["can_delete"], false);
    assert_eq!(item(&body, orphan)["can_delete"], false);

    // Admins and the owner may delete any of them; a viewer none.
    for (who, expected) in [(&admin, true), (&owner, true), (&viewer, false)] {
        let (_, _, body) = list(&who.token, &format!("org_id={org}")).await;
        for id in [editors, colleagues, orphan] {
            assert_eq!(item(&body, id)["can_delete"], expected, "link {id}");
        }
    }

    // The flag is what DELETE then does.
    let (server, _) = spawn_real_app().await;
    let refused = server
        .delete(&format!("/links/{colleagues}"))
        .authorization_bearer(&editor.token)
        .await;
    assert_eq!(refused.status_code(), 403, "{}", refused.text());

    // The caller's own list (no org_id) is as it always was: no extra fields.
    let (_, _, body) = list(&editor.token, "limit=10").await;
    let own = item(&body, editors);
    assert!(own.get("created_by").is_none(), "{own}");
    assert!(own.get("can_delete").is_none(), "{own}");
}

#[tokio::test]
async fn export_follows_mine() {
    let (_server, db) = spawn_real_app().await;
    let owner = person(&db).await;
    let a = person(&db).await;
    let org = organization(&db, &owner).await;
    add_member(&db, org, a.id, "editor").await;
    let owners = insert_link(&db, owner.id, Some(org), "owner's").await;
    let mine = insert_link(&db, a.id, Some(org), "mine").await;

    let export = |query: String| {
        let token = a.token.clone();
        async move {
            let (server, _) = spawn_real_app().await;
            let res = server
                .get(&format!("/links/export?{query}"))
                .authorization_bearer(&token)
                .await;
            assert_eq!(res.status_code(), 200, "export {query}: {}", res.text());
            // The first column of each data row is the link id.
            res.text()
                .lines()
                .skip(1)
                .filter_map(|line| {
                    line.split(',')
                        .next()?
                        .trim_matches('"')
                        .parse::<i64>()
                        .ok()
                })
                .collect::<Vec<i64>>()
        }
    };

    assert_eq!(
        sorted(export(format!("org_id={org}")).await),
        sorted(vec![owners, mine])
    );
    assert_eq!(export(format!("org_id={org}&mine=true")).await, [mine]);
}

async fn body_of(response: Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("JSON body"))
}

#[tokio::test]
async fn with_no_default_organization_configured_signing_in_joins_nothing() {
    let (server, db) = spawn_real_app().await;
    assert!(
        std::env::var("DEFAULT_ORG_SLUG").is_err(),
        "this binary runs with the feature off"
    );
    let state = AppState::for_tests(db.clone()).await;
    // An account with a personal link from before.
    let existing = person(&db).await;
    let old = insert_link(&db, existing.id, None, "from before").await;

    let (status, body) =
        body_of(sign_in_verified_email(&state, &existing.email, "test provider").await).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["user_id"], existing.id);

    let memberships = org_members::Entity::find()
        .filter(org_members::Column::UserId.eq(existing.id))
        .all(&db)
        .await
        .unwrap();
    assert!(memberships.is_empty());
    let row = links::Entity::find_by_id(old as i32)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.org_id, None, "the personal link stays personal");
    let settings: Value = server.get("/auth/settings").await.json();
    assert!(settings.get("default_org").is_none(), "{settings}");

    // A new link is personal, and the list is the caller's own, as before.
    let created = server
        .post("/links")
        .authorization_bearer(body["token"].as_str().unwrap())
        .json(&json!({ "original_url": format!("https://iana.org/off-{}", uuid::Uuid::new_v4().simple()) }))
        .await;
    assert_eq!(created.status_code(), 201, "{}", created.text());
    let created: Value = created.json();
    assert_eq!(created["org_id"], Value::Null);
    let (ids, total, body) = list(&existing.token, "limit=10").await;
    assert_eq!(
        sorted(ids),
        sorted(vec![old, created["id"].as_i64().unwrap()])
    );
    assert_eq!(total, 2);
    assert!(item(&body, old).get("created_by").is_none());
}
