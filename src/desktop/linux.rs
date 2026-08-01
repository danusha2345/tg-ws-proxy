use anyhow::{Context, Result};
use ksni::menu::StandardItem;
use ksni::{Category, Icon, MenuItem, Status, ToolTip, TrayMethods};
use tokio::sync::mpsc;
use tracing::warn;

use super::icon;
use super::update::UpdateState;
use super::{Labels, Language, WorkerCommand, WorkerEvent};
use crate::desktop_controller::ProxyStatus;

struct LinuxTray {
    command_tx: mpsc::UnboundedSender<WorkerCommand>,
    language: Language,
    link: String,
    proxy_status: ProxyStatus,
    update: UpdateState,
    icons: Vec<Icon>,
}

impl LinuxTray {
    fn send(&self, command: WorkerCommand) {
        if self.command_tx.send(command).is_err() {
            warn!(?command, "desktop proxy worker is no longer available");
        }
    }

    fn action(
        label: &str,
        icon_name: &str,
        enabled: bool,
        activate: impl Fn(&mut Self) + Send + 'static,
    ) -> MenuItem<Self> {
        StandardItem {
            label: label.to_owned(),
            icon_name: icon_name.to_owned(),
            enabled,
            activate: Box::new(activate),
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for LinuxTray {
    fn id(&self) -> String {
        "tg-ws-proxy".to_owned()
    }

    fn category(&self) -> Category {
        Category::Communications
    }

    fn title(&self) -> String {
        Labels::new(self.language).status(&self.proxy_status)
    }

    fn status(&self) -> Status {
        match self.proxy_status {
            ProxyStatus::Failed(_) => Status::NeedsAttention,
            ProxyStatus::Stopped => Status::Passive,
            _ => Status::Active,
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_pixmap: self.icons.clone(),
            title: Labels::app_name().to_owned(),
            description: Labels::new(self.language).status(&self.proxy_status),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(WorkerCommand::OpenTelegram);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let labels = Labels::new(self.language);
        let link_ready = link_is_actionable(&self.proxy_status, &self.link);
        vec![
            StandardItem {
                label: labels.status(&self.proxy_status),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            Self::action(labels.open_telegram(), "telegram", link_ready, |tray| {
                tray.send(WorkerCommand::OpenTelegram);
            }),
            Self::action(labels.copy_link(), "edit-copy", link_ready, |tray| {
                tray.send(WorkerCommand::CopyLink);
            }),
            MenuItem::Separator,
            Self::action(labels.open_settings(), "document-edit", true, |tray| {
                tray.send(WorkerCommand::OpenSettings);
            }),
            Self::action(labels.restart(), "view-refresh", true, |tray| {
                tray.send(WorkerCommand::Restart);
            }),
            Self::action(labels.open_logs(), "text-x-log", true, |tray| {
                tray.send(WorkerCommand::OpenLogs);
            }),
            Self::action(
                &labels.update(&self.update),
                "system-software-update",
                update_is_actionable(&self.update),
                |tray| tray.send(update_command(&tray.update)),
            ),
            MenuItem::Separator,
            Self::action(labels.exit(), "application-exit", true, |tray| {
                tray.send(WorkerCommand::Exit);
            }),
        ]
    }
}

pub(super) fn run(
    command_tx: mpsc::UnboundedSender<WorkerCommand>,
    event_rx: mpsc::UnboundedReceiver<WorkerEvent>,
    language: Language,
) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create the Linux tray runtime")?
        .block_on(run_async(command_tx, event_rx, language))
}

async fn run_async(
    command_tx: mpsc::UnboundedSender<WorkerCommand>,
    mut event_rx: mpsc::UnboundedReceiver<WorkerEvent>,
    language: Language,
) -> Result<()> {
    let tray = LinuxTray {
        command_tx: command_tx.clone(),
        language,
        link: String::new(),
        proxy_status: ProxyStatus::Starting,
        update: UpdateState::Idle,
        icons: tray_icons(),
    };
    let tray_handle = tray
        .spawn()
        .await
        .context("failed to register the Linux StatusNotifierItem")?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("failed to install the desktop SIGTERM handler")?;
    let mut exit_requested = false;

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                let Some(event) = event else {
                    break;
                };
                match event {
                    WorkerEvent::Config { language, link } => {
                        tray_handle
                            .update(move |tray| {
                                tray.language = language;
                                tray.link = link;
                            })
                            .await
                            .context("Linux tray service stopped while updating configuration")?;
                    }
                    WorkerEvent::Status(status) => {
                        tray_handle
                            .update(move |tray| tray.proxy_status = status)
                            .await
                            .context("Linux tray service stopped while updating status")?;
                    }
                    WorkerEvent::Update(update) => {
                        tray_handle
                            .update(move |tray| tray.update = update)
                            .await
                            .context("Linux tray service stopped while updating update state")?;
                    }
                    WorkerEvent::Exited => break,
                }
            }
            signal = tokio::signal::ctrl_c(), if !exit_requested => {
                if let Err(signal_error) = signal {
                    warn!(error = %signal_error, "desktop Ctrl-C handler failed");
                }
                exit_requested = true;
                let _ = command_tx.send(WorkerCommand::Exit);
            }
            _ = terminate.recv(), if !exit_requested => {
                exit_requested = true;
                let _ = command_tx.send(WorkerCommand::Exit);
            }
        }
    }

    tray_handle.shutdown().await;
    Ok(())
}

fn update_is_actionable(state: &UpdateState) -> bool {
    !matches!(
        state,
        UpdateState::Checking | UpdateState::Downloading { .. }
    )
}

fn update_command(state: &UpdateState) -> WorkerCommand {
    match state {
        UpdateState::Available { .. } => WorkerCommand::DownloadUpdate,
        UpdateState::Ready { .. } => WorkerCommand::InstallUpdate,
        UpdateState::Idle
        | UpdateState::Checking
        | UpdateState::Current
        | UpdateState::Downloading { .. }
        | UpdateState::Failed => WorkerCommand::CheckUpdates,
    }
}

fn link_is_actionable(status: &ProxyStatus, link: &str) -> bool {
    matches!(status, ProxyStatus::Running) && !link.is_empty()
}

fn tray_icons() -> Vec<Icon> {
    [32_u32, 64_u32]
        .into_iter()
        .map(|size| {
            let bitmap = icon::render(size);
            Icon {
                width: i32::try_from(bitmap.width).expect("desktop icon width fits i32"),
                height: i32::try_from(bitmap.height).expect("desktop icon height fits i32"),
                data: bitmap.into_argb(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_link_is_actionable_only_while_listener_is_running() {
        let link = "tg://proxy?server=127.0.0.1";
        assert!(link_is_actionable(&ProxyStatus::Running, link));
        assert!(!link_is_actionable(&ProxyStatus::Starting, link));
        assert!(!link_is_actionable(
            &ProxyStatus::Failed("occupied".to_owned()),
            link
        ));
        assert!(!link_is_actionable(&ProxyStatus::Running, ""));
    }
}
