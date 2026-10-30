//! Staggered address racing for hosts with a broken IPv4 or IPv6 route.
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::net::{TcpStream, lookup_host};
use tokio::time::{Instant, sleep, timeout};

const ADDRESS_DELAY: Duration = Duration::from_millis(250);
const ADDRESS_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PENDING: usize = 4;

pub(crate) async fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    let addresses = lookup_host((host, port)).await?.collect();
    race_addresses(addresses, TcpStream::connect).await
}

async fn race_addresses<T, F, Fut>(addresses: Vec<SocketAddr>, connect: F) -> io::Result<T>
where
    F: Fn(SocketAddr) -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    let first_ipv6 = addresses.first().is_some_and(SocketAddr::is_ipv6);
    let (preferred, alternate): (Vec<_>, Vec<_>) = addresses
        .into_iter()
        .partition(|address| address.is_ipv6() == first_ipv6);
    let mut preferred = preferred.into_iter();
    let mut alternate = alternate.into_iter();
    let mut ordered = Vec::new();
    loop {
        let pair = (preferred.next(), alternate.next());
        if pair == (None, None) {
            break;
        }
        ordered.extend(pair.0);
        ordered.extend(pair.1);
    }
    let mut addresses = ordered.into_iter();
    let mut last_error = io::Error::new(io::ErrorKind::AddrNotAvailable, "host has no addresses");
    let Some(first) = addresses.next() else {
        return Err(last_error);
    };
    let attempt = |address| {
        let future = connect(address);
        async move {
            timeout(ADDRESS_TIMEOUT, future)
                .await
                .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
        }
    };
    let mut pending = FuturesUnordered::new();
    pending.push(attempt(first));
    let delay = sleep(ADDRESS_DELAY);
    tokio::pin!(delay);
    loop {
        tokio::select! {
            result = pending.next(), if !pending.is_empty() => {
                match result.expect("pending is not empty") {
                    Ok(stream) => return Ok(stream),
                    Err(error) => last_error = error,
                }
                if let Some(address) = addresses.next() {
                    pending.push(attempt(address));
                    delay.as_mut().reset(Instant::now() + ADDRESS_DELAY);
                } else if pending.is_empty() {
                    return Err(last_error);
                }
            }
            () = &mut delay, if !addresses.as_slice().is_empty() && pending.len() < MAX_PENDING => {
                pending.push(attempt(addresses.next().expect("addresses remain")));
                delay.as_mut().reset(Instant::now() + ADDRESS_DELAY);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn ipv4_bypasses_stalled_ipv6_before_second_ipv6_address() {
        let addresses = ["[::1]:443", "[::2]:443", "127.0.0.1:443"]
            .map(|address| address.parse().unwrap())
            .to_vec();
        let result = timeout(
            Duration::from_secs(1),
            race_addresses(addresses, |address| async move {
                if address.is_ipv6() {
                    std::future::pending().await
                } else {
                    Ok(address)
                }
            }),
        )
        .await
        .expect("IPv4 must not wait for IPv6 TCP timeout")
        .unwrap();
        assert!(result.is_ipv4());
    }

    #[tokio::test]
    async fn healthy_preferred_address_does_not_open_alternate_connections() {
        let started = AtomicUsize::new(0);
        let addresses = ["[::1]:443", "127.0.0.1:443"]
            .map(|address| address.parse().unwrap())
            .to_vec();
        let selected = race_addresses(addresses, |address| {
            started.fetch_add(1, Ordering::Relaxed);
            async move { Ok(address) }
        })
        .await
        .unwrap();
        assert!(selected.is_ipv6());
        assert_eq!(started.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn winning_address_cancels_stalled_competitor() {
        struct Attempt(Arc<AtomicBool>);
        impl Drop for Attempt {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let addresses = ["[::1]:443", "127.0.0.1:443"]
            .map(|address| address.parse().unwrap())
            .to_vec();
        race_addresses(addresses, |address| {
            let cancelled = Arc::clone(&cancelled);
            async move {
                if address.is_ipv6() {
                    let _attempt = Attempt(cancelled);
                    std::future::pending::<io::Result<()>>().await
                } else {
                    Ok(())
                }
            }
        })
        .await
        .unwrap();
        assert!(cancelled.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn immediate_failures_try_remaining_addresses_without_stagger_delays() {
        let addresses = (1..=8)
            .map(|port| SocketAddr::from(([127, 0, 0, 1], port)))
            .collect();
        let error = timeout(
            Duration::from_millis(100),
            race_addresses(addresses, |address| async move {
                Err::<(), _>(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    address.to_string(),
                ))
            }),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.to_string(), "127.0.0.1:8");
    }
}
