//! The daily file sweep (`remove_dead_files` with `Sweep::All`).
//!
//! It looks at every file in the database, so these tests run alone
//! (`integration_serial`): in the parallel binary it would mark other tests'
//! files as removed from a bucket they were never in.

use crate::common;

use chrono::{Duration, Utc};
use common::{setup_test_db, unique_email};
use opn_onl_backend::entity::{link_files, links, users};
use opn_onl_backend::utils::files::{
    FileStore, MemoryFileStore, PENDING_UPLOAD_HOURS, Sweep, remove_dead_files, spawn_cleanup_task,
};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};
use std::sync::Arc;

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
    link_id: Option<i32>,
    key: String,
}

/// A stored object and its row, as the upload flow leaves them. `link` is
/// `(deleted, expires)` for a completed upload, `None` for a pending one made
/// `pending_hours_ago`.
async fn stored(
    db: &DatabaseConnection,
    store: &MemoryFileStore,
    user_id: i32,
    link: Option<(Option<chrono::NaiveDateTime>, Option<chrono::NaiveDateTime>)>,
    pending_hours_ago: i64,
) -> Stored {
    let key = format!("files/{}", uuid::Uuid::new_v4().simple());
    store.put(&key, 10);
    let file = link_files::ActiveModel {
        user_id: Set(Some(user_id)),
        storage_key: Set(key.clone()),
        filename: Set("a.pdf".to_string()),
        content_type: Set("application/pdf".to_string()),
        size_bytes: Set(10),
        status: Set("pending".to_string()),
        created_at: Set(Utc::now().naive_utc() - Duration::hours(pending_hours_ago)),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert file");
    let Some((deleted_at, expires_at)) = link else {
        return Stored {
            file_id: file.id,
            link_id: None,
            key,
        };
    };
    let link = links::ActiveModel {
        code: Set(format!(
            "fs{}",
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
    let file_id = file.id;
    let mut ready: link_files::ActiveModel = file.into();
    ready.link_id = Set(Some(link.id));
    ready.status = Set("ready".to_string());
    ready.update(db).await.expect("join file to link");
    Stored {
        file_id,
        link_id: Some(link.id),
        key,
    }
}

async fn removed(db: &DatabaseConnection, file: &Stored) -> bool {
    link_files::Entity::find_by_id(file.file_id)
        .one(db)
        .await
        .unwrap()
        .expect("file row is kept")
        .deleted_at
        .is_some()
}

#[tokio::test]
async fn the_sweep_removes_files_nothing_can_serve_and_keeps_the_rest() {
    let _serial = crate::common::serial_guard().await;
    let db = setup_test_db().await;
    let store = MemoryFileStore::new();
    let user = owner(&db).await;
    let now = Utc::now().naive_utc();
    let past = now - Duration::minutes(5);
    let future = now + Duration::days(30);

    // Dead.
    let deleted_link = stored(&db, &store, user, Some((Some(past), None)), 0).await;
    let expired_link = stored(&db, &store, user, Some((None, Some(past))), 0).await;
    let stale_upload = stored(&db, &store, user, None, PENDING_UPLOAD_HOURS + 1).await;
    let purged_link = stored(&db, &store, user, Some((Some(past), None)), 0).await;
    links::Entity::delete_by_id(purged_link.link_id.unwrap())
        .exec(&db)
        .await
        .expect("purge the link row");
    // Alive.
    let live = stored(&db, &store, user, Some((None, None)), 0).await;
    let not_yet_expired = stored(&db, &store, user, Some((None, Some(future))), 0).await;
    let fresh_upload = stored(&db, &store, user, None, PENDING_UPLOAD_HOURS - 1).await;

    let report = remove_dead_files(&db, &store, Sweep::All).await.unwrap();
    assert!(report.removed >= 4, "{report:?}");
    assert_eq!(report.failed, 0);

    for (file, what) in [
        (&deleted_link, "a deleted link's file"),
        (&expired_link, "an expired link's file"),
        (&stale_upload, "an upload never completed"),
        (&purged_link, "the file of a link whose row was purged"),
    ] {
        assert!(!store.contains(&file.key), "{what} must leave the bucket");
        assert!(removed(&db, file).await, "{what} must be marked removed");
    }
    for (file, what) in [
        (&live, "a live link's file"),
        (&not_yet_expired, "the file of a link that expires later"),
        (&fresh_upload, "an upload still in progress"),
    ] {
        assert!(store.contains(&file.key), "{what} must stay");
        assert!(
            !removed(&db, file).await,
            "{what} must not be marked removed"
        );
    }

    // A second sweep has nothing left to do.
    let again = remove_dead_files(&db, &store, Sweep::All).await.unwrap();
    assert_eq!(again.removed, 0, "{again:?}");
}

#[tokio::test]
async fn a_file_the_bucket_would_not_delete_is_removed_by_a_later_sweep() {
    let _serial = crate::common::serial_guard().await;
    let db = setup_test_db().await;
    let store = MemoryFileStore::new();
    let user = owner(&db).await;
    let past = Utc::now().naive_utc() - Duration::minutes(5);
    let file = stored(&db, &store, user, Some((Some(past), None)), 0).await;

    store.fail_deletes(true);
    let report = remove_dead_files(&db, &store, Sweep::All).await.unwrap();
    assert!(report.failed >= 1, "{report:?}");
    assert!(store.contains(&file.key));
    assert!(
        !removed(&db, &file).await,
        "a failed delete must not be recorded as done"
    );

    store.fail_deletes(false);
    let report = remove_dead_files(&db, &store, Sweep::All).await.unwrap();
    assert!(report.removed >= 1, "{report:?}");
    assert!(!store.contains(&file.key));
    assert!(removed(&db, &file).await);
}

#[tokio::test]
async fn the_daily_task_sweeps_when_it_starts() {
    let _serial = crate::common::serial_guard().await;
    let db = setup_test_db().await;
    let store = Arc::new(MemoryFileStore::new());
    let user = owner(&db).await;
    let past = Utc::now().naive_utc() - Duration::minutes(5);
    let expired = stored(&db, &store, user, Some((None, Some(past))), 0).await;
    let live = stored(&db, &store, user, Some((None, None)), 0).await;

    let as_store: Arc<dyn FileStore> = store.clone();
    spawn_cleanup_task(db.clone(), as_store);

    let mut swept = false;
    for _ in 0..100 {
        if !store.contains(&expired.key) && removed(&db, &expired).await {
            swept = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(swept, "the task must sweep on its first tick");
    assert!(store.contains(&live.key));
    assert!(!removed(&db, &live).await);
}
