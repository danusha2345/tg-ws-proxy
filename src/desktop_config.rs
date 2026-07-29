use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

const DEFAULT_DC_IPS: &[&str] = &["2:149.154.167.220", "4:149.154.167.220"];

/// Persisted configuration shared with the legacy Python tray applications.
///
/// Unknown fields are deliberately retained so that a newer or older desktop
/// frontend does not destroy settings it does not understand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
// These independent booleans are compatibility fields from the legacy JSON
// schema, not mutually exclusive states that could be represented by an enum.
#[allow(clippy::struct_excessive_bools)]
pub struct DesktopConfig {
    pub port: u16,
    pub host: String,
    pub dc_ip: Vec<String>,
    pub secret: String,
    pub verbose: bool,
    pub check_updates: bool,
    pub log_max_mb: f64,
    pub buf_kb: u64,
    pub pool_size: u64,
    pub cfproxy: bool,
    #[serde(deserialize_with = "deserialize_domains")]
    pub cfproxy_user_domain: Vec<String>,
    #[serde(deserialize_with = "deserialize_domains")]
    pub cfproxy_worker_domain: Vec<String>,
    pub force_test_dc: bool,
    pub ws_keepalive_interval: u64,
    pub language: String,
    pub appearance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autostart: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            port: 1443,
            host: "127.0.0.1".to_owned(),
            dc_ip: DEFAULT_DC_IPS.iter().map(ToString::to_string).collect(),
            secret: generate_secret_hex()
                .expect("operating system random-number generator must be available"),
            verbose: false,
            check_updates: true,
            log_max_mb: 5.0,
            buf_kb: 256,
            pool_size: 4,
            cfproxy: true,
            cfproxy_user_domain: Vec::new(),
            cfproxy_worker_domain: Vec::new(),
            force_test_dc: false,
            ws_keepalive_interval: 30,
            language: default_language(),
            appearance: "auto".to_owned(),
            autostart: cfg!(windows).then_some(false),
            extra: BTreeMap::new(),
        }
    }
}

impl DesktopConfig {
    /// Loads a config, creating and durably saving one when it does not exist.
    ///
    /// A legacy file without `secret` is upgraded immediately. This is what
    /// makes a generated secret stable across subsequent process starts.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let config = Self::default();
                config.save_atomic(path)?;
                return Ok(config);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read config {}", path.display()));
            }
        };

        let value: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse config {}", path.display()))?;
        let secret_was_missing = value
            .as_object()
            .is_some_and(|object| !object.contains_key("secret"));
        let config: Self = serde_json::from_value(value)
            .with_context(|| format!("invalid config schema in {}", path.display()))?;
        config.validate()?;
        #[cfg(unix)]
        restrict_config_permissions(path)?;

        if secret_was_missing {
            config.save_atomic(path)?;
        }
        Ok(config)
    }

    /// Validates values that JSON's numeric and string types cannot constrain.
    pub fn validate(&self) -> Result<()> {
        if self.port == 0 {
            bail!("port must be in the range 1..=65535");
        }
        if self.host.trim().is_empty() {
            bail!("host cannot be empty");
        }
        crate::config::parse_secret(&self.secret).context("invalid secret")?;
        if !self.log_max_mb.is_finite() || self.log_max_mb <= 0.0 {
            bail!("log_max_mb must be a finite positive number");
        }
        if self.buf_kb < 4 {
            bail!("buf_kb must be at least 4");
        }
        if self.buf_kb
            > u64::try_from(crate::config::MAX_SOCKET_BUFFER_SIZE / 1024)
                .expect("socket buffer limit fits u64")
        {
            bail!(
                "buf_kb cannot exceed {}",
                crate::config::MAX_SOCKET_BUFFER_SIZE / 1024
            );
        }
        if self.pool_size
            > u64::try_from(crate::config::MAX_POOL_SIZE).expect("pool size limit fits u64")
        {
            bail!("pool_size cannot exceed {}", crate::config::MAX_POOL_SIZE);
        }
        for entry in &self.dc_ip {
            crate::config::parse_dc_ip(entry)
                .with_context(|| format!("invalid dc_ip entry {entry:?}"))?;
        }
        for domain in self
            .cfproxy_user_domain
            .iter()
            .chain(self.cfproxy_worker_domain.iter())
        {
            crate::config::validate_domain(domain)
                .with_context(|| format!("invalid proxy domain {domain:?}"))?;
        }
        Ok(())
    }

    pub fn to_proxy_config(&self) -> Result<crate::config::ProxyConfig> {
        self.validate()?;
        let mut config = crate::config::ProxyConfig {
            host: self.host.clone(),
            port: self.port,
            secret: crate::config::parse_secret(&self.secret)?,
            buffer_size: usize::try_from(self.buf_kb)
                .context("buf_kb does not fit this platform")?
                .saturating_mul(1024),
            pool_size: usize::try_from(self.pool_size)
                .context("pool_size does not fit this platform")?,
            fallback_cfproxy: self.cfproxy,
            force_test_dc: self.force_test_dc,
            ..crate::config::ProxyConfig::default()
        };
        config.dc_redirects = self
            .dc_ip
            .iter()
            .map(|entry| crate::config::parse_dc_ip(entry))
            .collect::<Result<_>>()?;
        if !self.cfproxy_user_domain.is_empty() {
            config.cfproxy_domains = crate::config::normalize_domains(
                self.cfproxy_user_domain.iter().map(String::as_str),
            )?;
        }
        config.cfproxy_worker_domains = crate::config::normalize_domains(
            self.cfproxy_worker_domain.iter().map(String::as_str),
        )?;
        config.validate()?;
        Ok(config)
    }

    /// Writes a validated config through a same-directory temporary file.
    ///
    /// `NamedTempFile::persist` atomically replaces an existing destination on
    /// the supported desktop platforms. The temporary file is synchronized
    /// before replacement; on Unix, the containing directory is synchronized
    /// afterwards as well.
    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let mut encoded = serde_json::to_vec_pretty(self).context("failed to encode config")?;
        encoded.push(b'\n');

        let parent = normalized_parent(path);
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
        let prefix = format!(
            ".{}.",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("config")
        );
        let mut temporary = tempfile::Builder::new()
            .prefix(&prefix)
            .tempfile_in(parent)
            .with_context(|| {
                format!("failed to create temporary config in {}", parent.display())
            })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .context("failed to restrict temporary config permissions")?;
        }

        temporary
            .as_file_mut()
            .write_all(&encoded)
            .context("failed to write temporary config")?;
        temporary
            .as_file_mut()
            .flush()
            .context("failed to flush temporary config")?;
        temporary
            .as_file()
            .sync_all()
            .context("failed to synchronize temporary config")?;
        let persisted = temporary.persist(path).map_err(|error| error.error)?;
        persisted
            .sync_all()
            .with_context(|| format!("failed to synchronize config {}", path.display()))?;

        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| {
                format!(
                    "failed to synchronize config directory {}",
                    parent.display()
                )
            })?;

        Ok(())
    }
}

pub fn generate_secret_hex() -> Result<String> {
    let mut secret = [0_u8; 16];
    getrandom::fill(&mut secret)
        .map_err(|error| anyhow::anyhow!("failed to generate proxy secret: {error}"))?;
    Ok(hex::encode(secret))
}

fn normalized_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(unix)]
fn restrict_config_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict config permissions {}", path.display()))?;
    Ok(())
}

fn default_language() -> String {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if std::env::var(key)
            .ok()
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("ru"))
        {
            return "ru".to_owned();
        }
    }
    "en".to_owned()
}

fn deserialize_domains<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let entries = match value {
        Value::Null => return Ok(Vec::new()),
        Value::String(entry) => vec![entry],
        Value::Array(entries) => entries
            .into_iter()
            .map(|entry| {
                entry
                    .as_str()
                    .map(ToString::to_string)
                    .ok_or_else(|| D::Error::custom("domain array entries must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(D::Error::custom(
                "domains must be a string, an array of strings, or null",
            ));
        }
    };

    Ok(normalize_domains(entries))
}

fn normalize_domains(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut result = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        for item in value.replace([',', ';'], " ").split_whitespace() {
            let key = item.to_ascii_lowercase();
            if seen.insert(key.clone()) {
                result.push(key);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_both_legacy_domain_shapes_and_preserves_unknown_fields() {
        let input = r#"{
            "secret": "00112233445566778899aabbccddeeff",
            "cfproxy_user_domain": "One.Example, two.example;ONE.example",
            "cfproxy_worker_domain": ["worker.example", "next.example worker.example"],
            "future_option": {"enabled": true, "weight": 7}
        }"#;

        let config: DesktopConfig = serde_json::from_str(input).unwrap();
        assert_eq!(config.cfproxy_user_domain, ["one.example", "two.example"]);
        assert_eq!(
            config.cfproxy_worker_domain,
            ["worker.example", "next.example"]
        );
        assert_eq!(
            config.extra["future_option"],
            serde_json::json!({"enabled": true, "weight": 7})
        );

        let encoded = serde_json::to_value(&config).unwrap();
        assert_eq!(
            encoded["future_option"],
            serde_json::json!({"enabled": true, "weight": 7})
        );
        assert_eq!(
            encoded["cfproxy_user_domain"],
            serde_json::json!(["one.example", "two.example"])
        );
    }

    #[test]
    fn rejects_non_string_domain_array_entries() {
        let input = r#"{
            "secret": "00112233445566778899aabbccddeeff",
            "cfproxy_user_domain": ["valid.example", 42]
        }"#;
        assert!(serde_json::from_str::<DesktopConfig>(input).is_err());
    }

    #[test]
    fn generated_secret_is_saved_and_stable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");

        let first = DesktopConfig::load_or_create(&path).unwrap();
        let second = DesktopConfig::load_or_create(&path).unwrap();

        assert_eq!(first.secret, second.secret);
        assert_eq!(first.secret.len(), 32);
        assert!(hex::decode(first.secret).is_ok());
    }

    #[test]
    fn missing_legacy_secret_is_persisted_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, b"{\"future_option\":\"kept\"}").unwrap();

        let first = DesktopConfig::load_or_create(&path).unwrap();
        let second = DesktopConfig::load_or_create(&path).unwrap();

        assert_eq!(first.secret, second.secret);
        assert_eq!(second.extra["future_option"], "kept");
        let persisted: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted["secret"], first.secret);
    }

    #[test]
    fn invalid_float_does_not_replace_existing_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let config = DesktopConfig::default();
        config.save_atomic(&path).unwrap();
        let before = fs::read(&path).unwrap();

        let mut invalid = config;
        invalid.log_max_mb = f64::NAN;
        assert!(invalid.save_atomic(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn saved_config_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        DesktopConfig::default().save_atomic(&path).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn loading_legacy_config_restricts_its_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, br#"{"secret":"00112233445566778899aabbccddeeff"}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        DesktopConfig::load_or_create(&path).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn converts_legacy_config_to_proxy_config() {
        let config = DesktopConfig {
            secret: "00112233445566778899aabbccddeeff".to_owned(),
            dc_ip: vec!["4:149.154.167.220".to_owned()],
            cfproxy_user_domain: vec!["Proxy.Example".to_owned()],
            cfproxy_worker_domain: vec!["Worker.Example".to_owned()],
            ..DesktopConfig::default()
        };
        let proxy = config.to_proxy_config().unwrap();
        assert_eq!(
            proxy.secret,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
        assert_eq!(proxy.cfproxy_domains, ["proxy.example"]);
        assert_eq!(proxy.cfproxy_worker_domains, ["worker.example"]);
        assert_eq!(proxy.dc_redirects.len(), 1);
    }

    #[test]
    fn rejects_resource_exhausting_legacy_values() {
        let mut config = DesktopConfig::default();
        config.pool_size =
            u64::try_from(crate::config::MAX_POOL_SIZE + 1).expect("test limit fits u64");
        assert!(config.validate().is_err());

        config = DesktopConfig::default();
        config.buf_kb = u64::try_from(crate::config::MAX_SOCKET_BUFFER_SIZE / 1024 + 1)
            .expect("test limit fits u64");
        assert!(config.validate().is_err());
    }
}
