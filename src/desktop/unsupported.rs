use anyhow::{Result, bail};
use tokio::sync::mpsc;

use super::{Language, WorkerCommand, WorkerEvent};

pub(super) fn run(
    _command_tx: mpsc::UnboundedSender<WorkerCommand>,
    _event_rx: mpsc::UnboundedReceiver<WorkerEvent>,
    _language: Language,
) -> Result<()> {
    bail!("the desktop tray supports Linux, Windows, and macOS")
}
