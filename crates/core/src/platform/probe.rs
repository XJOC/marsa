//! The system reads of `marsa doctor` (task M5-12). Each is small, read-only,
//! and bounded in time; `crate::doctor` judges what they return.
//!
//! Every socket goes to `127.0.0.1`, and at most one plain `GET /` is sent.

use std::env::consts::EXE_SUFFIX;
use std::fs::File;
use std::hash::{BuildHasher as _, RandomState};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// The most of an HTTP answer kept: the greeting is in the first few hundred
/// bytes.
const MAX_ANSWER: usize = 16 * 1024;

/// A number that differs on each call: std seeds its hasher keys from the
/// OS's random source.
pub fn random() -> u64 {
    RandomState::new().hash_one(std::process::id())
}

/// What the system resolver gives `name`, as a browser would get it. The
/// lookup runs on its own thread: past `limit` this returns `TimedOut` and
/// leaves the thread to end by itself.
///
/// # Errors
///
/// The resolver's error, `TimedOut`, or the error of the thread's start.
pub fn resolve(name: &str, limit: Duration) -> io::Result<Vec<IpAddr>> {
    let (sender, receiver) = mpsc::channel();
    let host = name.to_owned();
    thread::Builder::new()
        .name("doctor-resolve".to_owned())
        .spawn(move || {
            let found = (host.as_str(), 0)
                .to_socket_addrs()
                .map(|addresses| addresses.map(|address| address.ip()).collect());
            // After a timeout nobody waits for this, so a failed send is fine.
            let _ = sender.send(found);
        })?;
    receiver
        .recv_timeout(limit)
        .unwrap_or_else(|_| Err(io::Error::from(io::ErrorKind::TimedOut)))
}

/// Sends `GET /` with `Host: <host>` to `127.0.0.1:<port>` and returns the
/// start of the answer: at most [`MAX_ANSWER`] bytes, read until the server
/// closes or `limit` has passed. `host` is a name Marsa made, never user text.
///
/// # Errors
///
/// The connection's error, or a timeout before any byte came.
pub fn http_get(port: u16, host: &str, limit: Duration) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + limit;
    let mut stream = TcpStream::connect_timeout(&loopback(port), limit)?;
    stream.set_read_timeout(Some(limit))?;
    stream.set_write_timeout(Some(limit))?;
    let request = format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: marsa-doctor\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    let mut answer = Vec::new();
    let mut buffer = [0; 4096];
    while answer.len() < MAX_ANSWER && Instant::now() < deadline {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => answer.extend_from_slice(&buffer[..read]),
            // A server that keeps the connection open: what came is enough.
            Err(err) if !answer.is_empty() && is_timeout(&err) => break,
            Err(err) => return Err(err),
        }
    }
    answer.truncate(MAX_ANSWER);
    Ok(answer)
}

/// Whether `127.0.0.1:<port>` takes a TCP connection within `limit`. Sends
/// nothing and closes at once.
pub fn accepts(port: u16, limit: Duration) -> bool {
    TcpStream::connect_timeout(&loopback(port), limit).is_ok()
}

/// The end of the file at `path`: its last `max` bytes, from the first whole
/// line. `Ok(None)` if there is no such file. The file is never read whole.
///
/// # Errors
///
/// The error of the open, the seek, or the read.
pub fn read_tail(path: &Path, max: u64) -> io::Result<Option<String>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let start = file.metadata()?.len().saturating_sub(max);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes)?;
    // Past the start of the file the first line is cut: it goes.
    let whole = if start > 0 {
        bytes
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(&[][..], |end| &bytes[end + 1..])
    } else {
        &bytes[..]
    };
    Ok(Some(String::from_utf8_lossy(whole).into_owned()))
}

/// `<dir>/<name>` (`<name>.exe` on Windows) if it is a file, or a link to
/// one.
pub fn program_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(format!("{name}{EXE_SUFFIX}"));
    path.is_file().then_some(path)
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

fn is_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    const LIMIT: Duration = Duration::from_secs(5);

    #[test]
    fn random_numbers_differ() {
        assert_ne!(random(), random());
    }

    #[test]
    fn an_address_resolves_to_itself_without_a_lookup() {
        let found = resolve("127.0.0.1", LIMIT).unwrap();
        assert_eq!(found, [IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    }

    #[test]
    fn the_request_names_the_host_and_the_answer_is_read_to_the_end() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "the request ended early");
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        let answer = http_get(port, "doctor-ab.test", LIMIT).unwrap();
        assert_eq!(answer, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let request = server.join().unwrap();
        assert!(request.starts_with("GET / HTTP/1.1\r\n"), "{request}");
        assert!(
            request.contains("\r\nHost: doctor-ab.test\r\n"),
            "{request}"
        );
    }

    #[test]
    fn a_closed_port_takes_no_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(accepts(port, LIMIT));
        drop(listener);
        assert!(!accepts(port, LIMIT));
        assert!(http_get(port, "doctor-ab.test", LIMIT).is_err());
    }

    #[test]
    fn only_the_end_of_the_file_is_read_from_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marsad.log");
        assert_eq!(read_tail(&path, 16).unwrap(), None);
        std::fs::write(&path, "first line\nsecond\nthird\n").unwrap();
        assert_eq!(
            read_tail(&path, 1024).unwrap().as_deref(),
            Some("first line\nsecond\nthird\n")
        );
        // The last 12 bytes are "econd\nthird\n": "econd" is cut.
        assert_eq!(read_tail(&path, 12).unwrap().as_deref(), Some("third\n"));
        // No line break in the part read: nothing whole.
        assert_eq!(read_tail(&path, 3).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn a_program_is_found_by_its_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(program_in(dir.path(), "php"), None);
        let php = dir.path().join(format!("php{EXE_SUFFIX}"));
        std::fs::write(&php, "").unwrap();
        assert_eq!(program_in(dir.path(), "php"), Some(php));
        std::fs::create_dir(dir.path().join(format!("node{EXE_SUFFIX}"))).unwrap();
        assert_eq!(program_in(dir.path(), "node"), None);
    }
}
