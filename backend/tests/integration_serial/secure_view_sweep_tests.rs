//! Secure View, the parts that look at every row: the daily file sweep
//! removes a protected copy with its original, and the retention scrub and
//! the account-deletion purge erase the emails visitors gave the viewer.
//!
//! These run alone (`integration_serial`): `Sweep::All` and the retention
//! scrub would touch the rows of tests running beside them.

use crate::common;

use chrono::{Duration, Utc};
use common::{setup_test_db, unique_email};
use opn_onl_backend::entity::{click_events, link_files, links, organizations, users};
use opn_onl_backend::utils::files::{
    FileStore, MemoryFileStore, Sweep, SweepReport, remove_dead_files,
};
use opn_onl_backend::utils::privacy::{
    purge_click_pii_for_user, scrub_deleted_users_click_pii, scrub_expired_click_pii,
};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};

async fn owner(db: &DatabaseConnection) -> i32 {
    users::ActiveModel {
        email: Set(unique_email()),
        password_hash: Set("not-a-real-hash".to_string()),
        created_at: Set(Utc::now().naive_utc()),
        email_verified: Set(true),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert owner")
    .id
}

struct Stored {
    file_id: i32,
    link_id: i32,
    key: String,
    protected: String,
}

/// A Secure View file with a protected copy, both in the bucket, on a link
/// that is live, deleted or expired.
async fn stored(
    db: &DatabaseConnection,
    store: &MemoryFileStore,
    user_id: i32,
    deleted_at: Option<chrono::NaiveDateTime>,
    expires_at: Option<chrono::NaiveDateTime>,
) -> Stored {
    let key = format!("files/{}", uuid::Uuid::new_v4().simple());
    let protected = format!("files/{}", uuid::Uuid::new_v4().simple());
    store.put_sized(&key, 10);
    store.put_sized(&protected, 12);
    let file = link_files::ActiveModel {
        user_id: Set(Some(user_id)),
        storage_key: Set(key.clone()),
        filename: Set("a.pdf".to_string()),
        content_type: Set("application/pdf".to_string()),
        size_bytes: Set(10),
        status: Set("ready".to_string()),
        created_at: Set(Utc::now().naive_utc()),
        secure_view: Set(true),
        protected_key: Set(Some(protected.clone())),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert file");
    let link = links::ActiveModel {
        code: Set(format!(
            "sv{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        )),
        original_url: Set(links::file_link_marker(file.id)),
        user_id: Set(Some(user_id)),
        deleted_at: Set(deleted_at),
        expires_at: Set(expires_at),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert link");
    let mut linked: link_files::ActiveModel = file.clone().into();
    linked.link_id = Set(Some(link.id));
    linked.update(db).await.expect("link the file");
    Stored {
        file_id: file.id,
        link_id: link.id,
        key,
        protected,
    }
}

async fn file(db: &DatabaseConnection, id: i32) -> link_files::Model {
    link_files::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("file row")
}

#[tokio::test]
async fn the_daily_sweep_removes_the_protected_copy_with_the_original() {
    let _serial = common::serial_guard().await;
    let db = setup_test_db().await;
    let store = MemoryFileStore::new();
    let user = owner(&db).await;
    let now = Utc::now().naive_utc();

    let live = stored(&db, &store, user, None, None).await;
    let deleted = stored(&db, &store, user, Some(now), None).await;
    let expired = stored(&db, &store, user, None, Some(now - Duration::hours(1))).await;
    // A link purged outright: the row has no link any more.
    let purged = stored(&db, &store, user, None, None).await;
    links::Entity::delete_by_id(purged.link_id)
        .exec(&db)
        .await
        .unwrap();

    remove_dead_files(&db, &store, Sweep::All).await.unwrap();

    for (gone, what) in [
        (&deleted, "deleted"),
        (&expired, "expired"),
        (&purged, "purged"),
    ] {
        assert!(!store.contains(&gone.key), "{what}: the original");
        assert!(
            !store.contains(&gone.protected),
            "{what}: the protected copy"
        );
        assert!(file(&db, gone.file_id).await.deleted_at.is_some(), "{what}");
    }
    assert!(store.contains(&live.key), "a live link keeps its file");
    assert!(store.contains(&live.protected), "and its protected copy");
    assert_eq!(file(&db, live.file_id).await.deleted_at, None);
    let mut left = store.keys();
    left.sort();
    let mut expected = vec![live.key.clone(), live.protected.clone()];
    expected.sort();
    assert_eq!(left, expected, "nothing else is left in the bucket");
}

#[tokio::test]
async fn a_copy_the_bucket_will_not_remove_keeps_the_row_for_the_next_sweep() {
    let _serial = common::serial_guard().await;
    let db = setup_test_db().await;
    let store = MemoryFileStore::new();
    let user = owner(&db).await;
    let deleted = stored(&db, &store, user, Some(Utc::now().naive_utc()), None).await;

    store.fail_deletes(true);
    let report = remove_dead_files(&db, &store, Sweep::Files(vec![deleted.file_id]))
        .await
        .unwrap();
    assert_eq!(
        report,
        SweepReport {
            removed: 0,
            failed: 1
        }
    );
    let row = file(&db, deleted.file_id).await;
    assert_eq!(row.deleted_at, None, "not marked while anything is left");
    assert_eq!(
        row.protected_key.as_deref(),
        Some(deleted.protected.as_str())
    );

    store.fail_deletes(false);
    let report = remove_dead_files(&db, &store, Sweep::Files(vec![deleted.file_id]))
        .await
        .unwrap();
    assert_eq!(report.removed, 1);
    assert!(!store.contains(&deleted.key));
    assert!(!store.contains(&deleted.protected));
}

#[tokio::test]
async fn a_copy_recorded_while_the_sweep_ran_is_removed_by_the_next_one() {
    let _serial = common::serial_guard().await;
    let db = setup_test_db().await;
    let store = std::sync::Arc::new(MemoryFileStore::new());
    let user = owner(&db).await;
    let deleted = stored(&db, &store, user, Some(Utc::now().naive_utc()), None).await;

    // The row as the sweep reads it has no copy yet...
    let mut without: link_files::ActiveModel = file(&db, deleted.file_id).await.into();
    without.protected_key = Set(None);
    without.update(&db).await.unwrap();

    // ...and one is recorded between that read and the sweep marking the row:
    // the sweep is held inside its delete of the original while it happens.
    struct RecordsACopy {
        inner: std::sync::Arc<MemoryFileStore>,
        db: DatabaseConnection,
        file_id: i32,
        protected: String,
    }
    impl FileStore for RecordsACopy {
        fn presign_put<'a>(
            &'a self,
            key: &'a str,
            content_type: &'a str,
            content_length: u64,
            ttl: std::time::Duration,
        ) -> futures::future::BoxFuture<
            'a,
            Result<
                opn_onl_backend::utils::files::PresignedUpload,
                opn_onl_backend::utils::files::StoreError,
            >,
        > {
            self.inner
                .presign_put(key, content_type, content_length, ttl)
        }
        fn presign_get<'a>(
            &'a self,
            key: &'a str,
            content_type: &'a str,
            disposition: &'a str,
            ttl: std::time::Duration,
        ) -> futures::future::BoxFuture<'a, Result<String, opn_onl_backend::utils::files::StoreError>>
        {
            self.inner.presign_get(key, content_type, disposition, ttl)
        }
        fn head<'a>(
            &'a self,
            key: &'a str,
        ) -> futures::future::BoxFuture<
            'a,
            Result<Option<u64>, opn_onl_backend::utils::files::StoreError>,
        > {
            self.inner.head(key)
        }
        fn delete<'a>(
            &'a self,
            key: &'a str,
        ) -> futures::future::BoxFuture<'a, Result<(), opn_onl_backend::utils::files::StoreError>>
        {
            Box::pin(async move {
                let mut row: link_files::ActiveModel = link_files::Entity::find_by_id(self.file_id)
                    .one(&self.db)
                    .await
                    .unwrap()
                    .unwrap()
                    .into();
                row.protected_key = Set(Some(self.protected.clone()));
                row.update(&self.db).await.unwrap();
                self.inner.delete(key).await
            })
        }
        fn get<'a>(
            &'a self,
            key: &'a str,
        ) -> futures::future::BoxFuture<
            'a,
            Result<
                Option<opn_onl_backend::utils::files::StoredObject>,
                opn_onl_backend::utils::files::StoreError,
            >,
        > {
            self.inner.get(key)
        }
        fn put<'a>(
            &'a self,
            key: &'a str,
            bytes: axum::body::Bytes,
            content_type: &'a str,
        ) -> futures::future::BoxFuture<'a, Result<(), opn_onl_backend::utils::files::StoreError>>
        {
            self.inner.put(key, bytes, content_type)
        }
    }
    let racing = RecordsACopy {
        inner: store.clone(),
        db: db.clone(),
        file_id: deleted.file_id,
        protected: deleted.protected.clone(),
    };

    let report = remove_dead_files(&db, &racing, Sweep::Files(vec![deleted.file_id]))
        .await
        .unwrap();
    assert_eq!(report.removed, 0, "the row was not marked: a copy appeared");
    assert_eq!(report.failed, 1);
    let row = file(&db, deleted.file_id).await;
    assert_eq!(row.deleted_at, None);
    assert!(store.contains(&deleted.protected), "still in the bucket");

    // The next sweep sees the copy and removes it.
    let report = remove_dead_files(&db, store.as_ref(), Sweep::Files(vec![deleted.file_id]))
        .await
        .unwrap();
    assert_eq!(report.removed, 1);
    assert!(!store.contains(&deleted.protected));
    assert!(file(&db, deleted.file_id).await.deleted_at.is_some());
}

async fn click(
    db: &DatabaseConnection,
    link_id: i32,
    age_days: i64,
    email: Option<&str>,
) -> click_events::Model {
    click_events::ActiveModel {
        link_id: Set(link_id),
        created_at: Set(Utc::now().naive_utc() - Duration::days(age_days)),
        country: Set(Some("US".to_string())),
        viewer_email: Set(email.map(str::to_string)),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert click")
}

async fn click_row(db: &DatabaseConnection, id: i64) -> click_events::Model {
    click_events::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("click row")
}

async fn personal_link(db: &DatabaseConnection, user_id: i32) -> i32 {
    links::ActiveModel {
        code: Set(format!(
            "sv{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        )),
        original_url: Set("https://iana.org/".to_string()),
        user_id: Set(Some(user_id)),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert link")
    .id
}

#[tokio::test]
async fn the_retention_scrub_erases_viewer_emails_past_the_window() {
    let _serial = common::serial_guard().await;
    let db = setup_test_db().await;
    let user = owner(&db).await;
    let link = personal_link(&db, user).await;

    // Only an email, no IP or user agent: the scrub must still find the row.
    let old = click(&db, link, 400, Some("old-reader@example.org")).await;
    let recent = click(&db, link, 30, Some("recent-reader@example.org")).await;

    scrub_expired_click_pii(&db, 396).await.unwrap();

    let old = click_row(&db, old.id).await;
    assert_eq!(old.viewer_email, None, "past the window");
    assert_eq!(old.country.as_deref(), Some("US"), "aggregates are kept");
    assert_eq!(
        click_row(&db, recent.id).await.viewer_email.as_deref(),
        Some("recent-reader@example.org"),
        "inside the window"
    );
}

#[tokio::test]
async fn deleting_an_account_erases_the_viewer_emails_on_its_links() {
    let _serial = common::serial_guard().await;
    let db = setup_test_db().await;
    let leaving = owner(&db).await;
    let staying = owner(&db).await;
    let theirs = click(
        &db,
        personal_link(&db, leaving).await,
        1,
        Some("reader@example.org"),
    )
    .await;
    let others = click(
        &db,
        personal_link(&db, staying).await,
        1,
        Some("other-reader@example.org"),
    )
    .await;

    let purged = purge_click_pii_for_user(&db, leaving).await.unwrap();
    assert_eq!(purged, 1);
    assert_eq!(click_row(&db, theirs.id).await.viewer_email, None);
    assert_eq!(
        click_row(&db, others.id).await.viewer_email.as_deref(),
        Some("other-reader@example.org"),
        "another account's links are untouched"
    );
}

#[tokio::test]
async fn the_daily_scrub_erases_viewer_emails_on_a_deleted_accounts_own_links() {
    // What the daily task runs for accounts whose deletion did not get as far
    // as its own scrub. Personal links only, like the IP and the user agent:
    // an organization's link is the organization's, and keeps its clicks'
    // emails until the retention window.
    let _serial = common::serial_guard().await;
    let db = setup_test_db().await;
    let gone = owner(&db).await;
    let staying = owner(&db).await;
    let mut deleted: users::ActiveModel = users::Entity::find_by_id(gone)
        .one(&db)
        .await
        .unwrap()
        .unwrap()
        .into();
    deleted.deleted_at = Set(Some(Utc::now().naive_utc()));
    deleted.update(&db).await.unwrap();

    // Only an email on the row, no IP, user agent or referer: the scrub must
    // find it by the email alone.
    let theirs = click(
        &db,
        personal_link(&db, gone).await,
        1,
        Some("reader@example.org"),
    )
    .await;
    let others = click(
        &db,
        personal_link(&db, staying).await,
        1,
        Some("other-reader@example.org"),
    )
    .await;
    let org = organizations::ActiveModel {
        name: Set("Team".to_string()),
        slug: Set(format!("sv-{}", uuid::Uuid::new_v4().simple())),
        owner_id: Set(staying),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert organization");
    let org_link = links::ActiveModel {
        code: Set(format!(
            "sv{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        )),
        original_url: Set("https://iana.org/".to_string()),
        user_id: Set(Some(gone)),
        org_id: Set(Some(org.id)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert organization link")
    .id;
    let the_teams = click(&db, org_link, 1, Some("team-reader@example.org")).await;

    scrub_deleted_users_click_pii(&db, 1000).await.unwrap();

    let theirs = click_row(&db, theirs.id).await;
    assert_eq!(theirs.viewer_email, None, "the deleted account's own link");
    assert_eq!(theirs.country.as_deref(), Some("US"), "aggregates are kept");
    assert_eq!(
        click_row(&db, others.id).await.viewer_email.as_deref(),
        Some("other-reader@example.org"),
        "a live account's links are untouched"
    );
    assert_eq!(
        click_row(&db, the_teams.id).await.viewer_email.as_deref(),
        Some("team-reader@example.org"),
        "an organization's link is not the departing member's"
    );
}
