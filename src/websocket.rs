use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use socket2::SockRef;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tokio_tungstenite::{WebSocketStream, client_async_with_config};

use crate::config::fronting_tls_config;

type WsStream = WebSocketStream<TlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

const TELEGRAM_FRONTING_SNI: &str = "sprinthost.ru";

fn build_request(
    domain: &str,
    path: &str,
    request_binary_subprotocol: bool,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, TungsteniteError> {
    let uri = format!("wss://{domain}{path}");
    let mut request = uri.into_client_request()?;
    if request_binary_subprotocol {
        request
            .headers_mut()
            .insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("binary"));
    }
    Ok(request)
}

#[derive(Debug, Error)]
pub enum WebSocketError {
    #[error("WebSocket handshake returned HTTP {status}")]
    Handshake {
        status: StatusCode,
        location: Option<String>,
    },
    #[error("WebSocket connection is closed")]
    Closed,
    #[error("WebSocket peer sent text data for the binary subprotocol")]
    UnexpectedText,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Protocol(#[from] TungsteniteError),
    #[error("operation timed out")]
    Timeout,
}

impl WebSocketError {
    #[must_use]
    pub fn is_redirect(&self) -> bool {
        matches!(
            self,
            Self::Handshake {
                status: StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT,
                ..
            }
        )
    }

    #[must_use]
    pub fn location(&self) -> Option<&str> {
        match self {
            Self::Handshake { location, .. } => location.as_deref(),
            _ => None,
        }
    }

    /// Retry Telegram fronting only for transient transport failures, never TLS
    /// authentication failures or HTTP redirects.
    #[must_use]
    pub fn permits_fronting_retry(&self) -> bool {
        match self {
            Self::Timeout => true,
            Self::Io(error) | Self::Protocol(TungsteniteError::Io(error)) => {
                error.kind() == io::ErrorKind::ConnectionReset
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
    }
}

#[derive(Clone)]
pub struct WsSender {
    sink: Arc<Mutex<WsSink>>,
    closed: Arc<AtomicBool>,
    close_started: Arc<AtomicBool>,
}

impl WsSender {
    pub async fn send_binary(&self, data: &[u8]) -> Result<(), WebSocketError> {
        if self.is_closed() {
            return Err(WebSocketError::Closed);
        }
        self.sink
            .lock()
            .await
            .send(Message::Binary(data.to_vec().into()))
            .await
            .map_err(WebSocketError::from)
    }

    pub async fn send_batch<'a>(
        &self,
        parts: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<(), WebSocketError> {
        if self.is_closed() {
            return Err(WebSocketError::Closed);
        }
        let mut sink = self.sink.lock().await;
        for part in parts {
            sink.feed(Message::Binary(part.to_vec().into())).await?;
        }
        sink.flush().await?;
        Ok(())
    }

    pub async fn close(&self) {
        if self.close_started.swap(true, Ordering::AcqRel) {
            return;
        }
        self.closed.store(true, Ordering::Release);
        let mut sink = self.sink.lock().await;
        let _ = sink.send(Message::Close(None)).await;
        let _ = sink.close().await;
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

pub struct WsReceiver {
    source: WsSource,
    closed: Arc<AtomicBool>,
}

impl WsReceiver {
    pub async fn receive(&mut self) -> Result<Option<Vec<u8>>, WebSocketError> {
        loop {
            let Some(message) = self.source.next().await else {
                self.closed.store(true, Ordering::Release);
                return Ok(None);
            };
            match message? {
                Message::Binary(data) => return Ok(Some(data.to_vec())),
                Message::Text(_) => return Err(WebSocketError::UnexpectedText),
                Message::Close(_) => {
                    self.closed.store(true, Ordering::Release);
                    return Ok(None);
                }
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
    }
}

pub struct RawWebSocket {
    pub receiver: WsReceiver,
    pub sender: WsSender,
}

impl RawWebSocket {
    #[allow(clippy::too_many_arguments)]
    pub async fn connect(
        host: &str,
        domain: &str,
        sni: Option<&str>,
        path: &str,
        operation_timeout: Duration,
        tls_config: Arc<ClientConfig>,
        buffer_size: usize,
        max_frame_size: usize,
        request_binary_subprotocol: bool,
    ) -> Result<Self, WebSocketError> {
        timeout(operation_timeout, async {
            let stream = TcpStream::connect((host, 443)).await?;
            stream.set_nodelay(true)?;
            let socket = SockRef::from(&stream);
            let _ = socket.set_recv_buffer_size(buffer_size);
            let _ = socket.set_send_buffer_size(buffer_size);

            let tls_server_name = sni.unwrap_or(domain);
            let server_name = ServerName::try_from(tls_server_name.to_owned())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS SNI"))?;
            let tls_config = if tls_server_name == domain {
                tls_config
            } else {
                if !is_telegram_fronting_route(domain, tls_server_name) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "TLS SNI override is restricted to Telegram fronting",
                    )
                    .into());
                }
                fronting_tls_config().map_err(io::Error::other)?
            };
            let tls = TlsConnector::from(tls_config)
                .connect(server_name, stream)
                .await?;

            let request = build_request(domain, path, request_binary_subprotocol)?;
            let ws_config = WebSocketConfig::default()
                .max_frame_size(Some(max_frame_size))
                .max_message_size(Some(max_frame_size));

            let websocket = match client_async_with_config(request, tls, Some(ws_config)).await {
                Ok((websocket, _response)) => websocket,
                Err(TungsteniteError::Http(response)) => {
                    let location = response
                        .headers()
                        .get("location")
                        .and_then(|value| value.to_str().ok())
                        .map(ToOwned::to_owned);
                    return Err(WebSocketError::Handshake {
                        status: response.status(),
                        location,
                    });
                }
                Err(error) => return Err(WebSocketError::Protocol(error)),
            };
            let (sink, source) = websocket.split();
            let closed = Arc::new(AtomicBool::new(false));
            let sender = WsSender {
                sink: Arc::new(Mutex::new(sink)),
                closed: Arc::clone(&closed),
                close_started: Arc::new(AtomicBool::new(false)),
            };
            Ok(Self {
                receiver: WsReceiver { source, closed },
                sender,
            })
        })
        .await
        .map_err(|_| WebSocketError::Timeout)?
    }

    pub async fn close(&self) {
        self.sender.close().await;
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
}

fn is_telegram_fronting_route(domain: &str, sni: &str) -> bool {
    if sni != TELEGRAM_FRONTING_SNI {
        return false;
    }
    let Some(label) = domain.strip_suffix(".web.telegram.org") else {
        return false;
    };
    let Some(dc) = label.strip_prefix("kws") else {
        return false;
    };
    let dc = dc.strip_suffix("-1").unwrap_or(dc);
    matches!(dc.parse::<i16>(), Ok(1..=5 | 203))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fronting_retries_resets_but_not_authentication_or_redirect_errors() {
        assert!(WebSocketError::Timeout.permits_fronting_retry());
        assert!(WebSocketError::Io(io::ErrorKind::ConnectionReset.into()).permits_fronting_retry());
        assert!(
            WebSocketError::Protocol(TungsteniteError::Io(io::ErrorKind::ConnectionReset.into()))
                .permits_fronting_retry()
        );
        assert!(!WebSocketError::Io(io::ErrorKind::InvalidData.into()).permits_fronting_retry());
        assert!(
            !WebSocketError::Handshake {
                status: StatusCode::FOUND,
                location: None
            }
            .permits_fronting_retry()
        );
    }

    #[test]
    fn worker_compatibility_can_omit_binary_subprotocol() {
        let direct = build_request("kws4.web.telegram.org", "/apiws", true).unwrap();
        assert_eq!(
            direct.headers().get(SEC_WEBSOCKET_PROTOCOL).unwrap(),
            "binary"
        );

        let worker = build_request("example.workers.dev", "/apiws?dst=127.0.0.1", false).unwrap();
        assert!(worker.headers().get(SEC_WEBSOCKET_PROTOCOL).is_none());
    }

    #[test]
    fn sni_override_is_limited_to_known_telegram_websocket_hosts() {
        assert!(is_telegram_fronting_route(
            "kws4.web.telegram.org",
            TELEGRAM_FRONTING_SNI
        ));
        assert!(is_telegram_fronting_route(
            "kws4-1.web.telegram.org",
            TELEGRAM_FRONTING_SNI
        ));
        assert!(!is_telegram_fronting_route(
            "attacker.example",
            TELEGRAM_FRONTING_SNI
        ));
        assert!(!is_telegram_fronting_route(
            "kws4.web.telegram.org",
            "attacker.example"
        ));
    }
}
