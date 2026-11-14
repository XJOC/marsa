//! The DNS server for `.test` (architecture.md section 5). Task M4-12.
//!
//! macOS sends every `.test` name here through `/etc/resolver/test`, and
//! systemd-resolved does the same on Linux (`marsa_core::dns`). The server
//! answers `127.0.0.1` for `test.` and every name under it, and refuses
//! every other name: it is not a resolver and never asks another server.
//! `AAAA` gets no address (task M5-17): Caddy listens on `127.0.0.1` only
//! (architecture.md section 10), so `::1` sent clients that try IPv6 first
//! to a port where nothing listens. Windows finds sites in the hosts file,
//! so the daemon does not start it there.
//!
//! `hickory-server` reads the queries and writes the answers on the sockets;
//! this file only decides what to answer.

use std::future::Future;
use std::iter;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use anyhow::Context as _;
use hickory_server::Server;
use hickory_server::net::runtime::Time;
use hickory_server::proto::op::{
    Edns, Header, HeaderCounts, LowerQuery, Metadata, OpCode, ResponseCode,
};
use hickory_server::proto::rr::rdata::A;
use hickory_server::proto::rr::{DNSClass, Name, RData, Record, RecordType};
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::zone_handler::MessageResponseBuilder;
use marsa_core::dns::DNS_PORT;
use marsa_core::recipes::Platform;
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long a resolver may keep an answer, in seconds.
pub const TTL: u32 = 1;
/// A TCP client that sends no query for this long is disconnected.
const TCP_IDLE: Duration = Duration::from_secs(5);
/// How many answers may wait to be written on one TCP connection.
const TCP_QUEUE: usize = 32;

/// Whether `platform` sends `.test` names to this server: macOS through a
/// resolver file, Linux through systemd-resolved. Windows uses the hosts
/// file, and a system Marsa has no build for has no DNS setup at all.
pub fn serves_test_names(platform: Option<Platform>) -> bool {
    matches!(
        platform,
        Some(
            Platform::DarwinArm64 | Platform::DarwinX64 | Platform::LinuxX64 | Platform::LinuxArm64
        )
    )
}

/// Starts the server on `127.0.0.1:DNS_PORT` for the daemon, on the systems
/// that use it. `None` on Windows, or when it cannot start: then a warning
/// is logged and the daemon runs on without it.
pub async fn start_for_daemon(shutdown: &CancellationToken) -> Option<DnsServer> {
    if !serves_test_names(Platform::current()) {
        tracing::debug!("no DNS server: this system finds .test names in the hosts file");
        return None;
    }
    start_or_warn(SocketAddr::from((Ipv4Addr::LOCALHOST, DNS_PORT)), shutdown).await
}

/// [`start`], but an error is only a warning in the log, and `None`. The
/// usual error is a port that another program holds.
pub async fn start_or_warn(addr: SocketAddr, shutdown: &CancellationToken) -> Option<DnsServer> {
    match start(addr, shutdown).await {
        Ok(server) => {
            tracing::info!(addr = %server.addr(), "DNS server for .test started");
            Some(server)
        }
        Err(err) => {
            tracing::warn!(
                %addr,
                error = format!("{err:#}"),
                "cannot start the DNS server; .test names will not resolve"
            );
            None
        }
    }
}

/// Binds UDP on `addr`, then TCP on the same port (so port 0 gives one port
/// for both), and answers until `shutdown` is cancelled or
/// [`DnsServer::stop`] is called.
///
/// # Errors
///
/// If `addr` is not a loopback address, or if a socket cannot be bound.
pub async fn start(addr: SocketAddr, shutdown: &CancellationToken) -> anyhow::Result<DnsServer> {
    // Loopback only, never `0.0.0.0` or `::` (AGENTS.md section 6).
    anyhow::ensure!(addr.ip().is_loopback(), "{addr} is not a loopback address");
    let (udp, tcp) = bind_both(addr).await?;
    let addr = udp.local_addr().context("no local UDP address")?;

    let mut server = Server::new(TestZone);
    server.register_socket(udp);
    server.register_listener(tcp, TCP_IDLE, TCP_QUEUE);
    // A child, so `stop` does not cancel the daemon's own token.
    let cancel = shutdown.child_token();
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            tokio::select! {
                () = cancel.cancelled() => {}
                result = server.block_until_done() => {
                    // Before the token, only a socket error ends the server.
                    if let Err(err) = result {
                        tracing::warn!(error = %err, "the DNS server stopped");
                    }
                    return;
                }
            }
            // Closes both sockets and waits for their tasks.
            if let Err(err) = server.shutdown_gracefully().await {
                tracing::warn!(error = %err, "the DNS server did not stop cleanly");
            }
        }
    });
    Ok(DnsServer { addr, cancel, task })
}

/// How often port 0 may pick a port that one of the two protocols refuses.
const PICK_TRIES: usize = 50;

/// Binds UDP and TCP on the same port.
///
/// With port 0 the system picks: TCP first, because on Windows a range of
/// TCP ports can be reserved (`WSAEACCES`, os error 10013) and the system
/// never hands out a reserved TCP port, then UDP on the same number. A number
/// UDP refuses is dropped and a new one picked. Picking UDP first failed on
/// Windows CI even after 10 tries: UDP ports come in sequence, often inside
/// the same reserved TCP range. A fixed port fails at once, so the caller can
/// name it.
async fn bind_both(addr: SocketAddr) -> anyhow::Result<(UdpSocket, TcpListener)> {
    if addr.port() != 0 {
        let udp = UdpSocket::bind(addr)
            .await
            .with_context(|| format!("cannot bind UDP {addr}"))?;
        let tcp = TcpListener::bind(addr)
            .await
            .with_context(|| format!("cannot bind TCP {addr}"))?;
        return Ok((udp, tcp));
    }
    let mut tries = 0;
    loop {
        let tcp = TcpListener::bind(addr)
            .await
            .with_context(|| format!("cannot bind TCP {addr}"))?;
        let picked = tcp.local_addr().context("no local TCP address")?;
        match UdpSocket::bind(picked).await {
            Ok(udp) => return Ok((udp, tcp)),
            Err(_) if tries + 1 < PICK_TRIES => tries += 1,
            Err(err) => {
                return Err(anyhow::Error::new(err).context(format!("cannot bind UDP {picked}")));
            }
        }
    }
}

/// A running DNS server, from [`start`].
#[derive(Debug)]
pub struct DnsServer {
    addr: SocketAddr,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl DnsServer {
    /// The address it answers on, over UDP and TCP.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops the server and waits until both sockets are closed, `limit` at
    /// most. After that the task is aborted, which closes them too.
    pub async fn stop(self, limit: Duration) {
        self.cancel.cancel();
        let abort = self.task.abort_handle();
        match tokio::time::timeout(limit, self.task).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!(error = %err, "the DNS server task failed"),
            Err(_) => {
                tracing::warn!(?limit, "the DNS server is still running; aborting it");
                abort.abort();
            }
        }
    }
}

/// The handler that `hickory-server` calls for each request.
#[derive(Debug, Clone, Copy, Default)]
pub struct TestZone;

/// What `RequestHandler::handle_request` returns. The trait is written with
/// the `async_trait` macro, so an impl without that macro names the boxed
/// future itself.
type Reply<'a> = Pin<Box<dyn Future<Output = ResponseInfo> + Send + 'a>>;

impl RequestHandler for TestZone {
    fn handle_request<'life0, 'life1, 'a, R, T>(
        &'life0 self,
        request: &'life1 Request,
        response_handle: R,
    ) -> Reply<'a>
    where
        R: ResponseHandler + 'a,
        T: Time + 'a,
        'life0: 'a,
        'life1: 'a,
        Self: 'a,
    {
        Box::pin(reply(request, response_handle))
    }
}

/// Builds the response to `request` and sends it through `handle`.
async fn reply<R: ResponseHandler>(request: &Request, mut handle: R) -> ResponseInfo {
    let mut metadata = Metadata::response_from_request(&request.metadata);
    let mut record = None;
    match (request.metadata.op_code, request.queries.queries()) {
        (OpCode::Query, [query]) => {
            let (code, found) = answer(query);
            metadata.response_code = code;
            // This server owns `test.`: its answers there are final.
            metadata.authoritative = code == ResponseCode::NoError;
            record = found;
        }
        // DNS servers in practice take exactly one question per message.
        (OpCode::Query, _) => metadata.response_code = ResponseCode::FormErr,
        // No updates, notifies, or zone transfers.
        _ => metadata.response_code = ResponseCode::NotImp,
    }
    // A client that sent an OPT record gets one back (RFC 6891).
    let edns = request.edns.as_ref().map(|asked| {
        let mut edns = Edns::new();
        edns.set_max_payload(asked.max_payload().max(512));
        edns
    });
    let response = MessageResponseBuilder::new(&request.queries, edns.as_ref()).build(
        metadata,
        record.iter(),
        iter::empty(),
        iter::empty(),
        iter::empty(),
    );
    match handle.send_response(response).await {
        Ok(info) => info,
        Err(err) => {
            tracing::debug!(error = %err, "cannot send a DNS answer");
            metadata.response_code = ResponseCode::ServFail;
            ResponseInfo::from(Header {
                metadata,
                counts: HeaderCounts::default(),
            })
        }
    }
}

/// The answer to one question: `127.0.0.1` for `A` in `.test`, no record for
/// any other type there (`AAAA` included: `NOERROR` with no answer, so a
/// client falls back to IPv4 at once), and `REFUSED` outside it.
fn answer(query: &LowerQuery) -> (ResponseCode, Option<Record>) {
    if !in_test(query.name()) {
        return (ResponseCode::Refused, None);
    }
    let data = match (query.query_type(), query.query_class()) {
        (RecordType::A, DNSClass::IN) => Some(RData::A(A(Ipv4Addr::LOCALHOST))),
        _ => None,
    };
    // The name as asked, letter case kept: some resolvers check it.
    let name = query.original().name().clone();
    let record = data.map(|data| Record::from_rdata(name, TTL, data));
    (ResponseCode::NoError, record)
}

/// Whether `name` is `test.` or a name under it, in any letter case.
fn in_test(name: &Name) -> bool {
    name.iter()
        .next_back()
        .is_some_and(|label| label.eq_ignore_ascii_case(b"test"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_server::proto::op::{Message, Query};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;

    /// For each answer and for the server to stop.
    const LIMIT: Duration = Duration::from_secs(5);

    #[derive(Debug, Clone, Copy)]
    enum Transport {
        Udp,
        Tcp,
    }

    fn any_port() -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
    }

    fn query(name: &str, kind: RecordType) -> Vec<u8> {
        let mut message = Message::query();
        message.metadata.recursion_desired = true;
        message.add_query(Query::query(Name::from_ascii(name).unwrap(), kind));
        message.to_vec().unwrap()
    }

    async fn ask(
        transport: Transport,
        server: SocketAddr,
        name: &str,
        kind: RecordType,
    ) -> Message {
        let bytes = query(name, kind);
        let reply = match transport {
            Transport::Udp => {
                let socket = UdpSocket::bind(any_port()).await.unwrap();
                socket.send_to(&bytes, server).await.unwrap();
                let mut buf = vec![0; 4096];
                let (len, _) = tokio::time::timeout(LIMIT, socket.recv_from(&mut buf))
                    .await
                    .expect("a UDP answer in time")
                    .unwrap();
                buf.truncate(len);
                buf
            }
            Transport::Tcp => {
                // Over TCP each message starts with its length (RFC 1035 4.2.2).
                let mut stream = TcpStream::connect(server).await.unwrap();
                let mut framed = u16::try_from(bytes.len()).unwrap().to_be_bytes().to_vec();
                framed.extend_from_slice(&bytes);
                stream.write_all(&framed).await.unwrap();
                let mut len = [0; 2];
                tokio::time::timeout(LIMIT, stream.read_exact(&mut len))
                    .await
                    .expect("a TCP answer in time")
                    .unwrap();
                let mut buf = vec![0; usize::from(u16::from_be_bytes(len))];
                stream.read_exact(&mut buf).await.unwrap();
                buf
            }
        };
        Message::from_vec(&reply).unwrap()
    }

    /// The response code, then each answer as `name ttl data`.
    fn summary(message: &Message) -> (ResponseCode, Vec<String>) {
        let answers = message.answers.iter();
        let answers = answers.map(|r| format!("{} {} {}", r.name, r.ttl, r.data));
        (message.metadata.response_code, answers.collect())
    }

    async fn answers_every_case(transport: Transport) {
        let shutdown = CancellationToken::new();
        let server = start(any_port(), &shutdown).await.unwrap();
        let ok = ResponseCode::NoError;
        let cases = [
            (
                "shop.test.",
                RecordType::A,
                ok,
                vec!["shop.test. 1 127.0.0.1"],
            ),
            (
                "api.shop.test.",
                RecordType::A,
                ok,
                vec!["api.shop.test. 1 127.0.0.1"],
            ),
            // No `::1`: nothing of Marsa's listens on IPv6 (task M5-17).
            ("api.shop.test.", RecordType::AAAA, ok, vec![]),
            (
                "SHOP.TEST.",
                RecordType::A,
                ok,
                vec!["SHOP.TEST. 1 127.0.0.1"],
            ),
            ("test.", RecordType::A, ok, vec!["test. 1 127.0.0.1"]),
            ("shop.test.", RecordType::MX, ok, vec![]),
            ("example.com.", RecordType::A, ResponseCode::Refused, vec![]),
        ];
        for (name, kind, code, answers) in cases {
            let reply = ask(transport, server.addr(), name, kind).await;
            let expected = (code, answers.iter().map(ToString::to_string).collect());
            assert_eq!(
                summary(&reply),
                expected,
                "{name} {kind} over {transport:?}"
            );
            assert_eq!(reply.metadata.authoritative, code == ok, "{name} {kind}");
            assert!(!reply.metadata.recursion_available, "{name} {kind}");
        }
        server.stop(LIMIT).await;
    }

    #[tokio::test]
    async fn answers_over_udp() {
        answers_every_case(Transport::Udp).await;
    }

    #[tokio::test]
    async fn answers_over_tcp() {
        answers_every_case(Transport::Tcp).await;
    }

    #[test]
    fn only_names_in_test_are_ours() {
        for name in ["test.", "shop.test.", "a.b.Shop.TEST."] {
            assert!(in_test(&Name::from_ascii(name).unwrap()), "{name}");
        }
        for name in [".", "example.com.", "test.com.", "xtest.", "shop.testing."] {
            assert!(!in_test(&Name::from_ascii(name).unwrap()), "{name}");
        }
    }

    #[test]
    fn only_macos_and_linux_use_the_server() {
        for platform in Platform::ALL {
            let expected = !matches!(platform, Platform::WindowsX64 | Platform::Any);
            assert_eq!(serves_test_names(Some(platform)), expected, "{platform:?}");
        }
        assert!(!serves_test_names(None));
    }

    #[tokio::test]
    async fn a_taken_port_is_a_warning_not_a_failure() {
        let shutdown = CancellationToken::new();
        let udp = UdpSocket::bind(any_port()).await.unwrap();
        let taken = udp.local_addr().unwrap();
        let started = tokio::time::timeout(LIMIT, start_or_warn(taken, &shutdown));
        assert!(started.await.expect("returns at once").is_none());

        let tcp = TcpListener::bind(any_port()).await.unwrap();
        let taken = tcp.local_addr().unwrap();
        let started = tokio::time::timeout(LIMIT, start_or_warn(taken, &shutdown));
        assert!(started.await.expect("returns at once").is_none());
    }

    #[tokio::test]
    async fn only_loopback_addresses_are_accepted() {
        let shutdown = CancellationToken::new();
        let any = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        let err = start(any, &shutdown).await.unwrap_err();
        assert!(err.to_string().contains("loopback"), "{err:#}");
    }

    #[tokio::test]
    async fn the_shutdown_token_stops_it_and_frees_the_port() {
        let shutdown = CancellationToken::new();
        let server = start(any_port(), &shutdown).await.unwrap();
        let addr = server.addr();
        let reply = ask(Transport::Udp, addr, "shop.test.", RecordType::A).await;
        assert_eq!(reply.answers.len(), 1);

        shutdown.cancel();
        let stopped = tokio::time::timeout(LIMIT, server.task).await;
        stopped.expect("stops in time").unwrap();
        let started = std::time::Instant::now();
        loop {
            let udp = std::net::UdpSocket::bind(addr);
            let tcp = std::net::TcpListener::bind(addr);
            if udp.is_ok() && tcp.is_ok() {
                break;
            }
            assert!(started.elapsed() < LIMIT, "{addr} is still taken");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
