use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

pub struct SingleInstance {
    file: File,
    path: PathBuf,
}

impl SingleInstance {
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "failed to create single-instance directory {}",
                        parent.display()
                    )
                })?;
            }
        }

        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&path)
            .with_context(|| format!("failed to open instance lock {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                bail!("another tg-ws-proxy desktop instance is already running");
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to lock instance file {}", path.display()));
            }
        }

        file.set_len(0)
            .context("failed to truncate instance lock")?;
        file.seek(SeekFrom::Start(0))
            .context("failed to seek instance lock")?;
        writeln!(file, "{}", std::process::id()).context("failed to write instance PID")?;
        file.sync_data()
            .context("failed to synchronize instance lock")?;
        Ok(Self { file, path })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_atomic_and_released_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop.lock");
        let first = SingleInstance::acquire(&path).unwrap();
        assert_eq!(first.path(), path);
        assert!(SingleInstance::acquire(&path).is_err());
        drop(first);
        SingleInstance::acquire(path).unwrap();
    }
}
