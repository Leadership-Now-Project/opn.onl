//! Media links: a short link that opens a page with a video's player on it.
//!
//! Real router, real Postgres. Nothing here talks to YouTube, Vimeo or
//! Google: the embed check is a stand-in handed to the app through
//! `AppState`, or switched off. What these tests pin:
//!
//! * which video a link is for is read by the server from the address, and
//!   an address that is not one video's creates no link;
//! * the embed check: the host's "no" refuses, our own trouble does not;
//! * a Drive video needs a Drive address and a video file;
//! * `GET /{code}` runs every gate, counts one click and sends the visitor to
//!   the page with a ticket; a password link gives no ticket without the
//!   password;
//! * `POST /{code}/media` says nothing without a valid ticket, and the
//!   player's address it gives is only ever on one of three hosts;
//! * the button's link is `https` and nothing else;
//! * every other link is what it always was.

use crate::common;

use std::sync::{Arc, Mutex};

use axum::http::header::{CACHE_CONTROL, LOCATION, REFERRER_POLICY};
use axum_test::{TestResponse, TestServer};
use common::{spawn_real_app_with_embed_check, unique_code, unique_email};
use futures::future::BoxFuture;
use opn_onl_backend::entity::{
    link_drive_files, link_media, links, org_members, organizations, users,
};
use opn_onl_backend::handlers::links::NOT_A_VIDEO_ADDRESS;
use opn_onl_backend::utils::ClickBuffer;
use opn_onl_backend::utils::create_jwt;
use opn_onl_backend::utils::media::{
    CTA_BOTH_OR_NEITHER, EMBED_ORIGINS, EmbedChecker, EmbedVerdict, NOT_EMBEDDABLE, ParsedMedia,
    VIDEO_NOT_FOUND, create_media_ticket, create_media_ticket_with_ttl,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter,
};
use serde_json::{Value, json};

// ------------------------------------------------------------------ helpers

/// The stand-in for YouTube's and Vimeo's answer. Remembers what it was asked.
struct FakeCheck {
    verdict: EmbedVerdict,
    asked: Mutex<Vec<String>>,
}

impl FakeCheck {
    fn answering(verdict: EmbedVerdict) -> Arc<Self> {
        Arc::new(Self {
            verdict,
            asked: Mutex::new(Vec::new()),
        })
    }

    fn embeddable(title: &str) -> Arc<Self> {
        Self::answering(EmbedVerdict::Embeddable {
            title: Some(title.to_string()),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl EmbedChecker for FakeCheck {
    fn check<'a>(&'a self, media: &'a ParsedMedia) -> BoxFuture<'a, EmbedVerdict> {
        self.asked.lock().unwrap().push(media.canonical_url());
        let verdict = self.verdict.clone();
        Box::pin(async move { verdict })
    }
}

struct World {
    app: TestServer,
    db: DatabaseConnection,
    clicks: Arc<ClickBuffer>,
}

/// An app whose embed check always says "embeddable", with no title.
async fn world() -> World {
    world_with(Some(FakeCheck::answering(EmbedVerdict::Embeddable {
        title: None,
    })))
    .await
}

async fn world_with(check: Option<Arc<FakeCheck>>) -> World {
    let check = check.map(|check| check as Arc<dyn EmbedChecker>);
    let (app, db, clicks) = spawn_real_app_with_embed_check(check).await;
    World { app, db, clicks }
}

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

/// An 11-character YouTube id no other test uses.
fn video_id() -> String {
    format!("{}Ab_-9", unique_code())
}

fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

async fn create(app: &TestServer, who: &Person, body: Value) -> TestResponse {
    app.post("/links")
        .authorization_bearer(&who.token)
        .json(&body)
        .await
}

/// Create a YouTube media link; `extra` is merged into the request.
async fn media_link(world: &World, who: &Person, extra: Value) -> Value {
    let mut body = json!({"original_url": watch_url(&video_id()), "media": {}});
    for (key, value) in extra.as_object().cloned().unwrap_or_default() {
        body[key] = value;
    }
    let res = create(&world.app, who, body).await;
    assert_eq!(res.status_code(), 201, "create: {}", res.text());
    res.json()
}

fn id_of(link: &Value) -> i32 {
    link["id"].as_i64().unwrap() as i32
}

fn code_of(link: &Value) -> String {
    link["code"].as_str().unwrap().to_string()
}

async fn links_of(db: &DatabaseConnection, who: &Person) -> u64 {
    links::Entity::find()
        .filter(links::Column::UserId.eq(who.id))
        .count(db)
        .await
        .unwrap()
}

async fn media_rows(db: &DatabaseConnection, link_id: i32) -> Vec<link_media::Model> {
    link_media::Entity::find()
        .filter(link_media::Column::LinkId.eq(link_id))
        .all(db)
        .await
        .unwrap()
}

async fn link_row(db: &DatabaseConnection, id: i32) -> links::Model {
    links::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("link row")
}

async fn change_link(
    db: &DatabaseConnection,
    id: i32,
    change: impl FnOnce(&mut links::ActiveModel),
) {
    let mut active: links::ActiveModel = link_row(db, id).await.into();
    change(&mut active);
    active.update(db).await.expect("update link");
}

/// The ticket in the fragment of a redirect to the media page.
fn ticket_in(res: &TestResponse, code: &str) -> String {
    assert_eq!(res.status_code(), 302, "{}", res.text());
    let location = res.header(LOCATION).to_str().unwrap().to_string();
    let (page, ticket) = location
        .split_once("#t=")
        .expect("a ticket in the fragment");
    assert!(
        page.ends_with(&format!("/m/{code}")),
        "the media page, not {page}"
    );
    assert!(!page.contains('?'), "nothing in the query string: {page}");
    urlencoding::decode(ticket).unwrap().into_owned()
}

async fn visit(world: &World, code: &str) -> TestResponse {
    world.app.get(&format!("/{code}")).await
}

async fn open(world: &World, code: &str, ticket: &str) -> TestResponse {
    world
        .app
        .post(&format!("/{code}/media"))
        .json(&json!({"ticket": ticket}))
        .await
}

fn on_an_embed_host(src: &str) -> bool {
    EMBED_ORIGINS
        .iter()
        .any(|origin| src.starts_with(&format!("{origin}/")))
}

// ------------------------------------------------------------------- create

#[tokio::test]
async fn the_server_reads_the_video_out_of_the_address_and_stores_its_own_spelling() {
    let world = world().await;
    let owner = person(&world.db).await;
    let id = video_id();

    for (pasted, provider, stored, media_id) in [
        (
            format!("https://youtu.be/{id}?si=tracking&t=1m30s"),
            "youtube",
            format!("https://www.youtube.com/watch?v={id}&t=90s"),
            id.clone(),
        ),
        (
            format!("https://m.youtube.com/watch?feature=share&v={id}"),
            "youtube",
            watch_url(&id),
            id.clone(),
        ),
        (
            format!("https://www.youtube.com/shorts/{id}"),
            "youtube",
            watch_url(&id),
            id.clone(),
        ),
        (
            "https://player.vimeo.com/video/76979871?h=abc123def0".to_string(),
            "vimeo",
            "https://vimeo.com/76979871/abc123def0".to_string(),
            "76979871:abc123def0".to_string(),
        ),
        (
            "https://vimeo.com/76979872".to_string(),
            "vimeo",
            "https://vimeo.com/76979872".to_string(),
            "76979872".to_string(),
        ),
    ] {
        let res = create(
            &world.app,
            &owner,
            json!({"original_url": pasted, "media": {}}),
        )
        .await;
        assert_eq!(res.status_code(), 201, "{pasted}: {}", res.text());
        let link: Value = res.json();
        assert_eq!(link["original_url"], stored, "{pasted}");
        assert_eq!(link["media"]["provider"], provider, "{pasted}");
        let rows = media_rows(&world.db, id_of(&link)).await;
        assert_eq!(rows.len(), 1, "{pasted}");
        assert_eq!(rows[0].provider, provider);
        assert_eq!(rows[0].media_id, media_id);
        assert_eq!(link_row(&world.db, id_of(&link)).await.original_url, stored);
    }
}

#[tokio::test]
async fn an_address_that_is_not_one_video_creates_no_link() {
    let world = world().await;
    let owner = person(&world.db).await;
    let id = video_id();

    for hostile in [
        "https://example.org/".to_string(),
        format!("https://youtube.com.evil.test/watch?v={id}"),
        format!("https://evil.test/?u=https://youtu.be/{id}"),
        format!("https://youtube.com@evil.test/watch?v={id}"),
        format!("https://user:pw@youtu.be/{id}"),
        format!("https://www.youtube.com:8443/watch?v={id}"),
        format!("https://www.youtube.com/shorts/{id}/extra"),
        format!("https://youtu.be/{id}X"),
        format!("https://youtu.be/{}", &id[..10]),
        "https://youtu.be/dQw4w9WgXcé".to_string(),
        "https://www.youtube.com/playlist?list=PL1234567890".to_string(),
        "https://www.youtube.com/embed/videoseries?list=PL1234567890".to_string(),
        "https://vimeo.com/channels/staffpicks/76979871".to_string(),
        "https://vimeo.com/76979871/abc123def0/extra".to_string(),
        "https://vimeo.com/0123".to_string(),
        format!("javascript:alert(1)//youtu.be/{id}"),
        String::new(),
    ] {
        let res = create(
            &world.app,
            &owner,
            json!({"original_url": hostile, "media": {}}),
        )
        .await;
        assert_eq!(res.status_code(), 400, "{hostile:?}: {}", res.text());
        assert_eq!(
            res.json::<Value>()["error"],
            NOT_A_VIDEO_ADDRESS,
            "{hostile:?}"
        );
    }
    assert_eq!(links_of(&world.db, &owner).await, 0, "no link was created");

    // A provider named that the address does not agree with, and one that
    // does not exist.
    for (provider, wanted) in [("vimeo", "does not match"), ("zoom", "youtube, vimeo")] {
        let res = create(
            &world.app,
            &owner,
            json!({"original_url": watch_url(&id), "media": {"provider": provider}}),
        )
        .await;
        assert_eq!(res.status_code(), 400, "{}", res.text());
        assert!(res.text().contains(wanted), "{}", res.text());
    }
    // The reserved words of the media page cannot be taken as an alias.
    let res = create(
        &world.app,
        &owner,
        json!({"original_url": "https://iana.org/", "custom_alias": "media"}),
    )
    .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    assert!(res.text().contains("reserved"), "{}", res.text());
    assert_eq!(links_of(&world.db, &owner).await, 0);
}

// -------------------------------------------------------------- embed check

#[tokio::test]
async fn an_embeddable_video_becomes_a_link_titled_by_its_host() {
    let check = FakeCheck::embeddable("How a bill becomes law");
    let world = world_with(Some(check.clone())).await;
    let owner = person(&world.db).await;
    let id = video_id();

    let res = create(
        &world.app,
        &owner,
        json!({"original_url": format!("https://youtu.be/{id}"), "media": {}}),
    )
    .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let link: Value = res.json();
    assert_eq!(link["title"], "How a bill becomes law");
    assert_eq!(link["media"]["embed_check"], "checked");
    assert_eq!(
        link_row(&world.db, id_of(&link)).await.title.as_deref(),
        Some("How a bill becomes law")
    );
    // The host was asked about the server's own spelling of the address.
    assert_eq!(check.asked(), vec![watch_url(&id)]);

    // A title the person typed is kept.
    let titled = media_link(&world, &owner, json!({"title": "My title"})).await;
    assert_eq!(titled["title"], "My title");
}

#[tokio::test]
async fn a_video_its_host_will_not_embed_is_refused_and_no_link_is_made() {
    for reason in [NOT_EMBEDDABLE, VIDEO_NOT_FOUND] {
        let check = FakeCheck::answering(EmbedVerdict::Refused(reason));
        let world = world_with(Some(check.clone())).await;
        let owner = person(&world.db).await;

        let res = create(
            &world.app,
            &owner,
            json!({"original_url": watch_url(&video_id()), "media": {}}),
        )
        .await;
        assert_eq!(res.status_code(), 400, "{}", res.text());
        assert_eq!(res.json::<Value>()["error"], reason);
        assert_eq!(check.asked().len(), 1);
        assert_eq!(links_of(&world.db, &owner).await, 0, "no link was created");
    }
}

#[tokio::test]
async fn a_host_that_cannot_be_reached_does_not_block_the_link() {
    let check = FakeCheck::answering(EmbedVerdict::Unknown);
    let world = world_with(Some(check.clone())).await;
    let owner = person(&world.db).await;

    let link = media_link(&world, &owner, json!({})).await;
    assert_eq!(link["media"]["embed_check"], "not_checked");
    assert_eq!(link["title"], Value::Null);
    assert_eq!(check.asked().len(), 1);
    assert_eq!(media_rows(&world.db, id_of(&link)).await.len(), 1);
}

#[tokio::test]
async fn with_the_check_switched_off_the_link_is_made_and_says_it_was_not_checked() {
    let world = world_with(None).await;
    let owner = person(&world.db).await;

    let link = media_link(&world, &owner, json!({})).await;
    assert_eq!(link["media"]["embed_check"], "not_checked");
    assert_eq!(link["media"]["provider"], "youtube");
    // The lists never repeat the note: it is about the moment of creation.
    let got: Value = world
        .app
        .get(&format!("/links/{}", link["id"]))
        .authorization_bearer(&owner.token)
        .await
        .json();
    assert!(got["media"].get("embed_check").is_none(), "{got}");
}

// -------------------------------------------------------------------- drive

fn drive_video(file_id: &str, mime_type: &str) -> Value {
    json!({"file_id": file_id, "name": "Town hall.mp4", "mime_type": mime_type})
}

#[tokio::test]
async fn a_drive_video_needs_a_drive_address_and_a_video_file() {
    let check = FakeCheck::embeddable("never asked");
    let world = world_with(Some(check.clone())).await;
    let owner = person(&world.db).await;
    let file_id = format!("1{}_-x", unique_code());
    let url = format!("https://drive.google.com/file/d/{file_id}/view?usp=drivesdk");
    let drive = json!({"provider": "drive"});

    // Not a video.
    for mime in ["application/pdf", "image/png", "", "video/", "xvideo/mp4"] {
        let res = create(
            &world.app,
            &owner,
            json!({"original_url": url, "drive_file": drive_video(&file_id, mime), "media": drive}),
        )
        .await;
        assert_eq!(res.status_code(), 400, "{mime:?}: {}", res.text());
        assert!(res.text().contains("not a video"), "{}", res.text());
    }
    // Not in Drive.
    for elsewhere in [
        format!("https://drive.google.com.evil.test/file/d/{file_id}/view"),
        format!("http://drive.google.com/file/d/{file_id}/view"),
        format!("https://example.org/file/d/{file_id}/view"),
    ] {
        let res = create(
            &world.app,
            &owner,
            json!({"original_url": elsewhere, "drive_file": drive_video(&file_id, "video/mp4"), "media": drive}),
        )
        .await;
        assert_eq!(res.status_code(), 400, "{elsewhere}: {}", res.text());
    }
    // No file named, and a file named for a video that is not in Drive.
    let res = create(
        &world.app,
        &owner,
        json!({"original_url": url, "media": drive}),
    )
    .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    let res = create(
        &world.app,
        &owner,
        json!({
            "original_url": watch_url(&video_id()),
            "drive_file": drive_video(&file_id, "video/mp4"),
            "media": {},
        }),
    )
    .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    assert_eq!(links_of(&world.db, &owner).await, 0, "no link was created");

    // A video in Drive.
    let res = create(
        &world.app,
        &owner,
        json!({"original_url": url, "drive_file": drive_video(&file_id, "video/mp4"), "media": drive}),
    )
    .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let link: Value = res.json();
    assert_eq!(link["media"]["provider"], "drive");
    assert!(link["media"].get("embed_check").is_none(), "{link}");
    assert_eq!(link["drive"]["name"], "Town hall.mp4");
    assert_eq!(
        link["title"], "Town hall.mp4",
        "the file's name is the title"
    );
    assert_eq!(link["original_url"], url);
    let rows = media_rows(&world.db, id_of(&link)).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].media_id, file_id);
    assert_eq!(
        link_drive_files::Entity::find()
            .filter(link_drive_files::Column::LinkId.eq(id_of(&link)))
            .count(&world.db)
            .await
            .unwrap(),
        1,
        "it is a Drive link too"
    );
    assert!(
        check.asked().is_empty(),
        "Google is never asked by the server"
    );

    // The page frames Google's own player for that file.
    let ticket = ticket_in(&visit(&world, &code_of(&link)).await, &code_of(&link));
    let page: Value = open(&world, &code_of(&link), &ticket).await.json();
    assert_eq!(
        page["embed"]["src"],
        format!("https://drive.google.com/file/d/{file_id}/preview")
    );

    // A list of Drive links shows `media` on the video and not on a document.
    let doc = create(
        &world.app,
        &owner,
        json!({
            "original_url": format!("https://docs.google.com/document/d/{file_id}/edit"),
            "drive_file": {"file_id": file_id, "name": "Plan", "mime_type": "application/vnd.google-apps.document"},
        }),
    )
    .await;
    assert_eq!(doc.status_code(), 201, "{}", doc.text());
    let items: Vec<Value> = world
        .app
        .get("/links")
        .authorization_bearer(&owner.token)
        .await
        .json();
    let by_id = |id: &Value| items.iter().find(|item| &item["id"] == id).unwrap();
    assert_eq!(by_id(&link["id"])["media"]["provider"], "drive");
    assert!(by_id(&doc.json::<Value>()["id"]).get("media").is_none());
}

// ----------------------------------------------------------------- redirect

#[tokio::test]
async fn opening_a_media_link_counts_one_click_and_goes_to_the_page_with_a_ticket() {
    let world = world().await;
    let owner = person(&world.db).await;
    let link = media_link(&world, &owner, json!({"title": "Town hall"})).await;
    let (id, code) = (id_of(&link), code_of(&link));

    // A HEAD is not a visit: what an ordinary link answers, and no click.
    let head = world
        .app
        .method(axum::http::Method::HEAD, &format!("/{code}"))
        .await;
    assert_eq!(head.status_code(), 307);
    assert_eq!(
        head.header(LOCATION),
        link["original_url"].as_str().unwrap()
    );
    assert_eq!(world.clicks.pending_count(id), 0);

    let res = visit(&world, &code).await;
    let ticket = ticket_in(&res, &code);
    assert_eq!(res.header(CACHE_CONTROL), "no-store");
    assert_eq!(res.header(REFERRER_POLICY), "no-referrer");
    assert!(
        !res.header(LOCATION)
            .to_str()
            .unwrap()
            .contains("youtube.com"),
        "the visitor is not sent to the video's host"
    );
    assert_eq!(world.clicks.pending_count(id), 1, "one click");

    // The page's call, and a reload of the page, count nothing more.
    for _ in 0..2 {
        let page = open(&world, &code, &ticket).await;
        assert_eq!(page.status_code(), 200, "{}", page.text());
        assert_eq!(page.header(CACHE_CONTROL), "no-store");
        assert_eq!(page.header("x-ratelimit-limit"), "120");
        let page: Value = page.json();
        assert_eq!(page["title"], "Town hall");
        assert_eq!(page["provider"], "youtube");
        assert_eq!(page["cta"], Value::Null);
    }
    assert_eq!(world.clicks.pending_count(id), 1, "still one click");
}

#[tokio::test]
async fn a_capped_media_link_is_not_used_up_by_a_head_or_an_unfurler() {
    let world = world().await;
    let owner = person(&world.db).await;
    let link = media_link(&world, &owner, json!({"burn_after_reading": true})).await;
    let (id, code) = (id_of(&link), code_of(&link));
    let path = format!("/{code}");

    let head = world.app.method(axum::http::Method::HEAD, &path).await;
    assert_eq!(head.status_code(), 200);
    assert!(head.maybe_header(LOCATION).is_none());
    let unfurl = world
        .app
        .get(&path)
        .add_header(
            "user-agent",
            "Slackbot-LinkExpanding 1.0 (+https://api.slack.com/robots)",
        )
        .await;
    assert_eq!(unfurl.status_code(), 200, "{}", unfurl.text());
    assert!(unfurl.maybe_header(LOCATION).is_none());
    assert_eq!(link_row(&world.db, id).await.click_count, 0);

    // The one visit: the page opens, and opens again on a reload.
    let ticket = ticket_in(&visit(&world, &code).await, &code);
    assert_eq!(link_row(&world.db, id).await.click_count, 1);
    for _ in 0..2 {
        assert_eq!(open(&world, &code, &ticket).await.status_code(), 200);
    }
    // A second visit finds the link used up.
    let again = visit(&world, &code).await;
    assert_eq!(again.status_code(), 410, "{}", again.text());
    assert!(again.maybe_header(LOCATION).is_none());
}

#[tokio::test]
async fn a_password_link_gives_no_ticket_and_no_video_without_the_password() {
    let world = world().await;
    let owner = person(&world.db).await;
    let link = media_link(&world, &owner, json!({"password": "open sesame"})).await;
    let (id, code) = (id_of(&link), code_of(&link));
    let video = link["original_url"].as_str().unwrap().to_string();
    let video_id = media_rows(&world.db, id).await[0].media_id.clone();
    let leaks = |res: &TestResponse, what: &str| {
        let mut text = res.text();
        for (name, value) in res.headers() {
            text.push_str(&format!("\n{name}: {}", value.to_str().unwrap_or("")));
        }
        assert!(!text.contains(&video_id), "{what} names the video: {text}");
        assert!(!text.contains("#t="), "{what} carries a ticket: {text}");
        assert!(!text.contains(&video), "{what} names the address: {text}");
    };

    // No password: the password page, and nothing about the video.
    let res = visit(&world, &code).await;
    assert_eq!(res.status_code(), 307, "{}", res.text());
    assert!(
        res.header(LOCATION)
            .to_str()
            .unwrap()
            .ends_with(&format!("/password/{code}"))
    );
    leaks(&res, "the redirect to the password page");
    let head = world
        .app
        .method(axum::http::Method::HEAD, &format!("/{code}"))
        .await;
    leaks(&head, "a HEAD");

    // A wrong password.
    let wrong = world
        .app
        .get(&format!("/{code}"))
        .add_header("x-link-password", "guess")
        .await;
    assert_eq!(wrong.status_code(), 401);
    leaks(&wrong, "a wrong password");

    // The page's call without a ticket, with nonsense, and with a ticket for
    // another (open) link of the same owner.
    let other = media_link(&world, &owner, json!({})).await;
    let other_ticket = ticket_in(&visit(&world, &code_of(&other)).await, &code_of(&other));
    for ticket in ["", "nonsense", other_ticket.as_str()] {
        let res = open(&world, &code, ticket).await;
        assert_eq!(res.status_code(), 401, "{}", res.text());
        leaks(&res, "the page's call without a ticket");
    }
    assert_eq!(world.clicks.pending_count(id), 0, "nothing was counted");

    // With the password: the unlock, then the page.
    let verified = world
        .app
        .post(&format!("/{code}/verify"))
        .json(&json!({"password": "open sesame"}))
        .await;
    assert_eq!(verified.status_code(), 200, "{}", verified.text());
    let continue_at = verified.json::<Value>()["redirect_url"]
        .as_str()
        .unwrap()
        .to_string();
    let unlock = url::Url::parse(&continue_at)
        .unwrap()
        .query_pairs()
        .find(|(name, _)| name == "unlock")
        .map(|(_, value)| value.to_string())
        .expect("unlock token");
    let res = world
        .app
        .get(&format!("/{code}"))
        .add_query_param("unlock", &unlock)
        .await;
    let ticket = ticket_in(&res, &code);
    assert!(
        !res.header(LOCATION).to_str().unwrap().contains("unlock="),
        "the unlock token does not travel on to the page"
    );
    // An unlock token is not a ticket.
    assert_eq!(open(&world, &code, &unlock).await.status_code(), 401);
    let page = open(&world, &code, &ticket).await;
    assert_eq!(page.status_code(), 200, "{}", page.text());
    assert!(
        page.json::<Value>()["embed"]["src"]
            .as_str()
            .unwrap()
            .contains(&video_id)
    );

    // The password changes: the ticket is over.
    change_link(&world.db, id, |link| {
        link.password_hash = Set(Some("a-new-password-hash".to_string()));
    })
    .await;
    assert_eq!(open(&world, &code, &ticket).await.status_code(), 401);
    // And so is it when the password is taken off.
    change_link(&world.db, id, |link| link.password_hash = Set(None)).await;
    assert_eq!(open(&world, &code, &ticket).await.status_code(), 401);
}

// ----------------------------------------------------------- the page's call

#[tokio::test]
async fn without_a_valid_ticket_every_refusal_is_the_same_401() {
    let world = world().await;
    let owner = person(&world.db).await;
    let link = media_link(&world, &owner, json!({})).await;
    let (id, code) = (id_of(&link), code_of(&link));
    let good = ticket_in(&visit(&world, &code).await, &code);
    assert_eq!(open(&world, &code, &good).await.status_code(), 200);

    let other = media_link(&world, &owner, json!({})).await;
    let other_code = code_of(&other);
    let other_ticket = ticket_in(&visit(&world, &other_code).await, &other_code);

    // An ordinary link to a video, and one to somewhere else: not media.
    let plain_video = create(
        &world.app,
        &owner,
        json!({"original_url": watch_url(&video_id())}),
    )
    .await
    .json::<Value>();
    let plain = create(
        &world.app,
        &owner,
        json!({"original_url": "https://iana.org/"}),
    )
    .await
    .json::<Value>();
    // A ticket signed for the plain video link, as if it were a media link.
    let plain_ticket =
        create_media_ticket(&link_row(&world.db, id_of(&plain_video)).await).unwrap();
    let expired = create_media_ticket_with_ttl(&link_row(&world.db, id).await, -5).unwrap();

    // A deleted media link, with a ticket from while it lived.
    let deleted = media_link(&world, &owner, json!({})).await;
    let deleted_code = code_of(&deleted);
    let deleted_ticket = ticket_in(&visit(&world, &deleted_code).await, &deleted_code);
    let res = world
        .app
        .delete(&format!("/links/{}", deleted["id"]))
        .authorization_bearer(&owner.token)
        .await;
    assert!(res.status_code().is_success(), "{}", res.text());

    let missing = format!("zz{}", unique_code());
    let mut answers: Vec<(String, u16, String)> = Vec::new();
    for (what, at, ticket) in [
        ("no ticket", code.as_str(), ""),
        ("nonsense", code.as_str(), "not.a.ticket"),
        ("an expired ticket", code.as_str(), expired.as_str()),
        (
            "another link's ticket",
            code.as_str(),
            other_ticket.as_str(),
        ),
        (
            "a code that does not exist",
            missing.as_str(),
            good.as_str(),
        ),
        (
            "a deleted link",
            deleted_code.as_str(),
            deleted_ticket.as_str(),
        ),
        (
            "an ordinary link to a video",
            plain_video["code"].as_str().unwrap(),
            plain_ticket.as_str(),
        ),
        (
            "an ordinary link",
            plain["code"].as_str().unwrap(),
            good.as_str(),
        ),
    ] {
        let res = open(&world, at, ticket).await;
        answers.push((what.to_string(), res.status_code().as_u16(), res.text()));
    }
    for (what, status, body) in &answers {
        assert_eq!(*status, 401, "{what}: {body}");
        assert_eq!(body, &answers[0].2, "{what} answers like no ticket");
        assert!(!body.contains("youtube"), "{what}: {body}");
    }

    // The good ticket still works: nothing above used it up.
    assert_eq!(open(&world, &code, &good).await.status_code(), 200);
}

#[tokio::test]
async fn a_link_that_stopped_being_live_stops_showing_its_video() {
    let world = world().await;
    let owner = person(&world.db).await;
    let link = media_link(&world, &owner, json!({})).await;
    let (id, code) = (id_of(&link), code_of(&link));
    let ticket = ticket_in(&visit(&world, &code).await, &code);
    assert_eq!(open(&world, &code, &ticket).await.status_code(), 200);

    // Expired since the ticket was issued.
    change_link(&world.db, id, |link| {
        link.expires_at = Set(Some(
            chrono::Utc::now().naive_utc() - chrono::Duration::minutes(1),
        ));
    })
    .await;
    let res = open(&world, &code, &ticket).await;
    assert_eq!(res.status_code(), 410, "{}", res.text());
    assert!(res.json::<Value>().get("embed").is_none());
    assert_eq!(visit(&world, &code).await.status_code(), 410);

    // Live again, then not started yet.
    change_link(&world.db, id, |link| {
        link.expires_at = Set(None);
        link.starts_at = Set(Some(
            chrono::Utc::now().naive_utc() + chrono::Duration::hours(1),
        ));
    })
    .await;
    assert_eq!(open(&world, &code, &ticket).await.status_code(), 410);

    // Live again, then its owner is disabled.
    change_link(&world.db, id, |link| link.starts_at = Set(None)).await;
    assert_eq!(open(&world, &code, &ticket).await.status_code(), 200);
    let mut user: users::ActiveModel = users::Entity::find_by_id(owner.id)
        .one(&world.db)
        .await
        .unwrap()
        .unwrap()
        .into();
    user.disabled_at = Set(Some(chrono::Utc::now().naive_utc()));
    user.update(&world.db).await.unwrap();
    assert_eq!(open(&world, &code, &ticket).await.status_code(), 410);
}

#[tokio::test]
async fn the_players_address_is_only_ever_on_one_of_three_hosts() {
    let world = world().await;
    let owner = person(&world.db).await;
    let id = video_id();

    for (pasted, src) in [
        (
            format!("https://youtu.be/{id}?t=75"),
            format!("https://www.youtube-nocookie.com/embed/{id}?start=75"),
        ),
        (
            "https://vimeo.com/76979873/abc123def0".to_string(),
            "https://player.vimeo.com/video/76979873?h=abc123def0".to_string(),
        ),
        (
            "https://vimeo.com/76979874".to_string(),
            "https://player.vimeo.com/video/76979874".to_string(),
        ),
    ] {
        let link = create(
            &world.app,
            &owner,
            json!({"original_url": pasted, "media": {}}),
        )
        .await
        .json::<Value>();
        let code = code_of(&link);
        let ticket = ticket_in(&visit(&world, &code).await, &code);
        let page: Value = open(&world, &code, &ticket).await.json();
        assert_eq!(page["embed"]["src"], src, "{pasted}");
        assert!(on_an_embed_host(page["embed"]["src"].as_str().unwrap()));
    }

    // A row nobody could have written through the API frames nothing: the
    // address is built from an id that is checked again on the way out.
    let link = media_link(&world, &owner, json!({})).await;
    let code = code_of(&link);
    let ticket = ticket_in(&visit(&world, &code).await, &code);
    for tampered in [
        "../../../evil",
        "abc\"><script>",
        "https://evil.test/x",
        "videoseries",
    ] {
        let mut row: link_media::ActiveModel =
            media_rows(&world.db, id_of(&link)).await.remove(0).into();
        row.media_id = Set(tampered.to_string());
        row.update(&world.db).await.unwrap();
        let res = open(&world, &code, &ticket).await;
        assert_eq!(res.status_code(), 410, "{tampered}: {}", res.text());
        assert!(!res.text().contains("evil"), "{}", res.text());
    }
}

// --------------------------------------------------------------- the button

#[tokio::test]
async fn the_button_needs_both_a_label_and_an_https_link() {
    let world = world().await;
    let owner = person(&world.db).await;

    for (media, wanted) in [
        (json!({"cta_label": "Join us"}), CTA_BOTH_OR_NEITHER),
        (
            json!({"cta_url": "https://iana.org/join"}),
            CTA_BOTH_OR_NEITHER,
        ),
        (
            json!({"cta_label": "Go", "cta_url": "javascript:alert(document.domain)"}),
            "https://",
        ),
        (
            json!({"cta_label": "Go", "cta_url": "JaVaScRiPt:alert(1)"}),
            "https://",
        ),
        (
            json!({"cta_label": "Go", "cta_url": "data:text/html,<script>alert(1)</script>"}),
            "https://",
        ),
        (
            json!({"cta_label": "Go", "cta_url": "http://iana.org/join"}),
            "https://",
        ),
        (
            json!({"cta_label": "Go", "cta_url": "//iana.org/join"}),
            "https://",
        ),
        (
            json!({"cta_label": "Go", "cta_url": "https://localhost/admin"}),
            "refused",
        ),
        (
            json!({"cta_label": "x".repeat(41), "cta_url": "https://iana.org/join"}),
            "40 characters",
        ),
    ] {
        let res = create(
            &world.app,
            &owner,
            json!({"original_url": watch_url(&video_id()), "media": media}),
        )
        .await;
        assert_eq!(res.status_code(), 400, "{media}: {}", res.text());
        assert!(res.text().contains(wanted), "{media}: {}", res.text());
    }
    assert_eq!(links_of(&world.db, &owner).await, 0, "no link was created");

    // Both: stored, listed and shown on the page.
    let link = media_link(
        &world,
        &owner,
        json!({"media": {"cta_label": " Join us ", "cta_url": "https://iana.org/join"}}),
    )
    .await;
    let (id, code) = (id_of(&link), code_of(&link));
    assert_eq!(link["media"]["cta_label"], "Join us");
    assert_eq!(link["media"]["cta_url"], "https://iana.org/join");
    let ticket = ticket_in(&visit(&world, &code).await, &code);
    let page: Value = open(&world, &code, &ticket).await.json();
    assert_eq!(
        page["cta"],
        json!({"label": "Join us", "url": "https://iana.org/join"})
    );

    // Editing it: one half alone and a script are refused and change nothing.
    let put = |body: Value| {
        world
            .app
            .put(&format!("/links/{id}"))
            .authorization_bearer(&owner.token)
            .json(&body)
    };
    for bad in [
        json!({"media_cta": {"label": "Only a label"}}),
        json!({"media_cta": {"label": "Go", "url": "javascript:alert(1)"}}),
        json!({"media_cta": {"label": "Go", "url": "http://iana.org/"}}),
    ] {
        let res = put(bad.clone()).await;
        assert_eq!(res.status_code(), 400, "{bad}: {}", res.text());
    }
    assert_eq!(
        media_rows(&world.db, id).await[0].cta_url.as_deref(),
        Some("https://iana.org/join")
    );
    // A new one.
    let res = put(json!({"media_cta": {"label": "Donate", "url": "https://iana.org/give"}})).await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(res.json::<Value>()["media"]["cta_label"], "Donate");
    let page: Value = open(&world, &code, &ticket).await.json();
    assert_eq!(
        page["cta"],
        json!({"label": "Donate", "url": "https://iana.org/give"})
    );
    // An edit that does not mention the button leaves it alone.
    let res = put(json!({"title": "Renamed"})).await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(res.json::<Value>()["media"]["cta_label"], "Donate");
    // Both empty: removed.
    let res = put(json!({"media_cta": {"label": "", "url": ""}})).await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(res.json::<Value>()["media"]["cta_label"], Value::Null);
    let page: Value = open(&world, &code, &ticket).await.json();
    assert_eq!(page["cta"], Value::Null);

    // A row with a link that is not https puts no button on the page.
    let mut row: link_media::ActiveModel = media_rows(&world.db, id).await.remove(0).into();
    row.cta_label = Set(Some("Click".to_string()));
    row.cta_url = Set(Some("javascript:alert(1)".to_string()));
    row.update(&world.db).await.unwrap();
    let page = open(&world, &code, &ticket).await;
    assert_eq!(page.status_code(), 200);
    assert_eq!(page.json::<Value>()["cta"], Value::Null);
    assert!(!page.text().contains("javascript"), "{}", page.text());

    // An ordinary link has no button to set.
    let plain = create(
        &world.app,
        &owner,
        json!({"original_url": "https://iana.org/"}),
    )
    .await
    .json::<Value>();
    let res = world
        .app
        .put(&format!("/links/{}", plain["id"]))
        .authorization_bearer(&owner.token)
        .json(&json!({"media_cta": {"label": "Go", "url": "https://iana.org/"}}))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
}

// ------------------------------------------------------------ lists and edits

#[tokio::test]
async fn a_clone_of_a_media_link_is_a_media_link() {
    let world = world().await;
    let owner = person(&world.db).await;
    let link = media_link(
        &world,
        &owner,
        json!({"media": {"cta_label": "Join", "cta_url": "https://iana.org/join"}}),
    )
    .await;

    let res = world
        .app
        .post(&format!("/links/{}/clone", link["id"]))
        .authorization_bearer(&owner.token)
        .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let clone: Value = res.json();
    let (clone_id, clone_code) = (id_of(&clone), code_of(&clone));
    assert_ne!(clone_code, code_of(&link));

    let original = media_rows(&world.db, id_of(&link)).await.remove(0);
    let copied = media_rows(&world.db, clone_id).await;
    assert_eq!(copied.len(), 1, "the clone has a row of its own");
    assert_eq!(copied[0].provider, original.provider);
    assert_eq!(copied[0].media_id, original.media_id);
    assert_eq!(copied[0].cta_url.as_deref(), Some("https://iana.org/join"));

    // It opens the page, with a ticket of its own; the original's does not fit.
    let ticket = ticket_in(&visit(&world, &clone_code).await, &clone_code);
    let page = open(&world, &clone_code, &ticket).await;
    assert_eq!(page.status_code(), 200, "{}", page.text());
    assert_eq!(
        page.json::<Value>()["cta"],
        json!({"label": "Join", "url": "https://iana.org/join"})
    );
    let original_ticket = ticket_in(&visit(&world, &code_of(&link)).await, &code_of(&link));
    assert_eq!(
        open(&world, &clone_code, &original_ticket)
            .await
            .status_code(),
        401
    );

    // An ordinary link's clone has no row.
    let plain = create(
        &world.app,
        &owner,
        json!({"original_url": watch_url(&video_id())}),
    )
    .await
    .json::<Value>();
    let res = world
        .app
        .post(&format!("/links/{}/clone", plain["id"]))
        .authorization_bearer(&owner.token)
        .await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    assert!(
        media_rows(&world.db, id_of(&res.json::<Value>()))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn every_answer_that_shows_the_link_shows_media_to_colleagues_too() {
    let world = world().await;
    let owner = person(&world.db).await;
    let viewer = person(&world.db).await;
    let outsider = person(&world.db).await;
    let org = organizations::ActiveModel {
        name: Set("Team".to_string()),
        slug: Set(format!("team-{}", unique_code().to_lowercase())),
        owner_id: Set(owner.id),
        ..Default::default()
    }
    .insert(&world.db)
    .await
    .unwrap();
    for (who, role) in [(&owner, "owner"), (&viewer, "viewer")] {
        org_members::ActiveModel {
            org_id: Set(org.id),
            user_id: Set(who.id),
            role: Set(role.to_string()),
            ..Default::default()
        }
        .insert(&world.db)
        .await
        .unwrap();
    }
    let link = media_link(
        &world,
        &owner,
        json!({
            "org_id": org.id,
            "media": {"cta_label": "Join", "cta_url": "https://iana.org/join"},
        }),
    )
    .await;
    let shown = json!({
        "provider": "youtube",
        "cta_label": "Join",
        "cta_url": "https://iana.org/join",
    });

    for who in [&owner, &viewer] {
        let items: Vec<Value> = world
            .app
            .get(&format!("/links?org_id={}", org.id))
            .authorization_bearer(&who.token)
            .await
            .json();
        let item = items.iter().find(|item| item["id"] == link["id"]).unwrap();
        assert_eq!(item["media"], shown);
        let got: Value = world
            .app
            .get(&format!("/links/{}", link["id"]))
            .authorization_bearer(&who.token)
            .await
            .json();
        assert_eq!(got["media"], shown);
        // The video's id is not a separate field anywhere in a list.
        assert!(got["media"].get("media_id").is_none());
    }
    let res = world
        .app
        .get(&format!("/links/{}", link["id"]))
        .authorization_bearer(&outsider.token)
        .await;
    assert_eq!(res.status_code(), 403, "{}", res.text());
}

#[tokio::test]
async fn a_new_destination_ends_the_media_page() {
    let world = world().await;
    let owner = person(&world.db).await;
    let link = media_link(
        &world,
        &owner,
        json!({"media": {"cta_label": "Join", "cta_url": "https://iana.org/join"}}),
    )
    .await;
    let (id, code) = (id_of(&link), code_of(&link));
    let ticket = ticket_in(&visit(&world, &code).await, &code);

    // The button cannot be set in the request that ends the page.
    let res = world
        .app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&owner.token)
        .json(&json!({
            "original_url": "https://iana.org/elsewhere",
            "media_cta": {"label": "Go", "url": "https://iana.org/"},
        }))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    assert_eq!(media_rows(&world.db, id).await.len(), 1);

    // The same destination again changes nothing.
    let res = world
        .app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&owner.token)
        .json(&json!({"original_url": link["original_url"]}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert_eq!(res.json::<Value>()["media"]["provider"], "youtube");

    let res = world
        .app
        .put(&format!("/links/{id}"))
        .authorization_bearer(&owner.token)
        .json(&json!({"original_url": "https://iana.org/elsewhere"}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
    assert!(res.json::<Value>().get("media").is_none());
    assert!(
        media_rows(&world.db, id).await.is_empty(),
        "the row is gone"
    );

    // The link is an ordinary redirect now, and the old ticket opens nothing.
    let res = visit(&world, &code).await;
    assert_eq!(res.status_code(), 307, "{}", res.text());
    assert_eq!(res.header(LOCATION), "https://iana.org/elsewhere");
    assert_eq!(open(&world, &code, &ticket).await.status_code(), 401);
}

#[tokio::test]
async fn a_media_link_cannot_have_routing_rules() {
    let world = world().await;
    let owner = person(&world.db).await;
    let link = media_link(&world, &owner, json!({})).await;
    let res = world
        .app
        .put(&format!("/links/{}/rules", link["id"]))
        .authorization_bearer(&owner.token)
        .json(&json!({"rules": [{"match_device": "mobile", "destination_url": "https://iana.org/m"}]}))
        .await;
    assert_eq!(res.status_code(), 400, "{}", res.text());
    assert!(res.text().contains("media link"), "{}", res.text());
}

// ------------------------------------------------------------ ordinary links

#[tokio::test]
async fn a_link_made_without_media_is_what_it_always_was() {
    let check = FakeCheck::embeddable("never asked");
    let world = world_with(Some(check.clone())).await;
    let owner = person(&world.db).await;
    let url = format!("https://youtu.be/{}?si=abc", video_id());

    let res = create(&world.app, &owner, json!({"original_url": url})).await;
    assert_eq!(res.status_code(), 201, "{}", res.text());
    let link: Value = res.json();
    assert!(link.get("media").is_none(), "{link}");
    assert_eq!(
        link["original_url"], url,
        "the address is kept as it was typed"
    );
    assert_eq!(link["title"], Value::Null);
    assert!(media_rows(&world.db, id_of(&link)).await.is_empty());
    assert!(
        check.asked().is_empty(),
        "nobody is asked about an ordinary link"
    );

    // It redirects to the video's host, like any link.
    let res = visit(&world, &code_of(&link)).await;
    assert_eq!(res.status_code(), 307, "{}", res.text());
    assert_eq!(res.header(LOCATION), url.as_str());
    assert_eq!(world.clicks.pending_count(id_of(&link)), 1);

    let got: Value = world
        .app
        .get(&format!("/links/{}", link["id"]))
        .authorization_bearer(&owner.token)
        .await
        .json();
    assert!(got.get("media").is_none());

    // And rules still work for it.
    let res = world
        .app
        .put(&format!("/links/{}/rules", link["id"]))
        .authorization_bearer(&owner.token)
        .json(&json!({"rules": [{"match_device": "mobile", "destination_url": "https://iana.org/m"}]}))
        .await;
    assert_eq!(res.status_code(), 200, "{}", res.text());
}
