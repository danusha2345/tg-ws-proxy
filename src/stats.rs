use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Stats {
    pub connections_total: AtomicU64,
    pub connections_active: AtomicU64,
    pub connections_ws: AtomicU64,
    pub connections_tcp_fallback: AtomicU64,
    pub connections_cfproxy: AtomicU64,
    pub connections_fronting: AtomicU64,
    pub connections_bad: AtomicU64,
    pub connections_masked: AtomicU64,
    pub ws_errors: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
    pub pool_hits: AtomicU64,
    pub pool_misses: AtomicU64,
}

impl Stats {
    pub fn increment(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(counter: &AtomicU64, value: usize) {
        counter.fetch_add(value as u64, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            total: self.connections_total.load(Ordering::Relaxed),
            active: self.connections_active.load(Ordering::Relaxed),
            websocket: self.connections_ws.load(Ordering::Relaxed),
            tcp_fallback: self.connections_tcp_fallback.load(Ordering::Relaxed),
            cloudflare: self.connections_cfproxy.load(Ordering::Relaxed),
            bad: self.connections_bad.load(Ordering::Relaxed),
            bytes_up: self.bytes_up.load(Ordering::Relaxed),
            bytes_down: self.bytes_down.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StatsSnapshot {
    pub total: u64,
    pub active: u64,
    pub websocket: u64,
    pub tcp_fallback: u64,
    pub cloudflare: u64,
    pub bad: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}
