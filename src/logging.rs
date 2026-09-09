use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use tracing::Metadata;
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

/// Wraps another `MakeWriter` and masks third-party domain names in every
/// log line, mirroring upstream's `DomainCensorFilter`: all labels except the
/// TLD keep their first half and the rest is replaced with `*`. `telegram.org`
/// and `.log` file names stay untouched.
#[derive(Clone)]
pub struct CensoringMakeWriter<M> {
    inner: M,
}

impl<M> CensoringMakeWriter<M> {
    pub const fn new(inner: M) -> Self {
        Self { inner }
    }
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for CensoringMakeWriter<M> {
    type Writer = CensoringWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        CensoringWriter {
            inner: self.inner.make_writer(),
        }
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        CensoringWriter {
            inner: self.inner.make_writer_for(meta),
        }
    }
}

pub struct CensoringWriter<W> {
    inner: W,
}

impl<W: Write> Write for CensoringWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let censored = censor_domains(&String::from_utf8_lossy(buffer));
        self.inner.write_all(censored.as_bytes())?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[must_use]
pub fn censor_domains(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut run = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() || matches!(character, '-' | '.' | '_') {
            run.push(character);
        } else {
            flush_run(&mut output, &run);
            run.clear();
            output.push(character);
        }
    }
    flush_run(&mut output, &run);
    output
}

fn flush_run(output: &mut String, run: &str) {
    let core = run.trim_matches('.');
    let leading = &run[..run.len() - run.trim_start_matches('.').len()];
    let trailing = &run[leading.len() + core.len()..];
    output.push_str(leading);
    if is_censorable_domain(core) {
        let labels: Vec<&str> = core.split('.').collect();
        for (index, label) in labels.iter().enumerate() {
            if index > 0 {
                output.push('.');
            }
            if index + 1 == labels.len() {
                output.push_str(label);
            } else {
                let keep = label.len() / 2;
                output.push_str(&label[..keep]);
                output.extend(std::iter::repeat_n('*', label.len() - keep));
            }
        }
    } else {
        output.push_str(core);
    }
    output.push_str(trailing);
}

// `normalized` is already lowercase, so the suffix check is case-insensitive.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_censorable_domain(candidate: &str) -> bool {
    let labels: Vec<&str> = candidate.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    };
    if !labels.iter().all(|label| valid_label(label)) {
        return false;
    }
    let tld = labels[labels.len() - 1];
    if tld.len() < 2 || !tld.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return false;
    }
    let normalized = candidate.to_ascii_lowercase();
    !(normalized == "telegram.org"
        || normalized.ends_with(".telegram.org")
        || normalized.ends_with(".log"))
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
    fn censors_third_party_domains_but_keeps_telegram_ips_and_logs() {
        assert_eq!(
            censor_domains("WS cdn.example.com -> 149.154.167.51 ok"),
            "WS c**.exa****.com -> 149.154.167.51 ok"
        );
        assert_eq!(
            censor_domains("fronting via web.telegram.org, log proxy.log, v1.11.0."),
            "fronting via web.telegram.org, log proxy.log, v1.11.0."
        );
        assert_eq!(
            censor_domains("worker a.b.example.dev."),
            "worker *.*.exa****.dev."
        );
        assert_eq!(
            censor_domains("host_name.com -bad.com"),
            "host_name.com -bad.com"
        );
        assert_eq!(censor_domains("домен example.org"), "домен exa****.org");
    }

    #[test]
    fn censoring_writer_rewrites_each_line() {
        let mut sink = Vec::new();
        {
            let mut writer = CensoringWriter { inner: &mut sink };
            writer.write_all(b"DC2 via worker.example.net\n").unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(sink, b"DC2 via wor***.exa****.net\n");
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
