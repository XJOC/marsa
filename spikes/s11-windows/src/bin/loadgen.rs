//! A tiny HTTP/1.1 load generator, `std` only, so the spike downloads no
//! third-party load tool.
//!
//! ```text
//! loadgen <ip:port> <host header> <path> <concurrency> <total> [timeout secs]
//! ```
//!
//! `concurrency` threads share `total` requests. Every request is a new
//! connection (`Connection: close`). It prints one line: how many answered
//! 200, the other status codes (0 = no answer, timeout, or broken reply),
//! requests per second, and the latency spread in milliseconds. p95 is the
//! time that 95% of the requests stayed under.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = (|| {
        let addr: SocketAddr = args.first()?.parse().ok()?;
        let host = args.get(1)?.clone();
        let path = args.get(2)?.clone();
        let concurrency: usize = args.get(3)?.parse().ok()?;
        let total: usize = args.get(4)?.parse().ok()?;
        let timeout = args.get(5).and_then(|secs| secs.parse().ok()).unwrap_or(30);
        Some((
            addr,
            host,
            path,
            concurrency,
            total,
            Duration::from_secs(timeout),
        ))
    })();
    let Some((addr, host, path, concurrency, total, timeout)) = parsed else {
        eprintln!("usage: loadgen <ip:port> <host> <path> <concurrency> <total> [timeout secs]");
        std::process::exit(2);
    };

    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(Vec::with_capacity(total)));
    let started = Instant::now();
    let workers: Vec<_> = (0..concurrency)
        .map(|_| {
            let (next, results) = (Arc::clone(&next), Arc::clone(&results));
            let (host, path) = (host.clone(), path.clone());
            thread::spawn(move || {
                while next.fetch_add(1, Ordering::Relaxed) < total {
                    let begin = Instant::now();
                    let status = request(addr, &host, &path, timeout).unwrap_or(0);
                    let mut results = results.lock().unwrap_or_else(|err| err.into_inner());
                    results.push((status, begin.elapsed()));
                }
            })
        })
        .collect();
    for worker in workers {
        let _ = worker.join();
    }
    let elapsed = started.elapsed().as_secs_f64();

    let results = results.lock().unwrap_or_else(|err| err.into_inner());
    let mut statuses: BTreeMap<u16, usize> = BTreeMap::new();
    let mut times: Vec<f64> = Vec::with_capacity(results.len());
    for (status, time) in results.iter() {
        *statuses.entry(*status).or_default() += 1;
        times.push(time.as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    let at = |fraction: f64| {
        let index = ((times.len() as f64 * fraction).ceil() as usize).clamp(1, times.len().max(1));
        times.get(index - 1).copied().unwrap_or(0.0)
    };
    let ok = statuses.get(&200).copied().unwrap_or(0);
    println!(
        "path={path} c={concurrency} n={total} ok={ok} failed={} statuses={statuses:?} secs={elapsed:.2} rps={:.1} p50_ms={:.1} p95_ms={:.1} p99_ms={:.1} max_ms={:.1}",
        results.len() - ok,
        results.len() as f64 / elapsed,
        at(0.50),
        at(0.95),
        at(0.99),
        at(1.0),
    );
}

/// One request. The status code, or `None` if there was no usable answer.
fn request(addr: SocketAddr, host: &str, path: &str, timeout: Duration) -> Option<u16> {
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).ok()?;
    // "HTTP/1.1 200 OK"
    let line = reply.split(|byte| *byte == b'\r').next()?;
    let status = std::str::from_utf8(line).ok()?.split(' ').nth(1)?;
    status.parse().ok()
}
