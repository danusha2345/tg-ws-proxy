use std::time::Duration;

use tg_ws_proxy::config::tls_config;
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
    )
    .await
    .expect("fronted Telegram WebSocket handshake must succeed with authenticated TLS");
    websocket.close().await;
}
