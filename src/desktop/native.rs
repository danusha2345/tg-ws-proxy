use std::thread;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::warn;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::WindowId;

use super::icon;
use super::{Labels, Language, WorkerCommand, WorkerEvent};
use crate::desktop_controller::ProxyStatus;

const ID_OPEN_TELEGRAM: &str = "open-telegram";
const ID_COPY_LINK: &str = "copy-link";
const ID_OPEN_SETTINGS: &str = "open-settings";
const ID_RESTART: &str = "restart";
const ID_OPEN_LOGS: &str = "open-logs";
const ID_EXIT: &str = "exit";

#[derive(Debug)]
enum UserEvent {
    Menu(MenuEvent),
    Worker(WorkerEvent),
}

struct NativeMenu {
    status: MenuItem,
    open_telegram: MenuItem,
    copy_link: MenuItem,
    open_settings: MenuItem,
    restart: MenuItem,
    open_logs: MenuItem,
    exit: MenuItem,
}

impl NativeMenu {
    fn build(language: Language, status: &ProxyStatus, link_ready: bool) -> Result<(Menu, Self)> {
        let labels = Labels::new(language);
        let menu = Menu::new();
        let items = Self {
            status: MenuItem::with_id("status", labels.status(status), false, None),
            open_telegram: MenuItem::with_id(
                ID_OPEN_TELEGRAM,
                labels.open_telegram(),
                link_ready,
                None,
            ),
            copy_link: MenuItem::with_id(ID_COPY_LINK, labels.copy_link(), link_ready, None),
            open_settings: MenuItem::with_id(ID_OPEN_SETTINGS, labels.open_settings(), true, None),
            restart: MenuItem::with_id(ID_RESTART, labels.restart(), true, None),
            open_logs: MenuItem::with_id(ID_OPEN_LOGS, labels.open_logs(), true, None),
            exit: MenuItem::with_id(ID_EXIT, labels.exit(), true, None),
        };
        menu.append(&items.status)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&items.open_telegram)?;
        menu.append(&items.copy_link)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&items.open_settings)?;
        menu.append(&items.restart)?;
        menu.append(&items.open_logs)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&items.exit)?;
        Ok((menu, items))
    }

    fn update(&self, language: Language, status: &ProxyStatus, link_ready: bool) {
        let labels = Labels::new(language);
        let link_ready = link_ready && matches!(status, ProxyStatus::Running);
        self.status.set_text(labels.status(status));
        self.open_telegram.set_text(labels.open_telegram());
        self.open_telegram.set_enabled(link_ready);
        self.copy_link.set_text(labels.copy_link());
        self.copy_link.set_enabled(link_ready);
        self.open_settings.set_text(labels.open_settings());
        self.restart.set_text(labels.restart());
        self.open_logs.set_text(labels.open_logs());
        self.exit.set_text(labels.exit());
    }
}

struct NativeApp {
    command_tx: mpsc::UnboundedSender<WorkerCommand>,
    language: Language,
    status: ProxyStatus,
    link_ready: bool,
    menu: Option<NativeMenu>,
    tray: Option<TrayIcon>,
    startup_error: Option<String>,
}

impl NativeApp {
    fn send(&self, command: WorkerCommand) {
        if self.command_tx.send(command).is_err() {
            warn!(?command, "desktop proxy worker is no longer available");
        }
    }

    fn update_menu(&self) {
        if let Some(menu) = &self.menu {
            menu.update(self.language, &self.status, self.link_ready);
        }
        if let Some(tray) = &self.tray {
            let tooltip = Labels::new(self.language).status(&self.status);
            if let Err(error) = tray.set_tooltip(Some(tooltip)) {
                warn!(%error, "failed to update native tray tooltip");
            }
        }
    }

    fn build_tray(&mut self) -> Result<()> {
        let labels = Labels::new(self.language);
        let (menu, menu_items) = NativeMenu::build(self.language, &self.status, self.link_ready)?;
        let bitmap = icon::render(64);
        let icon = Icon::from_rgba(bitmap.rgba, bitmap.width, bitmap.height)
            .context("failed to create the native tray icon")?;
        let tray = TrayIconBuilder::new()
            .with_id("tg-ws-proxy")
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_icon_as_template(false)
            .with_tooltip(labels.status(&self.status))
            .with_menu_on_left_click(true)
            .build()
            .context("failed to create the native tray icon")?;
        self.menu = Some(menu_items);
        self.tray = Some(tray);
        Ok(())
    }

    fn handle_menu(&self, event: &MenuEvent) {
        let command = match event.id.0.as_str() {
            ID_OPEN_TELEGRAM => Some(WorkerCommand::OpenTelegram),
            ID_COPY_LINK => Some(WorkerCommand::CopyLink),
            ID_OPEN_SETTINGS => Some(WorkerCommand::OpenSettings),
            ID_RESTART => Some(WorkerCommand::Restart),
            ID_OPEN_LOGS => Some(WorkerCommand::OpenLogs),
            ID_EXIT => Some(WorkerCommand::Exit),
            _ => None,
        };
        if let Some(command) = command {
            self.send(command);
        }
    }
}

impl ApplicationHandler<UserEvent> for NativeApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.tray.is_some() || self.startup_error.is_some() {
            return;
        }
        if let Err(error) = self.build_tray() {
            self.startup_error = Some(error.to_string());
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Menu(event) => self.handle_menu(&event),
            UserEvent::Worker(WorkerEvent::Config { language, link }) => {
                self.language = language;
                self.link_ready = !link.is_empty();
                self.update_menu();
            }
            UserEvent::Worker(WorkerEvent::Status(status)) => {
                self.status = status;
                self.update_menu();
            }
            UserEvent::Worker(WorkerEvent::Exited) => event_loop.exit(),
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.send(WorkerCommand::Exit);
    }
}

pub(super) fn run(
    command_tx: &mpsc::UnboundedSender<WorkerCommand>,
    mut event_rx: mpsc::UnboundedReceiver<WorkerEvent>,
    language: Language,
) -> Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("failed to create the native tray event loop")?;

    let menu_proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));

    let worker_proxy = event_loop.create_proxy();
    let bridge = thread::Builder::new()
        .name("tg-ws-proxy-ui-events".to_owned())
        .spawn(move || {
            while let Some(event) = event_rx.blocking_recv() {
                if worker_proxy.send_event(UserEvent::Worker(event)).is_err() {
                    break;
                }
            }
        })
        .context("failed to start the native tray event bridge")?;

    let mut app = NativeApp {
        command_tx: command_tx.clone(),
        language,
        status: ProxyStatus::Starting,
        link_ready: false,
        menu: None,
        tray: None,
        startup_error: None,
    };
    let event_result = event_loop
        .run_app(&mut app)
        .context("native tray event loop failed");
    let _ = command_tx.send(WorkerCommand::Exit);
    bridge
        .join()
        .map_err(|_| anyhow::anyhow!("native tray event bridge panicked"))?;
    event_result?;
    if let Some(error) = app.startup_error {
        anyhow::bail!("{error}");
    }
    Ok(())
}
