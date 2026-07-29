use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use tracing_subscriber::fmt::MakeWriter;

const MIN_LOG_SIZE: u64 = 32 * 1024;
const MAX_LOG_BACKUPS: usize = 100;

#[derive(Clone)]
pub struct RotatingMakeWriter {
    state: Arc<Mutex<RotationState>>,
}

struct RotationState {
    path: PathBuf,
    max_bytes: u64,
    backups: usize,
    file: Option<File>,
    size: u64,
}

impl RotatingMakeWriter {
    pub fn new(path: impl Into<PathBuf>, max_bytes: u64, backups: usize) -> Result<Self> {
        if backups > MAX_LOG_BACKUPS {
            anyhow::bail!("log backup count cannot exceed {MAX_LOG_BACKUPS}");
        }
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create log directory {}", parent.display())
                })?;
            }
        }
        let file = open_log(&path)?;
        let size = file.metadata().map_or(0, |metadata| metadata.len());
        Ok(Self {
            state: Arc::new(Mutex::new(RotationState {
                path,
                max_bytes: max_bytes.max(MIN_LOG_SIZE),
                backups: backups.max(1),
                file: Some(file),
                size,
            })),
        })
    }
}

impl<'a> MakeWriter<'a> for RotatingMakeWriter {
    type Writer = RotatingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RotatingWriter {
            state: Arc::clone(&self.state),
        }
    }
}

pub struct RotatingWriter {
    state: Arc<Mutex<RotationState>>,
}

impl Write for RotatingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut state = lock_state(&self.state)?;
        let incoming = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        if state.size > 0 && state.size.saturating_add(incoming) > state.max_bytes {
            rotate(&mut state)?;
        }
        let written = state
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is not open"))?
            .write(buffer)?;
        state.size = state
            .size
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = lock_state(&self.state)?;
        state
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is not open"))?
            .flush()
    }
}

fn rotate(state: &mut RotationState) -> io::Result<()> {
    if let Some(mut file) = state.file.take() {
        file.flush()?;
        drop(file);
    }

    for index in (1..state.backups).rev() {
        let source = backup_path(&state.path, index);
        if !source.exists() {
            continue;
        }
        let destination = backup_path(&state.path, index + 1);
        remove_if_exists(&destination)?;
        fs::rename(source, destination)?;
    }
    let first_backup = backup_path(&state.path, 1);
    remove_if_exists(&first_backup)?;
    if state.path.exists() {
        fs::rename(&state.path, first_backup)?;
    }
    state.file = Some(open_log(&state.path)?);
    state.size = 0;
    Ok(())
}

fn open_log(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn backup_path(path: &Path, index: usize) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(format!(".{index}"));
    PathBuf::from(value)
}

fn lock_state(state: &Mutex<RotationState>) -> io::Result<MutexGuard<'_, RotationState>> {
    state
        .lock()
        .map_err(|_| io::Error::other("log writer mutex is poisoned"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_clamps_backup_count() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("proxy.log");
        let writer = RotatingMakeWriter::new(&path, 32 * 1024, 0).unwrap();
        let mut output = writer.make_writer();
        output.write_all(&vec![b'a'; 20 * 1024]).unwrap();
        output.write_all(&vec![b'b'; 20 * 1024]).unwrap();
        output.flush().unwrap();
        assert!(backup_path(&path, 1).is_file());
        assert!(path.is_file());
        #[cfg(unix)]
        for log in [&path, &backup_path(&path, 1)] {
            assert_eq!(
                fs::metadata(log).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn restricts_permissions_on_an_existing_log() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("proxy.log");
        fs::write(&path, b"old secret-bearing log").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        RotatingMakeWriter::new(&path, 32 * 1024, 1).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn rejects_excessive_backup_count() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            RotatingMakeWriter::new(
                directory.path().join("proxy.log"),
                MIN_LOG_SIZE,
                MAX_LOG_BACKUPS + 1
            )
            .is_err()
        );
    }
}
