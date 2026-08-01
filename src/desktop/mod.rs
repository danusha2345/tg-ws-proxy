mod icon;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(windows, target_os = "macos"))]
mod native;
mod paths;
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod unsupported;
mod update;
mod worker;

use std::io;

use anyhow::{Context, Result, bail};
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::{BoxMakeWriter, MakeWriterExt};

use crate::desktop_config::DesktopConfig;
use crate::desktop_controller::ProxyStatus;
use crate::logging::RotatingMakeWriter;
use crate::single_instance::SingleInstance;

pub use paths::AppPaths;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Language {
    English,
    Russian,
}

impl Language {
    fn from_code(code: &str) -> Self {
        if code.trim().to_ascii_lowercase().starts_with("ru") {
            Self::Russian
        } else {
            Self::English
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkerCommand {
    OpenTelegram,
    CopyLink,
    OpenSettings,
    Restart,
    OpenLogs,
    CheckUpdates,
    DownloadUpdate,
    InstallUpdate,
    Exit,
}

#[derive(Debug)]
pub(crate) enum WorkerEvent {
    Config { language: Language, link: String },
    Status(ProxyStatus),
    Update(update::UpdateState),
    Exited,
}

pub(crate) struct Labels {
    language: Language,
}

impl Labels {
    pub(crate) fn new(language: Language) -> Self {
        Self { language }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn app_name() -> &'static str {
        "TgWsProxy"
    }

    pub(crate) fn open_telegram(&self) -> &'static str {
        match self.language {
            Language::Russian => "Открыть в Telegram",
            Language::English => "Open in Telegram",
        }
    }

    pub(crate) fn copy_link(&self) -> &'static str {
        match self.language {
            Language::Russian => "Копировать ссылку",
            Language::English => "Copy link",
        }
    }

    pub(crate) fn open_settings(&self) -> &'static str {
        match self.language {
            Language::Russian => "Открыть настройки",
            Language::English => "Open settings",
        }
    }

    pub(crate) fn restart(&self) -> &'static str {
        match self.language {
            Language::Russian => "Перезапустить прокси",
            Language::English => "Restart proxy",
        }
    }

    pub(crate) fn open_logs(&self) -> &'static str {
        match self.language {
            Language::Russian => "Открыть логи",
            Language::English => "Open logs",
        }
    }

    pub(crate) fn update(&self, state: &update::UpdateState) -> String {
        use update::UpdateState;
        match (self.language, state) {
            (Language::Russian, UpdateState::Idle) => "Проверить обновления".to_owned(),
            (Language::Russian, UpdateState::Checking) => "Проверка обновлений…".to_owned(),
            (Language::Russian, UpdateState::Current) => "Установлена актуальная версия".to_owned(),
            (Language::Russian, UpdateState::Available { version }) => {
                format!("Скачать обновление {version}")
            }
            (Language::Russian, UpdateState::Downloading { version }) => {
                format!("Скачивается {version}…")
            }
            (Language::Russian, UpdateState::Ready { version }) => {
                format!("Установить обновление {version}")
            }
            (Language::Russian, UpdateState::Failed) => "Повторить проверку обновлений".to_owned(),
            (Language::English, UpdateState::Idle) => "Check for updates".to_owned(),
            (Language::English, UpdateState::Checking) => "Checking for updates…".to_owned(),
            (Language::English, UpdateState::Current) => {
                "The latest version is installed".to_owned()
            }
            (Language::English, UpdateState::Available { version }) => {
                format!("Download update {version}")
            }
            (Language::English, UpdateState::Downloading { version }) => {
                format!("Downloading {version}…")
            }
            (Language::English, UpdateState::Ready { version }) => {
                format!("Install update {version}")
            }
            (Language::English, UpdateState::Failed) => "Retry update check".to_owned(),
        }
    }

    pub(crate) fn exit(&self) -> &'static str {
        match self.language {
            Language::Russian => "Выйти",
            Language::English => "Exit",
        }
    }

    pub(crate) fn status(&self, status: &ProxyStatus) -> String {
        match (self.language, status) {
            (Language::Russian, ProxyStatus::Starting) => "Статус: запускается…".to_owned(),
            (Language::Russian, ProxyStatus::Running) => "Статус: работает".to_owned(),
            (Language::Russian, ProxyStatus::Stopping) => "Статус: останавливается…".to_owned(),
            (Language::Russian, ProxyStatus::Stopped) => "Статус: остановлен".to_owned(),
            (Language::Russian, ProxyStatus::Failed(error)) => {
                format!("Статус: ошибка — {}", one_line(error))
            }
            (Language::English, ProxyStatus::Starting) => "Status: starting…".to_owned(),
            (Language::English, ProxyStatus::Running) => "Status: running".to_owned(),
            (Language::English, ProxyStatus::Stopping) => "Status: stopping…".to_owned(),
            (Language::English, ProxyStatus::Stopped) => "Status: stopped".to_owned(),
            (Language::English, ProxyStatus::Failed(error)) => {
                format!("Status: failed — {}", one_line(error))
            }
        }
    }
}

pub fn run() -> Result<()> {
    let paths = AppPaths::discover()?;
    paths.ensure_directory()?;
    let _instance = SingleInstance::acquire(&paths.lock)?;
    let config = DesktopConfig::load_or_create(&paths.config)?;
    init_logging(&paths, &config)?;
    info!(
        config = %paths.config.display(),
        log = %paths.log.display(),
        "desktop frontend starting"
    );

    let initial_language = Language::from_code(&config.language);
    let worker = worker::spawn(paths)?;
    let ui_result = run_platform(&worker.command_tx, worker.event_rx, initial_language);
    let _ = worker.command_tx.send(WorkerCommand::Exit);
    let worker_result = worker
        .thread
        .join()
        .map_err(|_| anyhow::anyhow!("desktop proxy worker thread panicked"))?;

    ui_result?;
    worker_result
}

#[cfg(target_os = "linux")]
fn run_platform(
    command_tx: &tokio::sync::mpsc::UnboundedSender<WorkerCommand>,
    event_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerEvent>,
    language: Language,
) -> Result<()> {
    linux::run(command_tx.clone(), event_rx, language)
}

#[cfg(any(windows, target_os = "macos"))]
fn run_platform(
    command_tx: &tokio::sync::mpsc::UnboundedSender<WorkerCommand>,
    event_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerEvent>,
    language: Language,
) -> Result<()> {
    native::run(command_tx, event_rx, language)
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn run_platform(
    command_tx: &tokio::sync::mpsc::UnboundedSender<WorkerCommand>,
    event_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerEvent>,
    language: Language,
) -> Result<()> {
    unsupported::run(command_tx.clone(), event_rx, language)
}

fn init_logging(paths: &AppPaths, config: &DesktopConfig) -> Result<()> {
    let max_bytes = log_max_bytes(config.log_max_mb)?;
    let file = RotatingMakeWriter::new(&paths.log, max_bytes, 1)
        .with_context(|| format!("failed to configure log file {}", paths.log.display()))?;
    let filter = if config.verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(BoxMakeWriter::new(io::stderr.and(file)))
        .with_target(false)
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize desktop logging: {error}"))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn log_max_bytes(megabytes: f64) -> Result<u64> {
    const MAX_LOG_MEGABYTES: f64 = 1_048_576.0;
    if !megabytes.is_finite() || megabytes <= 0.0 || megabytes > MAX_LOG_MEGABYTES {
        bail!("log_max_mb must be a positive finite number no greater than {MAX_LOG_MEGABYTES}");
    }
    Ok((megabytes * 1024.0 * 1024.0).round() as u64)
}

fn one_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_legacy_language_and_report_exact_failure() {
        let russian = Labels::new(Language::from_code("ru_RU"));
        let english = Labels::new(Language::from_code("en"));
        assert_eq!(russian.restart(), "Перезапустить прокси");
        assert_eq!(english.restart(), "Restart proxy");
        assert_eq!(
            russian.status(&ProxyStatus::Failed("bind\nfailed".to_owned())),
            "Статус: ошибка — bind failed"
        );
    }
}
