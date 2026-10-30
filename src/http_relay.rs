//! Короткие HTTP-запросы с сохранением Telegram TCP-сессии в Worker.
use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use reqwest::{Client, Method, StatusCode};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use crate::config::tls_config;

const CHUNK_BYTES: usize = 8192;
const PLAIN_CACHE: Duration = Duration::from_secs(60);

pub struct HttpRelayConnector {
    client: Client,
    plain_until: Mutex<HashMap<String, Instant>>,
}

struct Reply {
    status: StatusCode,
    protocol: bool,
    seq: Option<u64>,
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
        let task = tokio::spawn(async move {
            let result = relay(&client, &base, &sid, remote).await;
            if let Err(error) = result {
                tracing::debug!(%error, "HTTP relay session closed");
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
            let mut body = Vec::new();
            while let Some(chunk) = timeout(Duration::from_secs(3), response.chunk()).await?? {
                if body.len() + chunk.len() > CHUNK_BYTES {
                    bail!("HTTP relay response exceeded chunk limit");
                }
                body.extend_from_slice(&chunk);
            }
            Ok::<_, anyhow::Error>(Reply {
                status,
                protocol,
                seq,
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

async fn relay(client: &Client, base: &str, sid: &str, stream: DuplexStream) -> Result<()> {
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
        let relay_task =
            tokio::spawn(async move { relay(&client, &base, "test-session", remote).await });
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
