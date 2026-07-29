use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::BaseDirs;

const APP_DIRECTORY: &str = "TgWsProxy";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub directory: PathBuf,
    pub config: PathBuf,
    pub log: PathBuf,
    pub lock: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let base =
            BaseDirs::new().context("failed to discover the user configuration directory")?;
        Ok(Self::from_config_root(base.config_dir()))
    }

    pub fn ensure_directory(&self) -> Result<()> {
        fs::create_dir_all(&self.directory).with_context(|| {
            format!(
                "failed to create desktop data directory {}",
                self.directory.display()
            )
        })
    }

    fn from_config_root(root: &Path) -> Self {
        let directory = root.join(APP_DIRECTORY);
        Self {
            config: directory.join("config.json"),
            log: directory.join("proxy.log"),
            lock: directory.join("desktop.lock"),
            directory,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_legacy_names_below_platform_config_root() {
        let paths = AppPaths::from_config_root(Path::new("/config-root"));
        assert_eq!(paths.directory, Path::new("/config-root/TgWsProxy"));
        assert_eq!(paths.config, paths.directory.join("config.json"));
        assert_eq!(paths.log, paths.directory.join("proxy.log"));
        assert_eq!(paths.lock, paths.directory.join("desktop.lock"));
    }

    #[test]
    fn discovered_directory_uses_legacy_application_component() {
        let paths = AppPaths::discover().unwrap();
        assert_eq!(
            paths.directory.file_name().and_then(|name| name.to_str()),
            Some(APP_DIRECTORY)
        );
    }
}
