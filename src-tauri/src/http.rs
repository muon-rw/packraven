use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::multipart::{Form, Part};
use reqwest::{RequestBuilder, Response};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::jobs::Log;

pub const USER_AGENT: &str = concat!("muon-rw/packraven/", env!("CARGO_PKG_VERSION"));

// We send keepalive probes so we notice a connection that died while we wait on a slow answer; otherwise we'd never get an error.
pub static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(30))
        .build()
        .expect("http client")
});

// reqwest never fails a stalled upload on its own, so we give up when we've sent no data for this long.
const UPLOAD_STALL: Duration = Duration::from_secs(120);
// After the last byte, the platform still checks the file before it answers, which takes a while for a large pack.
const UPLOAD_ANSWER_WAIT: Duration = Duration::from_secs(600);
const UPLOAD_CHUNK: usize = 256 * 1024;
const LOGS_PROGRESS_FROM: u64 = 64 * 1024 * 1024;

/// The platform received the whole file but never confirmed it, so it may have kept the file.
#[derive(Debug)]
pub struct UploadUnconfirmed {
    pub service: &'static str,
    pub reason: String,
}

impl std::fmt::Display for UploadUnconfirmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} received the whole file, but {}, so it may have kept the file anyway", self.service, self.reason)
    }
}

impl std::error::Error for UploadUnconfirmed {}

pub struct UploadFile<'a> {
    pub field: &'static str,
    pub path: &'a Path,
    pub file_name: String,
    pub mime: &'static str,
}

struct UploadProgress {
    size: u64,
    sent: AtomicU64,
    started: Instant,
    last_sent_ms: AtomicU64,
}

impl UploadProgress {
    fn record(&self, bytes: u64) -> u64 {
        self.last_sent_ms.store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.sent.fetch_add(bytes, Ordering::Relaxed) + bytes
    }

    fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    fn complete(&self) -> bool {
        self.sent() >= self.size
    }

    fn idle(&self) -> Duration {
        self.started.elapsed().saturating_sub(Duration::from_millis(self.last_sent_ms.load(Ordering::Relaxed)))
    }
}

pub fn size_text(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{:.0} KB", bytes as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.0} MB", bytes as f64 / 1e6),
        _ => format!("{:.2} GB", bytes as f64 / 1e9),
    }
}

// We read the file from disk as the upload goes, so a pack near 1 GB never sits in memory whole.
fn streamed_part(file: tokio::fs::File, upload: UploadFile<'_>, progress: Arc<UploadProgress>, service: &'static str, log: &Log) -> Result<Part> {
    let size = progress.size;
    let log = log.clone();
    let chunks = futures_util::stream::try_unfold((file, 0u64), move |(mut file, mut tenths)| {
        let progress = progress.clone();
        let log = log.clone();
        async move {
            let mut chunk = vec![0; UPLOAD_CHUNK];
            let read = file.read(&mut chunk).await?;
            if read == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            chunk.truncate(read);
            let sent = progress.record(read as u64);
            if size >= LOGS_PROGRESS_FROM && sent * 10 / size > tenths {
                tenths = sent * 10 / size;
                match sent >= size {
                    true => log.info(format!("Sent all {} to {service}, waiting for it to confirm", size_text(size))),
                    false => log.info(format!("Uploaded {}% ({} of {})", tenths * 10, size_text(sent), size_text(size))),
                }
            }
            Ok(Some((chunk, (file, tenths))))
        }
    });
    Ok(Part::stream_with_length(reqwest::Body::wrap_stream(chunks), size).file_name(upload.file_name).mime_str(upload.mime)?)
}

/// Sends a multipart upload with the file streamed from disk. Fails with [`UploadUnconfirmed`] when the
/// platform got the whole file but we never heard back, since only then could it have kept the file.
pub async fn send_upload(request: RequestBuilder, form: Form, upload: UploadFile<'_>, service: &'static str, log: &Log) -> Result<Response> {
    let file = tokio::fs::File::open(upload.path).await.with_context(|| format!("reading {}", upload.path.display()))?;
    let size = file.metadata().await?.len();
    let progress = Arc::new(UploadProgress { size, sent: AtomicU64::new(0), started: Instant::now(), last_sent_ms: AtomicU64::new(0) });
    let field = upload.field;
    let part = streamed_part(file, upload, progress.clone(), service, log)?;
    let send = request.multipart(form.part(field, part)).send();
    tokio::pin!(send);
    let mut check = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            response = &mut send => {
                return match response {
                    Ok(response) => Ok(response),
                    Err(error) if progress.complete() => Err(UploadUnconfirmed { service, reason: format!("the connection dropped before it answered ({error})") }.into()),
                    Err(error) => Err(anyhow!(error).context(format!("The upload to {service} failed after {} of {}", size_text(progress.sent()), size_text(size)))),
                };
            }
            _ = check.tick() => {
                let idle = progress.idle();
                if !progress.complete() && idle > UPLOAD_STALL {
                    bail!("The upload to {service} stalled after {} of {}, with nothing sent for {} minutes. Check your connection, then try again.", size_text(progress.sent()), size_text(size), UPLOAD_STALL.as_secs() / 60);
                }
                if progress.complete() && idle > UPLOAD_ANSWER_WAIT {
                    return Err(UploadUnconfirmed { service, reason: format!("it didn't answer within {} minutes", UPLOAD_ANSWER_WAIT.as_secs() / 60) }.into());
                }
            }
        }
    }
}

pub async fn download(url: &str, dest: &Path, sha1: Option<&str>) -> Result<()> {
    let mut request = CLIENT.get(url);
    if let Some(key) = curseforge_cdn_key(url) {
        request = request.header("x-api-key", key);
    }
    let response = request.send().await?.error_for_status().with_context(|| format!("downloading {url}"))?;
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&temp).await?;
    let mut hasher = Sha1::new();
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    if let Some(expected) = sha1 {
        let actual = hex::encode(hasher.finalize());
        if actual != expected {
            tokio::fs::remove_file(&temp).await?;
            bail!("hash mismatch for {url}: expected {expected}, got {actual}");
        }
    }
    tokio::fs::rename(&temp, dest).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{send_upload, UploadFile, UploadUnconfirmed, CLIENT};
    use crate::jobs::Log;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Server {
        HangUpAfter(usize),
        HangUpAtEnd,
        Answer,
    }

    async fn upload_to(size: usize, server: Server) -> anyhow::Result<reqwest::StatusCode> {
        let path = std::env::temp_dir().join(format!("packraven-upload-{}-{size}-{server:?}.zip", std::process::id()));
        std::fs::write(&path, vec![7u8; size]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0; 64 * 1024];
            let mut request = Vec::new();
            // A multipart body ends with its closing boundary, "--" and a line break.
            let done = |request: &[u8]| match server {
                Server::HangUpAfter(limit) => request.len() >= limit,
                Server::HangUpAtEnd | Server::Answer => request.ends_with(b"--\r\n"),
            };
            while !done(&request) {
                match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => request.extend_from_slice(&buffer[..n]),
                }
            }
            if server == Server::Answer {
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}").await;
            }
        });
        let request = CLIENT.post(format!("http://{address}/upload"));
        let form = reqwest::multipart::Form::new().text("metadata", "{}");
        let file = UploadFile { field: "file", path: &path, file_name: "pack.zip".into(), mime: "application/zip" };
        let result = send_upload(request, form, file, "CurseForge", &Log::console("test")).await.map(|r| r.status());
        std::fs::remove_file(&path).unwrap();
        result
    }

    #[tokio::test]
    async fn a_lost_answer_after_the_whole_file_may_have_kept_it() {
        let error = upload_to(1024 * 1024, Server::HangUpAtEnd).await.unwrap_err();
        assert!(error.is::<UploadUnconfirmed>(), "{error:#}");
    }

    #[tokio::test]
    async fn a_connection_lost_partway_through_left_nothing_behind() {
        let error = upload_to(32 * 1024 * 1024, Server::HangUpAfter(1024 * 1024)).await.unwrap_err();
        assert!(!error.is::<UploadUnconfirmed>(), "{error:#}");
        assert!(format!("{error:#}").contains("The upload to CurseForge failed after"), "{error:#}");
    }

    #[tokio::test]
    async fn streams_the_file_and_returns_the_answer() {
        assert_eq!(upload_to(3 * 1024 * 1024, Server::Answer).await.unwrap(), reqwest::StatusCode::OK);
    }
}

// CurseForge rejects keyless downloads from edge.forgecdn.net with a 401. Its other CDN hosts
// don't need the key, so we keep it off them.
fn curseforge_cdn_key(url: &str) -> Option<&'static str> {
    if reqwest::Url::parse(url).ok()?.host_str()? != "edge.forgecdn.net" {
        return None;
    }
    crate::curseforge::core_key()
}
