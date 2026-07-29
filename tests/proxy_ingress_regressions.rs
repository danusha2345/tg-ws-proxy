use std::io;
use std::net::{Ipv4Addr, TcpListener as StdTcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tg_ws_proxy::websocket::WebSocketError;
use tg_ws_proxy::{Proxy, ProxyConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

struct RunningProxy {
    proxy: Proxy,
    port: u16,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl RunningProxy {
    async fn start(proxy_protocol: bool) -> Self {
        let reservation =
            StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("test port reservation");
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);

        let config = ProxyConfig {
            host: Ipv4Addr::LOCALHOST.to_string(),
            port,
            pool_size: 0,
            fallback_cfproxy: false,
            cfproxy_domains: Vec::new(),
            cfproxy_worker_domains: Vec::new(),
            proxy_protocol,
            ..ProxyConfig::default()
        };
        let proxy = Proxy::new(config).expect("test proxy config must be valid");

        let (shutdown, shutdown_receiver) = oneshot::channel();
        let runner = proxy.clone();
        let task = tokio::spawn(async move {
            runner
                .run_until(async {
                    let _ = shutdown_receiver.await;
                })
                .await
        });

        for _ in 0..100 {
            match TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await {
                Ok(stream) => {
                    drop(stream);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("unexpected readiness probe failure: {error}"),
            }
        }

        Self {
            proxy,
            port,
            shutdown: Some(shutdown),
            task,
        }
    }

    async fn connect(&self) -> TcpStream {
        TcpStream::connect((Ipv4Addr::LOCALHOST, self.port))
            .await
            .expect("proxy must accept test client")
    }

    async fn stop(mut self) {
        self.shutdown
            .take()
            .expect("shutdown signal is sent once")
            .send(())
            .ok();
        timeout(Duration::from_secs(2), self.task)
            .await
            .expect("proxy shutdown must not hang")
            .expect("proxy task must not panic")
            .expect("proxy shutdown must succeed");
    }
}

async fn assert_peer_closed(stream: &mut TcpStream) {
    let mut byte = [0_u8; 1];
    let read = timeout(Duration::from_secs(1), stream.read(&mut byte))
        .await
        .expect("proxy must reject malformed ingress without draining forever")
        .expect("socket read after rejection must succeed");
    assert_eq!(read, 0, "rejected connection must end at EOF");
}

#[tokio::test]
async fn invalid_mtproto_init_is_closed_promptly_and_counted() {
    let running = RunningProxy::start(false).await;
    let baseline = running.proxy.stats();
    let mut stream = running.connect().await;
    stream.write_all(&[0_u8; 64]).await.unwrap();
    assert_peer_closed(&mut stream).await;

    for _ in 0..20 {
        let stats = running.proxy.stats();
        if stats.bad == baseline.bad + 1 && stats.active == 0 {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    let stats = running.proxy.stats();
    assert_eq!(stats.bad, baseline.bad + 1);
    assert_eq!(stats.active, 0);
    running.stop().await;
}

#[tokio::test]
async fn valid_proxy_v1_header_reaches_mtproto_validation() {
    let running = RunningProxy::start(true).await;
    let baseline = running.proxy.stats();
    let mut stream = running.connect().await;
    stream
        .write_all(b"PROXY TCP4 192.0.2.10 198.51.100.20 45678 443\r\n")
        .await
        .unwrap();
    stream.write_all(&[0_u8; 64]).await.unwrap();
    assert_peer_closed(&mut stream).await;

    for _ in 0..20 {
        if running.proxy.stats().bad == baseline.bad + 1 {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(running.proxy.stats().bad, baseline.bad + 1);
    running.stop().await;
}

#[tokio::test]
async fn malformed_proxy_v1_header_is_rejected_before_mtproto_validation() {
    let running = RunningProxy::start(true).await;
    let baseline = running.proxy.stats();
    let mut stream = running.connect().await;
    stream
        .write_all(b"PROXY TCP4 192.0.2.10 198.51.100.20 invalid 443\r\n")
        .await
        .unwrap();
    assert_peer_closed(&mut stream).await;

    assert_eq!(running.proxy.stats().bad, baseline.bad);
    running.stop().await;
}

#[tokio::test]
async fn proxy_v1_rejects_address_family_mismatch() {
    let running = RunningProxy::start(true).await;
    let baseline = running.proxy.stats();
    let mut stream = running.connect().await;
    stream
        .write_all(b"PROXY TCP4 2001:db8::10 2001:db8::20 45678 443\r\n")
        .await
        .unwrap();
    assert_peer_closed(&mut stream).await;

    assert_eq!(running.proxy.stats().bad, baseline.bad);
    running.stop().await;
}

#[tokio::test]
async fn bind_failure_does_not_report_readiness() {
    let reservation = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    let config = ProxyConfig {
        host: Ipv4Addr::LOCALHOST.to_string(),
        port,
        pool_size: 0,
        fallback_cfproxy: false,
        ..ProxyConfig::default()
    };
    let proxy = Proxy::new(config).unwrap();
    let ready = Arc::new(AtomicBool::new(false));
    let ready_for_callback = Arc::clone(&ready);
    let result = proxy
        .run_until_ready(std::future::pending(), move || {
            ready_for_callback.store(true, Ordering::Release);
        })
        .await;

    assert!(result.is_err());
    assert!(!ready.load(Ordering::Acquire));
}

#[test]
fn websocket_errors_keep_redirect_and_timeout_outcomes_distinct() {
    for status in [301_u16, 302, 303, 307, 308] {
        let error = WebSocketError::Handshake {
            status: status.try_into().expect("valid HTTP status"),
            location: Some("https://fallback.example/apiws".to_owned()),
        };
        assert!(error.is_redirect(), "HTTP {status} must be a redirect");
        assert!(!error.is_timeout());
        assert_eq!(error.location(), Some("https://fallback.example/apiws"));
    }

    let non_redirect = WebSocketError::Handshake {
        status: 101_u16.try_into().expect("valid HTTP status"),
        location: None,
    };
    assert!(!non_redirect.is_redirect());
    assert!(!non_redirect.is_timeout());

    let timeout = WebSocketError::Timeout;
    assert!(timeout.is_timeout());
    assert!(!timeout.is_redirect());
    assert_eq!(timeout.location(), None);

    let io_error = WebSocketError::Io(io::Error::other("connection reset"));
    assert!(!io_error.is_timeout());
    assert!(!io_error.is_redirect());
}
