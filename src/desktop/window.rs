//! Desktop control window. The worker remains the sole owner of proxy lifetime.
use std::time::Duration;

use anyhow::{Context, Result};
use eframe::egui::{self, Color32, RichText};
use tokio::sync::mpsc;

use super::update::UpdateState;
use super::{AppPaths, Labels, Language, WorkerCommand, WorkerEvent};
use crate::desktop_config::DesktopConfig;
use crate::desktop_controller::ProxyStatus;

const BLUE: Color32 = Color32::from_rgb(21, 92, 196);

struct ControlWindow {
    commands: mpsc::UnboundedSender<WorkerCommand>,
    events: mpsc::UnboundedReceiver<WorkerEvent>,
    language: Language,
    status: ProxyStatus,
    link: String,
    update: UpdateState,
    config: DesktopConfig,
    saved_config: DesktopConfig,
    paths: AppPaths,
    settings: bool,
    secret_visible: bool,
    workers: String,
    domains: String,
    notice: Option<String>,
    exiting: bool,
    #[cfg(windows)]
    tray: Option<tray_icon::TrayIcon>,
    #[cfg(windows)]
    tray_events: std::sync::mpsc::Receiver<String>,
}

impl ControlWindow {
    fn text<'a>(&self, ru: &'a str, en: &'a str) -> &'a str {
        if self.language == Language::Russian {
            ru
        } else {
            en
        }
    }

    fn send(&mut self, command: WorkerCommand) {
        if self.commands.send(command).is_err() {
            self.notice = Some(
                self.text(
                    "Фоновый процесс завершён. Перезапустите приложение.",
                    "The background process stopped. Restart the app.",
                )
                .to_owned(),
            );
        } else if matches!(command, WorkerCommand::Restart | WorkerCommand::Stop) {
            self.status = if command == WorkerCommand::Stop {
                ProxyStatus::Stopping
            } else {
                ProxyStatus::Starting
            };
            self.link.clear();
        }
    }

    fn update_action(&mut self) {
        self.send(match self.update {
            UpdateState::Available { .. } => WorkerCommand::DownloadUpdate,
            UpdateState::Ready { .. } => WorkerCommand::InstallUpdate,
            _ => WorkerCommand::CheckUpdates,
        });
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                WorkerEvent::Config { language, link } => {
                    self.language = language;
                    self.link = link;
                }
                WorkerEvent::Status(status) => self.status = status,
                WorkerEvent::Update(update) => self.update = update,
                WorkerEvent::Exited => {
                    self.exiting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        let running = self.status == ProxyStatus::Running;
        let busy = matches!(self.status, ProxyStatus::Starting | ProxyStatus::Stopping);
        let title = match self.status {
            ProxyStatus::Running => self.text("Прокси запущен", "Proxy is running"),
            ProxyStatus::Starting => self.text("Запускается…", "Starting…"),
            ProxyStatus::Stopping => self.text("Останавливается…", "Stopping…"),
            ProxyStatus::Stopped => self.text("Прокси остановлен", "Proxy is stopped"),
            ProxyStatus::Failed(_) => self.text("Не удалось запустить", "Could not start"),
        };
        egui::Frame::group(ui.style())
            .fill(ui.visuals().widgets.noninteractive.bg_fill).inner_margin(24.0).rounding(18.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(28.0).strong());
            ui.add_space(12.0);
            ui.label(if running {
                self.text("Локальный прокси готов. Подключите его в Telegram. Доступность сети проверяется самим Telegram.", "The local proxy is ready. Connect it in Telegram. Telegram checks network availability.")
            } else {
                self.text("Запустите прокси, затем добавьте его в Telegram кнопкой ниже.", "Start the proxy, then add it to Telegram using the button below.")
            });
            if let ProxyStatus::Failed(error) = &self.status {
                ui.add_space(12.0);
                ui.colored_label(ui.visuals().error_fg_color, error);
                if ui.button(self.text("Открыть журнал", "Open log")).clicked() { self.send(WorkerCommand::OpenLogs); }
            }
            ui.add_space(20.0);
            let action = if running { self.text("Остановить прокси", "Stop proxy") } else { self.text("Запустить прокси", "Start proxy") };
            if ui.add_enabled(!busy, egui::Button::new(RichText::new(action).color(Color32::WHITE)).fill(BLUE).min_size(egui::vec2(ui.available_width(), 50.0))).clicked() {
                self.send(if running { WorkerCommand::Stop } else { WorkerCommand::Restart });
            }
            let link_ready = running && !self.link.is_empty();
            ui.add_space(6.0);
            if ui.add_enabled(link_ready, egui::Button::new(Labels::new(self.language).open_telegram()).min_size(egui::vec2(ui.available_width(), 48.0))).clicked() { self.send(WorkerCommand::OpenTelegram); }
            if ui.add_enabled(link_ready, egui::Button::new(Labels::new(self.language).copy_link()).min_size(egui::vec2(ui.available_width(), 40.0))).clicked() {
                ui.output_mut(|out| out.copied_text.clone_from(&self.link));
                self.notice = Some(self.text("Ссылка скопирована", "Link copied").to_owned());
            }
        });
        ui.add_space(16.0);
        ui.label(self.text(
            "Работает только для Telegram. Другие приложения используют обычное подключение.",
            "Only Telegram uses this proxy. Other apps keep their usual connection.",
        ));
        ui.add_space(12.0);
        ui.weak(format!(
            "{}:{}",
            self.saved_config.host, self.saved_config.port
        ));
        #[cfg(windows)]
        ui.weak(self.text(
            "Закрытие окна оставляет прокси в трее. Для завершения выберите «Выйти».",
            "Closing this window keeps the proxy in the tray. Choose Exit to stop the app.",
        ));
    }

    fn save_settings(&mut self) {
        let result = (|| -> Result<()> {
            // Reload before applying edited fields, retaining unknown legacy settings.
            let latest = DesktopConfig::load_or_create(&self.paths.config)?;
            let mut updated = self.config.clone();
            updated.extra = latest.extra;
            updated.cfproxy_worker_domain = split_domains(&self.workers);
            updated.cfproxy_user_domain = split_domains(&self.domains);
            updated.to_proxy_config()?;
            updated.save_atomic(&self.paths.config)?;
            self.language = Language::from_code(&updated.language);
            self.saved_config = updated.clone();
            self.config = updated;
            Ok(())
        })();
        self.notice = Some(match result {
            Ok(()) => self
                .text(
                    "Сохранено. Настройки применятся при следующем запуске прокси.",
                    "Saved. Settings apply the next time the proxy starts.",
                )
                .to_owned(),
            Err(error) => format!(
                "{}: {error:#}",
                self.text("Не удалось сохранить", "Could not save")
            ),
        });
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        let editable = matches!(self.status, ProxyStatus::Stopped | ProxyStatus::Failed(_));
        ui.heading(self.text("Настройки подключения", "Connection settings"));
        ui.label(self.text(
            "Обычно менять ничего не нужно.",
            "The defaults work for most users.",
        ));
        if !editable {
            ui.label(self.text(
                "Сначала остановите прокси на главном экране.",
                "Stop the proxy on the home screen before editing.",
            ));
        }
        if self.config != self.saved_config
            || split_domains(&self.workers) != self.saved_config.cfproxy_worker_domain
            || split_domains(&self.domains) != self.saved_config.cfproxy_user_domain
        {
            ui.label(self.text(
                "Есть несохранённые изменения. Нажмите «Сохранить настройки» ниже.",
                "You have unsaved changes. Choose Save settings below.",
            ));
        }
        ui.add_space(16.0);
        ui.add_enabled_ui(editable, |ui| {
            ui.label(self.text("Адрес прослушивания", "Listen address"));
            ui.text_edit_singleline(&mut self.config.host);
            ui.weak(self.text(
                "127.0.0.1 — только этот компьютер. 0.0.0.0 — доступ из сети.",
                "127.0.0.1 — this computer only. 0.0.0.0 — network access.",
            ));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(self.text("Порт", "Port"));
                ui.add(egui::DragValue::new(&mut self.config.port).clamp_range(1..=65535));
            });
            ui.add_space(10.0);
            ui.label(self.text("Ключ подключения", "Connection key"));
            ui.add(
                egui::TextEdit::singleline(&mut self.config.secret)
                    .password(!self.secret_visible)
                    .desired_width(f32::INFINITY),
            );
            ui.weak(self.text(
                "При смене ключа добавьте прокси в Telegram заново.",
                "After changing the key, add the proxy to Telegram again.",
            ));
            let reveal = self.text("Показать ключ", "Show key");
            ui.checkbox(&mut self.secret_visible, reveal);
            ui.add_space(12.0);
            let cf = self.text("Резервный маршрут Cloudflare", "Cloudflare fallback");
            ui.checkbox(&mut self.config.cfproxy, cf);
            let worker = self.text(
                "Использовать свои Cloudflare Worker",
                "Use custom Cloudflare Workers",
            );
            ui.checkbox(&mut self.config.cfproxy_worker_enabled, worker);
            ui.label(self.text(
                "Worker-домены через запятую",
                "Worker domains, separated by commas",
            ));
            ui.add(
                egui::TextEdit::multiline(&mut self.workers)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
            let cf_custom = self.text("Использовать свои CF-домены", "Use custom CF domains");
            ui.checkbox(&mut self.config.cfproxy_user_domain_enabled, cf_custom);
            ui.add(
                egui::TextEdit::multiline(&mut self.domains)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.label(self.text("Размер пула", "Pool size"));
                ui.add(egui::DragValue::new(&mut self.config.pool_size).clamp_range(0..=128));
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(self.text("Язык", "Language"));
                ui.selectable_value(&mut self.config.language, "ru".into(), "Русский");
                ui.selectable_value(&mut self.config.language, "en".into(), "English");
            });
            let updates = self.text(
                "Проверять обновления при запуске",
                "Check for updates on startup",
            );
            ui.checkbox(&mut self.config.check_updates, updates);
            ui.add_space(16.0);
            if ui
                .add(
                    egui::Button::new(self.text("Сохранить настройки", "Save settings"))
                        .min_size(egui::vec2(ui.available_width(), 48.0)),
                )
                .clicked()
            {
                self.save_settings();
            }
        });
    }
}

fn split_domains(value: &str) -> Vec<String> {
    value
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

impl eframe::App for ControlWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive(ctx);
        // Keep both system themes in the same visual language as the mobile app.
        let mut style = (*ctx.style()).clone();
        let dark = style.visuals.dark_mode;
        style.visuals.panel_fill = if dark {
            Color32::from_rgb(16, 27, 43)
        } else {
            Color32::from_rgb(242, 245, 250)
        };
        style.visuals.override_text_color = Some(if dark {
            Color32::from_rgb(236, 242, 250)
        } else {
            Color32::from_rgb(23, 43, 71)
        });
        style.visuals.widgets.noninteractive.bg_fill = if dark {
            Color32::from_rgb(25, 40, 60)
        } else {
            Color32::WHITE
        };
        style.visuals.widgets.inactive.rounding = egui::Rounding::same(12.0);
        style.visuals.widgets.hovered.rounding = egui::Rounding::same(12.0);
        style.visuals.widgets.active.rounding = egui::Rounding::same(12.0);
        ctx.set_style(style);
        #[cfg(windows)]
        {
            if let Some(tray) = &self.tray {
                let _ = tray.set_tooltip(Some(Labels::new(self.language).status(&self.status)));
            }
            while let Ok(event) = self.tray_events.try_recv() {
                match event.as_str() {
                    "show" => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    "settings" => {
                        self.settings = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    "restart" => self.send(WorkerCommand::Restart),
                    "logs" => self.send(WorkerCommand::OpenLogs),
                    "exit" => {
                        self.exiting = true;
                        self.send(WorkerCommand::Exit);
                    }
                    _ => {}
                }
            }
            if ctx.input(|input| input.viewport().close_requested())
                && !self.exiting
                && self.tray.is_some()
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }
        ctx.request_repaint_after(Duration::from_millis(200));
        egui::CentralPanel::default().frame(egui::Frame::central_panel(&ctx.style()).inner_margin(28.0)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("TG WS Proxy").size(26.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| { egui::widgets::global_dark_light_mode_switch(ui); });
            });
            ui.weak(self.text("Telegram через локальный прокси", "Telegram through a local proxy"));
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                let home = self.text("Подключение", "Connection"); let settings = self.text("Настройки", "Settings");
                if ui.selectable_label(!self.settings, home).clicked() { self.settings = false; self.notice = None; }
                if ui.selectable_label(self.settings, settings).clicked() { self.settings = true; self.notice = None; }
            });
            ui.add_space(16.0);
            egui::ScrollArea::vertical().id_source("content").show(ui, |ui| {
                if self.settings { self.settings(ui); } else { self.home(ui); }
                ui.add_space(16.0);
                if let Some(notice) = &self.notice { ui.label(notice); }
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui.button(self.text("Журнал", "Log")).clicked() { self.send(WorkerCommand::OpenLogs); }
                    let enabled = !matches!(self.update, UpdateState::Checking | UpdateState::Downloading { .. });
                    if ui.add_enabled(enabled, egui::Button::new(Labels::new(self.language).update(&self.update))).clicked() { self.update_action(); }
                    if ui.button(self.text("Выйти", "Exit")).clicked() { self.exiting = true; self.send(WorkerCommand::Exit); }
                });
                if matches!(self.update, UpdateState::Failed) {
                    ui.label(self.text("Проверка или загрузка не удалась. Проверьте доступ к GitHub и повторите. Подробности — в журнале.", "Check or download failed. Check GitHub access and retry. See the log for details."));
                }
                ui.add_space(12.0);
                ui.weak(format!("{} {}", self.text("Версия", "Version"), env!("CARGO_PKG_VERSION")));
            });
        });
    }
}

#[cfg(windows)]
fn create_tray(
    language: Language,
    ctx: &egui::Context,
    events: std::sync::mpsc::Sender<String>,
) -> Result<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuItem};

    // Restore through an event callback: hidden windows do not reliably redraw,
    // so polling the tray from App::update alone can strand the application.
    let wake = ctx.clone();
    let menu_events = events.clone();
    tray_icon::menu::MenuEvent::set_event_handler(Some(
        move |event: tray_icon::menu::MenuEvent| {
            let _ = menu_events.send(event.id.0);
            wake.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            wake.send_viewport_cmd(egui::ViewportCommand::Focus);
            wake.request_repaint();
        },
    ));
    let wake = ctx.clone();
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(event, tray_icon::TrayIconEvent::DoubleClick { .. }) {
            let _ = events.send("show".into());
            wake.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            wake.send_viewport_cmd(egui::ViewportCommand::Focus);
            wake.request_repaint();
        }
    }));
    let menu = Menu::new();
    menu.append(&MenuItem::with_id(
        "show",
        if language == Language::Russian {
            "Открыть TG WS Proxy"
        } else {
            "Open TG WS Proxy"
        },
        true,
        None,
    ))?;
    menu.append(&MenuItem::with_id(
        "exit",
        Labels::new(language).exit(),
        true,
        None,
    ))?;
    let labels = Labels::new(language);
    menu.append(&MenuItem::with_id(
        "settings",
        labels.open_settings(),
        true,
        None,
    ))?;
    menu.append(&MenuItem::with_id("restart", labels.restart(), true, None))?;
    menu.append(&MenuItem::with_id("logs", labels.open_logs(), true, None))?;
    let bitmap = super::icon::render(64);
    let icon = tray_icon::Icon::from_rgba(bitmap.rgba, bitmap.width, bitmap.height)?;
    Ok(tray_icon::TrayIconBuilder::new()
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_tooltip("TG WS Proxy")
        .build()?)
}

pub(super) fn run(
    commands: &mpsc::UnboundedSender<WorkerCommand>,
    events: mpsc::UnboundedReceiver<WorkerEvent>,
    language: Language,
) -> Result<()> {
    let paths = AppPaths::discover()?;
    let config = DesktopConfig::load_or_create(&paths.config)?;
    #[cfg(windows)]
    let (tray_sender, tray_events) = std::sync::mpsc::channel();
    let app = ControlWindow {
        commands: commands.clone(),
        events,
        language,
        status: ProxyStatus::Starting,
        link: String::new(),
        update: UpdateState::Idle,
        workers: config.cfproxy_worker_domain.join(", "),
        domains: config.cfproxy_user_domain.join(", "),
        saved_config: config.clone(),
        config,
        paths,
        settings: false,
        secret_visible: false,
        notice: None,
        exiting: false,
        #[cfg(windows)]
        tray: None,
        #[cfg(windows)]
        tray_events,
    };
    let bitmap = super::icon::render(64);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 760.0])
            .with_icon(egui::IconData {
                rgba: bitmap.rgba,
                width: bitmap.width,
                height: bitmap.height,
            })
            .with_min_inner_size([380.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "TG WS Proxy",
        options,
        Box::new(move |context| {
            if app.config.appearance.eq_ignore_ascii_case("light") {
                context.egui_ctx.set_visuals(egui::Visuals::light());
            }
            if app.config.appearance.eq_ignore_ascii_case("dark") {
                context.egui_ctx.set_visuals(egui::Visuals::dark());
            }
            let mut style = (*context.egui_ctx.style()).clone();
            style.spacing.item_spacing = egui::vec2(10.0, 8.0);
            style.spacing.button_padding = egui::vec2(14.0, 10.0);
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(16.0));
            context.egui_ctx.set_style(style);
            #[cfg(windows)]
            let app = {
                let mut app = app;
                match create_tray(language, &context.egui_ctx, tray_sender) {
                    Ok(tray) => app.tray = Some(tray),
                    Err(error) => app.notice = Some(format!("Tray: {error}")),
                }
                app
            };
            Box::new(app)
        }),
    )
    .map_err(|error| anyhow::anyhow!("desktop window failed: {error}"))
    .context("failed to run desktop control window")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_reject_invalid_input_and_preserve_unknown_config_fields() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            directory: directory.path().to_path_buf(),
            config: directory.path().join("config.json"),
            log: directory.path().join("proxy.log"),
            lock: directory.path().join("desktop.lock"),
        };
        let initial = DesktopConfig::load_or_create(&paths.config).unwrap();
        let (commands, _) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ControlWindow {
            commands,
            events,
            language: Language::Russian,
            status: ProxyStatus::Stopped,
            link: String::new(),
            update: UpdateState::Idle,
            config: initial.clone(),
            saved_config: initial.clone(),
            paths,
            settings: true,
            secret_visible: false,
            workers: String::new(),
            domains: String::new(),
            notice: None,
            exiting: false,
            #[cfg(windows)]
            tray: None,
            #[cfg(windows)]
            tray_events: std::sync::mpsc::channel().1,
        };
        app.config.secret = "invalid".into();
        app.save_settings();
        assert_eq!(
            DesktopConfig::load_or_create(&app.paths.config).unwrap(),
            initial
        );
        app.config = initial.clone();
        let mut external = initial.clone();
        external
            .extra
            .insert("future_setting".into(), serde_json::json!(42));
        external.save_atomic(&app.paths.config).unwrap();
        app.workers = "one.workers.dev, two.workers.dev".into();
        app.config.cfproxy_worker_enabled = true;
        app.save_settings();
        let saved = DesktopConfig::load_or_create(&app.paths.config).unwrap();
        assert_eq!(saved.secret, initial.secret);
        assert_eq!(saved.extra["future_setting"], 42);
        assert_eq!(
            saved.cfproxy_worker_domain,
            ["one.workers.dev", "two.workers.dev"]
        );
    }
}
