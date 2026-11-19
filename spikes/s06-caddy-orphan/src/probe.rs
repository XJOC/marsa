//! `s06 probe`: sends a request every 25 ms and reports how long the site was
//! unreachable in one stretch (the "gap").

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Args as ClapArgs;

use crate::{Env, http_client};

#[derive(ClapArgs)]
pub struct Args {
    /// How long to probe, in seconds.
    #[arg(long, default_value_t = 8)]
    pub duration_secs: u64,
    /// Pause between two requests, in milliseconds.
    #[arg(long, default_value_t = 25)]
    pub interval_ms: u64,
    /// A request slower than this counts as failed, in milliseconds.
    #[arg(long, default_value_t = 300)]
    pub timeout_ms: u64,
}

pub async fn run(env: &Env, args: Args) -> Result<()> {
    let client = http_client(Duration::from_millis(args.timeout_ms))?;
    let url = env.app_url();
    let start = Instant::now();
    let end = start + Duration::from_secs(args.duration_secs);
    println!("PROBE_START");

    let mut total = 0u64;
    let mut failed = 0u64;
    let mut last_ok: Option<Instant> = None;
    // The gap runs from the last success before the failures to the first
    // success after them, so it is slightly longer than the true outage.
    let mut longest = Duration::ZERO;
    let mut down_since: Option<Instant> = None;

    while Instant::now() < end {
        total += 1;
        let ok = match client.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        };
        let now = Instant::now();
        if ok {
            if let Some(since) = down_since.take() {
                longest = longest.max(now.duration_since(since));
            }
            last_ok = Some(now);
        } else {
            failed += 1;
            if down_since.is_none() {
                down_since = Some(last_ok.unwrap_or(start));
            }
        }
        tokio::time::sleep(Duration::from_millis(args.interval_ms)).await;
    }
    let ended_down = down_since.is_some();
    if let Some(since) = down_since {
        longest = longest.max(Instant::now().duration_since(since));
    }
    println!(
        "PROBE total={total} failed={failed} longest_gap_ms={} ended_down={ended_down}",
        longest.as_millis()
    );
    Ok(())
}
