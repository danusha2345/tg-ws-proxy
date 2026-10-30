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
    let client = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(3))
        .tls_backend_preconfigured((*tls_config()).clone())
        .user_agent("tg-ws-relay-benchmark/1")
        .build()?;
    for size in [8192, 12288, 14336] {
        for window in [1, 2, 4] {
            let start = Instant::now();
            let results: Vec<_> = stream::iter(0..16)
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
                .buffer_unordered(window)
                .collect()
                .await;
            let bytes: usize = results.iter().filter_map(|value| value.as_ref().ok()).sum();
            let failures = results.iter().filter(|value| value.is_err()).count();
            let elapsed = start.elapsed().as_secs_f64();
            let bytes_number =
                f64::from(u32::try_from(bytes).context("benchmark byte count overflow")?);
            println!(
                "{{\"chunk\":{size},\"window\":{window},\"bytes\":{bytes},\"failures\":{failures},\"seconds\":{elapsed:.3},\"KiB_s\":{:.1}}}",
                bytes_number / 1024.0 / elapsed
            );
        }
    }
    Ok(())
}
