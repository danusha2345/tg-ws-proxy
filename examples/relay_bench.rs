//! Измерение коротких HTTP-ответов: только синтетические данные, без Telegram account.
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, stream};
use tg_ws_proxy::config::tls_config;

#[tokio::main]
async fn main() -> Result<()> {
    let endpoint = std::env::args()
        .nth(1)
        .context("usage: relay_bench http[s]://HOST/chunk")?;
    let endpoint = url::Url::parse(&endpoint)?;
    let timeout_seconds: u64 = std::env::args()
        .nth(2)
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(3);
    ensure!(
        (1..=30).contains(&timeout_seconds),
        "timeout must be 1..=30 seconds"
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(timeout_seconds))
        .tls_backend_preconfigured((*tls_config()).clone())
        .user_agent("tg-ws-relay-benchmark/1")
        .build()?;
    for size in [24576, 32768, 49152, 65536, 98304, 131_072, 262_144, 524_288] {
        for window in [4, 8, 16] {
            let start = Instant::now();
            let results = stream::iter(0..window.max(8) * 2)
                .map(|index| {
                    let client = &client;
                    let endpoint = &endpoint;
                    async move {
                        let mut url = endpoint.clone();
                        url.query_pairs_mut()
                            .append_pair("bytes", &size.to_string())
                            .append_pair("i", &index.to_string());
                        let mut response = client
                            .get(url)
                            .header(reqwest::header::CONNECTION, "close")
                            .send()
                            .await
                            .map_err(reqwest::Error::without_url)?;
                        ensure!(response.status().is_success(), "HTTP {}", response.status());
                        let mut received = 0;
                        while let Some(bytes) = response
                            .chunk()
                            .await
                            .map_err(reqwest::Error::without_url)?
                        {
                            received += bytes.len();
                            ensure!(received <= size, "oversized response");
                        }
                        ensure!(received == size, "wrong response size");
                        Ok::<_, anyhow::Error>(received)
                    }
                })
                .buffer_unordered(window);
            futures_util::pin_mut!(results);
            let mut bytes = 0;
            let mut failures = 0;
            let mut completed = 0;
            let mut timeouts = 0;
            while let Some(result) = results.next().await {
                completed += 1;
                match result {
                    Ok(count) => bytes += count,
                    Err(error) => {
                        failures += 1;
                        if error
                            .downcast_ref::<reqwest::Error>()
                            .is_some_and(reqwest::Error::is_timeout)
                        {
                            timeouts += 1;
                        }
                    }
                }
                if failures >= 8 && bytes == 0 {
                    break;
                }
            }
            let elapsed = start.elapsed().as_secs_f64();
            let bytes_number =
                f64::from(u32::try_from(bytes).context("benchmark byte count overflow")?);
            println!(
                "{{\"chunk\":{size},\"window\":{window},\"bytes\":{bytes},\"failures\":{failures},\"completed\":{completed},\"timeouts\":{timeouts},\"timeout_s\":{timeout_seconds},\"seconds\":{elapsed:.3},\"KiB_s\":{:.1}}}",
                bytes_number / 1024.0 / elapsed
            );
        }
    }
    Ok(())
}
