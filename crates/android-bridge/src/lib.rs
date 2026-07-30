use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jstring;
use serde::{Deserialize, Serialize};
use tg_ws_proxy::config::{ProxyConfig, parse_secret};
use tg_ws_proxy::logging::RotatingMakeWriter;
use tg_ws_proxy::{Proxy, stats::StatsSnapshot};
use tokio::sync::oneshot;
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_LOG_SIZE: u64 = 2 * 1024 * 1024;
const DEFAULT_LOG_BACKUPS: usize = 3;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MobileConfig {
    port: u16,
    secret: String,
    #[serde(default = "default_pool_size")]
    pool_size: usize,
    #[serde(default = "default_true")]
    fallback_cfproxy: bool,
    #[serde(default)]
    fake_tls_domain: Option<String>,
    #[serde(default)]
    masking_upstream: Option<String>,
    log_path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MobileStatus {
    state: &'static str,
    error: Option<String>,
    telegram_url: Option<String>,
    started_at_epoch_seconds: Option<u64>,
    total_connections: u64,
    active_connections: u64,
    websocket_connections: u64,
    tcp_fallback_connections: u64,
    cloudflare_connections: u64,
    bad_connections: u64,
    bytes_up: u64,
    bytes_down: u64,
}

impl Default for MobileStatus {
    fn default() -> Self {
        Self {
            state: "stopped",
            error: None,
            telegram_url: None,
            started_at_epoch_seconds: None,
            total_connections: 0,
            active_connections: 0,
            websocket_connections: 0,
            tcp_fallback_connections: 0,
            cloudflare_connections: 0,
            bad_connections: 0,
            bytes_up: 0,
            bytes_down: 0,
        }
    }
}

#[derive(Serialize)]
struct NativeResponse {
    ok: bool,
    error: Option<String>,
}

#[derive(Default)]
struct RuntimeState {
    status: MobileStatus,
    proxy: Option<Proxy>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

fn state() -> &'static Mutex<RuntimeState> {
    static STATE: OnceLock<Mutex<RuntimeState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(RuntimeState::default()))
}

fn lock_state() -> MutexGuard<'static, RuntimeState> {
    state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn default_pool_size() -> usize {
    4
}

fn default_true() -> bool {
    true
}

fn normalize_optional_domain(value: Option<String>) -> Option<String> {
    value
        .map(|domain| domain.trim().to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
}

fn build_proxy_config(config: &MobileConfig) -> Result<ProxyConfig> {
    let mut proxy_config = ProxyConfig {
        host: "127.0.0.1".to_owned(),
        port: config.port,
        pool_size: config.pool_size,
        fallback_cfproxy: config.fallback_cfproxy,
        fake_tls_domain: normalize_optional_domain(config.fake_tls_domain.clone()),
        masking_upstream: normalize_optional_domain(config.masking_upstream.clone()),
        ..ProxyConfig::default()
    };
    proxy_config.secret = parse_secret(&config.secret)?;
    proxy_config.validate()?;
    Ok(proxy_config)
}

fn init_logging(path: &PathBuf) -> Result<()> {
    static LOGGING: OnceLock<()> = OnceLock::new();
    if LOGGING.get().is_some() {
        return Ok(());
    }
    let writer = RotatingMakeWriter::new(path, DEFAULT_LOG_SIZE, DEFAULT_LOG_BACKUPS)?;
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("info"))
        .with_ansi(false)
        .with_writer(writer)
        .finish();
    if tracing::subscriber::set_global_default(subscriber).is_ok() {
        let _ = LOGGING.set(());
    }
    Ok(())
}

fn reap_finished_thread(runtime: &mut RuntimeState) -> Result<()> {
    let Some(thread) = runtime.thread.take() else {
        return Ok(());
    };
    if thread.is_finished() {
        thread
            .join()
            .map_err(|_| anyhow!("Android proxy runtime thread panicked"))?;
    } else {
        runtime.thread = Some(thread);
    }
    Ok(())
}

fn start_proxy(config_json: &str) -> Result<()> {
    let config: MobileConfig =
        serde_json::from_str(config_json).context("invalid Android proxy configuration")?;
    init_logging(&config.log_path)?;
    let proxy_config = build_proxy_config(&config)?;
    let telegram_url = proxy_config.telegram_url("127.0.0.1");
    let proxy = Proxy::new(proxy_config)?;

    let mut runtime = lock_state();
    reap_finished_thread(&mut runtime)?;
    if runtime.thread.is_some() {
        bail!("proxy is already running");
    }

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    runtime.status = MobileStatus {
        state: "starting",
        telegram_url: Some(telegram_url),
        ..MobileStatus::default()
    };
    runtime.proxy = Some(proxy.clone());
    runtime.shutdown = Some(shutdown_tx);
    runtime.thread = Some(
        thread::Builder::new()
            .name("tg-ws-proxy-android".to_owned())
            .spawn(move || run_proxy(&proxy, shutdown_rx))
            .context("failed to start Android proxy runtime thread")?,
    );
    Ok(())
}

fn run_proxy(proxy: &Proxy, shutdown_rx: oneshot::Receiver<()>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("tg-ws-proxy-mobile")
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(proxy.run_until_ready(
            async {
                let _ = shutdown_rx.await;
            },
            || {
                let mut state = lock_state();
                state.status.state = "running";
                state.status.started_at_epoch_seconds = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs());
                info!("Android foreground proxy is ready");
            },
        )),
        Err(error) => Err(error.into()),
    };

    let mut state = lock_state();
    state.shutdown = None;
    state.proxy = None;
    match result {
        Ok(()) => {
            state.status.state = "stopped";
            state.status.started_at_epoch_seconds = None;
            state.status.error = None;
        }
        Err(error) => {
            state.status.state = "failed";
            state.status.started_at_epoch_seconds = None;
            state.status.error = Some(format!("{error:#}"));
        }
    }
}

fn stop_proxy() -> Result<()> {
    let mut runtime = lock_state();
    reap_finished_thread(&mut runtime)?;
    let Some(shutdown) = runtime.shutdown.take() else {
        if runtime.thread.is_none() {
            runtime.status.state = "stopped";
            return Ok(());
        }
        bail!("proxy is already stopping");
    };
    runtime.status.state = "stopping";
    shutdown
        .send(())
        .map_err(|()| anyhow!("proxy runtime is no longer available"))
}

fn proxy_status() -> MobileStatus {
    let runtime = lock_state();
    let mut status = runtime.status.clone();
    let snapshot = runtime.proxy.as_ref().map_or_else(
        || StatsSnapshot {
            total: 0,
            active: 0,
            websocket: 0,
            tcp_fallback: 0,
            cloudflare: 0,
            bad: 0,
            bytes_up: 0,
            bytes_down: 0,
        },
        Proxy::stats,
    );
    status.total_connections = snapshot.total;
    status.active_connections = snapshot.active;
    status.websocket_connections = snapshot.websocket;
    status.tcp_fallback_connections = snapshot.tcp_fallback;
    status.cloudflare_connections = snapshot.cloudflare;
    status.bad_connections = snapshot.bad;
    status.bytes_up = snapshot.bytes_up;
    status.bytes_down = snapshot.bytes_down;
    status
}

fn response(result: Result<()>) -> String {
    let response = match result {
        Ok(()) => NativeResponse {
            ok: true,
            error: None,
        },
        Err(error) => NativeResponse {
            ok: false,
            error: Some(format!("{error:#}")),
        },
    };
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.to_owned())
}

fn java_string(env: &JNIEnv<'_>, value: String) -> jstring {
    env.new_string(value)
        .map_or(std::ptr::null_mut(), JString::into_raw)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_danusha_tgwsproxy_NativeBridge_nativeStart(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    config: JString<'_>,
) -> jstring {
    let result = env
        .get_string(&config)
        .map(String::from)
        .map_err(anyhow::Error::from)
        .and_then(|config| start_proxy(&config));
    java_string(&env, response(result))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_danusha_tgwsproxy_NativeBridge_nativeStop(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jstring {
    java_string(&env, response(stop_proxy()))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_danusha_tgwsproxy_NativeBridge_nativeStatus(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jstring {
    let value = serde_json::to_string(&proxy_status()).unwrap_or_else(|error| {
        format!(r#"{{"state":"failed","error":"status serialization failed: {error}"}}"#)
    });
    java_string(&env, value)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn mobile_config_builds_loopback_proxy_and_url() {
        let config = MobileConfig {
            port: 1443,
            secret: "00112233445566778899aabbccddeeff".to_owned(),
            pool_size: 2,
            fallback_cfproxy: true,
            fake_tls_domain: None,
            masking_upstream: None,
            log_path: PathBuf::from("proxy.log"),
        };
        let proxy_config = build_proxy_config(&config).unwrap();
        assert_eq!(proxy_config.host, "127.0.0.1");
        assert_eq!(proxy_config.port, 1443);
        assert_eq!(
            proxy_config.telegram_url("127.0.0.1"),
            "tg://proxy?server=127.0.0.1&port=1443&secret=dd00112233445566778899aabbccddeeff"
        );
    }

    #[test]
    fn mobile_config_rejects_invalid_secret() {
        let config = MobileConfig {
            port: 1443,
            secret: "short".to_owned(),
            pool_size: 4,
            fallback_cfproxy: true,
            fake_tls_domain: None,
            masking_upstream: None,
            log_path: PathBuf::from("proxy.log"),
        };
        assert!(build_proxy_config(&config).is_err());
    }

    #[test]
    fn mobile_runtime_reports_readiness_and_stops() {
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let directory = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "port": port,
            "secret": "00112233445566778899aabbccddeeff",
            "poolSize": 0,
            "fallbackCfproxy": false,
            "logPath": directory.path().join("proxy.log"),
        });

        start_proxy(&config.to_string()).unwrap();
        wait_for_state("running");
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stop_proxy().unwrap();
        wait_for_state("stopped");
        TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .expect("listener port must be released after Android runtime stops");
    }

    fn wait_for_state(expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = proxy_status();
            if status.state == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected}, last status: {status:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
