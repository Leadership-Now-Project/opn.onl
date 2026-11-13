//! The real bucket: S3, or anything that speaks its API.

use aws_config::BehaviorVersion;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::presigning::PresigningConfig;
use futures::future::BoxFuture;
use std::time::Duration;

use super::{FileStore, FilesConfig, PresignedUpload, StoreError};

pub struct S3FileStore {
    client: S3Client,
    bucket: String,
}

impl S3FileStore {
    /// Build the client. With `FILES_S3_ACCESS_KEY` and `FILES_S3_SECRET_KEY`
    /// set, those are the credentials. With both blank the AWS default chain
    /// is used (environment, shared profile, container or EC2 instance role),
    /// so an instance with a role attached needs no keys in its `.env`.
    ///
    /// Nothing is sent to the bucket here: a wrong bucket, region or
    /// credential shows up on the first upload, not at start.
    pub async fn connect(config: &FilesConfig) -> Self {
        let region = Region::new(config.region.clone());
        let mut builder = match &config.static_keys {
            Some((access_key, secret_key)) => aws_sdk_s3::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .region(region)
                .credentials_provider(Credentials::new(
                    access_key.clone(),
                    secret_key.clone(),
                    None,
                    None,
                    "static",
                )),
            None => {
                let shared = aws_config::defaults(BehaviorVersion::latest())
                    .region(region)
                    .load()
                    .await;
                aws_sdk_s3::config::Builder::from(&shared)
            }
        };
        if let Some(endpoint) = &config.endpoint {
            // A custom endpoint is an S3-compatible service; those are
            // addressed by path, as the backup bucket is.
            builder = builder.endpoint_url(endpoint).force_path_style(true);
        }
        tracing::info!(
            "File sharing enabled ({})",
            if config.static_keys.is_some() {
                "static credentials"
            } else {
                "default AWS credential chain"
            }
        );
        Self {
            client: S3Client::from_conf(builder.build()),
            bucket: config.bucket.clone(),
        }
    }
}

fn presigning(ttl: Duration) -> Result<PresigningConfig, StoreError> {
    PresigningConfig::expires_in(ttl).map_err(|error| StoreError(error.to_string()))
}

/// The SDK's error with its cause, which is where the HTTP status and the S3
/// error code are. It names the operation and the key at most, no credentials.
fn store_error<E: std::error::Error + 'static>(operation: &str, error: E) -> StoreError {
    let mut message = format!("{operation}: {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    StoreError(message)
}

impl FileStore for S3FileStore {
    fn presign_put<'a>(
        &'a self,
        key: &'a str,
        content_type: &'a str,
        content_length: u64,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<PresignedUpload, StoreError>> {
        Box::pin(async move {
            let length = i64::try_from(content_length)
                .map_err(|_| StoreError("content length out of range".to_string()))?;
            let request = self
                .client
                .put_object()
                .bucket(&self.bucket)
                .key(key)
                .content_type(content_type)
                .content_length(length)
                .presigned(presigning(ttl)?)
                .await
                .map_err(|error| store_error("presign PutObject", error))?;
            Ok(PresignedUpload {
                url: request.uri().to_string(),
                // `Host` is the browser's to set, and it cannot set
                // `Content-Length` either: it sends the body's real length,
                // which S3 then checks against the signed one.
                headers: request
                    .headers()
                    .filter(|(name, _)| {
                        !name.eq_ignore_ascii_case("host")
                            && !name.eq_ignore_ascii_case("content-length")
                    })
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
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
            let request = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(key)
                .response_content_type(response_content_type)
                .response_content_disposition(response_content_disposition)
                .presigned(presigning(ttl)?)
                .await
                .map_err(|error| store_error("presign GetObject", error))?;
            Ok(request.uri().to_string())
        })
    }

    fn head<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<u64>, StoreError>> {
        Box::pin(async move {
            match self
                .client
                .head_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
            {
                Ok(object) => Ok(Some(
                    object
                        .content_length()
                        .and_then(|length| u64::try_from(length).ok())
                        .unwrap_or(0),
                )),
                Err(error)
                    if error
                        .as_service_error()
                        .is_some_and(|service| service.is_not_found()) =>
                {
                    Ok(None)
                }
                Err(error) => Err(store_error("HeadObject", error)),
            }
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.client
                .delete_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
                .map(|_| ())
                .map_err(|error| store_error("DeleteObject", error))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Signing is local arithmetic, so these run without a bucket. They are the
    // only part of this file a test here can reach: HEAD and DELETE need S3.

    async fn store() -> S3FileStore {
        S3FileStore::connect(&FilesConfig {
            bucket: "lnp-files-test".to_string(),
            region: "us-east-2".to_string(),
            endpoint: None,
            static_keys: Some(("AKIDEXAMPLE".to_string(), "secret".to_string())),
            max_bytes: 1,
            upload_url_ttl: Duration::from_secs(900),
            download_url_ttl: Duration::from_secs(300),
        })
        .await
    }

    fn query(url: &str) -> std::collections::HashMap<String, String> {
        url::Url::parse(url)
            .expect("a URL")
            .query_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn an_upload_url_is_bound_to_the_type_the_length_and_the_lifetime() {
        let upload = store()
            .await
            .presign_put(
                "files/abc",
                "application/pdf",
                1234,
                Duration::from_secs(900),
            )
            .await
            .expect("signed");

        let url = url::Url::parse(&upload.url).unwrap();
        assert_eq!(
            url.host_str(),
            Some("lnp-files-test.s3.us-east-2.amazonaws.com")
        );
        assert_eq!(url.path(), "/files/abc");
        let query = query(&upload.url);
        assert_eq!(query["X-Amz-Expires"], "900");
        let signed: Vec<&str> = query["X-Amz-SignedHeaders"].split(';').collect();
        assert!(signed.contains(&"content-type"), "{signed:?}");
        assert!(signed.contains(&"content-length"), "{signed:?}");
        assert_eq!(
            upload.headers,
            vec![("content-type".to_string(), "application/pdf".to_string())],
            "the browser is told to send the content type and nothing it cannot set"
        );
    }

    #[tokio::test]
    async fn a_download_url_overrides_the_type_and_the_disposition() {
        let url = store()
            .await
            .presign_get(
                "files/abc",
                "application/octet-stream",
                "attachment; filename=\"a.html\"; filename*=UTF-8''a.html",
                Duration::from_secs(300),
            )
            .await
            .expect("signed");

        let query = query(&url);
        assert_eq!(query["X-Amz-Expires"], "300");
        assert_eq!(query["response-content-type"], "application/octet-stream");
        assert_eq!(
            query["response-content-disposition"],
            "attachment; filename=\"a.html\"; filename*=UTF-8''a.html"
        );
        assert!(query.contains_key("X-Amz-Signature"));
    }
}
