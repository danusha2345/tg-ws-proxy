use std::collections::HashMap;
use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tg_ws_proxy::Proxy;
use tg_ws_proxy::config::{
    ProxyConfig, load_or_create_secret, normalize_domains, parse_dc_ip, parse_secret,
};
use tg_ws_proxy::logging::RotatingMakeWriter;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::{BoxMakeWriter, MakeWriterExt};

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Parser)]
#[command(
    name = "tg-ws-proxy",
    version,
    about = "Telegram MTProto WebSocket bridge proxy"
)]
struct Args {
    #[arg(long, default_value_t = 1443, env = "TG_WS_PROXY_PORT")]
    port: u16,

    #[arg(long, default_value = "127.0.0.1", env = "TG_WS_PROXY_HOST")]
    host: String,

    #[arg(long, env = "TG_WS_PROXY_ADVERTISE_HOST")]
    advertise_host: Option<String>,

    #[arg(long, env = "TG_WS_PROXY_SECRET")]
    secret: Option<String>,

    #[arg(long, value_name = "PATH", env = "TG_WS_PROXY_SECRET_FILE")]
    secret_file: Option<PathBuf>,

    #[arg(
        long = "dc-ip",
        value_name = "DC:IP",
        env = "TG_WS_PROXY_DC_IPS",
        value_delimiter = ' '
    )]
    dc_ip: Vec<String>,

    #[arg(short, long, env = "TG_WS_PROXY_VERBOSE")]
    verbose: bool,

    #[arg(long, value_name = "PATH", env = "TG_WS_PROXY_LOG_FILE")]
    log_file: Option<PathBuf>,

    #[arg(
        long,
        default_value_t = 5.0,
        value_name = "MB",
        env = "TG_WS_PROXY_LOG_MAX_MB"
    )]
    log_max_mb: f64,

    #[arg(
        long,
        default_value_t = 1,
        value_name = "N",
        env = "TG_WS_PROXY_LOG_BACKUPS"
    )]
    log_backups: usize,

    #[arg(
        long,
        default_value_t = 256,
        value_name = "KB",
        env = "TG_WS_PROXY_BUF_KB"
    )]
    buf_kb: usize,

    #[arg(
        long,
        default_value_t = 4,
        value_name = "N",
        env = "TG_WS_PROXY_POOL_SIZE"
    )]
    pool_size: usize,

    #[arg(
        long = "cfproxy-domain",
        value_name = "DOMAIN",
        env = "TG_WS_PROXY_CFPROXY_DOMAINS"
    )]
    cfproxy_domain: Vec<String>,

    #[arg(
        long = "cfproxy-worker-domain",
        value_name = "DOMAIN",
        env = "TG_WS_PROXY_CF_WORKER"
    )]
    cfproxy_worker_domain: Vec<String>,

    #[arg(long, env = "TG_WS_PROXY_NO_CFPROXY")]
    no_cfproxy: bool,

    #[arg(long, value_name = "DOMAIN", env = "TG_WS_PROXY_FAKE_TLS_DOMAIN")]
    fake_tls_domain: Option<String>,

    #[arg(long, value_name = "DOMAIN", env = "TG_WS_PROXY_MASKING_UPSTREAM")]
    masking_upstream: Option<String>,

    #[arg(long, env = "TG_WS_PROXY_FORCE_TEST_DC")]
    force_test_dc: bool,

    #[arg(long, env = "TG_WS_PROXY_PROXY_PROTOCOL")]
    proxy_protocol: bool,

    #[arg(
        long,
        default_value_t = 16,
        value_name = "MB",
        env = "TG_WS_PROXY_MAX_WS_MESSAGE_MB"
    )]
    max_ws_message_mb: usize,

    #[arg(
        long,
        default_value_t = 1024,
        value_name = "N",
        env = "TG_WS_PROXY_MAX_CONNECTIONS"
    )]
    max_connections: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(&args)?;

    let mut config = ProxyConfig {
        host: args.host,
        port: args.port,
        ..ProxyConfig::default()
    };
    if let Some(secret) = args
        .secret
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        config.secret = parse_secret(secret)?;
    } else if let Some(path) = args.secret_file.as_deref() {
        config.secret = load_or_create_secret(path, config.secret)?;
        info!(path = %path.display(), "loaded persistent proxy secret");
    } else {
        info!(secret = %config.secret_hex(), "generated a random proxy secret");
    }
    if !args.dc_ip.is_empty() {
        config.dc_redirects = args
            .dc_ip
            .iter()
            .map(|value| parse_dc_ip(value))
            .collect::<Result<HashMap<_, _>>>()?;
    }
    config.buffer_size = args
        .buf_kb
        .max(4)
        .checked_mul(1024)
        .context("--buf-kb is too large")?;
    config.pool_size = args.pool_size;
    config.fallback_cfproxy = !args.no_cfproxy;
    if !args.cfproxy_domain.is_empty() {
        config.cfproxy_domains = normalize_domains(args.cfproxy_domain.iter().map(String::as_str))?;
    }
    config.cfproxy_worker_domains =
        normalize_domains(args.cfproxy_worker_domain.iter().map(String::as_str))?;
    config.fake_tls_domain = args
        .fake_tls_domain
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    config.masking_upstream = args
        .masking_upstream
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    config.force_test_dc = args.force_test_dc;
    config.proxy_protocol = args.proxy_protocol;
    config.max_ws_frame_size = args
        .max_ws_message_mb
        .max(1)
        .checked_mul(1024 * 1024)
        .context("--max-ws-message-mb is too large")?;
    config.max_connections = args.max_connections;

    let advertised_host = args
        .advertise_host
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| discover_advertised_host(&config.host));
    let connect_url = config.telegram_url(&advertised_host);
    let proxy = Proxy::new(config)?;

    proxy
        .run_until_ready(shutdown_signal(), move || {
            info!(url = %connect_url, "Telegram connection URL");
            println!("{connect_url}");
        })
        .await
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if let Err(error) = result {
                            warn!(%error, "failed to wait for Ctrl-C");
                        }
                    }
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                warn!(%error, "failed to install SIGTERM handler");
                if let Err(error) = tokio::signal::ctrl_c().await {
                    warn!(%error, "failed to wait for Ctrl-C");
                }
            }
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to wait for Ctrl-C");
    }
}

fn init_logging(args: &Args) -> Result<()> {
    let filter = if args.verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter));
    let writer = if let Some(path) = &args.log_file {
        let max_bytes = log_max_bytes(args.log_max_mb)?;
        let file = RotatingMakeWriter::new(path, max_bytes, args.log_backups)
            .with_context(|| format!("failed to configure log file {}", path.display()))?;
        BoxMakeWriter::new(std::io::stderr.and(file))
    } else {
        BoxMakeWriter::new(std::io::stderr)
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_target(false)
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize logging: {error}"))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn log_max_bytes(megabytes: f64) -> Result<u64> {
    const MAX_LOG_MEGABYTES: f64 = 1_048_576.0;
    if !megabytes.is_finite() || megabytes <= 0.0 || megabytes > MAX_LOG_MEGABYTES {
        anyhow::bail!(
            "--log-max-mb must be a positive finite number no greater than {MAX_LOG_MEGABYTES}"
        );
    }
    Ok((megabytes * 1024.0 * 1024.0).round() as u64)
}

fn discover_advertised_host(bind_host: &str) -> String {
    if bind_host != "0.0.0.0" && bind_host != "::" {
        return bind_host.to_owned();
    }
    let family_probe = if bind_host == "::" {
        "[2001:4860:4860::8888]:80"
    } else {
        "8.8.8.8:80"
    };
    let bind = if bind_host == "::" {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    UdpSocket::bind(bind)
        .and_then(|socket| {
            socket.connect(family_probe)?;
            socket.local_addr()
        })
        .map_or_else(
            |_| "127.0.0.1".to_owned(),
            |address| match address.ip() {
                IpAddr::V4(ip) => ip.to_string(),
                IpAddr::V6(ip) => format!("[{ip}]"),
            },
        )
}
