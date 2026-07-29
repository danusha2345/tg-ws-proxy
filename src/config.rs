use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, RootCertStore};
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};

pub const DEFAULT_DC_IPS: &[(i16, Ipv4Addr)] = &[
    (1, Ipv4Addr::new(149, 154, 175, 50)),
    (2, Ipv4Addr::new(149, 154, 167, 51)),
    (3, Ipv4Addr::new(149, 154, 175, 100)),
    (4, Ipv4Addr::new(149, 154, 167, 91)),
    (5, Ipv4Addr::new(149, 154, 171, 5)),
    (203, Ipv4Addr::new(91, 105, 192, 100)),
];

pub const TEST_DC_IPS: &[(i16, Ipv4Addr)] = &[
    (1, Ipv4Addr::new(149, 154, 175, 10)),
    (2, Ipv4Addr::new(149, 154, 167, 40)),
    (3, Ipv4Addr::new(149, 154, 175, 117)),
];

pub const MAX_SOCKET_BUFFER_SIZE: usize = 16 * 1024 * 1024;
pub const MAX_POOL_SIZE: usize = 128;
pub const MAX_WS_FRAME_SIZE: usize = 64 * 1024 * 1024;
pub const MAX_CONNECTIONS: usize = 65_536;

pub const DEFAULT_CFPROXY_DOMAINS: &[&str] = &[
    "pclead.co.uk",
    "offshor.co.uk",
    "cakeisalie.co.uk",
    "noskomnadzor.co.uk",
    "lovetrue.co.uk",
    "sorokdva.co.uk",
    "pyatdesyatdva.co.uk",
    "kartoshka.co.uk",
    "sorokodin.co.uk",
    "pyatdesyatodin.co.uk",
    "notelega.co.uk",
    "ebally.co.uk",
    "nebally.co.uk",
    "havegreatday.co.uk",
    "pomogite.co.uk",
    "fixtelega.co.uk",
    "sadnews.co.uk",
    "onedaychamp.co.uk",
    "stopblocking.co.uk",
    "nothingthere.co.uk",
];

const TELEGRAM_FRONTING_CERTIFICATE_NAME: &str = "telegram.org";

#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    pub secret: [u8; 16],
    pub dc_redirects: HashMap<i16, IpAddr>,
    pub buffer_size: usize,
    pub pool_size: usize,
    pub fallback_cfproxy: bool,
    pub cfproxy_domains: Vec<String>,
    pub cfproxy_worker_domains: Vec<String>,
    pub fake_tls_domain: Option<String>,
    pub masking_upstream: Option<String>,
    pub proxy_protocol: bool,
    pub force_test_dc: bool,
    pub max_ws_frame_size: usize,
    pub max_connections: usize,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        let mut secret = [0_u8; 16];
        getrandom::fill(&mut secret).expect("operating system RNG must be available");

        Self {
            host: "127.0.0.1".to_owned(),
            port: 1443,
            secret,
            dc_redirects: HashMap::from([
                (2, IpAddr::V4(Ipv4Addr::new(149, 154, 167, 220))),
                (4, IpAddr::V4(Ipv4Addr::new(149, 154, 167, 220))),
            ]),
            buffer_size: 256 * 1024,
            pool_size: 4,
            fallback_cfproxy: true,
            cfproxy_domains: DEFAULT_CFPROXY_DOMAINS
                .iter()
                .map(ToString::to_string)
                .collect(),
            cfproxy_worker_domains: Vec::new(),
            fake_tls_domain: None,
            masking_upstream: None,
            proxy_protocol: false,
            force_test_dc: false,
            max_ws_frame_size: 16 * 1024 * 1024,
            max_connections: 1024,
        }
    }
}

impl ProxyConfig {
    #[must_use]
    pub fn secret_hex(&self) -> String {
        hex::encode(self.secret)
    }

    #[must_use]
    pub fn connect_secret(&self) -> String {
        if let Some(domain) = &self.fake_tls_domain {
            format!("ee{}{}", self.secret_hex(), hex::encode(domain.as_bytes()))
        } else {
            format!("dd{}", self.secret_hex())
        }
    }

    #[must_use]
    pub fn telegram_url(&self, advertised_host: &str) -> String {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("server", advertised_host)
            .append_pair("port", &self.port.to_string())
            .append_pair("secret", &self.connect_secret())
            .finish();
        format!("tg://proxy?{query}")
    }

    #[must_use]
    pub fn fallback_ip(&self, dc: i16, test: bool) -> Option<IpAddr> {
        let table = if test { TEST_DC_IPS } else { DEFAULT_DC_IPS };
        table
            .iter()
            .find_map(|(candidate, ip)| (*candidate == dc).then_some(IpAddr::V4(*ip)))
    }

    pub fn validate(&self) -> Result<()> {
        if self.host.trim().is_empty() {
            bail!("listen host cannot be empty");
        }
        if self.port == 0 {
            bail!("listen port must be in the range 1..=65535");
        }
        if self.buffer_size < 4 * 1024 {
            bail!("socket buffer must be at least 4 KiB");
        }
        if self.buffer_size > MAX_SOCKET_BUFFER_SIZE {
            bail!(
                "socket buffer cannot exceed {} MiB",
                MAX_SOCKET_BUFFER_SIZE / 1024 / 1024
            );
        }
        if self.pool_size > MAX_POOL_SIZE {
            bail!("WebSocket pool size cannot exceed {MAX_POOL_SIZE}");
        }
        if self.max_ws_frame_size < 64 {
            bail!("maximum WebSocket frame size must be at least 64 bytes");
        }
        if self.max_ws_frame_size > MAX_WS_FRAME_SIZE {
            bail!(
                "maximum WebSocket frame size cannot exceed {} MiB",
                MAX_WS_FRAME_SIZE / 1024 / 1024
            );
        }
        if self.max_connections == 0 {
            bail!("maximum connection count must be positive");
        }
        if self.max_connections > MAX_CONNECTIONS {
            bail!("maximum connection count cannot exceed {MAX_CONNECTIONS}");
        }
        if let Some(domain) = &self.fake_tls_domain {
            validate_domain(domain).context("invalid Fake TLS domain")?;
        }
        if let Some(domain) = &self.masking_upstream {
            validate_domain(domain).context("invalid masking upstream")?;
        }
        if self.masking_upstream.is_some() && self.fake_tls_domain.is_none() {
            bail!("masking upstream requires a Fake TLS domain");
        }
        if self.fake_tls_domain == self.masking_upstream && self.fake_tls_domain.is_some() {
            bail!(
                "Fake TLS domain and masking upstream must differ to prevent a recursive self-loop"
            );
        }
        for domain in self
            .cfproxy_domains
            .iter()
            .chain(self.cfproxy_worker_domains.iter())
        {
            validate_domain(domain).with_context(|| format!("invalid domain {domain:?}"))?;
        }
        Ok(())
    }
}

pub fn parse_secret(value: &str) -> Result<[u8; 16]> {
    let raw = hex::decode(value.trim()).context("secret must be valid hexadecimal")?;
    raw.try_into()
        .map_err(|_| anyhow::anyhow!("secret must contain exactly 32 hexadecimal characters"))
}

pub fn load_or_create_secret(path: &Path, generated: [u8; 16]) -> Result<[u8; 16]> {
    match fs::read_to_string(path) {
        Ok(value) => {
            return parse_secret(&value).with_context(|| {
                format!(
                    "secret file {} does not contain a valid secret",
                    path.display()
                )
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read secret file {}", path.display()));
        }
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create secret directory {}", parent.display())
            })?;
        }
    }

    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let prefix = format!(
        ".{}.",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("secret")
    );
    let mut temporary = tempfile::Builder::new()
        .prefix(&prefix)
        .tempfile_in(parent)
        .with_context(|| {
            format!(
                "failed to create temporary secret file in {}",
                parent.display()
            )
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .context("failed to restrict temporary secret permissions")?;
    }
    let payload = format!("{}\n", hex::encode(generated));
    temporary
        .as_file_mut()
        .write_all(payload.as_bytes())
        .context("failed to write temporary secret file")?;
    temporary
        .as_file()
        .sync_all()
        .context("failed to synchronize temporary secret file")?;

    match temporary.persist_noclobber(path) {
        Ok(file) => {
            file.sync_all()
                .with_context(|| format!("failed to synchronize secret file {}", path.display()))?;
            #[cfg(unix)]
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .with_context(|| {
                    format!(
                        "failed to synchronize secret directory {}",
                        parent.display()
                    )
                })?;
            Ok(generated)
        }
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            let value = fs::read_to_string(path).with_context(|| {
                format!(
                    "failed to read concurrently created secret file {}",
                    path.display()
                )
            })?;
            parse_secret(&value).with_context(|| {
                format!(
                    "secret file {} does not contain a valid secret",
                    path.display()
                )
            })
        }
        Err(error) => Err(error.error)
            .with_context(|| format!("failed to atomically create secret file {}", path.display())),
    }
}

pub fn parse_dc_ip(value: &str) -> Result<(i16, IpAddr)> {
    let (dc, ip) = value
        .split_once(':')
        .with_context(|| format!("invalid --dc-ip {value:?}; expected DC:IP"))?;
    let dc = dc
        .parse::<i16>()
        .with_context(|| format!("invalid DC number in {value:?}"))?;
    if dc <= 0 {
        bail!("DC number must be positive in {value:?}");
    }
    let ip = IpAddr::from_str(ip).with_context(|| format!("invalid IP address in {value:?}"))?;
    Ok((dc, ip))
}

pub fn normalize_domains<'a>(values: impl IntoIterator<Item = &'a str>) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for value in values {
        for item in value.replace([',', ';'], " ").split_whitespace() {
            let normalized = item.to_ascii_lowercase();
            validate_domain(&normalized)?;
            if !result.contains(&normalized) {
                result.push(normalized);
            }
        }
    }
    Ok(result)
}

pub fn validate_domain(domain: &str) -> Result<()> {
    if domain.is_empty() || domain.len() > 253 || domain.starts_with('.') || domain.ends_with('.') {
        bail!("invalid domain {domain:?}");
    }
    let labels: Vec<_> = domain.split('.').collect();
    if labels.len() < 2 {
        bail!("domain must contain a dot: {domain:?}");
    }
    for label in &labels {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            bail!("invalid domain label in {domain:?}");
        }
    }
    let tld = labels.last().expect("labels are non-empty");
    if tld.len() < 2 || !tld.chars().any(|ch| ch.is_ascii_alphabetic()) {
        bail!("invalid top-level domain in {domain:?}");
    }
    Ok(())
}

#[must_use]
pub fn tls_config() -> Arc<ClientConfig> {
    install_crypto_provider();
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(root_store())
            .with_no_client_auth(),
    )
}

pub(crate) fn fronting_tls_config() -> Result<Arc<ClientConfig>> {
    install_crypto_provider();
    let verify_name = ServerName::try_from(TELEGRAM_FRONTING_CERTIFICATE_NAME.to_owned())
        .expect("static Telegram certificate identity must be a valid DNS name");
    let verifier = WebPkiServerVerifier::builder(Arc::new(root_store()))
        .build()
        .context("failed to build WebPKI verifier")?;
    let verifier = Arc::new(FrontingServerVerifier {
        verifier,
        verify_name,
    });
    Ok(Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth(),
    ))
}

fn install_crypto_provider() {
    static PROVIDER: OnceLock<()> = OnceLock::new();
    PROVIDER.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn root_store() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

/// Keeps Telegram fronting authenticated: SNI controls routing, while the
/// certificate is checked against Telegram's fixed identity and normal `WebPKI`
/// roots. Telegram's fronting response currently carries `*.telegram.org`,
/// which cannot authenticate nested HTTP hosts such as
/// `kws4.web.telegram.org`.
#[derive(Debug)]
struct FrontingServerVerifier {
    verifier: Arc<WebPkiServerVerifier>,
    verify_name: ServerName<'static>,
}

impl ServerCertVerifier for FrontingServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        self.verifier.verify_server_cert(
            end_entity,
            intermediates,
            &self.verify_name,
            ocsp_response,
            now,
        )
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.verifier.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.verifier.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.verifier.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dc_mapping() {
        assert_eq!(
            parse_dc_ip("4:149.154.167.220").unwrap(),
            (4, IpAddr::V4(Ipv4Addr::new(149, 154, 167, 220)))
        );
        assert!(parse_dc_ip("4:not-an-ip").is_err());
        assert!(parse_dc_ip("0:127.0.0.1").is_err());
    }

    #[test]
    fn normalizes_and_deduplicates_domains() {
        assert_eq!(
            normalize_domains(["Example.COM,example.com", "two.example"]).unwrap(),
            ["example.com", "two.example"]
        );
    }

    #[test]
    fn secret_file_is_created_once_and_reused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("secret");
        let first = [0x11; 16];
        let second = [0x22; 16];
        assert_eq!(load_or_create_secret(&path, first).unwrap(), first);
        assert_eq!(load_or_create_secret(&path, second).unwrap(), first);
        assert_eq!(
            fs::read_to_string(&path).unwrap().trim(),
            hex::encode(first)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn rejects_recursive_or_unused_masking_upstream() {
        let mut config = ProxyConfig {
            masking_upstream: Some("origin.example".to_owned()),
            ..ProxyConfig::default()
        };
        assert!(config.validate().is_err());

        config.fake_tls_domain = Some("origin.example".to_owned());
        assert!(config.validate().is_err());

        config.masking_upstream = Some("cover.example".to_owned());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn telegram_url_escapes_query_metacharacters() {
        let config = ProxyConfig {
            secret: [0x11; 16],
            ..ProxyConfig::default()
        };
        let url = config.telegram_url("host.example&port=1");
        assert!(url.contains("server=host.example%26port%3D1"));
        assert!(url.ends_with("secret=dd11111111111111111111111111111111"));
    }

    #[test]
    fn rejects_resource_exhausting_numeric_limits() {
        let mut config = ProxyConfig {
            buffer_size: MAX_SOCKET_BUFFER_SIZE,
            pool_size: MAX_POOL_SIZE,
            max_ws_frame_size: MAX_WS_FRAME_SIZE,
            max_connections: MAX_CONNECTIONS,
            ..ProxyConfig::default()
        };
        assert!(config.validate().is_ok());

        let mut invalid = config.clone();
        invalid.buffer_size = MAX_SOCKET_BUFFER_SIZE + 1;
        assert!(invalid.validate().is_err());

        let mut invalid = config.clone();
        invalid.pool_size = MAX_POOL_SIZE + 1;
        assert!(invalid.validate().is_err());

        let mut invalid = config.clone();
        invalid.max_ws_frame_size = MAX_WS_FRAME_SIZE + 1;
        assert!(invalid.validate().is_err());

        config.max_connections = MAX_CONNECTIONS + 1;
        assert!(config.validate().is_err());
    }
}
