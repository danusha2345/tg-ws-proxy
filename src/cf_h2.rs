//! `MTProto` HTTP API через CF DNS proxy: без Workers и их суточной квоты.
//! Framing and idle polling follow Flowseal/tg-ws-proxy (MIT).
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ctr::cipher::StreamCipher;
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use reqwest::{Client, Version};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::time::{sleep, timeout};

use crate::client::{ClientReader, ClientWriter};
use crate::config::tls_config;
use crate::crypto::{Aes256Ctr, CryptoContext};
use crate::mtproto::Transport;
use crate::stats::Stats;

const MAX_PACKET: usize = 4 * 1024 * 1024;
const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING: usize = 8;
const REPLAY_BYTES: usize = 64 * 1024;
const BODY_IDLE: Duration = Duration::from_secs(8);
const POLL_TIMEOUT: Duration = Duration::from_secs(65);

#[derive(Debug, thiserror::Error)]
#[error("H2 idle poll capacity busy")]
struct PollCapacityBusy;

#[derive(Default)]
struct HostState {
    verified: Option<Instant>,
    failed_until: Option<Instant>,
}

pub struct H2Connector {
    client: Client,
    hosts: Mutex<HashMap<String, Arc<Mutex<HostState>>>>,
    normal_slots: Arc<Semaphore>,
    poll_slots: Arc<Semaphore>,
}

#[derive(Clone)]
pub struct H2Session {
    client: Client,
    url: String,
    state: Arc<Mutex<HostState>>,
    normal_slots: Arc<Semaphore>,
    poll_slots: Arc<Semaphore>,
}

impl H2Connector {
    pub fn new() -> Result<Self> {
        let mut tls = (*tls_config()).clone();
        tls.alpn_protocols = vec![b"h2".to_vec()];
        Ok(Self {
            client: Client::builder()
                .no_proxy()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .http2_prior_knowledge()
                .http2_initial_stream_window_size(256 * 1024)
                .http2_initial_connection_window_size(4 * 1024 * 1024)
                .pool_max_idle_per_host(1)
                .connect_timeout(Duration::from_secs(8))
                // Keep healthy large responses alive; per-I/O idle deadlines bound stalls.
                .tls_backend_preconfigured(tls)
                .user_agent("tg-ws-proxy-h2/1")
                .build()?,
            hosts: Mutex::new(HashMap::new()),
            normal_slots: Arc::new(Semaphore::new(16)),
            poll_slots: Arc::new(Semaphore::new(8)),
        })
    }

    pub async fn connect(&self, host: &str) -> Result<H2Session> {
        let state = self
            .hosts
            .lock()
            .await
            .entry(host.to_owned())
            .or_default()
            .clone();
        let mut guard = state.lock().await;
        ensure!(
            guard
                .failed_until
                .is_none_or(|until| until <= Instant::now()),
            "H2 endpoint cooldown"
        );
        if guard
            .verified
            .is_none_or(|since| since.elapsed() >= Duration::from_secs(60))
        {
            let probe = async {
                let reply = self
                    .client
                    .head(format!("https://{host}/api"))
                    .send()
                    .await
                    .map_err(reqwest::Error::without_url)?;
                ensure!(
                    reply.version() == Version::HTTP_2,
                    "CF API did not negotiate HTTP/2"
                );
                ensure!(
                    reply.status().is_success() || matches!(reply.status().as_u16(), 405 | 501),
                    "CF API preflight HTTP {}",
                    reply.status()
                );
                Ok::<_, anyhow::Error>(())
            };
            match timeout(Duration::from_secs(8), probe).await {
                Ok(Ok(())) => guard.verified = Some(Instant::now()),
                result => {
                    guard.failed_until = Some(Instant::now() + Duration::from_secs(30));
                    return match result {
                        Ok(Err(error)) => Err(error),
                        _ => Err(anyhow::anyhow!("CF API preflight timed out")),
                    };
                }
            }
        }
        drop(guard);
        Ok(H2Session {
            client: self.client.clone(),
            url: format!("https://{host}/api"),
            state,
            normal_slots: Arc::clone(&self.normal_slots),
            poll_slots: Arc::clone(&self.poll_slots),
        })
    }
}

impl H2Session {
    async fn post_once(&self, body: Bytes, poll: bool) -> Result<Bytes> {
        let _permit = if poll {
            Arc::clone(&self.poll_slots)
                .try_acquire_owned()
                .map_err(|_| PollCapacityBusy)?
        } else {
            Arc::clone(&self.normal_slots).acquire_owned().await?
        };
        let header_wait = if poll || body.len() > REPLAY_BYTES {
            POLL_TIMEOUT
        } else {
            BODY_IDLE
        };
        let mut response = timeout(
            header_wait,
            self.client
                .post(&self.url)
                .header("content-type", "application/octet-stream")
                .header("accept-encoding", "identity")
                .body(body)
                .send(),
        )
        .await
        .context("H2 response headers timed out")?
        .map_err(reqwest::Error::without_url)?;
        ensure!(
            response.version() == Version::HTTP_2,
            "CF API downgraded HTTP version"
        );
        if matches!(response.status().as_u16(), 403 | 404 | 429 | 444) {
            self.failed().await;
            return Ok(Bytes::copy_from_slice(
                &(-i32::from(response.status().as_u16())).to_le_bytes(),
            ));
        }
        ensure!(
            response.status().is_success(),
            "CF API HTTP {}",
            response.status()
        );
        ensure!(
            !response
                .headers()
                .get("content-type")
                .and_then(|x| x.to_str().ok())
                .is_some_and(|x| x.contains("text/html")),
            "CF API returned HTML"
        );
        let declared = response.content_length();
        ensure!(
            declared.is_none_or(|n| n <= MAX_PACKET as u64),
            "CF API response too large"
        );
        let mut data = Vec::new();
        while let Some(chunk) = timeout(BODY_IDLE, response.chunk())
            .await
            .context("H2 response body stalled")?
            .map_err(reqwest::Error::without_url)?
        {
            ensure!(
                data.len() + chunk.len() <= MAX_PACKET,
                "CF API response exceeded packet limit"
            );
            data.extend_from_slice(&chunk);
        }
        ensure!(
            declared.is_none_or(|n| n == data.len() as u64),
            "CF API truncated response"
        );
        ensure!(data.len() % 4 == 0, "CF API unaligned response");
        Ok(Bytes::from(data))
    }

    async fn post(&self, body: Bytes, poll: bool) -> Result<Bytes> {
        let replayable =
            body.len() <= REPLAY_BYTES && body.get(..8).is_some_and(|key| key != [0; 8]);
        let attempts = if replayable && !poll { 3 } else { 1 };
        for attempt in 0..attempts {
            let result = self.post_once(body.clone(), poll).await;
            if result.is_ok() || attempt + 1 == attempts {
                return result;
            }
            // Retry the same encrypted MTProto packet; do not reset other H2 streams.
            sleep(Duration::from_millis(250 * (attempt + 1))).await;
        }
        unreachable!("at least one attempt")
    }

    async fn failed(&self) {
        let mut state = self.state.lock().await;
        state.verified = None;
        state.failed_until = Some(Instant::now() + Duration::from_secs(30));
    }
}

struct Packet {
    body: Bytes,
    wire_len: usize,
}

async fn decrypt_read(
    reader: &mut ClientReader,
    cipher: &mut Aes256Ctr,
    size: usize,
) -> Result<Vec<u8>> {
    let mut data = vec![0; size];
    reader.read_exact(&mut data).await?;
    cipher.apply_keystream(&mut data);
    Ok(data)
}

async fn read_packet(
    reader: &mut ClientReader,
    cipher: &mut Aes256Ctr,
    transport: Transport,
) -> Result<Option<Packet>> {
    let mut first = [0];
    if reader.read(&mut first).await? == 0 {
        return Ok(None);
    }
    cipher.apply_keystream(&mut first);
    let (length, header) = match transport {
        Transport::Abridged => {
            if first[0] & 0x7f == 0x7f {
                let tail = decrypt_read(reader, cipher, 3).await?;
                (
                    u32::from_le_bytes([tail[0], tail[1], tail[2], 0]) as usize * 4,
                    4,
                )
            } else {
                (usize::from(first[0] & 0x7f) * 4, 1)
            }
        }
        Transport::Intermediate | Transport::PaddedIntermediate => {
            let tail = decrypt_read(reader, cipher, 3).await?;
            (
                (u32::from_le_bytes([first[0], tail[0], tail[1], tail[2]]) & 0x7fff_ffff) as usize,
                4,
            )
        }
    };
    ensure!(
        (24..=MAX_PACKET + 256).contains(&length),
        "native packet size outside limit"
    );
    let data = decrypt_read(reader, cipher, length).await?;
    let body = strip_padding(data, transport)?;
    Ok(Some(Packet {
        body: Bytes::from(body),
        wire_len: header + length,
    }))
}

fn strip_padding(mut data: Vec<u8>, transport: Transport) -> Result<Vec<u8>> {
    ensure!(data.len() >= 24, "short native MTProto packet");
    if transport == Transport::PaddedIntermediate {
        let length = if data[..8] == [0; 8] {
            ensure!(data.len() >= 20, "short unencrypted MTProto envelope");
            20_usize
                .checked_add(u32::from_le_bytes(data[16..20].try_into()?) as usize)
                .context("packet length overflow")?
        } else {
            24 + ((data.len() - 24) / 16) * 16
        };
        let maximum = if data[..8] == [0; 8] { 256 } else { 15 };
        ensure!(
            length <= data.len() && data.len() - length <= maximum,
            "invalid native padding"
        );
        data.truncate(length);
    }
    ensure!(
        (24..=MAX_PACKET).contains(&data.len()) && data.len() % 4 == 0,
        "invalid native packet alignment"
    );
    Ok(data)
}

fn encode_reply(body: &[u8], transport: Transport) -> Result<Vec<u8>> {
    ensure!(
        !body.is_empty() && body.len() <= MAX_PACKET && body.len() % 4 == 0,
        "invalid API reply size"
    );
    let mut result = Vec::with_capacity(body.len() + 8);
    if transport == Transport::Abridged {
        let words = u32::try_from(body.len() / 4)?;
        if words < 0x7f {
            result.push(u8::try_from(words)?);
        } else {
            result.push(0x7f);
            result.extend_from_slice(&words.to_le_bytes()[..3]);
        }
        result.extend_from_slice(body);
    } else {
        let mut padding = [0_u8; 3];
        getrandom::fill(&mut padding)?;
        let size = if transport == Transport::PaddedIntermediate {
            usize::from(padding[0] & 3)
        } else {
            0
        };
        result.extend_from_slice(&u32::try_from(body.len() + size)?.to_le_bytes());
        result.extend_from_slice(body);
        result.extend_from_slice(&padding[..size]);
    }
    Ok(result)
}

/// H2 responses carry complete `MTProto` packets, not an ordered raw TCP byte stream.
/// Cancelling the reader only when the whole bridge ends preserves framing/CTR state.
#[allow(clippy::too_many_lines)]
pub async fn bridge(
    mut reader: ClientReader,
    mut writer: ClientWriter,
    crypto: CryptoContext,
    transport: Transport,
    session: H2Session,
    stats: Arc<Stats>,
) -> Result<()> {
    let mut decrypt = crypto.upstream.client_decrypt;
    let mut encrypt = crypto.downstream.client_encrypt;
    let (sender, mut receiver) = mpsc::channel::<Packet>(2);
    let upload = async move {
        while let Some(packet) = read_packet(&mut reader, &mut decrypt, transport).await? {
            if sender.send(packet).await.is_err() {
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let forward = async {
        type Pending = BoxFuture<'static, (bool, usize, Result<Bytes>)>;
        let mut requests = FuturesUnordered::<Pending>::new();
        let mut in_flight = 0_usize;
        let mut queued = 0_usize;
        let mut last = None::<(Bytes, Instant)>;
        let mut last_progress = Instant::now();
        let mut polls = 0_u8;
        let mut polling = false;
        let mut recent = VecDeque::<[u8; 32]>::new();
        loop {
            tokio::select! {
                packet = receiver.recv(), if in_flight < MAX_PENDING && queued <= MAX_PENDING_BYTES - MAX_PACKET => {
                    let Some(packet) = packet else { return Ok::<_, anyhow::Error>(()); };
                    Stats::add(&stats.bytes_up, packet.wire_len);
                    let body = packet.body;
                    if body.len() <= REPLAY_BYTES && body[..8] != [0; 8] {
                        last = Some((body.clone(), Instant::now()));
                        polls = 0;
                    }
                    last_progress = Instant::now();
                    let size = body.len();
                    queued += size;
                    in_flight += 1;
                    let client = session.clone();
                    requests.push(async move { (false, size, client.post(body, false).await) }.boxed());
                }
                result = requests.next(), if !requests.is_empty() => {
                    let Some((is_poll, size, result)) = result else { unreachable!() };
                    if is_poll {
                        polling = false;
                    } else {
                        in_flight -= 1;
                        queued -= size;
                    }
                    let body = match result {
                        Ok(data) => data,
                        Err(error) if is_poll => {
                            if error.is::<PollCapacityBusy>() { polls = polls.saturating_sub(1); }
                            tracing::debug!(%error, "H2 idle poll failed");
                            continue;
                        }
                        Err(error) => {
                            session.failed().await;
                            return Err(error);
                        }
                    };
                    last_progress = Instant::now();
                    if body.is_empty() { continue; }
                    let hash: [u8; 32] = Sha256::digest(&body).into();
                    if recent.contains(&hash) { continue; }
                    recent.push_back(hash);
                    if recent.len() > 64 { recent.pop_front(); }
                    polls = 0;
                    let terminal = body.len() == 4 && i32::from_le_bytes(body[..4].try_into()?) < 0;
                    let mut frame = encode_reply(&body, transport)?;
                    encrypt.apply_keystream(&mut frame);
                    writer.write_all(&frame).await?;
                    Stats::add(&stats.bytes_down, body.len());
                    if terminal { return Ok(()); }
                }
                () = sleep(Duration::from_secs(1)) => {
                    // Native TCP clients do not send http_wait. Reuse a recent encrypted
                    // packet for a bounded idle poll, as in upstream; never replay auth setup.
                    if in_flight == 0 && !polling && polls < 3
                        && last_progress.elapsed() >= Duration::from_secs(1)
                        && let Some((body, since)) = &last
                        && since.elapsed() < Duration::from_secs(30)
                        && session.poll_slots.available_permits() > 0 {
                        let body = body.clone();
                        let client = session.clone();
                        polling = true;
                        polls += 1;
                        requests.push(async move { (true, 0, client.post(body, true).await) }.boxed());
                    }
                }
            }
        }
    };
    let result = tokio::select! {result=upload=>result,result=forward=>result};
    let _ = writer.shutdown().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::new_aes_ctr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Notify;

    #[tokio::test]
    async fn fragmented_obfuscated_packets_preserve_ctr_and_remove_only_transport_padding() {
        for transport in [
            Transport::Abridged,
            Transport::Intermediate,
            Transport::PaddedIntermediate,
        ] {
            for size in [40, 520] {
                let body = vec![1; size];
                let mut frame = encode_reply(&body, transport).unwrap();
                new_aes_ctr(&[7; 32], &[8; 16]).apply_keystream(&mut frame);
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let stream = TcpStream::connect(listener.local_addr().unwrap())
                    .await
                    .unwrap();
                let (accepted, _) = listener.accept().await.unwrap();
                let (_, mut writer) = stream.into_split();
                let sending = tokio::spawn(async move {
                    for chunk in frame.chunks(7) {
                        writer.write_all(chunk).await.unwrap();
                    }
                });
                let mut reader = ClientReader::Plain(accepted.into_split().0);
                let mut cipher = new_aes_ctr(&[7; 32], &[8; 16]);
                let packet = read_packet(&mut reader, &mut cipher, transport)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(packet.body, body);
                assert!(
                    read_packet(&mut reader, &mut cipher, transport)
                        .await
                        .unwrap()
                        .is_none()
                );
                sending.await.unwrap();
            }
        }
    }

    #[test]
    fn rejects_bad_framing_and_preserves_unencrypted_envelope_length() {
        assert!(strip_padding(vec![0; 8], Transport::PaddedIntermediate).is_err());
        assert!(encode_reply(&[0; 3], Transport::Intermediate).is_err());
        let mut body = vec![0; 24];
        body[16..20].copy_from_slice(&4_u32.to_le_bytes());
        let mut padded = body.clone();
        padded.extend_from_slice(&[9; 100]);
        assert_eq!(
            strip_padding(padded, Transport::PaddedIntermediate).unwrap(),
            body
        );
        let mut invalid = vec![0; 24];
        invalid[16..20].copy_from_slice(&4096_u32.to_le_bytes());
        assert!(strip_padding(invalid, Transport::PaddedIntermediate).is_err());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn real_h2_streams_retry_one_packet_without_restarting_a_slow_neighbor() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api", listener.local_addr().unwrap());
        let slow_release = Arc::new(Notify::new());
        let slow_seen = Arc::new(Notify::new());
        let requests_seen = Arc::new(AtomicUsize::new(0));
        let retry_counter = Arc::new(AtomicUsize::new(0));
        let server_release = Arc::clone(&slow_release);
        let server_seen = Arc::clone(&slow_seen);
        let count = Arc::clone(&requests_seen);
        let retries = Arc::clone(&retry_counter);
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut connection = h2::server::handshake(socket).await.unwrap();
            let mut children = tokio::task::JoinSet::new();
            while let Some(request) = connection.accept().await {
                let (request, mut response) = request.unwrap();
                let release = Arc::clone(&server_release);
                let seen = Arc::clone(&server_seen);
                let count = Arc::clone(&count);
                let retries = Arc::clone(&retries);
                children.spawn(async move {
                    if request.method() == http::Method::HEAD {
                        response
                            .send_response(
                                http::Response::builder().status(501).body(()).unwrap(),
                                true,
                            )
                            .unwrap();
                        return;
                    }
                    let mut receive = request.into_body();
                    let mut body = Vec::new();
                    while let Some(data) = receive.data().await {
                        let data = data.unwrap();
                        receive.flow_control().release_capacity(data.len()).unwrap();
                        body.extend_from_slice(&data);
                    }
                    count.fetch_add(1, Ordering::Relaxed);
                    if body[0] == 1 {
                        seen.notify_one();
                        release.notified().await;
                    }
                    let mut status = 200;
                    if body[0] == 2 && retries.fetch_add(1, Ordering::Relaxed) == 0 {
                        status = 503;
                    }
                    let reply = if body[0] == 3 {
                        vec![9; 512 * 1024]
                    } else {
                        vec![body[0]; 40]
                    };
                    let declared = if body[0] == 4 {
                        reply.len() + 4
                    } else {
                        reply.len()
                    };
                    let head = http::Response::builder()
                        .status(status)
                        .header("content-length", declared.to_string())
                        .body(())
                        .unwrap();
                    let mut stream = response.send_response(head, false).unwrap();
                    let _ = stream.send_data(Bytes::from(reply), true);
                });
            }
            children.abort_all();
        });
        let client = Client::builder()
            .no_proxy()
            .tls_backend_preconfigured((*tls_config()).clone())
            .http2_prior_knowledge()
            .build()
            .unwrap();
        assert_eq!(
            client.head(&url).send().await.unwrap().version(),
            Version::HTTP_2
        );
        let session = H2Session {
            client,
            url,
            state: Arc::default(),
            normal_slots: Arc::new(Semaphore::new(16)),
            poll_slots: Arc::new(Semaphore::new(4)),
        };
        let slow_session = session.clone();
        let slow =
            tokio::spawn(async move { slow_session.post(Bytes::from(vec![1; 40]), false).await });
        slow_seen.notified().await;
        let result = timeout(
            Duration::from_secs(3),
            session.post(Bytes::from(vec![2; 40]), false),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, vec![2; 40]);
        assert_eq!(retry_counter.load(Ordering::Relaxed), 2);
        assert!(
            !slow.is_finished(),
            "a healthy slow neighbor must stay alive during retries"
        );
        slow_release.notify_one();
        assert_eq!(slow.await.unwrap().unwrap(), vec![1; 40]);
        assert_eq!(
            session.post(Bytes::from(vec![3; 40]), false).await.unwrap(),
            vec![9; 512 * 1024]
        );
        assert!(
            session
                .post_once(Bytes::from(vec![4; 40]), false)
                .await
                .is_err(),
            "a truncated HTTP body must never be delivered"
        );
        assert_eq!(requests_seen.load(Ordering::Relaxed), 5);
        server.abort();
    }
}
