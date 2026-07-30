use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::{Proxy, ProxyConfig};

const STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed(String),
}

/// Owns one proxy runtime and reports actual listener readiness.
///
/// A new controller must be created for a restart. This makes it impossible
/// for a desktop frontend to start a second listener before the previous
/// runtime has finished shutting down.
pub struct DesktopProxyController {
    status_tx: watch::Sender<ProxyStatus>,
    status_rx: watch::Receiver<ProxyStatus>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<()>>>,
}

impl DesktopProxyController {
    pub async fn start(config: ProxyConfig) -> Result<Self> {
        let proxy = Proxy::new(config)?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (status_tx, status_rx) = watch::channel(ProxyStatus::Starting);
        let ready_status = status_tx.clone();
        let final_status = status_tx.clone();

        let mut task = tokio::spawn(async move {
            let result = proxy
                .run_until_ready(
                    async {
                        let _ = shutdown_rx.await;
                    },
                    move || {
                        ready_status.send_replace(ProxyStatus::Running);
                        let _ = ready_tx.send(());
                    },
                )
                .await;
            match &result {
                Ok(()) => final_status.send_replace(ProxyStatus::Stopped),
                Err(error) => final_status.send_replace(ProxyStatus::Failed(error.to_string())),
            };
            result
        });

        tokio::select! {
            biased;
            result = &mut task => {
                return Err(startup_task_error(result));
            }
            ready = ready_rx => {
                if ready.is_err() {
                    return Err(startup_task_error(task.await));
                }
            }
        }

        Ok(Self {
            status_tx,
            status_rx,
            shutdown: Some(shutdown_tx),
            task: Some(task),
        })
    }

    #[must_use]
    pub fn status(&self) -> ProxyStatus {
        self.status_rx.borrow().clone()
    }

    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<ProxyStatus> {
        self.status_rx.clone()
    }

    pub async fn stop(mut self) -> Result<()> {
        self.status_tx.send_replace(ProxyStatus::Stopping);
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        let Ok(result) = tokio::time::timeout(STOP_TIMEOUT, &mut task).await else {
            task.abort();
            let _ = task.await;
            return Err(anyhow!(
                "proxy runtime did not stop within {} seconds",
                STOP_TIMEOUT.as_secs()
            ));
        };
        result.context("proxy runtime task panicked")?
    }
}

impl Drop for DesktopProxyController {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        // Dropping a JoinHandle detaches it. The shutdown signal lets the
        // runtime finish its owned client/pool cleanup asynchronously.
    }
}

fn startup_task_error(
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> anyhow::Error {
    match result {
        Ok(Ok(())) => anyhow!("proxy stopped before reporting listener readiness"),
        Ok(Err(error)) => error,
        Err(error) => anyhow!("proxy runtime task panicked during startup: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener as StdTcpListener};

    use super::*;

    fn available_port() -> u16 {
        StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[tokio::test]
    async fn reports_running_only_after_bind_and_releases_port_on_stop() {
        let port = available_port();
        let config = ProxyConfig {
            host: Ipv4Addr::LOCALHOST.to_string(),
            port,
            pool_size: 0,
            fallback_cfproxy: false,
            ..ProxyConfig::default()
        };
        let controller = DesktopProxyController::start(config).await.unwrap();
        assert_eq!(controller.status(), ProxyStatus::Running);
        assert!(StdTcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err());

        controller.stop().await.unwrap();
        StdTcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .expect("listener port must be released after a completed stop");
    }

    #[tokio::test]
    async fn bind_failure_is_returned_instead_of_fake_running_state() {
        let reservation = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let config = ProxyConfig {
            host: Ipv4Addr::LOCALHOST.to_string(),
            port: reservation.local_addr().unwrap().port(),
            pool_size: 0,
            fallback_cfproxy: false,
            ..ProxyConfig::default()
        };

        for _ in 0..32 {
            let Err(error) = DesktopProxyController::start(config.clone()).await else {
                panic!("occupied port must fail startup");
            };
            assert!(
                error.to_string().contains("failed to listen"),
                "unexpected startup error: {error:#}"
            );
        }
    }
}
