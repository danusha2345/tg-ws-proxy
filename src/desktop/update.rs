use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

const RELEASES_API: &str =
    "https://api.github.com/repos/danusha2345/tg-ws-proxy/releases?per_page=100";
const TAG_PREFIX: &str = "rust-v";
const MAX_CHECKSUM_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UpdateState {
    Idle,
    Checking,
    Current,
    Available { version: String },
    Downloading { version: String },
    Ready { version: String },
    Failed,
}

#[derive(Clone, Debug)]
pub(super) struct ReleaseInfo {
    pub version: Version,
    assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

pub(super) async fn find_update() -> Result<Option<ReleaseInfo>> {
    let releases = client()
        .get(RELEASES_API)
        .send()
        .await
        .context("не удалось запросить список GitHub Releases")?
        .error_for_status()
        .context("GitHub вернул ошибку при проверке обновлений")?
        .json::<Vec<GithubRelease>>()
        .await
        .context("GitHub вернул некорректный список релизов")?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("текущая версия приложения некорректна")?;
    let latest = releases
        .into_iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            let version = Version::parse(release.tag_name.strip_prefix(TAG_PREFIX)?).ok()?;
            Some(ReleaseInfo {
                version,
                assets: release.assets,
            })
        })
        .max_by(|left, right| left.version.cmp(&right.version));
    Ok(latest.filter(|release| release.version > current))
}

pub(super) async fn download_update(release: &ReleaseInfo, destination: &Path) -> Result<PathBuf> {
    fs::create_dir_all(destination).with_context(|| {
        format!(
            "не удалось создать каталог обновлений {}",
            destination.display()
        )
    })?;
    let checksum_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == "SHA256SUMS.txt")
        .context("в релизе отсутствует SHA256SUMS.txt")?;
    let selected = select_asset(&release.assets)?;
    let checksums = download_small_text(&checksum_asset.browser_download_url).await?;
    let expected = checksum_for(&checksums, &selected.name)?;
    let target = destination.join(&selected.name);

    let response = client()
        .get(&selected.browser_download_url)
        .send()
        .await
        .context("не удалось скачать обновление")?
        .error_for_status()
        .context("GitHub вернул ошибку при скачивании обновления")?;
    let mut temporary = NamedTempFile::new_in(destination)
        .context("не удалось создать временный файл обновления")?;
    let mut hasher = Sha256::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("соединение оборвалось при скачивании обновления")?;
        hasher.update(&chunk);
        temporary
            .write_all(&chunk)
            .context("не удалось сохранить обновление")?;
    }
    temporary
        .as_file_mut()
        .sync_all()
        .context("не удалось синхронизировать файл обновления")?;
    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("контрольная сумма обновления не совпала");
    }
    temporary
        .persist(&target)
        .map_err(|error| error.error)
        .with_context(|| format!("не удалось сохранить обновление {}", target.display()))?;
    Ok(target)
}

async fn download_small_text(url: &str) -> Result<String> {
    let response = client()
        .get(url)
        .send()
        .await
        .context("не удалось скачать контрольные суммы")?
        .error_for_status()
        .context("GitHub вернул ошибку при скачивании контрольных сумм")?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CHECKSUM_BYTES as u64)
    {
        bail!("файл контрольных сумм неожиданно велик");
    }
    let bytes = response
        .bytes()
        .await
        .context("не удалось прочитать контрольные суммы")?;
    if bytes.len() > MAX_CHECKSUM_BYTES {
        bail!("файл контрольных сумм неожиданно велик");
    }
    String::from_utf8(bytes.to_vec()).context("контрольные суммы не являются UTF-8")
}

fn checksum_for<'a>(contents: &'a str, asset_name: &str) -> Result<&'a str> {
    contents
        .lines()
        .find_map(|line| {
            let (checksum, name) = line.split_once(char::is_whitespace)?;
            let name = name.trim_start_matches([' ', '*']);
            (name == asset_name && checksum.len() == 64).then_some(checksum)
        })
        .ok_or_else(|| anyhow!("для {asset_name} отсутствует контрольная сумма"))
}

fn select_asset(assets: &[ReleaseAsset]) -> Result<&ReleaseAsset> {
    let names = platform_asset_names();
    names
        .iter()
        .find_map(|name| assets.iter().find(|asset| asset.name == *name))
        .ok_or_else(|| anyhow!("в релизе нет сборки для этой ОС и архитектуры"))
}

fn platform_asset_names() -> Vec<&'static str> {
    #[cfg(all(windows, target_arch = "x86_64"))]
    return vec!["TgWsProxy_windows_x64.exe"];
    #[cfg(all(windows, target_arch = "aarch64"))]
    return vec!["TgWsProxy_windows_arm64.exe"];
    #[cfg(target_os = "macos")]
    return vec!["TgWsProxy_macos_universal.dmg"];
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return linux_asset_names("amd64");
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return linux_asset_names("arm64");
    #[allow(unreachable_code)]
    Vec::new()
}

#[cfg(target_os = "linux")]
fn linux_asset_names(arch: &str) -> Vec<&'static str> {
    let rpm = fs::read_to_string("/etc/os-release").is_ok_and(|value| {
        value.contains("ID=fedora") || value.contains("ID_LIKE=\"rhel fedora\"")
    });
    match (rpm, arch) {
        (true, "amd64") => vec!["TgWsProxy_linux_amd64.rpm", "TgWsProxy_linux_amd64.deb"],
        (true, "arm64") => vec!["TgWsProxy_linux_arm64.rpm", "TgWsProxy_linux_arm64.deb"],
        (false, "amd64") => vec!["TgWsProxy_linux_amd64.deb", "TgWsProxy_linux_amd64.rpm"],
        (false, "arm64") => vec!["TgWsProxy_linux_arm64.deb", "TgWsProxy_linux_arm64.rpm"],
        _ => Vec::new(),
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("tg-ws-proxy/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("static GitHub HTTP client settings are valid")
}

pub(super) fn launch(path: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        use std::process::Command;

        let path = path
            .canonicalize()
            .with_context(|| format!("не удалось найти обновление {}", path.display()))?;
        let escaped = path.to_string_lossy().replace('\'', "''");
        let script = format!(
            "$process = Get-Process -Id {} -ErrorAction SilentlyContinue; \
             if ($process) {{ $process.WaitForExit() }}; \
             Start-Process -FilePath '{escaped}'",
            std::process::id()
        );
        Command::new("powershell.exe")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .spawn()
            .context("не удалось запустить установщик обновления")?;
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        open::that(path).context("не удалось открыть установщик обновления")?;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_only_exact_checksum_filename() {
        let sums = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other.exe\n\
                    bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb *wanted.exe\n";
        assert_eq!(checksum_for(sums, "wanted.exe").unwrap(), "b".repeat(64));
        assert!(checksum_for(sums, "want.exe").is_err());
    }

    #[test]
    fn release_versions_use_rust_tag_namespace() {
        let version = Version::parse("1.9.1").unwrap();
        assert!(version > Version::parse("1.9.0-alpha.2").unwrap());
        assert!("android-v0.1.0".strip_prefix(TAG_PREFIX).is_none());
    }
}
