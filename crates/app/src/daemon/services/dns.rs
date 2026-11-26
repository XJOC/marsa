//! The DNS server for `.test` (architecture.md section 5).
//!
//! macOS sends every `.test` name here through `/etc/resolver/test`, and
//! systemd-resolved does the same on Linux (`marsa_core::dns`). The server
//! answers `127.0.0.1` for `test.` and every name under it, and refuses
//! every other name: it is not a resolver and never asks another server.
//! `AAAA` gets no address: Caddy listens on `127.0.0.1` only
//! (architecture.md section 10), so `::1` sent clients that try IPv6 first
//! to a port where nothing listens. Windows finds sites in the hosts file,
//! so the daemon does not start it there.
//!
//! `hickory-server` reads the queries and writes the answers on the sockets;
//! this file only decides what to answer.
//!
//! [`DnsService`] makes the server a service of the daemon, `dns` in
//! `marsa services`: a port that another program holds is a
//! failed service, and `marsa services start dns` tries again.

use marsa_core::codes;
use marsa_core::types::ServiceId;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fmt, io, iter};

use anyhow::Context as _;
use hickory_server::Server;
use hickory_server::net::runtime::Time;
use hickory_server::proto::op::{
    Edns, Header, HeaderCounts, LowerQuery, Metadata, OpCode, ResponseCode,
};
use hickory_server::proto::rr::rdata::{A, SOA};
use hickory_server::proto::rr::{DNSClass, Name, RData, Record, RecordType};
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::zone_handler::MessageResponseBuilder;
use marsa_core::platform::process;
use marsa_core::recipes::Platform;
use marsa_core::types::{Event, LogLine, LogStream, ServiceAction, ServiceState, ServiceStatus};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::supervisor::{ServiceRuntime, lock, now_ms};

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

/// How long a stop waits for both sockets to close.
pub const STOP_LIMIT: Duration = Duration::from_secs(2);

/// The DNS server as a service of the daemon: its state and
/// recent lines for `GET /v1/services` and `marsa logs dns`, and the server
/// itself while it runs. A port that another program holds is a failed
/// service with a code, not only a line in the daemon's log: that program
/// may answer `.test` names with any address.
#[derive(Debug)]
pub struct DnsService {
    /// `None` where `.test` names never come here (Windows): no service.
    addr: Option<SocketAddr>,
    /// Short reads only: never held across `.await`.
    runtime: Arc<Mutex<ServiceRuntime>>,
    server: Mutex<Option<DnsServer>>,
    /// One action at a time. A `tokio::sync::Mutex` held across `.await`
    /// as a serialization token only (AGENTS.md section 6): it guards no
    /// data.
    turn: tokio::sync::Mutex<()>,
}

impl DnsService {
    /// A service on `addr`, not started; `None` lists no service.
    pub fn new(addr: Option<SocketAddr>) -> Self {
        Self {
            addr,
            runtime: Arc::new(Mutex::new(ServiceRuntime::new(ServiceId::Dns))),
            server: Mutex::default(),
            turn: tokio::sync::Mutex::default(),
        }
    }

    /// The daemon's: `127.0.0.1:port` on the systems that send `.test`
    /// names to it, no service elsewhere. `port` is [`marsa_core::dns::DNS_PORT`] except in
    /// tests.
    pub fn for_daemon(port: u16) -> Self {
        let used = serves_test_names(Platform::current());
        Self::new(used.then(|| SocketAddr::from((Ipv4Addr::LOCALHOST, port))))
    }

    /// Whether `GET /v1/services` lists it.
    pub fn listed(&self) -> bool {
        self.addr.is_some()
    }

    /// Its status, if it is listed.
    pub fn status(&self) -> Option<ServiceStatus> {
        self.addr?;
        Some(lock(&self.runtime).status.clone())
    }

    pub fn recent_logs(&self) -> Vec<LogLine> {
        lock(&self.runtime).logs.recent()
    }

    /// Why `.test` names sent to the DNS port get no answer from us: the
    /// failure code, or [`codes::DNS_NOT_RUNNING`]. `None` while the server runs,
    /// and where it is not used.
    pub fn not_serving(&self) -> Option<String> {
        self.addr?;
        match &lock(&self.runtime).status.state {
            ServiceState::Running => None,
            ServiceState::Failed { code } => Some(code.clone()),
            _ => Some(codes::DNS_NOT_RUNNING.to_owned()),
        }
    }

    /// Runs `action` and returns the status after it. `start` on a running
    /// server and `stop` on a stopped one change nothing; `restart` is
    /// `stop`, then `start`. A start that fails is the `Failed` state, not
    /// an error. Nothing starts once `shutdown` is cancelled.
    pub async fn run(
        &self,
        events: &broadcast::Sender<Event>,
        shutdown: &CancellationToken,
        action: ServiceAction,
    ) -> ServiceStatus {
        let _turn = self.turn.lock().await;
        if matches!(action, ServiceAction::Stop | ServiceAction::Restart) {
            self.stop_server(events).await;
        }
        if matches!(action, ServiceAction::Start | ServiceAction::Restart) {
            self.start_server(events, shutdown).await;
        }
        lock(&self.runtime).status.clone()
    }

    /// Stops the server at the daemon's end, after any action that runs.
    pub async fn shutdown(&self, limit: Duration) {
        let _turn = self.turn.lock().await;
        let server = lock(&self.server).take();
        if let Some(server) = server {
            server.stop(limit).await;
        }
    }

    async fn start_server(&self, events: &broadcast::Sender<Event>, shutdown: &CancellationToken) {
        let Some(addr) = self.addr else { return };
        if lock(&self.server).is_some() || shutdown.is_cancelled() {
            return;
        }
        self.set_state(events, ServiceState::Starting);
        match start(addr, shutdown).await {
            Ok(server) => {
                tracing::info!(addr = %server.addr(), "DNS server for .test started");
                let text = format!("answering .test names on {} (UDP and TCP)", server.addr());
                self.note(events, LogStream::Stdout, &text);
                *lock(&self.server) = Some(server);
                self.set_state(events, ServiceState::Running);
            }
            Err(err) => {
                let held = err
                    .downcast_ref::<BindError>()
                    .filter(|bind| bind.source.kind() == io::ErrorKind::AddrInUse);
                let code = if held.is_some() {
                    codes::DNS_PORT_IN_USE
                } else {
                    codes::DNS_START_FAILED
                };
                tracing::warn!(
                    %addr,
                    code,
                    error = format!("{err:#}"),
                    "cannot start the DNS server; .test names will not resolve"
                );
                let text = format!("{err:#}; .test names do not resolve");
                self.note(events, LogStream::Stderr, &text);
                self.set_state(
                    events,
                    ServiceState::Failed {
                        code: code.to_owned(),
                    },
                );
                if let Some(held) = held {
                    self.name_the_holder(held.protocol, addr.port(), events.clone());
                }
            }
        }
    }

    async fn stop_server(&self, events: &broadcast::Sender<Event>) {
        let server = lock(&self.server).take();
        let Some(server) = server else { return };
        self.set_state(events, ServiceState::Stopping);
        server.stop(STOP_LIMIT).await;
        self.note(events, LogStream::Stdout, "stopped");
        self.set_state(events, ServiceState::Stopped);
    }

    /// Looks for the program that holds the port, for its line only. On
    /// its own task, so a slow `lsof` (up to 8 s) never holds the daemon's
    /// start or an action.
    fn name_the_holder(&self, protocol: Protocol, port: u16, events: broadcast::Sender<Event>) {
        let runtime = Arc::clone(&self.runtime);
        tokio::spawn(async move {
            let owner = match protocol {
                Protocol::Udp => process::udp_port_owner(port).await,
                Protocol::Tcp => process::port_owner(port).await,
            };
            if let Some(owner) = owner {
                tracing::warn!(port, %protocol, owner, "another program holds the DNS port");
                let text = format!("{protocol} port {port} is held by {owner}");
                note(&runtime, &events, LogStream::Stderr, &text);
            }
        });
    }

    fn note(&self, events: &broadcast::Sender<Event>, stream: LogStream, text: &str) {
        note(&self.runtime, events, stream, text);
    }

    /// Sets `state` and tells every reader, under the lock like the
    /// supervisor, so events come in the order of the changes.
    fn set_state(&self, events: &broadcast::Sender<Event>, state: ServiceState) {
        let mut runtime = lock(&self.runtime);
        let status = &mut runtime.status;
        // The server runs inside `marsad`.
        status.pid = (state == ServiceState::Running).then(std::process::id);
        status.state = state;
        status.since_ms = Some(now_ms());
        // An error only means that nobody listens right now.
        let _ = events.send(Event::ServiceChanged(status.clone()));
    }
}

/// Adds a line to the recent lines of `dns`, and sends it to every reader
/// under the lock, so lines come in `seq` order.
fn note(
    runtime: &Mutex<ServiceRuntime>,
    events: &broadcast::Sender<Event>,
    stream: LogStream,
    text: &str,
) {
    let mut runtime = lock(runtime);
    let line = runtime.logs.push(stream, text, now_ms());
    let _ = events.send(Event::Log(line));
}

/// The protocol of a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Udp,
    Tcp,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Protocol::Udp => "UDP",
            Protocol::Tcp => "TCP",
        })
    }
}

/// A fixed port that one protocol could not bind.
#[derive(Debug)]
struct BindError {
    protocol: Protocol,
    addr: SocketAddr,
    source: io::Error,
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot bind {} {}", self.protocol, self.addr)
    }
}

impl std::error::Error for BindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
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

/// How often port 0 may pick a number that the other protocol refuses.
const PICK_TRIES: usize = 50;

/// Binds UDP and TCP on the same port.
///
/// With port 0 the system picks the number for one protocol and the other
/// binds it too ([`pick`]); the two take turns ([`in_turn`]). A fixed port
/// fails at once, so the caller can name it.
async fn bind_both(addr: SocketAddr) -> anyhow::Result<(UdpSocket, TcpListener)> {
    if addr.port() != 0 {
        let failed = |protocol| {
            move |source| BindError {
                protocol,
                addr,
                source,
            }
        };
        let udp = UdpSocket::bind(addr).await.map_err(failed(Protocol::Udp))?;
        let tcp = TcpListener::bind(addr)
            .await
            .map_err(failed(Protocol::Tcp))?;
        return Ok((udp, tcp));
    }
    in_turn(|picker| pick(picker, addr)).await
}

/// Calls `pick` for TCP, UDP, TCP and so on, until one call succeeds, and
/// gives the last error after [`PICK_TRIES`] calls.
///
/// On Windows a range of ports can be reserved for one protocol only
/// (`WSAEACCES`, os error 10013), and the system gives out numbers in
/// sequence. So every number picked for one protocol can fall inside a range
/// that the other refuses: this happened with 50 picks by TCP, and with 10 by
/// UDP. Each protocol has a sequence of its own.
async fn in_turn<T, F>(mut pick: impl FnMut(Protocol) -> F) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
{
    let mut refused = None;
    for picker in [Protocol::Tcp, Protocol::Udp]
        .into_iter()
        .cycle()
        .take(PICK_TRIES)
    {
        match pick(picker).await {
            Ok(picked) => return Ok(picked),
            Err(err) => refused = Some(err),
        }
    }
    Err(refused.expect("PICK_TRIES is above zero"))
}

/// The system picks a free number for `picker` on `any` (port 0), and the
/// other protocol binds the same one.
async fn pick(picker: Protocol, any: SocketAddr) -> anyhow::Result<(UdpSocket, TcpListener)> {
    let failed = |protocol: Protocol, addr| move || format!("cannot bind {protocol} {addr}");
    match picker {
        Protocol::Tcp => {
            let tcp = TcpListener::bind(any)
                .await
                .with_context(failed(Protocol::Tcp, any))?;
            let picked = tcp.local_addr().context("no local TCP address")?;
            let udp = UdpSocket::bind(picked)
                .await
                .with_context(failed(Protocol::Udp, picked))?;
            Ok((udp, tcp))
        }
        Protocol::Udp => {
            let udp = UdpSocket::bind(any)
                .await
                .with_context(failed(Protocol::Udp, any))?;
            let picked = udp.local_addr().context("no local UDP address")?;
            let tcp = TcpListener::bind(picked)
                .await
                .with_context(failed(Protocol::Tcp, picked))?;
            Ok((udp, tcp))
        }
    }
}

/// A UDP socket and a TCP listener on one loopback port that both
/// protocols accept, for a test to hold and then let go of, so that a start
/// on that port can bind both. A port picked for one protocol alone may sit
/// in a range that Windows reserves for the other ([`in_turn`]): a start there
/// fails with `dns_start_failed`.
#[cfg(test)]
pub(crate) async fn held_dns_port() -> (std::net::UdpSocket, std::net::TcpListener) {
    let any = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
    let (udp, tcp) = bind_both(any).await.expect("a port for UDP and TCP");
    let udp = udp.into_std().expect("a std UDP socket");
    let tcp = tcp.into_std().expect("a std TCP listener");
    (udp, tcp)
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
    let mut soa = None;
    match (request.metadata.op_code, request.queries.queries()) {
        (OpCode::Query, [query]) => {
            let (code, found) = answer(query);
            metadata.response_code = code;
            // This server owns `test.`: its answers there are final.
            metadata.authoritative = code == ResponseCode::NoError;
            // No record of that type (NODATA): the zone's SOA goes in the
            // authority section, so resolvers may cache the empty answer for
            // its MINIMUM, one second (RFC 2308 section 3). systemd-resolved
            // caches none without it.
            if code == ResponseCode::NoError && found.is_none() {
                soa = Some(zone_soa());
            }
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
        soa.iter(),
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
        // The zone's own SOA, asked at its top: `test.`.
        (RecordType::SOA, DNSClass::IN) if query.name().iter().count() == 1 => {
            Some(RData::SOA(soa_data()))
        }
        _ => None,
    };
    // The name as asked, letter case kept: some resolvers check it.
    let name = query.original().name().clone();
    let record = data.map(|data| Record::from_rdata(name, TTL, data));
    (ResponseCode::NoError, record)
}

/// The SOA record of the `test.` zone, for the authority section of an
/// empty answer.
fn zone_soa() -> Record {
    let zone = Name::from_ascii("test.").expect("`test.` is a valid name");
    Record::from_rdata(zone, TTL, RData::SOA(soa_data()))
}

/// The SOA of `test.`. Only MINIMUM matters to clients: one second, like
/// [`TTL`], so a negative answer is cached no longer than an address.
/// Refresh, retry, and expire are for secondary servers, which this zone
/// has none of.
fn soa_data() -> SOA {
    let mname = Name::from_ascii("localhost.").expect("`localhost.` is a valid name");
    let rname = Name::from_ascii("hostmaster.test.").expect("`hostmaster.test.` is a valid name");
    SOA::new(mname, rname, 1, 3600, 600, 86_400, TTL)
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

    /// The SOA in the authority section: its owner, TTL, and MINIMUM.
    fn authority_soa(message: &Message) -> Option<(String, u32, u32)> {
        match message.authorities.as_slice() {
            [record] => match &record.data {
                RData::SOA(soa) => Some((record.name.to_string(), record.ttl, soa.minimum)),
                _ => None,
            },
            _ => None,
        }
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
            // No `::1`: nothing of Marsa's listens on IPv6.
            ("api.shop.test.", RecordType::AAAA, ok, vec![]),
            (
                "SHOP.TEST.",
                RecordType::A,
                ok,
                vec!["SHOP.TEST. 1 127.0.0.1"],
            ),
            ("test.", RecordType::A, ok, vec!["test. 1 127.0.0.1"]),
            ("shop.test.", RecordType::MX, ok, vec![]),
            // The zone's own SOA at its top; under it, an empty answer.
            (
                "test.",
                RecordType::SOA,
                ok,
                vec!["test. 1 localhost. hostmaster.test. 1 3600 600 86400 1"],
            ),
            ("shop.test.", RecordType::SOA, ok, vec![]),
            // `*` is a label like any other: not the zone's top.
            ("*.test.", RecordType::SOA, ok, vec![]),
            (
                "TEST.",
                RecordType::SOA,
                ok,
                vec!["TEST. 1 localhost. hostmaster.test. 1 3600 600 86400 1"],
            ),
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
            // An empty answer in the zone carries its SOA, with MINIMUM 1
            // (RFC 2308); an answer with a record, or REFUSED, carries none.
            let soa = (code == ok && answers.is_empty()).then(|| ("test.".to_owned(), TTL, TTL));
            match soa {
                Some(_) => assert_eq!(authority_soa(&reply), soa, "{name} {kind}"),
                None => assert!(reply.authorities.is_empty(), "{name} {kind}"),
            }
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

    fn failed(code: &str) -> ServiceState {
        ServiceState::Failed { code: code.into() }
    }

    fn texts(service: &DnsService) -> Vec<String> {
        service.recent_logs().into_iter().map(|l| l.text).collect()
    }

    #[tokio::test]
    async fn a_held_port_is_a_failed_service_until_a_start_finds_it_free() {
        let shutdown = CancellationToken::new();
        let (events, mut seen) = broadcast::channel(64);
        // UDP is bound first, so the held UDP socket is the one named.
        let (udp, tcp) = held_dns_port().await;
        let addr = udp.local_addr().unwrap();
        let service = DnsService::new(Some(addr));
        assert_eq!(service.status().unwrap().state, ServiceState::Stopped);

        let run = service.run(&events, &shutdown, ServiceAction::Start);
        let status = tokio::time::timeout(LIMIT, run).await.expect("at once");
        assert_eq!(status.state, failed(codes::DNS_PORT_IN_USE));
        assert_eq!(status.pid, None);
        assert_eq!(service.status(), Some(status.clone()));
        assert_eq!(
            service.not_serving().as_deref(),
            Some(codes::DNS_PORT_IN_USE)
        );
        let line = format!("cannot bind UDP {addr}");
        assert!(
            texts(&service)[0].starts_with(&line),
            "{:?}",
            texts(&service)
        );
        // Each change went out as an event, in order.
        let mut states = Vec::new();
        while let Ok(event) = seen.try_recv() {
            if let Event::ServiceChanged(status) = event {
                states.push(status.state);
            }
        }
        assert_eq!(
            states,
            [ServiceState::Starting, failed(codes::DNS_PORT_IN_USE)]
        );

        drop((udp, tcp));
        let status = service.run(&events, &shutdown, ServiceAction::Start).await;
        assert_eq!(status.state, ServiceState::Running);
        assert_eq!(status.pid, Some(std::process::id()));
        assert_eq!(service.not_serving(), None);
        let reply = ask(Transport::Udp, addr, "shop.test.", RecordType::A).await;
        assert_eq!(reply.answers.len(), 1);

        let status = service.run(&events, &shutdown, ServiceAction::Stop).await;
        assert_eq!(status.state, ServiceState::Stopped);
        assert_eq!(
            service.not_serving().as_deref(),
            Some(codes::DNS_NOT_RUNNING)
        );
        let status = service
            .run(&events, &shutdown, ServiceAction::Restart)
            .await;
        assert_eq!(status.state, ServiceState::Running);
        service.shutdown(LIMIT).await;
    }

    #[tokio::test]
    async fn the_protocols_take_turns_to_pick_a_port() {
        // As on Windows when one protocol's numbers are all refused.
        for works in [Protocol::Tcp, Protocol::Udp] {
            let picked = in_turn(|picker| async move {
                anyhow::ensure!(picker == works, "refused");
                Ok(picker)
            });
            assert_eq!(picked.await.unwrap(), works);
        }
        let mut calls = 0;
        let none: anyhow::Result<()> = in_turn(|_| {
            calls += 1;
            std::future::ready(Err(anyhow::anyhow!("refused {calls}")))
        })
        .await;
        assert_eq!(none.unwrap_err().to_string(), "refused 50");
        assert_eq!(calls, PICK_TRIES);
    }

    /// Unix only: Windows may refuse every number one protocol picks.
    #[cfg(unix)]
    #[tokio::test]
    async fn either_protocol_picks_one_port_for_both() {
        for first in [Protocol::Tcp, Protocol::Udp] {
            // Another program may hold the number for the other protocol.
            let (udp, tcp) = in_turn(|_| pick(first, any_port())).await.unwrap();
            let addr = udp.local_addr().unwrap();
            assert_eq!(addr, tcp.local_addr().unwrap(), "{first}");
            assert!(addr.ip().is_loopback() && addr.port() != 0, "{addr}");
        }
    }

    #[tokio::test]
    async fn a_held_tcp_port_fails_too_and_frees_the_udp_one() {
        let shutdown = CancellationToken::new();
        let events = broadcast::channel(64).0;
        // A number UDP accepts, so the start gets as far as the held TCP one.
        let (udp, tcp) = held_dns_port().await;
        drop(udp);
        let addr = tcp.local_addr().unwrap();
        let service = DnsService::new(Some(addr));
        let status = service.run(&events, &shutdown, ServiceAction::Start).await;
        assert_eq!(status.state, failed(codes::DNS_PORT_IN_USE));
        assert!(texts(&service)[0].starts_with(&format!("cannot bind TCP {addr}")));
        std::net::UdpSocket::bind(addr).expect("the UDP socket was closed");
    }

    /// `lsof` runs on macOS only. The holder is this test.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn the_line_names_the_program_that_holds_the_port() {
        let shutdown = CancellationToken::new();
        let events = broadcast::channel(64).0;
        let udp = std::net::UdpSocket::bind(any_port()).unwrap();
        let addr = udp.local_addr().unwrap();
        let service = DnsService::new(Some(addr));
        service.run(&events, &shutdown, ServiceAction::Start).await;
        let held = format!("UDP port {} is held by ", addr.port());
        let pid = format!("({})", std::process::id());
        let started = std::time::Instant::now();
        while !texts(&service)
            .iter()
            .any(|text| text.starts_with(&held) && text.ends_with(&pid))
        {
            assert!(started.elapsed() < LIMIT, "{:?}", texts(&service));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn another_error_has_its_own_code_and_no_service_is_none() {
        let shutdown = CancellationToken::new();
        let events = broadcast::channel(64).0;
        let any = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        let service = DnsService::new(Some(any));
        let status = service.run(&events, &shutdown, ServiceAction::Start).await;
        assert_eq!(status.state, failed(codes::DNS_START_FAILED));

        let none = DnsService::new(None);
        assert!(!none.listed());
        assert_eq!(none.status(), None);
        assert_eq!(none.not_serving(), None);
        let status = none.run(&events, &shutdown, ServiceAction::Start).await;
        assert_eq!(status.state, ServiceState::Stopped);
        assert!(none.recent_logs().is_empty());
    }

    #[tokio::test]
    async fn nothing_starts_once_the_daemon_stops() {
        let shutdown = CancellationToken::new();
        let events = broadcast::channel(64).0;
        let service = DnsService::new(Some(any_port()));
        shutdown.cancel();
        let status = service.run(&events, &shutdown, ServiceAction::Start).await;
        assert_eq!(status.state, ServiceState::Stopped);
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
