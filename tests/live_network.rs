use std::process::Command;
use std::time::Duration;

use tg_ws_proxy::config::tls_config;
use tg_ws_proxy::pool::websocket_domains;
use tg_ws_proxy::websocket::RawWebSocket;

const TELEGRAM_DC4_IP: &str = "149.154.167.220";
const TELEGRAM_DC4_DOMAIN: &str = "kws4.web.telegram.org";

#[tokio::test]
#[ignore = "requires live Telegram network access"]
async fn direct_telegram_websocket_is_authenticated() {
    let websocket = RawWebSocket::connect(
        TELEGRAM_DC4_IP,
        TELEGRAM_DC4_DOMAIN,
        None,
        "/apiws",
        Duration::from_secs(10),
        tls_config(),
        256 * 1024,
        16 * 1024 * 1024,
        true,
        true,
    )
    .await
    .expect("direct Telegram WebSocket handshake must succeed");
    websocket.close().await;
}

#[tokio::test]
#[ignore = "requires live Telegram network access"]
async fn fronted_telegram_websocket_keeps_webpki_authentication() {
    let websocket = RawWebSocket::connect(
        TELEGRAM_DC4_IP,
        TELEGRAM_DC4_DOMAIN,
        Some("sprinthost.ru"),
        "/apiws",
        Duration::from_secs(10),
        tls_config(),
        256 * 1024,
        16 * 1024 * 1024,
        true,
        true,
    )
    .await
    .expect("fronted Telegram WebSocket handshake must succeed with authenticated TLS");
    websocket.close().await;
}

// The optional Python dependencies keep a complete MTProto/RSA client out of the proxy.
// Set TG_WS_PROXY_PROBE_PYTHON to a Python with telethon, pycryptodome, websockets>=15.
// These probes stop at req_DH: no account, messages or completed auth key.
fn probe_mtproto_dh(domain: &str, dc: i16, expected: &str) {
    let python = std::env::var_os("TG_WS_PROXY_PROBE_PYTHON").unwrap_or_else(|| "python3".into());
    let probe = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/mtproto_probe.py"
    );
    let output = Command::new(python)
        .arg(probe)
        .arg("--domain")
        .arg(domain)
        .arg(format!("--dc={dc}"))
        .arg(format!("--expect={expected}"))
        .output()
        .expect("MTProto probe Python must be executable");
    assert!(
        output.status.success(),
        "MTProto req_DH probe failed for {domain} dc={dc}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
#[ignore = "requires live Telegram and Python with telethon, pycryptodome, websockets>=15"]
fn ordinary_selected_routes_accept_mtproto_dh() {
    for dc in [2, 4] {
        for domain in websocket_domains(dc, false) {
            probe_mtproto_dh(&domain, dc, "ServerDHParamsOk");
        }
    }
}

#[test]
#[ignore = "requires live Telegram and Python with telethon, pycryptodome, websockets>=15"]
fn media_selected_routes_accept_mtproto_dh() {
    for dc in [2, 4] {
        for domain in websocket_domains(dc, true) {
            probe_mtproto_dh(&domain, -dc, "ServerDHParamsOk");
        }
    }
}

#[test]
#[ignore = "reproduces -444 using live Telegram and Python MTProto probe dependencies"]
fn media_route_rejects_ordinary_mtproto_dh() {
    for dc in [2, 4] {
        probe_mtproto_dh(&format!("kws{dc}-1.web.telegram.org"), dc, "-444");
    }
}
