#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    tg_ws_proxy::desktop::run()
}
