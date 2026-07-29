use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use futures_util::stream::{FuturesUnordered, StreamExt};
use rustls::ClientConfig;
use tokio::sync::{Mutex, Notify};
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, info};

use crate::config::ProxyConfig;
use crate::stats::Stats;
use crate::websocket::{RawWebSocket, WebSocketError};

const MAX_AGE: Duration = Duration::from_secs(120);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5);
const REFILL_BACKOFF_INITIAL: Duration = Duration::from_secs(60);
const REFILL_BACKOFF_MAX: Duration = Duration::from_secs(3600);
const IDLE_PROBE_TIMEOUT: Duration = Duration::from_millis(1);
const IDLE_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct PoolKey {
    dc: i16,
    media: bool,
}

#[derive(Clone, Debug)]
struct ConnectSpec {
    host: String,
    domains: Vec<String>,
}

struct PooledConnection {
    websocket: RawWebSocket,
    created: Instant,
}

#[derive(Clone, Copy, Debug)]
struct Backoff {
    failures: u32,
    retry_after: Instant,
}

struct PoolLifecycle {
    shutting_down: bool,
    stopped: bool,
    maintenance_started: bool,
    tasks: JoinSet<()>,
}

impl Default for PoolLifecycle {
    fn default() -> Self {
        Self {
            shutting_down: false,
            stopped: false,
            maintenance_started: false,
            tasks: JoinSet::new(),
        }
    }
}

struct PoolInner {
    config: Arc<ProxyConfig>,
    tls_config: Arc<ClientConfig>,
    stats: Arc<Stats>,
    idle: Mutex<HashMap<PoolKey, VecDeque<PooledConnection>>>,
    specs: Mutex<HashMap<PoolKey, ConnectSpec>>,
    refilling: StdMutex<HashSet<PoolKey>>,
    backoff: Mutex<HashMap<PoolKey, Backoff>>,
    prefer_fronting: StdMutex<HashSet<PoolKey>>,
    lifecycle: StdMutex<PoolLifecycle>,
    shutdown_lock: Mutex<()>,
    shutdown_complete: Notify,
}

#[derive(Clone)]
pub struct WsPool {
    inner: Arc<PoolInner>,
}

struct RefillGuard {
    inner: Arc<PoolInner>,
    key: PoolKey,
}

impl Drop for RefillGuard {
    fn drop(&mut self) {
        self.inner
            .refilling
            .lock()
            .expect("pool refill mutex poisoned")
            .remove(&self.key);
    }
}

impl WsPool {
    pub fn new(config: Arc<ProxyConfig>, tls_config: Arc<ClientConfig>, stats: Arc<Stats>) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                config,
                tls_config,
                stats,
                idle: Mutex::new(HashMap::new()),
                specs: Mutex::new(HashMap::new()),
                refilling: StdMutex::new(HashSet::new()),
                backoff: Mutex::new(HashMap::new()),
                prefer_fronting: StdMutex::new(HashSet::new()),
                lifecycle: StdMutex::new(PoolLifecycle::default()),
                shutdown_lock: Mutex::new(()),
                shutdown_complete: Notify::new(),
            }),
        }
    }

    pub async fn warm_up(&self) {
        if self.is_shutting_down() {
            return;
        }
        for (&dc, target) in &self.inner.config.dc_redirects {
            for media in [false, true] {
                let domains = websocket_domains(dc, media);
                self.register(PoolKey { dc, media }, target, domains).await;
                self.schedule_refill(PoolKey { dc, media }).await;
            }
        }
        info!(
            dc_count = self.inner.config.dc_redirects.len(),
            "WebSocket pool warm-up started"
        );
    }

    /// Starts pool-owned maintenance.
    ///
    /// The returned observer completes after [`Self::shutdown`]. Aborting it does not stop
    /// maintenance; lifecycle ownership stays with the pool.
    #[must_use]
    pub fn start_maintenance(&self) -> JoinHandle<()> {
        let pool = self.clone();
        {
            let mut lifecycle = self
                .inner
                .lifecycle
                .lock()
                .expect("pool lifecycle mutex poisoned");
            reap_finished_tasks(&mut lifecycle.tasks);
            if !lifecycle.shutting_down && !lifecycle.maintenance_started {
                lifecycle.maintenance_started = true;
                lifecycle.tasks.spawn(async move {
                    let mut interval = tokio::time::interval(MAINTENANCE_INTERVAL);
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        interval.tick().await;
                        if pool.is_shutting_down() {
                            return;
                        }
                        pool.maintain().await;
                    }
                });
            }
        }

        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            loop {
                let notified = inner.shutdown_complete.notified();
                if inner
                    .lifecycle
                    .lock()
                    .expect("pool lifecycle mutex poisoned")
                    .stopped
                {
                    return;
                }
                notified.await;
            }
        })
    }

    pub async fn get(
        &self,
        dc: i16,
        media: bool,
        target: IpAddr,
        domains: Vec<String>,
        allow_refill: bool,
    ) -> Option<RawWebSocket> {
        if self.is_shutting_down() {
            return None;
        }
        let key = PoolKey { dc, media };
        self.register(key, &target, domains).await;

        let hit = loop {
            let candidate = {
                let mut idle = self.inner.idle.lock().await;
                idle.entry(key).or_default().pop_front()
            };
            let Some(mut connection) = candidate else {
                break None;
            };
            if connection_is_usable(&mut connection, Instant::now()).await {
                break Some(connection.websocket);
            }
            connection.websocket.close().await;
        };

        if self.is_shutting_down() {
            if let Some(websocket) = hit {
                websocket.close().await;
            }
            return None;
        }

        if hit.is_some() {
            Stats::increment(&self.inner.stats.pool_hits);
            self.reset_backoff(key).await;
        } else {
            Stats::increment(&self.inner.stats.pool_misses);
        }
        if allow_refill {
            self.schedule_refill(key).await;
        }
        hit
    }

    pub async fn report_success(&self, dc: i16, media: bool) {
        self.reset_backoff(PoolKey { dc, media }).await;
    }

    async fn register(&self, key: PoolKey, target: &IpAddr, domains: Vec<String>) {
        self.inner.specs.lock().await.insert(
            key,
            ConnectSpec {
                host: target.to_string(),
                domains,
            },
        );
        self.inner.idle.lock().await.entry(key).or_default();
    }

    async fn schedule_refill(&self, key: PoolKey) {
        if self.inner.config.pool_size == 0 || self.is_shutting_down() {
            return;
        }
        if let Some(backoff) = self.inner.backoff.lock().await.get(&key) {
            if Instant::now() < backoff.retry_after {
                return;
            }
        }
        {
            let mut refilling = self
                .inner
                .refilling
                .lock()
                .expect("pool refill mutex poisoned");
            if !refilling.insert(key) {
                return;
            }
        }

        let pool = self.clone();
        let inner = Arc::clone(&self.inner);
        let spawned = self.spawn_owned(async move {
            let _guard = RefillGuard { inner, key };
            pool.refill(key).await;
        });
        if !spawned {
            self.inner
                .refilling
                .lock()
                .expect("pool refill mutex poisoned")
                .remove(&key);
        }
    }

    async fn refill(&self, key: PoolKey) {
        let Some(spec) = self.inner.specs.lock().await.get(&key).cloned() else {
            return;
        };
        let needed = {
            let idle = self.inner.idle.lock().await;
            self.inner
                .config
                .pool_size
                .saturating_sub(idle.get(&key).map_or(0, VecDeque::len))
        };
        if needed == 0 {
            return;
        }

        let mut attempts = FuturesUnordered::new();
        for _ in 0..needed {
            attempts.push(self.connect_one(key, &spec));
        }

        let mut connected = Vec::new();
        while let Some(result) = attempts.next().await {
            if let Some(websocket) = result {
                connected.push(PooledConnection {
                    websocket,
                    created: Instant::now(),
                });
            }
        }

        if connected.is_empty() {
            let mut backoffs = self.inner.backoff.lock().await;
            let failures = backoffs
                .get(&key)
                .map_or(1, |state| state.failures.saturating_add(1));
            let exponent = failures.saturating_sub(1).min(6);
            let delay_seconds = REFILL_BACKOFF_INITIAL
                .as_secs()
                .saturating_mul(1_u64 << exponent)
                .min(REFILL_BACKOFF_MAX.as_secs());
            backoffs.insert(
                key,
                Backoff {
                    failures,
                    retry_after: Instant::now() + Duration::from_secs(delay_seconds),
                },
            );
            info!(
                dc = key.dc,
                media = key.media,
                delay_seconds,
                "WebSocket pool refill failed"
            );
            return;
        }

        self.reset_backoff(key).await;
        let count = connected.len();
        let mut idle = self.inner.idle.lock().await;
        let bucket = idle.entry(key).or_default();
        let capacity = self.inner.config.pool_size.saturating_sub(bucket.len());
        let mut overflow = connected.split_off(capacity.min(connected.len()));
        bucket.extend(connected);
        drop(idle);
        for connection in overflow.drain(..) {
            connection.websocket.close().await;
        }
        debug!(
            dc = key.dc,
            media = key.media,
            count,
            "WebSocket pool refilled"
        );
    }

    async fn connect_one(&self, key: PoolKey, spec: &ConnectSpec) -> Option<RawWebSocket> {
        let prefer_fronting = self.prefers_fronting(key);
        for domain in &spec.domains {
            if prefer_fronting {
                if let Some(websocket) = self.connect_fronted(key, spec, domain).await {
                    return Some(websocket);
                }
            }

            match self.connect(&spec.host, domain, None, "/apiws").await {
                Ok(websocket) => {
                    self.set_fronting_preference(key, false);
                    return Some(websocket);
                }
                Err(error) if error.is_redirect() => {}
                Err(WebSocketError::Timeout) if !prefer_fronting => {
                    if let Some(websocket) = self.connect_fronted(key, spec, domain).await {
                        return Some(websocket);
                    }
                }
                Err(_) => {}
            }
        }
        None
    }

    async fn connect_fronted(
        &self,
        key: PoolKey,
        spec: &ConnectSpec,
        domain: &str,
    ) -> Option<RawWebSocket> {
        let websocket = self
            .connect(&spec.host, domain, Some("sprinthost.ru"), "/apiws")
            .await
            .ok()?;
        Stats::increment(&self.inner.stats.connections_fronting);
        self.set_fronting_preference(key, true);
        Some(websocket)
    }

    async fn connect(
        &self,
        host: &str,
        domain: &str,
        sni: Option<&str>,
        path: &str,
    ) -> Result<RawWebSocket, WebSocketError> {
        RawWebSocket::connect(
            host,
            domain,
            sni,
            path,
            Duration::from_secs(8),
            Arc::clone(&self.inner.tls_config),
            self.inner.config.buffer_size,
            self.inner.config.max_ws_frame_size,
            true,
        )
        .await
    }

    async fn maintain(&self) {
        let now = Instant::now();
        let mut stale = Vec::new();
        let keys = {
            let mut idle = self.inner.idle.lock().await;
            for bucket in idle.values_mut() {
                let mut ready = VecDeque::with_capacity(bucket.len());
                while let Some(connection) = bucket.pop_front() {
                    if now.duration_since(connection.created) >= MAX_AGE
                        || connection.websocket.is_closed()
                    {
                        stale.push(connection.websocket);
                    } else {
                        ready.push_back(connection);
                    }
                }
                *bucket = ready;
            }
            idle.keys().copied().collect::<Vec<_>>()
        };
        for websocket in stale {
            websocket.close().await;
        }
        for key in keys {
            self.schedule_refill(key).await;
        }
    }

    async fn reset_backoff(&self, key: PoolKey) {
        self.inner.backoff.lock().await.remove(&key);
    }

    fn prefers_fronting(&self, key: PoolKey) -> bool {
        self.inner
            .prefer_fronting
            .lock()
            .expect("pool fronting mutex poisoned")
            .contains(&key)
    }

    fn set_fronting_preference(&self, key: PoolKey, preferred: bool) {
        let mut preferences = self
            .inner
            .prefer_fronting
            .lock()
            .expect("pool fronting mutex poisoned");
        if preferred {
            preferences.insert(key);
        } else {
            preferences.remove(&key);
        }
    }

    fn is_shutting_down(&self) -> bool {
        self.inner
            .lifecycle
            .lock()
            .expect("pool lifecycle mutex poisoned")
            .shutting_down
    }

    fn spawn_owned<F>(&self, task: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut lifecycle = self
            .inner
            .lifecycle
            .lock()
            .expect("pool lifecycle mutex poisoned");
        reap_finished_tasks(&mut lifecycle.tasks);
        if lifecycle.shutting_down {
            return false;
        }
        lifecycle.tasks.spawn(task);
        true
    }

    /// Cancels and joins maintenance/refill work, then closes every idle socket.
    pub async fn shutdown(&self) {
        let _shutdown_guard = self.inner.shutdown_lock.lock().await;
        if self
            .inner
            .lifecycle
            .lock()
            .expect("pool lifecycle mutex poisoned")
            .stopped
        {
            return;
        }

        let mut tasks = {
            let mut lifecycle = self
                .inner
                .lifecycle
                .lock()
                .expect("pool lifecycle mutex poisoned");
            lifecycle.shutting_down = true;
            std::mem::replace(&mut lifecycle.tasks, JoinSet::new())
        };
        tasks.abort_all();
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                if !error.is_cancelled() {
                    debug!(%error, "WebSocket pool task failed during shutdown");
                }
            }
        }

        let idle = {
            let mut idle = self.inner.idle.lock().await;
            idle.drain()
                .flat_map(|(_, bucket)| bucket)
                .collect::<Vec<_>>()
        };
        let mut closes = FuturesUnordered::new();
        for connection in idle {
            closes.push(async move {
                let _ =
                    tokio::time::timeout(IDLE_CLOSE_TIMEOUT, connection.websocket.close()).await;
            });
        }
        while closes.next().await.is_some() {}

        self.inner.specs.lock().await.clear();
        self.inner.backoff.lock().await.clear();
        self.inner
            .refilling
            .lock()
            .expect("pool refill mutex poisoned")
            .clear();
        self.inner
            .prefer_fronting
            .lock()
            .expect("pool fronting mutex poisoned")
            .clear();
        self.inner
            .lifecycle
            .lock()
            .expect("pool lifecycle mutex poisoned")
            .stopped = true;
        self.inner.shutdown_complete.notify_waiters();
    }
}

fn reap_finished_tasks(tasks: &mut JoinSet<()>) {
    while let Some(result) = tasks.try_join_next() {
        if let Err(error) = result {
            if !error.is_cancelled() {
                debug!(%error, "WebSocket pool task failed");
            }
        }
    }
}

fn connection_is_stale(connection: &PooledConnection, now: Instant) -> bool {
    connection_state_is_stale(connection.created, connection.websocket.is_closed(), now)
}

fn connection_state_is_stale(created: Instant, closed: bool, now: Instant) -> bool {
    now.duration_since(created) >= MAX_AGE || closed
}

async fn connection_is_usable(connection: &mut PooledConnection, now: Instant) -> bool {
    if connection_is_stale(connection, now) {
        return false;
    }
    tokio::time::timeout(IDLE_PROBE_TIMEOUT, connection.websocket.receiver.receive())
        .await
        .is_err()
}

#[must_use]
pub fn websocket_domains(dc: i16, media: bool) -> Vec<String> {
    let effective_dc = if dc == 203 { 2 } else { dc };
    let primary = format!("kws{effective_dc}.web.telegram.org");
    let media_domain = format!("kws{effective_dc}-1.web.telegram.org");
    if media {
        vec![media_domain, primary]
    } else {
        vec![primary, media_domain]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::config::tls_config;

    fn test_pool(pool_size: usize) -> WsPool {
        let config = ProxyConfig {
            pool_size,
            ..ProxyConfig::default()
        };
        WsPool::new(Arc::new(config), tls_config(), Arc::new(Stats::default()))
    }

    #[test]
    fn maps_cdn_dc_to_dc2_domains() {
        assert_eq!(
            websocket_domains(203, false),
            ["kws2.web.telegram.org", "kws2-1.web.telegram.org"]
        );
    }

    #[test]
    fn prefers_media_domain_for_media_connections() {
        assert_eq!(
            websocket_domains(4, true),
            ["kws4-1.web.telegram.org", "kws4.web.telegram.org"]
        );
    }

    #[test]
    fn fronting_preference_is_scoped_to_pool_key() {
        let pool = test_pool(0);
        let dc2_regular = PoolKey {
            dc: 2,
            media: false,
        };
        let dc2_media = PoolKey { dc: 2, media: true };
        let dc4_regular = PoolKey {
            dc: 4,
            media: false,
        };

        pool.set_fronting_preference(dc2_regular, true);

        assert!(pool.prefers_fronting(dc2_regular));
        assert!(!pool.prefers_fronting(dc2_media));
        assert!(!pool.prefers_fronting(dc4_regular));
    }

    #[test]
    fn closed_or_expired_connection_state_is_stale() {
        let now = Instant::now();
        let max_age_ago = now.checked_sub(MAX_AGE).expect("test instant underflow");
        assert!(!connection_state_is_stale(
            max_age_ago + Duration::from_millis(1),
            false,
            now
        ));
        assert!(connection_state_is_stale(max_age_ago, false, now));
        assert!(connection_state_is_stale(now, true, now));
    }

    #[tokio::test]
    async fn maintenance_is_owned_and_shutdown_is_idempotent() {
        let pool = test_pool(0);
        let observer_one = pool.start_maintenance();
        let observer_two = pool.start_maintenance();
        assert_eq!(
            pool.inner
                .lifecycle
                .lock()
                .expect("pool lifecycle mutex poisoned")
                .tasks
                .len(),
            1
        );

        tokio::time::timeout(Duration::from_secs(1), pool.shutdown())
            .await
            .expect("pool shutdown timed out");
        observer_one.await.expect("shutdown observer failed");
        observer_two.await.expect("shutdown observer failed");
        tokio::time::timeout(Duration::from_secs(1), pool.shutdown())
            .await
            .expect("repeated pool shutdown timed out");

        let lifecycle = pool
            .inner
            .lifecycle
            .lock()
            .expect("pool lifecycle mutex poisoned");
        assert!(lifecycle.shutting_down);
        assert!(lifecycle.stopped);
        assert!(lifecycle.tasks.is_empty());
    }

    #[tokio::test]
    async fn shutdown_cancels_and_awaits_owned_tasks() {
        struct DropSignal(Arc<AtomicBool>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let pool = test_pool(0);
        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = Arc::clone(&dropped);
        assert!(pool.spawn_owned(async move {
            let _signal = DropSignal(task_dropped);
            std::future::pending::<()>().await;
        }));
        tokio::task::yield_now().await;

        pool.shutdown().await;

        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn refill_is_not_started_after_shutdown() {
        let pool = test_pool(1);
        let key = PoolKey {
            dc: 2,
            media: false,
        };

        pool.shutdown().await;
        pool.schedule_refill(key).await;

        assert!(
            pool.inner
                .refilling
                .lock()
                .expect("pool refill mutex poisoned")
                .is_empty()
        );
        assert!(
            pool.inner
                .lifecycle
                .lock()
                .expect("pool lifecycle mutex poisoned")
                .tasks
                .is_empty()
        );
    }
}
