//! S3 adapter: simplified aws-sdk-s3 implementation with multipart upload, retries, and validation.

use crate::cadapter::client::ObjectBackend;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_config::timeout::TimeoutConfig;
use aws_sdk_s3::config::RequestChecksumCalculation;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::{ByteStream, SdkBody};
use aws_sdk_s3::{Client, config::Region};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use bytes::Bytes;
use hyper::Body;
use md5;
use std::sync::Arc;
use tokio::time::{Duration, sleep};

const RUSTFS_EC_BLOCK_SIZE_HINT_KEY: &str = "rustfs-ec-block-size-hint";

pub(crate) fn validate_rustfs_ec_block_size_hint(hint: Option<usize>) -> Result<()> {
    if let Some(size) = hint
        && ![65_536, 262_144, 1_048_576, 4_194_304].contains(&size)
    {
        return Err(anyhow!(
            "RustFS EC block size hint must be 65536, 262144, 1048576 or 4194304 bytes"
        ));
    }
    Ok(())
}

/// S3 backend configuration options
#[derive(Debug, Clone)]
pub struct S3Config {
    /// S3 bucket name
    pub bucket: String,
    /// AWS region (optional, will use default if not specified)
    pub region: Option<String>,
    /// Part size for multipart uploads in bytes (default: 8MB)
    pub part_size: usize,
    /// Maximum concurrent multipart upload parts (default: 4)
    pub max_concurrency: usize,
    /// Maximum retry attempts for failed operations (default: 3)
    pub max_retries: u32,
    /// Base delay for exponential backoff in milliseconds (default: 100ms)
    pub retry_base_delay: u64,
    /// Enable MD5 checksums for uploads (default: false, matching JuiceFS behavior)
    pub enable_md5: bool,
    /// Custom endpoint URL (e.g. for MinIO or localstack)
    pub endpoint: Option<String>,
    /// Force path-style access (required for some S3-compatible services)
    pub force_path_style: bool,
    /// Disable SDK-level request checksums and SigV4 payload hashing.
    /// Safe for self-hosted S3 backends (RustFS/MinIO) over trusted networks.
    pub disable_payload_checksum: bool,
    /// Optional RustFS EC encoding block size hint in bytes. This does not
    /// change BrewFS block boundaries or S3 multipart part sizes.
    pub rustfs_ec_block_size_hint: Option<usize>,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            bucket: String::new(),
            region: None,
            part_size: 16 * 1024 * 1024, // 16MB — larger parts reduce HTTP overhead
            max_concurrency: 32,         // Raise S3 parallelism to keep multi-job reads saturated
            max_retries: 1,
            retry_base_delay: 100,
            enable_md5: false,
            endpoint: None,
            force_path_style: false,
            disable_payload_checksum: true,
            rustfs_ec_block_size_hint: None,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct S3Backend {
    client: Client,
    config: S3Config,
}

#[allow(dead_code)]
impl S3Backend {
    /// Create new S3 backend with default configuration
    pub async fn new(bucket: impl Into<String>) -> Result<Self> {
        let config = S3Config {
            bucket: bucket.into(),
            ..Default::default()
        };
        Self::with_config(config).await
    }

    /// Create new S3 backend with custom configuration
    pub async fn with_config(config: S3Config) -> Result<Self> {
        if config.bucket.is_empty() {
            return Err(anyhow!("Bucket name cannot be empty"));
        }
        validate_rustfs_ec_block_size_hint(config.rustfs_ec_block_size_hint)?;

        let mut aws_config_loader = aws_config::defaults(BehaviorVersion::latest());

        // Prevent indefinite hangs on stalled S3 connections.
        let timeout_config = TimeoutConfig::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .operation_timeout(Duration::from_secs(120))
            .build();
        aws_config_loader = aws_config_loader.timeout_config(timeout_config);

        if let Some(region) = &config.region {
            aws_config_loader = aws_config_loader.region(Region::new(region.clone()));
        }

        tracing::info!(
            endpoint = ?config.endpoint,
            region = ?config.region,
            bucket = %config.bucket,
            "s3 backend aws config load begin"
        );
        let aws_config = aws_config_loader.load().await;
        tracing::info!("s3 backend aws config load complete");

        let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&aws_config);

        if let Some(endpoint) = &config.endpoint {
            s3_config_builder = s3_config_builder.endpoint_url(endpoint);
        }

        if config.force_path_style {
            s3_config_builder = s3_config_builder.force_path_style(true);
        }

        if config.disable_payload_checksum {
            // Skip payload checksum (SigV4 SHA-256 of request body) to send
            // UNSIGNED-PAYLOAD. This matches JuiceFS behavior and avoids wasting
            // ~20% CPU on cryptographic hashing for non-AWS S3 backends (MinIO, RustFS, etc.).
            s3_config_builder = s3_config_builder
                .request_checksum_calculation(RequestChecksumCalculation::WhenRequired);
            s3_config_builder = s3_config_builder.response_checksum_validation(
                aws_sdk_s3::config::ResponseChecksumValidation::WhenRequired,
            );
        }

        let client = Client::from_conf(s3_config_builder.build());
        tracing::info!("s3 backend client ready");

        Ok(Self { client, config })
    }

    fn md5_base64(data: &[u8]) -> String {
        let sum = md5::compute(data);
        B64.encode(sum.0)
    }

    fn md5_base64_chunks(chunks: &[Bytes]) -> String {
        let mut ctx = md5::Context::new();
        for chunk in chunks {
            ctx.consume(chunk);
        }
        B64.encode(ctx.compute().0)
    }

    fn stream_from_chunks(chunks: &[Bytes]) -> ByteStream {
        let owned = chunks.to_vec();
        let stream = futures::stream::iter(owned.into_iter().map(Ok::<Bytes, std::io::Error>));
        ByteStream::from_body_0_4(Body::wrap_stream(stream))
    }

    fn direct_stream_from_chunks(chunks: &[Bytes], total_size: usize) -> ByteStream {
        if let [chunk] = chunks {
            return SdkBody::from(chunk.clone()).into();
        }

        let mut data = Vec::with_capacity(total_size);
        for chunk in chunks {
            data.extend_from_slice(chunk);
        }
        SdkBody::from(data).into()
    }

    #[tracing::instrument(level = "debug", skip(self, chunks), fields(key, total_size))]
    async fn put_object_vectored_simple(&self, key: &str, chunks: Vec<Bytes>) -> Result<()> {
        let total_size = chunks.iter().map(|c| c.len()).sum::<usize>();
        tracing::Span::current().record("total_size", total_size);
        let checksum = if self.config.enable_md5 && total_size > 0 {
            Some(Self::md5_base64_chunks(&chunks))
        } else {
            None
        };

        let mut attempt = 0;
        loop {
            attempt += 1;

            let body = Self::direct_stream_from_chunks(&chunks, total_size);
            let mut request = self
                .client
                .put_object()
                .bucket(&self.config.bucket)
                .key(key)
                .body(body)
                .content_length(total_size as i64);

            if let Some(size) = self.config.rustfs_ec_block_size_hint {
                request = request.metadata(RUSTFS_EC_BLOCK_SIZE_HINT_KEY, size.to_string());
            }
            if let Some(sum) = checksum.as_ref() {
                request = request.content_md5(sum.clone());
            }

            let result = if self.config.disable_payload_checksum {
                request.customize().disable_payload_signing().send().await
            } else {
                request.send().await
            };

            match result {
                Ok(_) => return Ok(()),
                Err(_e) if attempt < self.config.max_retries => {
                    let delay = self.config.retry_base_delay * (1 << (attempt - 1));
                    sleep(Duration::from_millis(delay)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Put small objects directly (simpler than multipart upload)
    #[tracing::instrument(level = "debug", skip(self, data), fields(key, size = data.len()))]
    async fn put_object_simple(&self, key: &str, data: &[u8]) -> Result<()> {
        let mut attempt = 0;
        loop {
            attempt += 1;

            let mut request = self
                .client
                .put_object()
                .bucket(&self.config.bucket)
                .key(key)
                .body(SdkBody::from(data.to_vec()).into());

            if let Some(size) = self.config.rustfs_ec_block_size_hint {
                request = request.metadata(RUSTFS_EC_BLOCK_SIZE_HINT_KEY, size.to_string());
            }
            if self.config.enable_md5 {
                let checksum = Self::md5_base64(data);
                request = request.content_md5(checksum);
            }

            let result = if self.config.disable_payload_checksum {
                request.customize().disable_payload_signing().send().await
            } else {
                request.send().await
            };

            match result {
                Ok(_) => return Ok(()),
                Err(_e) if attempt < self.config.max_retries => {
                    let delay = self.config.retry_base_delay * (1 << (attempt - 1));
                    sleep(Duration::from_millis(delay)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Handle multipart upload for large objects
    async fn multipart_upload(&self, key: &str, data: &[u8]) -> Result<()> {
        // Create multipart upload
        let mut request = self
            .client
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key);
        if let Some(size) = self.config.rustfs_ec_block_size_hint {
            request = request.metadata(RUSTFS_EC_BLOCK_SIZE_HINT_KEY, size.to_string());
        }
        let create = request.send().await?;

        let upload_id = create
            .upload_id()
            .ok_or_else(|| anyhow!("Missing upload_id in create_multipart_upload response"))?
            .to_string();

        // Ensure we clean up the multipart upload if it fails
        let cleanup_on_drop = MultipartCleanupGuard {
            client: self.client.clone(),
            bucket: self.config.bucket.clone(),
            key: key.to_string(),
            upload_id: upload_id.clone(),
        };

        let data_arc = Arc::new(data.to_vec());
        let sem = Arc::new(tokio::sync::Semaphore::new(self.config.max_concurrency));

        // Concurrent upload of parts
        let mut parts = Vec::new();
        let total = data.len();
        let mut idx = 0usize;
        let mut part_number = 1i32;

        while idx < total {
            let end = (idx + self.config.part_size).min(total);
            let chunk_vec = data_arc.as_slice()[idx..end].to_vec();
            let client = self.client.clone();
            let bucket = self.config.bucket.clone();
            let key = key.to_string();
            let upload_id_cloned = upload_id.clone();
            let pn = part_number;
            let sem_cloned = sem.clone();
            let enable_md5 = self.config.enable_md5;
            let max_retries = self.config.max_retries;
            let retry_base_delay = self.config.retry_base_delay;
            let disable_payload_checksum = self.config.disable_payload_checksum;

            let fut = async move {
                // Concurrency control
                let _permit = sem_cloned
                    .acquire_owned()
                    .await
                    .with_context(|| "Multipart upload semaphore closed unexpectedly");
                let mut attempt = 0;

                loop {
                    attempt += 1;
                    let mut request = client
                        .upload_part()
                        .bucket(&bucket)
                        .key(&key)
                        .upload_id(&upload_id_cloned)
                        .part_number(pn)
                        .body(SdkBody::from(chunk_vec.clone()).into());

                    if enable_md5 {
                        let part_md5 = Self::md5_base64(&chunk_vec);
                        request = request.content_md5(part_md5);
                    }

                    let result = if disable_payload_checksum {
                        request.customize().disable_payload_signing().send().await
                    } else {
                        request.send().await
                    };

                    match result {
                        Ok(ok) => break Ok((pn, ok.e_tag().map(|s| s.to_string()))),
                        Err(_e) if attempt < max_retries => {
                            let delay = retry_base_delay * (1 << (attempt - 1));
                            sleep(Duration::from_millis(delay)).await;
                            continue;
                        }
                        Err(e) => break Err(e),
                    }
                }
            };
            parts.push(fut);

            idx = end;
            part_number += 1;
        }

        // Execute all parts concurrently
        let results: Vec<(i32, Option<String>)> = match futures::future::try_join_all(parts).await {
            Ok(v) => v,
            Err(e) => return Err(e.into()),
        };

        // Build completed parts
        let completed_parts = results
            .into_iter()
            .map(|(pn, etag)| {
                aws_sdk_s3::types::CompletedPart::builder()
                    .part_number(pn)
                    .set_e_tag(etag)
                    .build()
            })
            .collect::<Vec<_>>();

        let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();

        // Complete multipart upload
        self.client
            .complete_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            .send()
            .await?;

        // Disarm cleanup guard since upload succeeded
        std::mem::forget(cleanup_on_drop);

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self, chunks), fields(key, parts))]
    async fn multipart_upload_vectored(&self, key: &str, chunks: Vec<Bytes>) -> Result<()> {
        let mut request = self
            .client
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key);
        if let Some(size) = self.config.rustfs_ec_block_size_hint {
            request = request.metadata(RUSTFS_EC_BLOCK_SIZE_HINT_KEY, size.to_string());
        }
        let create = request.send().await?;

        let upload_id = create
            .upload_id()
            .ok_or_else(|| anyhow!("Missing upload_id in create_multipart_upload response"))?
            .to_string();

        let cleanup_on_drop = MultipartCleanupGuard {
            client: self.client.clone(),
            bucket: self.config.bucket.clone(),
            key: key.to_string(),
            upload_id: upload_id.clone(),
        };

        let mut parts: Vec<Vec<Bytes>> = Vec::new();
        let mut cur_part: Vec<Bytes> = Vec::new();
        let mut cur_len: usize = 0;

        for chunk in chunks.into_iter() {
            let mut offset = 0usize;
            while offset < chunk.len() {
                let remaining_part = self.config.part_size - cur_len;
                let remaining_chunk = chunk.len() - offset;
                let take = remaining_part.min(remaining_chunk);
                cur_part.push(chunk.slice(offset..offset + take));
                cur_len += take;
                offset += take;

                if cur_len == self.config.part_size {
                    parts.push(cur_part);
                    cur_part = Vec::new();
                    cur_len = 0;
                }
            }
        }

        if cur_len > 0 {
            parts.push(cur_part);
        }

        let sem = Arc::new(tokio::sync::Semaphore::new(self.config.max_concurrency));
        let mut futures = Vec::new();

        for (idx, part_chunks) in parts.into_iter().enumerate() {
            let part_len = part_chunks.iter().map(|c| c.len()).sum::<usize>();
            let part_md5 = if self.config.enable_md5 && part_len > 0 {
                Some(Self::md5_base64_chunks(&part_chunks))
            } else {
                None
            };

            let client = self.client.clone();
            let bucket = self.config.bucket.clone();
            let key = key.to_string();
            let upload_id_cloned = upload_id.clone();
            let pn = (idx + 1) as i32;
            let sem_cloned = sem.clone();
            let max_retries = self.config.max_retries;
            let retry_base_delay = self.config.retry_base_delay;
            let disable_payload_checksum = self.config.disable_payload_checksum;

            let fut = async move {
                let _permit = sem_cloned.acquire_owned().await;
                let mut attempt = 0;

                loop {
                    attempt += 1;
                    let body = S3Backend::stream_from_chunks(&part_chunks);
                    let mut request = client
                        .upload_part()
                        .bucket(&bucket)
                        .key(&key)
                        .upload_id(&upload_id_cloned)
                        .part_number(pn)
                        .body(body)
                        .content_length(part_len as i64);

                    if let Some(md5) = part_md5.as_ref() {
                        request = request.content_md5(md5.clone());
                    }

                    let result = if disable_payload_checksum {
                        request.customize().disable_payload_signing().send().await
                    } else {
                        request.send().await
                    };

                    match result {
                        Ok(ok) => break Ok((pn, ok.e_tag().map(|s| s.to_string()))),
                        Err(_e) if attempt < max_retries => {
                            let delay = retry_base_delay * (1 << (attempt - 1));
                            sleep(Duration::from_millis(delay)).await;
                            continue;
                        }
                        Err(e) => break Err(e),
                    }
                }
            };
            futures.push(fut);
        }

        let results: Vec<(i32, Option<String>)> = match futures::future::try_join_all(futures).await
        {
            Ok(v) => v,
            Err(e) => return Err(e.into()),
        };

        let completed_parts = results
            .into_iter()
            .map(|(pn, etag)| {
                aws_sdk_s3::types::CompletedPart::builder()
                    .part_number(pn)
                    .set_e_tag(etag)
                    .build()
            })
            .collect::<Vec<_>>();

        let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();

        self.client
            .complete_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            .send()
            .await?;

        std::mem::forget(cleanup_on_drop);
        Ok(())
    }
}

/// Guard to automatically clean up multipart uploads if they fail
struct MultipartCleanupGuard {
    client: Client,
    bucket: String,
    key: String,
    upload_id: String,
}

impl Drop for MultipartCleanupGuard {
    fn drop(&mut self) {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key = self.key.clone();
        let upload_id = self.upload_id.clone();

        tokio::spawn(async move {
            let _ = client
                .abort_multipart_upload()
                .bucket(&bucket)
                .key(&key)
                .upload_id(&upload_id)
                .send()
                .await;
        });
    }
}

#[async_trait]
impl ObjectBackend for S3Backend {
    #[tracing::instrument(level = "trace", skip(self, chunks), fields(key, chunk_count = chunks.len()))]
    async fn put_object_vectored(&self, key: &str, chunks: Vec<Bytes>) -> Result<()> {
        let total_size = chunks.iter().map(|e| e.len()).sum::<usize>();

        if total_size == 0 {
            return self.put_object_simple(key, &[]).await;
        }
        if total_size <= self.config.part_size {
            // RustFS is more reliable with a direct small-object body; single-chunk
            // writes keep the Bytes allocation shared and multi-chunk writes copy once.
            return self.put_object_vectored_simple(key, chunks).await;
        }

        self.multipart_upload_vectored(key, chunks).await
    }

    #[tracing::instrument(level = "debug", skip(self, data), fields(key, size = data.len()))]
    async fn put_object(&self, key: &str, data: &[u8]) -> Result<()> {
        // Small objects use direct put_object; large objects use multipart upload
        if data.len() <= self.config.part_size {
            return self.put_object_simple(key, data).await;
        }

        // Multipart upload for large objects
        self.multipart_upload(key, data).await
    }

    #[tracing::instrument(level = "debug", skip(self, data), fields(key, size = data.len()))]
    async fn put_object_create_only(&self, key: &str, data: &[u8]) -> Result<()> {
        let mut request = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .if_none_match("*")
            .body(SdkBody::from(data.to_vec()).into());
        if let Some(size) = self.config.rustfs_ec_block_size_hint {
            request = request.metadata(RUSTFS_EC_BLOCK_SIZE_HINT_KEY, size.to_string());
        }
        if self.config.enable_md5 {
            request = request.content_md5(Self::md5_base64(data));
        }
        let result = if self.config.disable_payload_checksum {
            request.customize().disable_payload_signing().send().await
        } else {
            request.send().await
        };
        if let Err(error) = result {
            if self.get_object(key).await?.as_deref() == Some(data) {
                return Ok(());
            }
            return Err(error.into());
        }
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self), fields(key))]
    async fn get_object(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await;

        match resp {
            Ok(o) => {
                use tokio::io::AsyncReadExt;
                let capacity = o
                    .content_length()
                    .and_then(|len| usize::try_from(len).ok())
                    .unwrap_or_default();
                let mut body = o.body.into_async_read();
                let mut buf = Vec::with_capacity(capacity);
                body.read_to_end(&mut buf).await?;
                Ok(Some(buf))
            }
            Err(SdkError::ServiceError(err)) if err.err().is_no_such_key() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get a range of bytes from an object.
    /// Used for small range reads in intelligent read strategy.
    #[tracing::instrument(level = "debug", skip(self, buf), fields(key, offset, len = buf.len()))]
    async fn get_object_range(&self, key: &str, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let end = offset + buf.len() as u64 - 1;
        let range_header = format!("bytes={}-{}", offset, end);

        let resp = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .range(range_header)
            .send()
            .await;

        match resp {
            Ok(o) => {
                use tokio::io::AsyncReadExt;
                let mut body = o.body.into_async_read();
                let mut read = 0;

                while read < buf.len() {
                    let n = body.read(&mut buf[read..]).await?;
                    if n == 0 {
                        break;
                    }
                    read += n;
                }

                Ok(read)
            }
            Err(SdkError::ServiceError(err)) if err.err().is_no_such_key() => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_etag(&self, key: &str) -> Result<String> {
        let resp = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await?;
        Ok(resp.e_tag().unwrap_or_default().to_string())
    }

    #[tracing::instrument(level = "debug", skip(self), fields(key))]
    async fn delete_object(&self, key: &str) -> Result<()> {
        let mut attempt = 0;

        loop {
            attempt += 1;
            match self
                .client
                .delete_object()
                .bucket(&self.config.bucket)
                .key(key)
                .send()
                .await
            {
                Ok(_) => return Ok(()),
                Err(_e) if attempt < self.config.max_retries => {
                    let delay = self.config.retry_base_delay * (1 << (attempt - 1));
                    sleep(Duration::from_millis(delay)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::Config;
    use aws_sdk_s3::config::retry::RetryConfig;
    use aws_sdk_s3::config::{Credentials, Region};
    use axum::body::{Body as AxumBody, to_bytes};
    use axum::extract::{Request, State};
    use axum::http::{HeaderMap, Method, StatusCode};
    use axum::response::{IntoResponse, Response};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::AsyncReadExt;
    use tokio::time::timeout;

    #[derive(Debug)]
    struct CapturedRequest {
        method: Method,
        path: String,
        query: String,
        headers: HeaderMap,
        body: Bytes,
    }

    #[derive(Clone, Default)]
    struct CaptureState {
        requests: Arc<tokio::sync::Mutex<Vec<CapturedRequest>>>,
        fail_next_put: Arc<AtomicBool>,
    }

    struct MockS3 {
        endpoint: String,
        state: CaptureState,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for MockS3 {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn is_multipart_initiation(query: &str) -> bool {
        query
            .split('&')
            .any(|parameter| parameter.split('=').next() == Some("uploads"))
    }

    async fn capture_s3_request(State(state): State<CaptureState>, request: Request) -> Response {
        let method = request.method().clone();
        let path = request.uri().path().to_string();
        let query = request.uri().query().unwrap_or_default().to_string();
        let headers = request.headers().clone();
        let body = to_bytes(request.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();
        state.requests.lock().await.push(CapturedRequest {
            method: method.clone(),
            path,
            query: query.clone(),
            headers,
            body,
        });

        if method == Method::PUT && state.fail_next_put.swap(false, Ordering::SeqCst) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "<Error><Code>InternalError</Code></Error>",
            )
                .into_response();
        }

        let body = if method == Method::POST && is_multipart_initiation(&query) {
            "<InitiateMultipartUploadResult><Bucket>test-bucket</Bucket><Key>test</Key><UploadId>test-upload</UploadId></InitiateMultipartUploadResult>"
        } else if method == Method::POST {
            "<CompleteMultipartUploadResult><Bucket>test-bucket</Bucket><Key>test</Key><ETag>\"test-etag\"</ETag></CompleteMultipartUploadResult>"
        } else {
            ""
        };
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/xml")
            .header("etag", "\"test-etag\"")
            .body(AxumBody::from(body))
            .unwrap()
    }

    impl MockS3 {
        async fn new() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let state = CaptureState::default();
            let app = axum::Router::new()
                .fallback(capture_s3_request)
                .with_state(state.clone());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            Self {
                endpoint,
                state,
                task,
            }
        }

        fn backend(&self, hint: Option<usize>) -> S3Backend {
            let config = Config::builder()
                .endpoint_url(&self.endpoint)
                .force_path_style(true)
                .region(Region::new("us-east-1"))
                .credentials_provider(Credentials::new("test", "test", None, None, "test"))
                .retry_config(RetryConfig::standard().with_max_attempts(1))
                .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
                .response_checksum_validation(
                    aws_sdk_s3::config::ResponseChecksumValidation::WhenRequired,
                )
                .timeout_config(
                    TimeoutConfig::builder()
                        .operation_timeout(Duration::from_secs(10))
                        .build(),
                )
                .build();
            S3Backend {
                client: Client::from_conf(config),
                config: S3Config {
                    bucket: "test-bucket".to_string(),
                    part_size: 5 * 1024 * 1024,
                    max_concurrency: 1,
                    retry_base_delay: 1,
                    rustfs_ec_block_size_hint: hint,
                    ..Default::default()
                },
            }
        }
    }

    async fn check_hint_on_upload_paths(hint: Option<usize>) {
        let mock = MockS3::new().await;
        let backend = mock.backend(hint);
        let small = b"hint-upload-payload";
        backend.put_object("simple", small).await.unwrap();
        backend
            .put_object_vectored(
                "vectored",
                vec![
                    Bytes::copy_from_slice(&small[..5]),
                    Bytes::copy_from_slice(&small[5..]),
                ],
            )
            .await
            .unwrap();
        backend
            .put_object_create_only("create-only", small)
            .await
            .unwrap();
        backend.put_object_vectored("empty", vec![]).await.unwrap();
        let large = vec![42; 5 * 1024 * 1024 + 13];
        backend.put_object("multipart", &large).await.unwrap();
        backend
            .put_object_vectored(
                "multipart-vectored",
                vec![
                    Bytes::copy_from_slice(&large[..17]),
                    Bytes::copy_from_slice(&large[17..]),
                ],
            )
            .await
            .unwrap();

        let requests = mock.state.requests.lock().await;
        let mut creates = 0;
        let mut parts = 0;
        let mut completes = 0;
        for request in requests.iter() {
            let creates_object = (request.method == Method::PUT
                && !request.query.contains("uploadId="))
                || (request.method == Method::POST && is_multipart_initiation(&request.query));
            let expected = if creates_object {
                creates += 1;
                hint.map(|size| size.to_string())
            } else {
                None
            };
            assert_eq!(
                request
                    .headers
                    .get("x-amz-meta-rustfs-ec-block-size-hint")
                    .map(|value| value.to_str().unwrap()),
                expected.as_deref(),
                "unexpected hint for {} {}?{}",
                request.method,
                request.path,
                request.query,
            );
            if expected.is_some() {
                assert!(
                    request.headers["authorization"]
                        .to_str()
                        .unwrap()
                        .contains("x-amz-meta-rustfs-ec-block-size-hint")
                );
            }
            if request.path.ends_with("/create-only") {
                assert_eq!(request.headers["if-none-match"], "*");
            }
            if request.method == Method::PUT && request.query.contains("partNumber=") {
                parts += 1;
                assert!(!request.body.is_empty());
            } else if request.method == Method::POST && request.query.contains("uploadId=") {
                completes += 1;
            } else if request.method == Method::PUT {
                let expected_body = if request.path.ends_with("/empty") {
                    &[][..]
                } else {
                    small.as_slice()
                };
                assert_eq!(request.body.as_ref(), expected_body);
            }
        }
        assert_eq!(creates, 6);
        assert_eq!(parts, 4);
        assert_eq!(completes, 2);
    }

    #[tokio::test]
    async fn rustfs_ec_hint_reaches_all_upload_creation_paths() {
        check_hint_on_upload_paths(Some(4_194_304)).await;
    }

    #[tokio::test]
    async fn rustfs_ec_hint_is_absent_by_default_on_all_upload_paths() {
        check_hint_on_upload_paths(None).await;
    }

    #[tokio::test]
    async fn rustfs_ec_hint_is_preserved_on_put_retry() {
        let mock = MockS3::new().await;
        mock.state.fail_next_put.store(true, Ordering::SeqCst);
        let mut backend = mock.backend(Some(262_144));
        backend.config.max_retries = 2;
        backend.put_object("retry", b"retry-payload").await.unwrap();
        let requests = mock.state.requests.lock().await;
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert_eq!(
                request.headers["x-amz-meta-rustfs-ec-block-size-hint"],
                "262144"
            );
            assert_eq!(request.body.as_ref(), b"retry-payload");
        }
    }

    #[tokio::test]
    async fn rustfs_ec_hint_invalid_sdk_config_is_rejected_before_client_creation() {
        for size in [0, 65_535, 65_537, 2_097_152, usize::MAX] {
            let result = S3Backend::with_config(S3Config {
                bucket: "test-bucket".to_string(),
                rustfs_ec_block_size_hint: Some(size),
                ..Default::default()
            })
            .await;
            assert!(result.is_err(), "invalid hint {size} was accepted");
        }
    }

    #[test]
    fn s3_config_defaults_raise_parallelism() {
        let config = S3Config::default();

        assert_eq!(config.max_concurrency, 32);
    }

    #[tokio::test]
    async fn small_vectored_direct_stream_preserves_chunks() {
        let chunks = vec![
            Bytes::from_static(b"small-"),
            Bytes::from_static(b"vectored-"),
            Bytes::from_static(b"put"),
        ];
        let total_size = chunks.iter().map(|chunk| chunk.len()).sum();
        let mut reader =
            S3Backend::direct_stream_from_chunks(&chunks, total_size).into_async_read();
        let mut actual = Vec::new();

        reader.read_to_end(&mut actual).await.unwrap();

        assert_eq!(actual, b"small-vectored-put");
    }

    fn test_backend() -> S3Backend {
        let endpoint = std::env::var("BREWFS_S3_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:9000".to_string());
        let bucket =
            std::env::var("BREWFS_S3_BUCKET").unwrap_or_else(|_| "brewfs-data".to_string());
        let region = std::env::var("BREWFS_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());

        let s3_config = Config::builder()
            .endpoint_url(endpoint)
            .force_path_style(true)
            .region(Region::new(region))
            .timeout_config(
                TimeoutConfig::builder()
                    .connect_timeout(Duration::from_secs(5))
                    .read_timeout(Duration::from_secs(30))
                    .operation_timeout(Duration::from_secs(60))
                    .build(),
            )
            .credentials_provider(Credentials::new(
                std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "rustfsadmin".to_string()),
                std::env::var("AWS_SECRET_ACCESS_KEY")
                    .unwrap_or_else(|_| "rustfsadmin".to_string()),
                None,
                None,
                "rustfs-small-object-streaming-body-compat-test",
            ))
            .build();

        S3Backend {
            client: Client::from_conf(s3_config),
            config: S3Config {
                bucket,
                region: None,
                part_size: 8 * 1024 * 1024,
                max_concurrency: 1,
                max_retries: 1,
                retry_base_delay: 1,
                enable_md5: true,
                endpoint: None,
                force_path_style: true,
                disable_payload_checksum: true,
                rustfs_ec_block_size_hint: None,
            },
        }
    }

    #[derive(Clone, Debug, Default)]
    struct EffectiveBlockCapture(Arc<std::sync::Mutex<Option<String>>>);

    impl aws_sdk_s3::config::Intercept for EffectiveBlockCapture {
        fn name(&self) -> &'static str {
            "EffectiveBlockCapture"
        }

        fn read_before_deserialization(
            &self,
            context: &aws_sdk_s3::config::interceptors::BeforeDeserializationInterceptorContextRef<
                '_,
            >,
            _runtime: &aws_sdk_s3::config::RuntimeComponents,
            _config: &mut aws_sdk_s3::config::ConfigBag,
        ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
            *self.0.lock().unwrap() = context
                .response()
                .headers()
                .get("x-rustfs-effective-ec-block-size")
                .map(str::to_owned);
            Ok(())
        }
    }

    #[tokio::test]
    #[ignore = "requires isolated Dynamic Block RustFS with hint gates enabled and an existing test bucket"]
    async fn rustfs_ec_hint_live_upload_head_and_range() {
        std::env::var("BREWFS_S3_ENDPOINT").expect("set the isolated BREWFS_S3_ENDPOINT");
        std::env::var("BREWFS_S3_BUCKET").expect("set the isolated BREWFS_S3_BUCKET");
        let mut backend = test_backend();
        backend.config.part_size = 5 * 1024 * 1024;
        let payload: Vec<u8> = (0..6 * 1024 * 1024 + 257)
            .map(|index| (index % 251) as u8)
            .collect();
        let prefix = format!("diagnostics/dynamic-block/{}", uuid::Uuid::new_v4());
        eprintln!("Dynamic Block test prefix: {prefix}; failed probes are retained for diagnosis");
        for size in [65_536, 262_144, 1_048_576, 4_194_304] {
            backend.config.rustfs_ec_block_size_hint = Some(size);
            for mode in [
                "simple",
                "vectored",
                "create-only",
                "multipart",
                "multipart-vectored",
            ] {
                let key = format!("{prefix}/{size}/{mode}");
                let chunks = || {
                    vec![
                        Bytes::copy_from_slice(&payload[..23]),
                        Bytes::copy_from_slice(&payload[23..]),
                    ]
                };
                match mode {
                    "simple" => backend.put_object_simple(&key, &payload).await.unwrap(),
                    "vectored" => backend
                        .put_object_vectored_simple(&key, chunks())
                        .await
                        .unwrap(),
                    "create-only" => {
                        backend
                            .put_object_create_only(&key, &payload)
                            .await
                            .unwrap();
                        backend
                            .put_object_create_only(&key, &payload)
                            .await
                            .unwrap();
                    }
                    "multipart" => backend.multipart_upload(&key, &payload).await.unwrap(),
                    "multipart-vectored" => backend
                        .multipart_upload_vectored(&key, chunks())
                        .await
                        .unwrap(),
                    _ => unreachable!(),
                }
                let effective = EffectiveBlockCapture::default();
                backend
                    .client
                    .head_object()
                    .bucket(&backend.config.bucket)
                    .key(&key)
                    .customize()
                    .interceptor(effective.clone())
                    .send()
                    .await
                    .unwrap();
                assert_eq!(
                    *effective.0.lock().unwrap(),
                    Some(size.to_string()),
                    "layout for {key}"
                );
                let full = backend.get_object(&key).await.unwrap();
                assert!(
                    full.as_deref() == Some(payload.as_slice()),
                    "full GET payload mismatch for {key}"
                );
                for offset in [0, size - 31, 5 * 1024 * 1024 - 31, payload.len() - 4096] {
                    let mut actual = vec![0; 4096];
                    let read = backend
                        .get_object_range(&key, offset as u64, &mut actual)
                        .await
                        .unwrap();
                    assert_eq!(read, actual.len(), "short range for {key}");
                    assert_eq!(
                        actual,
                        &payload[offset..offset + read],
                        "range for {key} at {offset}"
                    );
                }
                backend.delete_object(&key).await.unwrap();
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires live S3-compatible endpoint; set BREWFS_S3_ENDPOINT and BREWFS_S3_BUCKET"]
    async fn rustfs_small_object_vectored_put_compat() {
        let backend = test_backend();
        let prefix = format!("diagnostics/rustfs-vectored-put/{}/", std::process::id());
        let simple_key = format!("{prefix}simple");
        let vectored_key = format!("{prefix}vectored");
        let payload = b"small-object-vectored-put-compat-payload";
        let chunks = vec![
            Bytes::copy_from_slice(&payload[..7]),
            Bytes::copy_from_slice(&payload[7..24]),
            Bytes::copy_from_slice(&payload[24..]),
        ];

        backend
            .put_object_simple(&simple_key, payload)
            .await
            .expect("contiguous put_object should succeed before testing streaming body");
        assert_eq!(
            backend.get_object(&simple_key).await.unwrap().as_deref(),
            Some(payload.as_slice())
        );

        timeout(
            Duration::from_secs(10),
            backend.put_object_vectored_simple(&vectored_key, chunks),
        )
        .await
        .expect("vectored put_object timed out; contiguous put_object already succeeded")
        .expect("vectored put_object returned an error");

        assert_eq!(
            backend.get_object(&vectored_key).await.unwrap().as_deref(),
            Some(payload.as_slice())
        );

        let _ = backend.delete_object(&simple_key).await;
        let _ = backend.delete_object(&vectored_key).await;
    }
}
