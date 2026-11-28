use std::net::{Ipv4Addr, TcpListener as StdListener};
use std::path::Path;

use marsa_core::dumps::MAX_LINE_BYTES;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::*;

/// The lines of `fixtures/dumps/<name>`.
fn fixture_lines(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dumps")
        .join(name);
    std::fs::read_to_string(path)
        .expect("read the fixture")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn line() -> String {
    fixture_lines("object.v74.dump").remove(0)
}

/// A service on a port of the system's choosing, listening.
async fn listening(idle: Duration) -> (DumpsService, CancellationToken) {
    let service = DumpsService::with_idle(0, idle);
    let shutdown = CancellationToken::new();
    assert_eq!(service.apply(true, &shutdown).await, DumpsStatus::Listening);
    (service, shutdown)
}

async fn connect(service: &DumpsService) -> TcpStream {
    let addr = service.local_addr().expect("it listens");
    TcpStream::connect(addr).await.expect("connect")
}

async fn send(service: &DumpsService, text: &str) {
    let mut stream = connect(service).await;
    stream.write_all(text.as_bytes()).await.expect("write");
    stream.shutdown().await.expect("close");
}

/// Waits until the store holds `count` dumps, or fails.
async fn until_count(service: &DumpsService, count: usize) -> DumpList {
    for _ in 0..500 {
        let list = service.list(None);
        if list.dumps.len() == count {
            return list;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "the store never held {count} dumps: {:?}",
        service.list(None)
    );
}

#[tokio::test]
async fn it_listens_on_loopback_only_and_keeps_what_php_sends() {
    let (service, _shutdown) = listening(Duration::from_secs(60)).await;
    let addr = service.local_addr().expect("address");
    assert_eq!(addr.ip(), Ipv4Addr::LOCALHOST);

    send(&service, &format!("{}\n", line())).await;
    let list = until_count(&service, 1).await;
    assert_eq!(list.status, DumpsStatus::Listening);
    assert_eq!(list.latest_id, 1);
    let dump = &list.dumps[0];
    assert_eq!(dump.id, 1);
    assert_eq!(dump.source_file.as_deref(), Some("/app/routes/web.php"));
    assert!(dump.text.starts_with("User {#2"), "{}", dump.text);
    assert!(dump.received_at > 0);
}

#[tokio::test]
async fn several_lines_on_one_connection_and_a_final_line_without_a_break() {
    let (service, _shutdown) = listening(Duration::from_secs(60)).await;
    let lines = fixture_lines("scalars.v74.dump");
    assert_eq!(lines.len(), 7);
    // The last line has no `\n`: not a whole dump, so it is dropped.
    send(&service, &lines.join("\n")).await;
    let list = until_count(&service, 6).await;
    assert_eq!(list.dumps[0].text, "\"hello\"", "newest first");
    assert_eq!(list.dumps[5].text, "null");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(service.list(None).dumps.len(), 6);
}

#[tokio::test]
async fn a_line_over_1_mib_is_skipped_and_leaves_one_note() {
    let (service, _shutdown) = listening(Duration::from_secs(60)).await;
    let mut stream = connect(&service).await;
    // 3 chunks of the big line, so it is skipped across reads.
    let chunk = vec![b'A'; MAX_LINE_BYTES / 2 + 1];
    stream.write_all(&chunk).await.expect("write");
    stream.write_all(&chunk).await.expect("write");
    stream.write_all(b"\n").await.expect("write");
    stream
        .write_all(format!("{}\n", line()).as_bytes())
        .await
        .expect("write");
    stream.shutdown().await.expect("close");
    let list = until_count(&service, 2).await;
    // The good one is newest.
    assert!(
        list.dumps[0].text.starts_with("User"),
        "{}",
        list.dumps[0].text
    );
    assert!(list.dumps[1].truncated);
    assert!(list.dumps[1].text.contains("1 MiB"));
    assert!(list.dumps[1].text.len() < 100, "the line was not kept");
}

#[tokio::test]
async fn a_line_of_exactly_the_limit_is_not_too_large() {
    let (service, _shutdown) = listening(Duration::from_secs(60)).await;
    // Not a dump, so it is dropped, but as invalid and not as too large.
    send(&service, &format!("{}\n", "A".repeat(MAX_LINE_BYTES))).await;
    send(&service, &format!("{}\n", line())).await;
    let list = until_count(&service, 1).await;
    assert!(list.dumps[0].text.starts_with("User"));
}

#[tokio::test]
async fn text_that_is_not_a_dump_is_dropped_and_the_next_line_still_works() {
    let (service, _shutdown) = listening(Duration::from_secs(60)).await;
    send(&service, &format!("not a dump\n\n\n!!!\n{}\n", line())).await;
    let list = until_count(&service, 1).await;
    assert!(list.dumps[0].text.starts_with("User"));
}

#[tokio::test]
async fn a_connection_that_never_ends_a_line_stores_nothing_and_leaks_nothing() {
    let (service, _shutdown) = listening(Duration::from_millis(150)).await;
    let mut stream = connect(&service).await;
    stream.write_all(line().as_bytes()).await.expect("write");
    // No `\n`, no close: the idle time ends it.
    let mut buf = [0_u8; 8];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("the server closed it");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    assert!(service.list(None).dumps.is_empty());
    // And it serves on.
    send(&service, &format!("{}\n", line())).await;
    until_count(&service, 1).await;
}

#[tokio::test]
async fn the_17th_connection_is_closed_and_a_freed_place_is_used_again() {
    let (service, _shutdown) = listening(Duration::from_secs(60)).await;
    let mut held = Vec::new();
    for _ in 0..listener::MAX_CONNECTIONS {
        let mut stream = connect(&service).await;
        // A byte-less connection still holds its place: prove it is served
        // by sending a line and waiting for it.
        stream
            .write_all(format!("{}\n", line()).as_bytes())
            .await
            .expect("write");
        held.push(stream);
    }
    until_count(&service, listener::MAX_CONNECTIONS).await;
    let mut extra = connect(&service).await;
    let mut buf = [0_u8; 8];
    let read = tokio::time::timeout(Duration::from_secs(5), extra.read(&mut buf))
        .await
        .expect("the extra connection was closed");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");

    // One place is free again.
    drop(held.pop());
    let mut again = None;
    for _ in 0..200 {
        let mut stream = connect(&service).await;
        stream
            .write_all(format!("{}\n", line()).as_bytes())
            .await
            .expect("write");
        if tokio::time::timeout(Duration::from_millis(100), stream.read(&mut buf))
            .await
            .is_err()
        {
            // Still open after 100 ms: it is served.
            again = Some(stream);
            break;
        }
    }
    assert!(again.is_some(), "a freed place was never used");
    drop(held);
}

#[tokio::test]
async fn a_port_that_another_program_holds_is_port_in_use_and_it_tries_again() {
    let blocker = StdListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = blocker.local_addr().expect("addr").port();
    let service = DumpsService::new(port);
    let shutdown = CancellationToken::new();
    assert_eq!(service.apply(true, &shutdown).await, DumpsStatus::PortInUse);
    assert_eq!(service.list(None).status, DumpsStatus::PortInUse);
    assert_eq!(service.local_addr(), None);
    drop(blocker);
    assert_eq!(service.apply(true, &shutdown).await, DumpsStatus::Listening);
    assert_eq!(service.local_addr().map(|a| a.port()), Some(port));
}

#[tokio::test]
async fn off_closes_the_port_and_on_opens_it() {
    let (service, shutdown) = listening(Duration::from_secs(60)).await;
    let addr = service.local_addr().expect("address");
    assert_eq!(service.apply(false, &shutdown).await, DumpsStatus::Disabled);
    assert!(
        TcpStream::connect(addr).await.is_err(),
        "the port is closed"
    );
    assert_eq!(service.local_addr(), None);
    assert_eq!(service.apply(true, &shutdown).await, DumpsStatus::Listening);
    send(&service, &format!("{}\n", line())).await;
    until_count(&service, 1).await;
}

#[tokio::test]
async fn nothing_starts_after_the_shutdown() {
    let service = DumpsService::new(0);
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    assert_eq!(service.apply(true, &shutdown).await, DumpsStatus::Disabled);
    assert_eq!(service.local_addr(), None);
}

#[tokio::test]
async fn the_shutdown_token_closes_the_listener_and_its_connections() {
    let (service, shutdown) = listening(Duration::from_secs(60)).await;
    let mut stream = connect(&service).await;
    shutdown.cancel();
    let mut buf = [0_u8; 8];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("the connection was closed");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    service.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn clear_empties_the_list_and_the_next_id_is_bigger() {
    let (service, _shutdown) = listening(Duration::from_secs(60)).await;
    send(&service, &format!("{}\n", line())).await;
    let before = until_count(&service, 1).await;
    let cleared = service.clear();
    assert!(cleared.dumps.is_empty());
    assert_eq!(cleared.latest_id, before.latest_id, "the counter stays");
    send(&service, &format!("{}\n", line())).await;
    let after = until_count(&service, 1).await;
    assert!(after.dumps[0].id > before.dumps[0].id);
    // `after` asks for the newer ones only.
    assert_eq!(service.list(Some(after.latest_id)).dumps.len(), 0);
    assert_eq!(service.list(Some(0)).dumps.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_answers_the_empty_snapshot_even_while_dumps_arrive() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use marsa_core::types::Dump;

    use crate::daemon::services::supervisor::now_ms;

    let (service, _shutdown) = listening(Duration::from_secs(60)).await;
    send(&service, &format!("{}\n", line())).await;
    until_count(&service, 1).await;

    // What the listener's `keep` does — the store lock and a push — from
    // another thread, without a pause. Before the fix, a push that landed
    // between the clear's two lock takes showed up in the DELETE answer.
    let shared = Arc::clone(&service.shared);
    let stop = Arc::new(AtomicBool::new(false));
    let pusher = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let dump = Dump {
                id: 0,
                received_at: 0,
                source_file: None,
                source_line: None,
                uri: None,
                method: None,
                label: None,
                text: "pushed".to_owned(),
                truncated: false,
            };
            while !stop.load(Ordering::Relaxed) {
                lock(&shared.store).push(dump.clone(), now_ms());
            }
        })
    };
    for round in 0..2_000 {
        let cleared = service.clear();
        assert!(
            cleared.dumps.is_empty() && cleared.count == 0,
            "round {round}: the DELETE answer held {} dumps",
            cleared.dumps.len()
        );
    }
    stop.store(true, Ordering::Relaxed);
    pusher.join().expect("the pusher ended");
}

#[tokio::test]
async fn many_dumps_make_a_few_events_and_the_last_one_is_current() {
    let (service, shutdown) = listening(Duration::from_secs(60)).await;
    let (events, mut received) = broadcast::channel(64);
    service.start_events(events, shutdown.clone()).await;
    let mut text = String::new();
    for _ in 0..40 {
        text.push_str(&line());
        text.push('\n');
    }
    send(&service, &text).await;
    until_count(&service, 40).await;
    // Wait for the quiet: one event for the first dump, one more for the rest.
    tokio::time::sleep(EVENT_EVERY * 3).await;
    let mut seen = Vec::new();
    while let Ok(event) = received.try_recv() {
        seen.push(event);
    }
    assert!(
        (1..=4).contains(&seen.len()),
        "40 dumps made {} events",
        seen.len()
    );
    assert_eq!(
        seen.last(),
        Some(&Event::DumpsChanged {
            latest_id: 40,
            count: 40
        })
    );
}
