//! A [`FileStore`] that keeps nothing but what it was asked to do.
//!
//! The integration tests run the real router on it: there is no bucket to test
//! against, and what the app must get right is which calls it makes and when.

use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

use super::{FileStore, PresignedUpload, StoreError};

/// A download URL the store signed, as the pieces it was asked to sign.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedDownload {
    pub key: String,
    pub content_type: String,
    pub content_disposition: String,
    pub ttl: Duration,
    pub url: String,
}

/// A stop inside one `head` call, so a test can do something at the exact
/// moment a request has read its upload and is asking the bucket about it.
pub struct HeadGate {
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}

impl HeadGate {
    /// Resolves once the held `head` call has started.
    pub async fn arrived(&self) {
        self.arrived.notified().await;
    }

    /// Let the held call go on.
    pub fn release(&self) {
        self.release.notify_one();
    }
}

#[derive(Default)]
pub struct MemoryFileStore {
    /// Key to size in bytes.
    objects: Mutex<HashMap<String, u64>>,
    downloads: Mutex<Vec<SignedDownload>>,
    fail_deletes: AtomicBool,
    fail_heads: AtomicBool,
    head_gate: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

impl MemoryFileStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// What the browser's `PUT` does: an object of `size` bytes at `key`.
    pub fn put(&self, key: &str, size: u64) {
        self.objects.lock().unwrap().insert(key.to_string(), size);
    }

    pub fn contains(&self, key: &str) -> bool {
        self.objects.lock().unwrap().contains_key(key)
    }

    /// Every download URL signed so far, oldest first.
    pub fn downloads(&self) -> Vec<SignedDownload> {
        self.downloads.lock().unwrap().clone()
    }

    /// Make every `delete` fail (or work again), as an unreachable bucket would.
    pub fn fail_deletes(&self, fail: bool) {
        self.fail_deletes.store(fail, Ordering::SeqCst);
    }

    /// Make every `head` fail (or work again), as an unreachable bucket would.
    pub fn fail_heads(&self, fail: bool) {
        self.fail_heads.store(fail, Ordering::SeqCst);
    }

    /// Hold the next `head` call until the returned gate is released.
    pub fn hold_next_head(&self) -> HeadGate {
        let gate = HeadGate {
            arrived: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        *self.head_gate.lock().unwrap() = Some((gate.arrived.clone(), gate.release.clone()));
        gate
    }
}

impl FileStore for MemoryFileStore {
    fn presign_put<'a>(
        &'a self,
        key: &'a str,
        content_type: &'a str,
        content_length: u64,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<PresignedUpload, StoreError>> {
        Box::pin(async move {
            Ok(PresignedUpload {
                url: format!(
                    "https://uploads.test/{key}?content-length={content_length}&X-Amz-Expires={}&X-Amz-Signature=upload",
                    ttl.as_secs()
                ),
                headers: vec![("content-type".to_string(), content_type.to_string())],
            })
        })
    }

    fn presign_get<'a>(
        &'a self,
        key: &'a str,
        response_content_type: &'a str,
        response_content_disposition: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<String, StoreError>> {
        Box::pin(async move {
            let url = format!(
                "https://downloads.test/{key}?response-content-type={}&response-content-disposition={}&X-Amz-Expires={}&X-Amz-Signature=download",
                urlencoding::encode(response_content_type),
                urlencoding::encode(response_content_disposition),
                ttl.as_secs()
            );
            self.downloads.lock().unwrap().push(SignedDownload {
                key: key.to_string(),
                content_type: response_content_type.to_string(),
                content_disposition: response_content_disposition.to_string(),
                ttl,
                url: url.clone(),
            });
            Ok(url)
        })
    }

    fn head<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<u64>, StoreError>> {
        Box::pin(async move {
            let gate = self.head_gate.lock().unwrap().take();
            if let Some((arrived, release)) = gate {
                arrived.notify_one();
                release.notified().await;
            }
            if self.fail_heads.load(Ordering::SeqCst) {
                return Err(StoreError("head refused (test)".to_string()));
            }
            Ok(self.objects.lock().unwrap().get(key).copied())
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            if self.fail_deletes.load(Ordering::SeqCst) {
                return Err(StoreError("delete refused (test)".to_string()));
            }
            self.objects.lock().unwrap().remove(key);
            Ok(())
        })
    }
}
