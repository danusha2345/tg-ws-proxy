//! Короткие HTTP-запросы с сохранением Telegram TCP-сессии в Worker.
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use reqwest::{Client, Method, StatusCode};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use crate::config::tls_config;

const CHUNK_BYTES: usize = 8192;
const MAX_RANGE_BYTES: usize = 14336;
const RANGE_LOOKAHEAD: u64 = 4 * MAX_RANGE_BYTES as u64;
const PLAIN_CACHE: Duration = Duration::from_secs(60);

pub struct HttpRelayConnector {
    client: Client,
    plain_until: Mutex<HashMap<String, Instant>>,
}

struct Reply {
    status: StatusCode,
    protocol: bool,
    seq: Option<u64>,
    ranges: bool,
    offset: Option<u64>,
    end: Option<u64>,
    body: Vec<u8>,
}

impl HttpRelayConnector {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .no_proxy()
            .http1_only()
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .tls_backend_preconfigured((*tls_config()).clone())
            .user_agent("tg-ws-proxy-relay/1")
            .build()?;
        Ok(Self {
            client,
            plain_until: Mutex::new(HashMap::new()),
        })
    }

    pub async fn open(
        &self,
        domain: &str,
        dst: IpAddr,
        init: &[u8; 64],
        force_plain: bool,
        tls_only: bool,
        media: bool,
    ) -> Result<Option<HttpRelayStream>> {
        let mut random = [0; 16];
        getrandom::fill(&mut random)?;
        let sid = hex::encode(random);
        let cached = self
            .plain_until
            .lock()
            .await
            .get(domain)
            .is_some_and(|until| Instant::now() < *until);
        let plain = force_plain || (!tls_only && cached);
        let scheme = if plain { "http" } else { "https" };
        let mut base = format!("{scheme}://{domain}/relay");
        let path = format!("/open?sid={sid}&dst={dst}");
        let attempt = timeout(
            Duration::from_secs(6),
            request(&self.client, Method::POST, &(base.clone() + &path), init, 1),
        )
        .await;
        let result = match attempt {
            Ok(result) => result,
            Err(_) => Err(anyhow!("relay TLS opening timed out")),
        };
        let reply = match result {
            Ok(reply) => reply,
            Err(error) if !plain && !tls_only && permits_plain_retry(&error) => {
                base = format!("http://{domain}/relay");
                let reply =
                    request(&self.client, Method::POST, &(base.clone() + &path), init, 1).await?;
                if reply.protocol && reply.status == StatusCode::OK {
                    self.plain_until
                        .lock()
                        .await
                        .insert(domain.to_owned(), Instant::now() + PLAIN_CACHE);
                    tracing::info!(%domain, "Worker HTTP relay recovered on port 80");
                }
                reply
            }
            Err(error) => return Err(error),
        };
        if !reply.protocol
            && matches!(
                reply.status,
                StatusCode::NOT_FOUND | StatusCode::UPGRADE_REQUIRED
            )
        {
            return Ok(None);
        }
        if !reply.protocol || reply.status != StatusCode::OK {
            bail!("Worker HTTP relay open returned {}", reply.status);
        }
        let (stream, remote) = tokio::io::duplex(64 * 1024);
        let client = self.client.clone();
        let close_url = format!("{base}/close?sid={sid}");
        let ranges = reply.ranges;
        let task = tokio::spawn(async move {
            let result = relay(&client, &base, &sid, remote, ranges, media).await;
            if let Err(error) = result {
                tracing::warn!(%error, "HTTP relay session failed");
            }
            let _ = timeout(
                Duration::from_secs(3),
                request(
                    &client,
                    Method::POST,
                    &format!("{base}/close?sid={sid}"),
                    &[],
                    1,
                ),
            )
            .await;
        });
        Ok(Some(HttpRelayStream {
            stream,
            task,
            close_client: self.client.clone(),
            close_url,
        }))
    }
}

fn permits_plain_retry(error: &anyhow::Error) -> bool {
    // Certificate failures never authorize a plaintext retry.
    if error
        .chain()
        .any(|cause| cause.downcast_ref::<rustls::Error>().is_some())
    {
        return false;
    }
    error.to_string() == "relay TLS opening timed out"
        || error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
                || cause.downcast_ref::<io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::UnexpectedEof
                    )
                })
        })
}

async fn request(
    client: &Client,
    method: Method,
    url: &str,
    data: &[u8],
    tries: u32,
) -> Result<Reply> {
    request_bounded(client, method, url, data, tries, CHUNK_BYTES).await
}

async fn request_bounded(
    client: &Client,
    method: Method,
    url: &str,
    data: &[u8],
    tries: u32,
    body_limit: usize,
) -> Result<Reply> {
    let mut last = None;
    for attempt in 0..tries {
        let result = async {
            let mut response = client
                .request(method.clone(), url)
                .header(reqwest::header::CONNECTION, "close")
                .header(reqwest::header::CACHE_CONTROL, "no-store")
                .body(data.to_vec())
                .send()
                .await
                .map_err(reqwest::Error::without_url)?;
            let status = response.status();
            let protocol = response
                .headers()
                .get("x-tg-relay")
                .is_some_and(|value| value == "1");
            let seq = response
                .headers()
                .get("x-relay-seq")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok());
            let number = |key| {
                response
                    .headers()
                    .get(key)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse().ok())
            };
            let offset = number("x-relay-offset");
            let end = number("x-relay-end");
            let ranges = response
                .headers()
                .get("x-relay-ranges")
                .is_some_and(|value| value == "1");
            let mut body = Vec::new();
            while let Some(chunk) = timeout(Duration::from_secs(3), response.chunk())
                .await?
                .map_err(reqwest::Error::without_url)?
            {
                if body.len() + chunk.len() > body_limit {
                    bail!("HTTP relay response exceeded chunk limit");
                }
                body.extend_from_slice(&chunk);
            }
            Ok::<_, anyhow::Error>(Reply {
                status,
                protocol,
                seq,
                ranges,
                offset,
                end,
                body,
            })
        }
        .await;
        match result {
            Ok(reply)
                if !reply.status.is_server_error()
                    && reply.status != StatusCode::TOO_MANY_REQUESTS =>
            {
                return Ok(reply);
            }
            Ok(reply) => last = Some(anyhow!("HTTP relay returned {}", reply.status)),
            Err(error) => last = Some(error),
        }
        if attempt + 1 < tries {
            sleep(Duration::from_millis(200)).await;
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("HTTP relay request not attempted")))
}

async fn relay(
    client: &Client,
    base: &str,
    sid: &str,
    stream: DuplexStream,
    ranges: bool,
    media: bool,
) -> Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let upload = async {
        let mut seq = 0_u64;
        let mut data = vec![0; CHUNK_BYTES];
        loop {
            let count = reader.read(&mut data).await?;
            if count == 0 {
                return Ok(());
            }
            let reply = request(
                client,
                Method::POST,
                &format!("{base}/up?sid={sid}&seq={seq}"),
                &data[..count],
                3,
            )
            .await?;
            if !reply.protocol || reply.status != StatusCode::OK {
                bail!("relay upload rejected: {}", reply.status);
            }
            seq += 1;
        }
    };
    let download = async {
        if ranges {
            return download_ranges(client, base, sid, &mut writer, media).await;
        }
        let mut seq = 0_u64;
        loop {
            let reply = request(
                client,
                Method::GET,
                &format!("{base}/down?sid={sid}&seq={seq}"),
                &[],
                3,
            )
            .await?;
            if !reply.protocol {
                bail!("relay download protocol marker missing");
            }
            match reply.status {
                StatusCode::NO_CONTENT => {}
                StatusCode::GONE => return Ok(()),
                StatusCode::OK if reply.seq == Some(seq) && !reply.body.is_empty() => {
                    writer.write_all(&reply.body).await?;
                    seq += 1;
                }
                status => bail!("relay download rejected: {status}"),
            }
        }
    };
    tokio::select! { result = upload => result, result = download => result }
}

struct DownloadPolicy {
    chunk: usize,
    ceiling: usize,
    window: usize,
    maximum_window: usize,
    successes: u32,
    idle: u32,
}

impl DownloadPolicy {
    fn new(media: bool) -> Self {
        Self {
            chunk: CHUNK_BYTES,
            ceiling: 12288,
            window: 1,
            maximum_window: if media { 4 } else { 2 },
            successes: 0,
            idle: 0,
        }
    }

    fn received(&mut self, count: usize, requested: usize) {
        self.idle = 0;
        if count == requested {
            self.successes += 1;
            if self.successes >= 2 {
                self.window = self.maximum_window;
            }
            if self.successes >= 16 && self.chunk < self.ceiling {
                self.chunk = self.ceiling.min(if self.chunk < CHUNK_BYTES {
                    CHUNK_BYTES
                } else {
                    12288
                });
                self.successes = 0;
            }
        }
    }

    fn failed(&mut self) {
        self.ceiling = self.ceiling.min(if self.chunk > CHUNK_BYTES {
            CHUNK_BYTES
        } else {
            4096
        });
        self.chunk = self.ceiling;
        self.window = 1;
        self.successes = 0;
    }
}

fn missing_range(
    delivered: u64,
    size: usize,
    end: Option<u64>,
    pending: &BTreeMap<u64, (usize, Instant)>,
    ready: &BTreeMap<u64, Vec<u8>>,
) -> Option<(u64, usize)> {
    let mut covered: Vec<_> = pending
        .iter()
        .map(|(&offset, &(size, _))| (offset, offset + size as u64))
        .chain(
            ready
                .iter()
                .map(|(&offset, data)| (offset, offset + data.len() as u64)),
        )
        .collect();
    covered.sort_unstable();
    let maximum = end
        .unwrap_or(u64::MAX)
        .min(delivered.saturating_add(RANGE_LOOKAHEAD));
    let mut cursor = delivered;
    for (start, stop) in covered {
        if stop <= cursor {
            continue;
        }
        if start > cursor {
            let length = start.min(maximum).saturating_sub(cursor).min(size as u64);
            return (length > 0)
                .then_some((cursor, usize::try_from(length).expect("bounded range")));
        }
        cursor = cursor.max(stop);
    }
    let length = maximum.saturating_sub(cursor).min(size as u64);
    (length > 0).then_some((cursor, usize::try_from(length).expect("bounded range")))
}

#[allow(clippy::too_many_lines)]
async fn download_ranges<W: AsyncWrite + Unpin>(
    client: &Client,
    base: &str,
    sid: &str,
    writer: &mut W,
    media: bool,
) -> Result<()> {
    type Pending<'a> = BoxFuture<'a, (u64, usize, Result<Reply>)>;
    let mut requests: FuturesUnordered<Pending<'_>> = FuturesUnordered::new();
    let mut in_flight = BTreeMap::new();
    let mut ready = BTreeMap::<u64, Vec<u8>>::new();
    let mut failures = HashMap::<u64, u32>::new();
    let mut policy = DownloadPolicy::new(media);
    let mut delivered = 0_u64;
    let mut end = None;
    let mut hol_restarts = 0;
    loop {
        while let Some(data) = ready.remove(&delivered) {
            writer.write_all(&data).await?;
            delivered += data.len() as u64;
            hol_restarts = 0;
        }
        if end.is_some_and(|end| delivered >= end) {
            return Ok(());
        }
        while requests.len() < policy.window {
            let Some((offset, size)) =
                missing_range(delivered, policy.chunk, end, &in_flight, &ready)
            else {
                break;
            };
            in_flight.insert(offset, (size, Instant::now()));
            let url = format!("{base}/range?sid={sid}&offset={offset}&size={size}&ack={delivered}");
            requests.push(
                async move {
                    (
                        offset,
                        size,
                        request_bounded(client, Method::GET, &url, &[], 1, size).await,
                    )
                }
                .boxed(),
            );
        }
        let result = tokio::select! {
            result = requests.next() => result,
            () = sleep(Duration::from_millis(500)), if !ready.is_empty() && hol_restarts < 2 => {
                if in_flight.get(&delivered).is_some_and(|(_, since)| since.elapsed() >= Duration::from_millis(500)) {
                    // Later ranges arrived, so these bytes already exist on the server.
                    // Cancel the stalled request and retry its byte offset over fresh TCP.
                    requests = FuturesUnordered::new();
                    in_flight.clear();
                    hol_restarts += 1;
                    if policy.chunk > CHUNK_BYTES { policy.failed(); }
                }
                continue;
            }
        };
        let Some((offset, size, result)) = result else {
            bail!("relay range scheduler exhausted before EOF");
        };
        in_flight.remove(&offset);
        match result {
            Ok(reply)
                if reply.protocol
                    && reply.status == StatusCode::OK
                    && reply.offset == Some(offset)
                    && !reply.body.is_empty() =>
            {
                policy.received(reply.body.len(), size);
                failures.remove(&offset);
                ready.insert(offset, reply.body);
            }
            Ok(reply) if reply.protocol && reply.status == StatusCode::NO_CONTENT => {
                policy.idle += 1;
                if policy.idle >= 2 {
                    policy.window = 1;
                    requests = FuturesUnordered::new();
                    in_flight.clear();
                }
            }
            Ok(reply) if reply.protocol && reply.status == StatusCode::GONE => {
                let Some(last) = reply.end else {
                    bail!("relay session lost before EOF");
                };
                if last < delivered || end.is_some_and(|known| known != last) {
                    bail!("inconsistent relay EOF");
                }
                end = Some(last);
            }
            result => {
                let tries = failures.entry(offset).or_default();
                *tries += 1;
                if *tries >= 3 {
                    return match result {
                        Err(error) => Err(error),
                        Ok(reply) => Err(anyhow!("relay range rejected: {}", reply.status)),
                    };
                }
                policy.failed();
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

pub struct HttpRelayStream {
    stream: DuplexStream,
    task: JoinHandle<()>,
    close_client: Client,
    close_url: String,
}

impl Drop for HttpRelayStream {
    fn drop(&mut self) {
        self.task.abort();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let client = self.close_client.clone();
            let url = self.close_url.clone();
            runtime.spawn(async move {
                let _ = timeout(
                    Duration::from_secs(3),
                    request(&client, Method::POST, &url, &[], 1),
                )
                .await;
            });
        }
    }
}

impl AsyncRead for HttpRelayStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for HttpRelayStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn adaptive_policy_grows_then_remembers_a_failed_size() {
        let mut policy = DownloadPolicy::new(true);
        for _ in 0..16 {
            policy.received(CHUNK_BYTES, CHUNK_BYTES);
        }
        assert_eq!((policy.chunk, policy.window), (12288, 4));
        policy.failed();
        assert_eq!((policy.chunk, policy.window), (8192, 1));
        for _ in 0..64 {
            policy.received(CHUNK_BYTES, CHUNK_BYTES);
        }
        assert_eq!(policy.chunk, 8192);
        policy.failed();
        assert_eq!(policy.chunk, 4096);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn range_pipeline_preserves_bytes_through_reordering_resizing_and_stalls() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex as StdMutex};
        const TOTAL: usize = 400_000;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/relay", listener.local_addr().unwrap());
        let trace = Arc::new(StdMutex::new(Vec::new()));
        let observed = Arc::clone(&trace);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::clone(&peak);
        let slow = Arc::new(AtomicBool::new(false));
        let reject_large = Arc::new(AtomicBool::new(false));
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = &mut stopped => break,
                };
                let (mut socket, _) = accepted.unwrap();
                let trace = Arc::clone(&observed);
                let active = Arc::clone(&active);
                let peak = Arc::clone(&maximum);
                let slow = Arc::clone(&slow);
                let reject_large = Arc::clone(&reject_large);
                tasks.spawn(async move {
                    struct Active(Arc<AtomicUsize>);
                    impl Drop for Active { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::Relaxed); } }
                    let count = active.fetch_add(1, Ordering::Relaxed) + 1;
                    peak.fetch_max(count, Ordering::Relaxed);
                    let _active = Active(active);
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        if socket.read_exact(&mut byte).await.is_err() { return; }
                        header.push(byte[0]);
                    }
                    let header = String::from_utf8(header).unwrap();
                    let path = header.split_whitespace().nth(1).unwrap();
                    let parsed = url::Url::parse(&format!("http://test{path}")).unwrap();
                    let query: HashMap<_, _> = parsed.query_pairs().collect();
                    let offset: usize = query["offset"].parse().unwrap();
                    let size: usize = query["size"].parse().unwrap();
                    trace.lock().unwrap().push((offset, size));
                    sleep(Duration::from_millis(10)).await;
                    let (status, extra, body) = if offset >= TOTAL {
                        ("410 Gone", format!("X-Relay-End: {TOTAL}\r\n"), Vec::new())
                    } else if size > CHUNK_BYTES && !reject_large.swap(true, Ordering::Relaxed) {
                        ("503 Unavailable", String::new(), Vec::new())
                    } else {
                        if offset == 16384 && !slow.swap(true, Ordering::Relaxed) { sleep(Duration::from_millis(800)).await; }
                        let length = size.min(TOTAL - offset).min(if offset == 49152 { 3000 } else { size });
                        let body = (offset..offset + length).map(|value| u8::try_from(value % 251).unwrap()).collect();
                        ("200 OK", format!("X-Relay-Offset: {offset}\r\n"), body)
                    };
                    let head = format!("HTTP/1.1 {status}\r\nX-Tg-Relay: 1\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        });
        let client = HttpRelayConnector::new().unwrap().client;
        let (mut output, mut input) = tokio::io::duplex(64 * 1024);
        let transfer = tokio::spawn(async move {
            download_ranges(&client, &base, "test-session", &mut output, true).await
        });
        let mut data = Vec::new();
        timeout(Duration::from_secs(10), input.read_to_end(&mut data))
            .await
            .unwrap()
            .unwrap();
        transfer.await.unwrap().unwrap();
        stop.send(()).unwrap();
        server.await.unwrap();
        let expected: Vec<_> = (0..TOTAL)
            .map(|value| u8::try_from(value % 251).unwrap())
            .collect();
        assert_eq!(data, expected);
        let trace = trace.lock().unwrap();
        assert!(trace.iter().filter(|(offset, _)| *offset == 16384).count() >= 2);
        assert!(trace.iter().any(|(_, size)| *size == 12288));
        assert!(peak.load(Ordering::Relaxed) >= 3);
    }

    #[test]
    fn plaintext_recovery_never_accepts_certificate_errors() {
        assert!(permits_plain_retry(&anyhow!(io::Error::from(
            io::ErrorKind::ConnectionReset
        ))));
        assert!(permits_plain_retry(&anyhow!("relay TLS opening timed out")));
        assert!(!permits_plain_retry(&anyhow!(
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)
        )));
        assert!(!permits_plain_retry(&anyhow!(io::Error::from(
            io::ErrorKind::InvalidData
        ))));
    }

    #[tokio::test]
    async fn partial_download_retries_the_same_sequence_without_duplicating_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/relay", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for index in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).await.unwrap();
                    header.push(byte[0]);
                }
                paths.push(String::from_utf8(header).unwrap());
                if index == 2 {
                    socket.write_all(b"HTTP/1.1 410 Gone\r\nX-Tg-Relay: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                } else {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nX-Tg-Relay: 1\r\nX-Relay-Seq: 0\r\nContent-Length: 8192\r\nConnection: close\r\n\r\n").await.unwrap();
                    socket
                        .write_all(&vec![7; if index == 0 { 4 } else { CHUNK_BYTES }])
                        .await
                        .unwrap();
                }
            }
            paths
        });
        let client = HttpRelayConnector::new().unwrap().client;
        let (mut local, remote) = tokio::io::duplex(64 * 1024);
        let relay_task = tokio::spawn(async move {
            relay(&client, &base, "test-session", remote, false, false).await
        });
        let mut data = vec![0; CHUNK_BYTES];
        timeout(Duration::from_secs(5), local.read_exact(&mut data))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(data, vec![7; CHUNK_BYTES]);
        relay_task.await.unwrap().unwrap();
        let paths = server.await.unwrap();
        assert!(paths[0].starts_with("GET /relay/down?sid=test-session&seq=0 "));
        assert!(paths[1].starts_with("GET /relay/down?sid=test-session&seq=0 "));
        assert!(paths[2].starts_with("GET /relay/down?sid=test-session&seq=1 "));
        assert!(
            paths
                .iter()
                .all(|path| path.to_lowercase().contains("connection: close"))
        );
    }
}
