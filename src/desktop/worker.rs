use std::net::{IpAddr, UdpSocket};
use std::path::Path;
use std::thread;

use anyhow::{Context, Result, anyhow};
use arboard::Clipboard;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use super::{AppPaths, Language, WorkerCommand, WorkerEvent};
use crate::desktop_config::DesktopConfig;
use crate::desktop_controller::{DesktopProxyController, ProxyStatus};

pub(super) struct WorkerHandle {
    pub command_tx: mpsc::UnboundedSender<WorkerCommand>,
    pub event_rx: mpsc::UnboundedReceiver<WorkerEvent>,
    pub thread: thread::JoinHandle<Result<()>>,
}

pub(super) fn spawn(paths: AppPaths) -> Result<WorkerHandle> {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let thread = thread::Builder::new()
        .name("tg-ws-proxy-worker".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("tg-ws-proxy-runtime")
                .enable_all()
                .build()
                .context("failed to create the desktop proxy runtime");
            let result =
                runtime.and_then(|runtime| runtime.block_on(run(paths, command_rx, &event_tx)));
            let _ = event_tx.send(WorkerEvent::Exited);
            result
        })
        .context("failed to start the desktop proxy worker thread")?;

    Ok(WorkerHandle {
        command_tx,
        event_rx,
        thread,
    })
}

async fn run(
    paths: AppPaths,
    mut command_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    event_tx: &mpsc::UnboundedSender<WorkerEvent>,
) -> Result<()> {
    let mut controller = None;
    let mut status_rx = None;
    let mut current_link = None;
    let mut clipboard = None;

    start_proxy(
        &paths,
        event_tx,
        &mut controller,
        &mut status_rx,
        &mut current_link,
    )
    .await;

    loop {
        tokio::select! {
            command = command_rx.recv() => {
                let Some(command) = command else {
                    stop_proxy(event_tx, &mut controller, &mut status_rx).await;
                    return Ok(());
                };
                match command {
                    WorkerCommand::OpenTelegram => {
                        if let Err(action_error) =
                            open_telegram(current_link.as_deref(), &mut clipboard)
                        {
                            warn!(error = %action_error, "failed to open Telegram connection URL");
                        }
                    }
                    WorkerCommand::CopyLink => {
                        if let Err(action_error) =
                            copy_link(current_link.as_deref(), &mut clipboard)
                        {
                            warn!(error = %action_error, "failed to copy Telegram connection URL");
                        }
                    }
                    WorkerCommand::OpenSettings => {
                        if let Err(action_error) = open_file(&paths.config) {
                            warn!(
                                path = %paths.config.display(),
                                error = %action_error,
                                "failed to open desktop settings"
                            );
                        }
                    }
                    WorkerCommand::Restart => {
                        stop_proxy(event_tx, &mut controller, &mut status_rx).await;
                        start_proxy(
                            &paths,
                            event_tx,
                            &mut controller,
                            &mut status_rx,
                            &mut current_link,
                        )
                        .await;
                    }
                    WorkerCommand::OpenLogs => {
                        if let Err(action_error) = open_file(&paths.log) {
                            warn!(
                                path = %paths.log.display(),
                                error = %action_error,
                                "failed to open desktop logs"
                            );
                        }
                    }
                    WorkerCommand::Exit => {
                        stop_proxy(event_tx, &mut controller, &mut status_rx).await;
                        return Ok(());
                    }
                }
            }
            status = next_status(&mut status_rx), if status_rx.is_some() => {
                match status {
                    Some(status) => {
                        debug!(?status, "desktop proxy status changed");
                        let _ = event_tx.send(WorkerEvent::Status(status));
                    }
                    None => status_rx = None,
                }
            }
        }
    }
}

async fn start_proxy(
    paths: &AppPaths,
    event_tx: &mpsc::UnboundedSender<WorkerEvent>,
    controller: &mut Option<DesktopProxyController>,
    status_rx: &mut Option<watch::Receiver<ProxyStatus>>,
    current_link: &mut Option<String>,
) {
    *current_link = None;
    let result = async {
        let desktop_config = DesktopConfig::load_or_create(&paths.config)?;
        let proxy_config = desktop_config.to_proxy_config()?;
        let language = Language::from_code(&desktop_config.language);
        let advertised_host = advertised_host(&proxy_config.host);
        let link = proxy_config.telegram_url(&advertised_host);
        let _ = event_tx.send(WorkerEvent::Config {
            language,
            link: String::new(),
        });
        let _ = event_tx.send(WorkerEvent::Status(ProxyStatus::Starting));

        let running = DesktopProxyController::start(proxy_config).await?;
        *current_link = Some(link.clone());
        let _ = event_tx.send(WorkerEvent::Config { language, link });
        *status_rx = Some(running.subscribe());
        *controller = Some(running);
        Ok::<(), anyhow::Error>(())
    }
    .await;

    match result {
        Ok(()) => {
            info!("desktop proxy listener is ready");
            let _ = event_tx.send(WorkerEvent::Status(ProxyStatus::Running));
        }
        Err(start_error) => {
            error!(error = %start_error, "desktop proxy failed to start");
            let _ = event_tx.send(WorkerEvent::Status(ProxyStatus::Failed(
                start_error.to_string(),
            )));
        }
    }
}

async fn stop_proxy(
    event_tx: &mpsc::UnboundedSender<WorkerEvent>,
    controller: &mut Option<DesktopProxyController>,
    status_rx: &mut Option<watch::Receiver<ProxyStatus>>,
) {
    *status_rx = None;
    let Some(running) = controller.take() else {
        let _ = event_tx.send(WorkerEvent::Status(ProxyStatus::Stopped));
        return;
    };
    let _ = event_tx.send(WorkerEvent::Status(ProxyStatus::Stopping));
    match running.stop().await {
        Ok(()) => {
            info!("desktop proxy stopped");
            let _ = event_tx.send(WorkerEvent::Status(ProxyStatus::Stopped));
        }
        Err(stop_error) => {
            error!(error = %stop_error, "desktop proxy stop failed");
            let _ = event_tx.send(WorkerEvent::Status(ProxyStatus::Failed(
                stop_error.to_string(),
            )));
        }
    }
}

async fn next_status(receiver: &mut Option<watch::Receiver<ProxyStatus>>) -> Option<ProxyStatus> {
    let receiver = receiver.as_mut()?;
    receiver.changed().await.ok()?;
    let status = receiver.borrow().clone();
    Some(status)
}

fn open_file(path: &Path) -> Result<()> {
    match open::that(path) {
        Ok(()) => Ok(()),
        Err(default_error) => {
            #[cfg(windows)]
            {
                warn!(
                    path = %path.display(),
                    error = %default_error,
                    "system file handler failed; opening the file in Notepad"
                );
                open::with(path, "notepad.exe").with_context(|| {
                    format!(
                        "system file handler failed ({default_error}); Notepad fallback also failed"
                    )
                })
            }
            #[cfg(not(windows))]
            {
                Err(default_error).context("system file handler failed")
            }
        }
    }
}

fn open_telegram(link: Option<&str>, clipboard: &mut Option<Clipboard>) -> Result<()> {
    let link = link.context("proxy URL is not available")?;
    match open::that(link) {
        Ok(()) => Ok(()),
        Err(open_error) => {
            copy_text(link, clipboard).with_context(|| {
                format!("system URL handler failed ({open_error}); clipboard fallback also failed")
            })?;
            warn!(error = %open_error, "system URL handler failed; copied URL instead");
            Ok(())
        }
    }
}

fn copy_link(link: Option<&str>, clipboard: &mut Option<Clipboard>) -> Result<()> {
    let link = link.context("proxy URL is not available")?;
    copy_text(link, clipboard)
}

fn copy_text(text: &str, clipboard: &mut Option<Clipboard>) -> Result<()> {
    if clipboard.is_none() {
        *clipboard = Some(Clipboard::new().context("failed to connect to the system clipboard")?);
    }
    clipboard
        .as_mut()
        .ok_or_else(|| anyhow!("system clipboard is unavailable"))?
        .set_text(text.to_owned())
        .context("failed to store text in the system clipboard")
}

fn advertised_host(bind_host: &str) -> String {
    if bind_host != "0.0.0.0" && bind_host != "::" {
        return bind_host.to_owned();
    }
    let (bind, probe) = if bind_host == "::" {
        ("[::]:0", "[2001:4860:4860::8888]:80")
    } else {
        ("0.0.0.0:0", "8.8.8.8:80")
    };
    UdpSocket::bind(bind)
        .and_then(|socket| {
            socket.connect(probe)?;
            socket.local_addr()
        })
        .map_or_else(
            |_| "127.0.0.1".to_owned(),
            |address| match address.ip() {
                IpAddr::V4(ip) => ip.to_string(),
                IpAddr::V6(ip) => format!("[{ip}]"),
            },
        )
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener as StdTcpListener};

    use super::*;

    #[test]
    fn explicit_listener_host_is_used_in_link() {
        assert_eq!(advertised_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(advertised_host("192.0.2.10"), "192.0.2.10");
    }

    #[tokio::test]
    async fn failed_start_clears_old_link_and_never_publishes_a_ready_link() {
        let directory = tempfile::tempdir().unwrap();
        let reservation = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let paths = AppPaths {
            directory: directory.path().to_path_buf(),
            config: directory.path().join("config.json"),
            log: directory.path().join("proxy.log"),
            lock: directory.path().join("desktop.lock"),
        };
        let config = DesktopConfig {
            host: Ipv4Addr::LOCALHOST.to_string(),
            port: reservation.local_addr().unwrap().port(),
            pool_size: 0,
            cfproxy: false,
            ..DesktopConfig::default()
        };
        config.save_atomic(&paths.config).unwrap();

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut controller = None;
        let mut status_rx = None;
        let mut current_link = Some("tg://proxy?server=stale".to_owned());
        start_proxy(
            &paths,
            &event_tx,
            &mut controller,
            &mut status_rx,
            &mut current_link,
        )
        .await;
        drop(event_tx);

        assert!(controller.is_none());
        assert!(status_rx.is_none());
        assert!(current_link.is_none());
        let mut published_ready_link = false;
        let mut saw_failure = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                WorkerEvent::Config { link, .. } => {
                    published_ready_link |= !link.is_empty();
                }
                WorkerEvent::Status(ProxyStatus::Failed(error)) => {
                    assert!(error.contains("failed to listen"));
                    saw_failure = true;
                }
                WorkerEvent::Status(_) | WorkerEvent::Exited => {}
            }
        }
        assert!(!published_ready_link);
        assert!(saw_failure);
    }
}
