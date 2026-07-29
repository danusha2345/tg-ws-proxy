use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use ctr::cipher::StreamCipher;
use rustls::ClientConfig;
use socket2::SockRef;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::{debug, info, warn};
use url::form_urlencoded;

use crate::client::{ClientReader, ClientWriter};
use crate::config::{ProxyConfig, tls_config};
use crate::fake_tls::{
    FakeTlsReader, FakeTlsWriter, TLS_RECORD_HANDSHAKE, build_server_hello, verify_client_hello,
};
use crate::mtproto::{
    ClientInit, HANDSHAKE_LEN, MessageSplitter, build_crypto_context, generate_relay_init,
    parse_client_init,
};
use crate::pool::{WsPool, websocket_domains};
use crate::stats::{Stats, StatsSnapshot};
use crate::websocket::{RawWebSocket, WebSocketError};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DC_FAILURE_COOLDOWN: Duration = Duration::from_secs(60);
const IP_FAILURE_COOLDOWN: Duration = Duration::from_secs(3600);
const REDIRECT_COOLDOWN: Duration = Duration::from_secs(600);
const FAKE_TLS_REPLAY_WINDOW: Duration = Duration::from_secs(240);
const MAX_FAKE_TLS_REPLAY_ENTRIES: usize = 65_536;
const PROXY_V1_MAX_LENGTH: usize = 108;
const MAX_FAKE_TLS_RECORD: usize = 18_432;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct DcKey {
    dc: i16,
    test: bool,
    media: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectOutcome {
    Redirect,
    Timeout,
    Error,
}

struct PreparedClient {
    handshake: [u8; HANDSHAKE_LEN],
    reader: ClientReader,
    writer: ClientWriter,
    label: String,
}

struct ProxyInner {
    config: Arc<ProxyConfig>,
    tls_config: Arc<ClientConfig>,
    stats: Arc<Stats>,
    pool: WsPool,
    connection_limit: Arc<Semaphore>,
    redirect_until: Mutex<HashMap<DcKey, Instant>>,
    dc_fail_until: Mutex<HashMap<DcKey, Instant>>,
    ip_fail_until: Mutex<HashMap<IpAddr, Instant>>,
    active_cf_domain: Mutex<HashMap<i16, String>>,
    fake_tls_replays: Mutex<HashMap<[u8; 32], Instant>>,
}

#[derive(Clone)]
pub struct Proxy {
    inner: Arc<ProxyInner>,
}

impl Proxy {
    pub fn new(config: ProxyConfig) -> Result<Self> {
        config.validate()?;
        let config = Arc::new(config);
        let tls_config = tls_config();
        let stats = Arc::new(Stats::default());
        let pool = WsPool::new(
            Arc::clone(&config),
            Arc::clone(&tls_config),
            Arc::clone(&stats),
        );
        Ok(Self {
            inner: Arc::new(ProxyInner {
                connection_limit: Arc::new(Semaphore::new(config.max_connections)),
                config,
                tls_config,
                stats,
                pool,
                redirect_until: Mutex::new(HashMap::new()),
                dc_fail_until: Mutex::new(HashMap::new()),
                ip_fail_until: Mutex::new(HashMap::new()),
                active_cf_domain: Mutex::new(HashMap::new()),
                fake_tls_replays: Mutex::new(HashMap::new()),
            }),
        })
    }

    #[must_use]
    pub fn config(&self) -> &ProxyConfig {
        &self.inner.config
    }

    #[must_use]
    pub fn stats(&self) -> StatsSnapshot {
        self.inner.stats.snapshot()
    }

    pub async fn run_until<F>(&self, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        self.run_until_ready(shutdown, || {}).await
    }

    pub async fn run_until_ready<F, R>(&self, shutdown: F, on_ready: R) -> Result<()>
    where
        F: Future<Output = ()> + Send,
        R: FnOnce() + Send,
    {
        let listener = TcpListener::bind((self.inner.config.host.as_str(), self.inner.config.port))
            .await
            .with_context(|| {
                format!(
                    "failed to listen on {}:{}",
                    self.inner.config.host, self.inner.config.port
                )
            })?;
        info!(
            host = %self.inner.config.host,
            port = self.inner.config.port,
            "Telegram MTProto WebSocket bridge is listening"
        );
        on_ready();

        self.inner.pool.warm_up().await;
        let maintenance = self.inner.pool.start_maintenance();
        let mut clients = JoinSet::new();
        let mut stats_interval = tokio::time::interval(Duration::from_secs(60));
        stats_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                () = &mut shutdown => break,
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, peer)) => {
                            let Ok(permit) = Arc::clone(&self.inner.connection_limit)
                                .try_acquire_owned()
                            else {
                                warn!(%peer, "connection limit reached; rejecting client");
                                drop(stream);
                                continue;
                            };
                            let proxy = self.clone();
                            clients.spawn(async move {
                                let _permit = permit;
                                if let Err(error) = Box::pin(proxy.handle_client(stream, peer)).await {
                                    debug!(%peer, error = %error, "client session failed");
                                }
                            });
                        }
                        Err(error) => {
                            warn!(%error, "listener accept failed");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
                result = clients.join_next(), if !clients.is_empty() => {
                    if let Some(Err(error)) = result {
                        warn!(%error, "client task panicked");
                    }
                }
                _ = stats_interval.tick() => {
                    let stats = self.stats();
                    info!(
                        total = stats.total,
                        active = stats.active,
                        websocket = stats.websocket,
                        tcp_fallback = stats.tcp_fallback,
                        cloudflare = stats.cloudflare,
                        bad = stats.bad,
                        bytes_up = stats.bytes_up,
                        bytes_down = stats.bytes_down,
                        "proxy statistics"
                    );
                }
            }
        }

        clients.shutdown().await;
        self.inner.pool.shutdown().await;
        let _ = maintenance.await;
        info!("proxy stopped");
        Ok(())
    }

    async fn handle_client(&self, stream: TcpStream, peer: SocketAddr) -> Result<()> {
        Stats::increment(&self.inner.stats.connections_total);
        Stats::increment(&self.inner.stats.connections_active);
        let _active = ActiveConnection(Arc::clone(&self.inner.stats));

        stream.set_nodelay(true)?;
        let socket = SockRef::from(&stream);
        let _ = socket.set_recv_buffer_size(self.inner.config.buffer_size);
        let _ = socket.set_send_buffer_size(self.inner.config.buffer_size);

        let Some(prepared) = self.prepare_client(stream, peer).await? else {
            return Ok(());
        };
        let Some(client_init) = parse_client_init(&prepared.handshake, &self.inner.config.secret)
        else {
            Stats::increment(&self.inner.stats.connections_bad);
            // The Python implementation drained forever here. Closing immediately keeps
            // unauthenticated clients from occupying an unbounded task and descriptor.
            debug!(label = %prepared.label, "invalid MTProto init");
            return Ok(());
        };
        Box::pin(self.route_client(prepared, client_init)).await
    }

    async fn prepare_client(
        &self,
        mut stream: TcpStream,
        peer: SocketAddr,
    ) -> Result<Option<PreparedClient>> {
        let mut label = peer.to_string();
        if self.inner.config.proxy_protocol {
            label = timeout(HANDSHAKE_TIMEOUT, read_proxy_v1_header(&mut stream))
                .await
                .map_err(|_| anyhow!("PROXY v1 header timed out"))??;
        }

        let mut first = [0_u8; 1];
        timeout(HANDSHAKE_TIMEOUT, stream.read_exact(&mut first))
            .await
            .map_err(|_| anyhow!("client init timed out"))??;

        if let Some(fake_domain) = &self.inner.config.fake_tls_domain {
            if first[0] != TLS_RECORD_HANDSHAKE {
                let response = format!(
                    "HTTP/1.1 301 Moved Permanently\r\n\
                     Location: https://{fake_domain}/\r\n\
                     Content-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                );
                stream.write_all(response.as_bytes()).await?;
                stream.shutdown().await?;
                return Ok(None);
            }

            let mut header_rest = [0_u8; 4];
            timeout(HANDSHAKE_TIMEOUT, stream.read_exact(&mut header_rest))
                .await
                .map_err(|_| anyhow!("Fake TLS header timed out"))??;
            let record_length = usize::from(u16::from_be_bytes([header_rest[2], header_rest[3]]));
            if record_length > MAX_FAKE_TLS_RECORD {
                bail!("Fake TLS ClientHello exceeds {MAX_FAKE_TLS_RECORD} bytes");
            }
            let mut hello = Vec::with_capacity(record_length + 5);
            hello.push(first[0]);
            hello.extend_from_slice(&header_rest);
            hello.resize(record_length + 5, 0);
            timeout(HANDSHAKE_TIMEOUT, stream.read_exact(&mut hello[5..]))
                .await
                .map_err(|_| anyhow!("Fake TLS ClientHello timed out"))??;

            let Some(verified) =
                verify_client_hello(&hello, &self.inner.config.secret, SystemTime::now())
            else {
                self.forward_masking_probe(stream, &hello, &label).await;
                return Ok(None);
            };
            if !self.register_fake_tls_hello(verified.client_random).await {
                Stats::increment(&self.inner.stats.connections_bad);
                warn!(%label, "replayed Fake TLS ClientHello rejected");
                return Ok(None);
            }
            let response = build_server_hello(
                &self.inner.config.secret,
                &verified.client_random,
                &verified.session_id,
            )?;
            stream.write_all(&response).await?;
            stream.flush().await?;

            let (reader, writer) = stream.into_split();
            let mut reader = ClientReader::FakeTls(FakeTlsReader::new(reader));
            let writer = ClientWriter::FakeTls(FakeTlsWriter::new(writer));
            let mut handshake = [0_u8; HANDSHAKE_LEN];
            timeout(HANDSHAKE_TIMEOUT, reader.read_exact(&mut handshake))
                .await
                .map_err(|_| anyhow!("MTProto init inside Fake TLS timed out"))??;
            return Ok(Some(PreparedClient {
                handshake,
                reader,
                writer,
                label,
            }));
        }

        let mut handshake = [0_u8; HANDSHAKE_LEN];
        handshake[0] = first[0];
        timeout(HANDSHAKE_TIMEOUT, stream.read_exact(&mut handshake[1..]))
            .await
            .map_err(|_| anyhow!("client init timed out"))??;
        let (reader, writer) = stream.into_split();
        Ok(Some(PreparedClient {
            handshake,
            reader: ClientReader::Plain(reader),
            writer: ClientWriter::Plain(writer),
            label,
        }))
    }

    async fn forward_masking_probe(&self, mut client: TcpStream, initial: &[u8], label: &str) {
        let Some(upstream) = &self.inner.config.masking_upstream else {
            debug!(%label, "invalid Fake TLS probe closed; no masking upstream configured");
            return;
        };
        let connected = timeout(
            CONNECT_TIMEOUT,
            TcpStream::connect((upstream.as_str(), 443)),
        )
        .await;
        let Ok(Ok(mut origin)) = connected else {
            warn!(%label, %upstream, "cannot connect to masking upstream");
            return;
        };
        if is_direct_masking_self_loop(origin.peer_addr().ok(), client.local_addr().ok()) {
            warn!(%label, %upstream, "masking upstream resolved to the listener; refusing self-loop");
            return;
        }
        Stats::increment(&self.inner.stats.connections_masked);
        if origin.write_all(initial).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut origin).await;
    }

    #[allow(clippy::too_many_lines)]
    async fn route_client(
        &self,
        prepared: PreparedClient,
        mut client_init: ClientInit,
    ) -> Result<()> {
        let test = self.inner.config.force_test_dc || client_init.dc >= 10_000;
        if client_init.dc >= 10_000 {
            client_init.dc -= 10_000;
        }
        let key = DcKey {
            dc: client_init.dc,
            test,
            media: client_init.media,
        };
        let relay_init =
            generate_relay_init(client_init.transport, client_init.dc, client_init.media)?;
        let crypto = build_crypto_context(
            &client_init.prekey_iv,
            &self.inner.config.secret,
            &relay_init,
        );

        let target = self.inner.config.dc_redirects.get(&client_init.dc).copied();
        let cloudflare_available = self.inner.config.fallback_cfproxy
            || !self.inner.config.cfproxy_worker_domains.is_empty();
        let force_fallback =
            target.is_none() || self.cooldown_active(&self.inner.redirect_until, key).await;
        let ip_cooled = if let Some(target) = target {
            cloudflare_available
                && self
                    .cooldown_active(&self.inner.ip_fail_until, target)
                    .await
        } else {
            false
        };

        if force_fallback || ip_cooled {
            Box::pin(self.fallback(prepared, client_init, test, relay_init, crypto)).await?;
            return Ok(());
        }
        let target = target.expect("checked above");
        let path = if test { "/apiws_test" } else { "/apiws" };
        let domains = websocket_domains(client_init.dc, client_init.media);
        let ws_timeout = if self.cooldown_active(&self.inner.dc_fail_until, key).await {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(5)
        };

        let allow_refill = !self
            .cooldown_active(&self.inner.ip_fail_until, target)
            .await;
        let mut websocket = if test {
            None
        } else {
            self.inner
                .pool
                .get(
                    client_init.dc,
                    client_init.media,
                    target,
                    domains.clone(),
                    allow_refill,
                )
                .await
        };
        let pool_hit = websocket.is_some();
        let mut outcomes = Vec::new();
        if websocket.is_none() {
            for domain in &domains {
                match self
                    .connect_websocket(&target.to_string(), domain, None, path, ws_timeout, true)
                    .await
                {
                    Ok(connected) => {
                        websocket = Some(connected);
                        break;
                    }
                    Err(error) if error.is_redirect() => {
                        outcomes.push(DirectOutcome::Redirect);
                    }
                    Err(error) if error.is_timeout() => {
                        outcomes.push(DirectOutcome::Timeout);
                        break;
                    }
                    Err(error) => {
                        debug!(
                            label = %prepared.label,
                            dc = client_init.dc,
                            %domain,
                            %error,
                            "direct WebSocket attempt failed"
                        );
                        outcomes.push(DirectOutcome::Error);
                    }
                }
            }
        }

        if let Some(websocket) = websocket {
            match websocket.sender.send_binary(&relay_init).await {
                Ok(()) => {
                    self.inner.redirect_until.lock().await.remove(&key);
                    self.inner.dc_fail_until.lock().await.remove(&key);
                    self.inner.ip_fail_until.lock().await.remove(&target);
                    self.inner
                        .pool
                        .report_success(client_init.dc, client_init.media)
                        .await;
                    Stats::increment(&self.inner.stats.connections_ws);
                    info!(
                        label = %prepared.label,
                        dc = client_init.dc,
                        media = client_init.media,
                        route = if pool_hit { "websocket-pool" } else { "websocket-direct" },
                        "upstream route connected"
                    );
                    let splitter = MessageSplitter::new(
                        &relay_init,
                        client_init.transport,
                        self.inner.config.max_ws_frame_size,
                    );
                    bridge_websocket(
                        prepared,
                        websocket,
                        crypto,
                        Some(splitter),
                        Arc::clone(&self.inner.stats),
                        client_init.dc,
                        client_init.media,
                    )
                    .await?;
                    return Ok(());
                }
                Err(error) => {
                    debug!(
                        label = %prepared.label,
                        %error,
                        "pooled/direct WebSocket failed before client data; falling back"
                    );
                    websocket.close().await;
                    outcomes.push(DirectOutcome::Error);
                }
            }
        }

        if outcomes.contains(&DirectOutcome::Timeout) {
            self.inner
                .ip_fail_until
                .lock()
                .await
                .insert(target, Instant::now() + IP_FAILURE_COOLDOWN);
        }
        if !outcomes.is_empty()
            && outcomes
                .iter()
                .all(|outcome| *outcome == DirectOutcome::Redirect)
        {
            self.inner
                .redirect_until
                .lock()
                .await
                .insert(key, Instant::now() + REDIRECT_COOLDOWN);
        } else {
            self.inner
                .dc_fail_until
                .lock()
                .await
                .insert(key, Instant::now() + DC_FAILURE_COOLDOWN);
        }

        Box::pin(self.fallback(prepared, client_init, test, relay_init, crypto)).await?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn fallback(
        &self,
        prepared: PreparedClient,
        client_init: ClientInit,
        test: bool,
        relay_init: [u8; 64],
        crypto: crate::crypto::CryptoContext,
    ) -> Result<()> {
        let Some(fallback_ip) = self.inner.config.fallback_ip(client_init.dc, test) else {
            warn!(
                label = %prepared.label,
                dc = client_init.dc,
                "no fallback route for DC"
            );
            return Ok(());
        };

        let mut prepared = Some(prepared);
        let mut crypto = Some(crypto);
        let mut workers = self.inner.config.cfproxy_worker_domains.clone();
        shuffle(&mut workers);
        for worker in workers {
            let path = form_urlencoded::Serializer::new(String::new())
                .append_pair("dst", &fallback_ip.to_string())
                .append_pair("dc", &client_init.dc.to_string())
                .finish();
            let path = format!("/apiws?{path}");
            match self
                .connect_websocket(&worker, &worker, None, &path, CONNECT_TIMEOUT, false)
                .await
            {
                Ok(websocket) => {
                    if let Err(error) = websocket.sender.send_binary(&relay_init).await {
                        debug!(%worker, %error, "Worker WebSocket failed before client data");
                        websocket.close().await;
                        continue;
                    }
                    Stats::increment(&self.inner.stats.connections_cfproxy);
                    info!(
                        label = %prepared.as_ref().expect("session is available").label,
                        dc = client_init.dc,
                        media = client_init.media,
                        route = "cloudflare-worker",
                        domain = %worker,
                        "upstream route connected"
                    );
                    bridge_websocket(
                        prepared.take().expect("session consumed once"),
                        websocket,
                        crypto.take().expect("crypto consumed once"),
                        None,
                        Arc::clone(&self.inner.stats),
                        client_init.dc,
                        client_init.media,
                    )
                    .await?;
                    return Ok(());
                }
                Err(error) => debug!(%worker, %error, "Worker fallback failed"),
            }
        }

        if self.inner.config.fallback_cfproxy && !test {
            let domains = self.cf_domains(client_init.dc).await;
            for base_domain in domains {
                let domain = format!("kws{}.{}", client_init.dc, base_domain);
                match self
                    .connect_websocket(&domain, &domain, None, "/apiws", CONNECT_TIMEOUT, true)
                    .await
                {
                    Ok(websocket) => {
                        if let Err(error) = websocket.sender.send_binary(&relay_init).await {
                            debug!(%domain, %error, "CF WebSocket failed before client data");
                            websocket.close().await;
                            continue;
                        }
                        self.inner
                            .active_cf_domain
                            .lock()
                            .await
                            .insert(client_init.dc, base_domain);
                        Stats::increment(&self.inner.stats.connections_cfproxy);
                        info!(
                            label = %prepared.as_ref().expect("session is available").label,
                            dc = client_init.dc,
                            media = client_init.media,
                            route = "cloudflare-proxy",
                            domain = %domain,
                            "upstream route connected"
                        );
                        let splitter = MessageSplitter::new(
                            &relay_init,
                            client_init.transport,
                            self.inner.config.max_ws_frame_size,
                        );
                        bridge_websocket(
                            prepared.take().expect("session consumed once"),
                            websocket,
                            crypto.take().expect("crypto consumed once"),
                            Some(splitter),
                            Arc::clone(&self.inner.stats),
                            client_init.dc,
                            client_init.media,
                        )
                        .await?;
                        return Ok(());
                    }
                    Err(error) => debug!(%domain, %error, "CF proxy fallback failed"),
                }
            }
        }

        let connected = timeout(CONNECT_TIMEOUT, TcpStream::connect((fallback_ip, 443))).await;
        let Ok(Ok(mut telegram)) = connected else {
            warn!(
                label = %prepared.as_ref().expect("not consumed").label,
                %fallback_ip,
                "TCP fallback failed"
            );
            return Ok(());
        };
        telegram.set_nodelay(true)?;
        telegram.write_all(&relay_init).await?;
        Stats::increment(&self.inner.stats.connections_tcp_fallback);
        info!(
            label = %prepared.as_ref().expect("session is available").label,
            dc = client_init.dc,
            media = client_init.media,
            route = "tcp-fallback",
            %fallback_ip,
            "upstream route connected"
        );
        bridge_tcp(
            prepared.take().expect("session consumed once"),
            telegram,
            crypto.take().expect("crypto consumed once"),
            Arc::clone(&self.inner.stats),
        )
        .await
    }

    async fn connect_websocket(
        &self,
        host: &str,
        domain: &str,
        sni: Option<&str>,
        path: &str,
        connect_timeout: Duration,
        request_binary_subprotocol: bool,
    ) -> Result<RawWebSocket, WebSocketError> {
        RawWebSocket::connect(
            host,
            domain,
            sni,
            path,
            connect_timeout,
            Arc::clone(&self.inner.tls_config),
            self.inner.config.buffer_size,
            self.inner.config.max_ws_frame_size,
            request_binary_subprotocol,
        )
        .await
    }

    async fn cf_domains(&self, dc: i16) -> Vec<String> {
        let active = self.inner.active_cf_domain.lock().await.get(&dc).cloned();
        let mut domains = self.inner.config.cfproxy_domains.clone();
        shuffle(&mut domains);
        if let Some(active) = active {
            if let Some(index) = domains.iter().position(|domain| domain == &active) {
                domains.swap(0, index);
            }
        }
        domains
    }

    async fn cooldown_active<K>(&self, cooldowns: &Mutex<HashMap<K, Instant>>, key: K) -> bool
    where
        K: Eq + std::hash::Hash + Copy,
    {
        let mut cooldowns = cooldowns.lock().await;
        match cooldowns.get(&key).copied() {
            Some(until) if Instant::now() < until => true,
            Some(_) => {
                cooldowns.remove(&key);
                false
            }
            None => false,
        }
    }

    async fn register_fake_tls_hello(&self, client_random: [u8; 32]) -> bool {
        let now = Instant::now();
        let mut replays = self.inner.fake_tls_replays.lock().await;
        replays.retain(|_, seen| now.duration_since(*seen) < FAKE_TLS_REPLAY_WINDOW);
        if replays.contains_key(&client_random) || replays.len() >= MAX_FAKE_TLS_REPLAY_ENTRIES {
            return false;
        }
        replays.insert(client_random, now);
        true
    }
}

fn is_direct_masking_self_loop(origin: Option<SocketAddr>, listener: Option<SocketAddr>) -> bool {
    origin.is_some() && origin == listener
}

struct ActiveConnection(Arc<Stats>);

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.0.connections_active.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn read_proxy_v1_header(stream: &mut TcpStream) -> Result<String> {
    let mut line = Vec::with_capacity(PROXY_V1_MAX_LENGTH);
    loop {
        if line.len() >= PROXY_V1_MAX_LENGTH {
            bail!("PROXY v1 header exceeds {PROXY_V1_MAX_LENGTH} bytes");
        }
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).await?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    if !line.ends_with(b"\r\n") {
        bail!("PROXY v1 header must end with CRLF");
    }
    let text = std::str::from_utf8(&line)?.trim_end_matches("\r\n");
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.first() != Some(&"PROXY") {
        bail!("missing PROXY v1 signature");
    }
    if fields.get(1) == Some(&"UNKNOWN") {
        return Ok("proxy-unknown".to_owned());
    }
    if fields.len() != 6 || !matches!(fields[1], "TCP4" | "TCP6") {
        bail!("malformed PROXY v1 header");
    }
    let source: IpAddr = fields[2].parse()?;
    let destination: IpAddr = fields[3].parse()?;
    let source_port: u16 = fields[4].parse()?;
    let _destination_port: u16 = fields[5].parse()?;
    let family_matches = matches!(
        (fields[1], source, destination),
        ("TCP4", IpAddr::V4(_), IpAddr::V4(_)) | ("TCP6", IpAddr::V6(_), IpAddr::V6(_))
    );
    if !family_matches {
        bail!("PROXY v1 address family does not match its protocol");
    }
    Ok(SocketAddr::new(source, source_port).to_string())
}

#[allow(clippy::too_many_arguments)]
async fn bridge_websocket(
    mut client: PreparedClient,
    websocket: RawWebSocket,
    crypto: crate::crypto::CryptoContext,
    mut splitter: Option<MessageSplitter>,
    stats: Arc<Stats>,
    dc: i16,
    media: bool,
) -> Result<()> {
    let start = Instant::now();
    let sender = websocket.sender.clone();
    let mut receiver = websocket.receiver;
    let mut upstream_crypto = crypto.upstream;
    let mut downstream_crypto = crypto.downstream;
    let up_sender = sender.clone();
    let mut upload_bytes = 0_u64;
    let mut download_bytes = 0_u64;

    let upload = async {
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = client.reader.read(&mut buffer).await?;
            if read == 0 {
                if let Some(splitter) = &mut splitter {
                    for tail in splitter.flush() {
                        up_sender
                            .send_binary(&tail)
                            .await
                            .map_err(io::Error::other)?;
                    }
                }
                return io::Result::Ok(());
            }
            Stats::add(&stats.bytes_up, read);
            upload_bytes += read as u64;
            let mut data = buffer[..read].to_vec();
            upstream_crypto.client_decrypt.apply_keystream(&mut data);
            upstream_crypto.telegram_encrypt.apply_keystream(&mut data);
            if let Some(splitter) = &mut splitter {
                let parts = splitter.split(&data);
                if !parts.is_empty() {
                    up_sender
                        .send_batch(parts.iter().map(Vec::as_slice))
                        .await
                        .map_err(io::Error::other)?;
                }
            } else {
                up_sender
                    .send_binary(&data)
                    .await
                    .map_err(io::Error::other)?;
            }
        }
    };

    let download = async {
        while let Some(mut data) = receiver.receive().await.map_err(io::Error::other)? {
            Stats::add(&stats.bytes_down, data.len());
            download_bytes += data.len() as u64;
            downstream_crypto
                .telegram_decrypt
                .apply_keystream(&mut data);
            downstream_crypto.client_encrypt.apply_keystream(&mut data);
            client.writer.write_all(&data).await?;
        }
        io::Result::Ok(())
    };

    let result = tokio::select! {
        result = upload => result,
        result = download => result,
    };
    sender.close().await;
    let _ = client.writer.shutdown().await;
    info!(
        label = %client.label,
        dc,
        media,
        upload_bytes,
        download_bytes,
        elapsed_seconds = start.elapsed().as_secs_f64(),
        "WebSocket session closed"
    );
    result.map_err(Into::into)
}

async fn bridge_tcp(
    mut client: PreparedClient,
    telegram: TcpStream,
    crypto: crate::crypto::CryptoContext,
    stats: Arc<Stats>,
) -> Result<()> {
    let (mut telegram_reader, mut telegram_writer) = telegram.into_split();
    let mut upstream_crypto = crypto.upstream;
    let mut downstream_crypto = crypto.downstream;

    let upload = async {
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = client.reader.read(&mut buffer).await?;
            if read == 0 {
                telegram_writer.shutdown().await?;
                return io::Result::Ok(());
            }
            Stats::add(&stats.bytes_up, read);
            let data = &mut buffer[..read];
            upstream_crypto.client_decrypt.apply_keystream(data);
            upstream_crypto.telegram_encrypt.apply_keystream(data);
            telegram_writer.write_all(data).await?;
        }
    };
    let download = async {
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = telegram_reader.read(&mut buffer).await?;
            if read == 0 {
                return io::Result::Ok(());
            }
            Stats::add(&stats.bytes_down, read);
            let data = &mut buffer[..read];
            downstream_crypto.telegram_decrypt.apply_keystream(data);
            downstream_crypto.client_encrypt.apply_keystream(data);
            client.writer.write_all(data).await?;
        }
    };
    let result = tokio::select! {
        result = upload => result,
        result = download => result,
    };
    let _ = client.writer.shutdown().await;
    result.map_err(Into::into)
}

fn shuffle<T>(values: &mut [T]) {
    for index in (1..values.len()).rev() {
        let mut random = [0_u8; 4];
        if getrandom::fill(&mut random).is_err() {
            return;
        }
        let selected = usize::try_from(u32::from_le_bytes(random))
            .expect("supported targets have at least 32-bit usize")
            % (index + 1);
        values.swap(index, selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_blacklist_requires_every_attempt_to_redirect() {
        let all_redirects = |outcomes: &[DirectOutcome]| {
            !outcomes.is_empty()
                && outcomes
                    .iter()
                    .all(|outcome| *outcome == DirectOutcome::Redirect)
        };
        assert!(all_redirects(&[
            DirectOutcome::Redirect,
            DirectOutcome::Redirect
        ]));
        assert!(!all_redirects(&[
            DirectOutcome::Redirect,
            DirectOutcome::Timeout
        ]));
        assert!(!all_redirects(&[
            DirectOutcome::Timeout,
            DirectOutcome::Redirect
        ]));
        assert!(!all_redirects(&[
            DirectOutcome::Redirect,
            DirectOutcome::Error
        ]));
    }

    #[tokio::test]
    async fn fake_tls_client_random_is_single_use_within_replay_window() {
        let proxy = Proxy::new(ProxyConfig::default()).unwrap();
        let random = [7_u8; 32];
        assert!(proxy.register_fake_tls_hello(random).await);
        assert!(!proxy.register_fake_tls_hello(random).await);
        assert!(proxy.register_fake_tls_hello([8_u8; 32]).await);
    }

    #[test]
    fn masking_self_loop_requires_the_same_ip_and_port() {
        let listener: SocketAddr = "127.0.0.1:1443".parse().unwrap();
        let cohosted_https: SocketAddr = "127.0.0.1:443".parse().unwrap();
        assert!(!is_direct_masking_self_loop(
            Some(cohosted_https),
            Some(listener)
        ));
        assert!(is_direct_masking_self_loop(Some(listener), Some(listener)));
        assert!(!is_direct_masking_self_loop(None, Some(listener)));
    }
}
