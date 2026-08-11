//! A one-file HTTP/1.1 upstream, enough to drive the real clients.
//!
//! The landing zone's whole claim is that the bytes stored are the bytes
//! received, so the gate has to exercise the actual `reqwest` path rather
//! than hand a `serde_json::Value` to a parser. That needs a server, and a
//! server that can be told to fail on request N — which is the only way to
//! assert what a run that dies partway leaves behind.
//!
//! The listener binds port 0: the kernel assigns the port. Nothing here may
//! ever pick one (`tests/lib/ports_test.sh` is the repo-wide gate for that
//! rule, and a test that picks a port is a test that fails when two of them
//! run at once).

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// What the upstream should answer for one request.
pub struct Reply {
    /// HTTP status.
    pub status: u16,
    /// Response body.
    pub body: String,
}

impl Reply {
    /// A 200 carrying `body`.
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
        }
    }
}

/// A running fake upstream. Dropping it leaves the accept thread to die
/// with the process — tests are short and the alternative is shutdown
/// machinery that would outweigh the server itself.
pub struct FakeUpstream {
    addr: SocketAddr,
    /// Every request line the server has served, in order — `GET /path`
    /// with the query string, so a test can assert what was actually asked
    /// for.
    pub seen: Arc<Mutex<Vec<String>>>,
}

impl FakeUpstream {
    /// Start a server that answers each request from `route`, which is given
    /// the request target (`/3/groups`, query string included) and the
    /// zero-based request ordinal.
    pub fn start(route: impl Fn(&str, usize) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let seen = Arc::new(Mutex::new(Vec::new()));

        let served = Arc::new(AtomicUsize::new(0));
        let route = Arc::new(route);
        let thread_seen = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let n = served.fetch_add(1, Ordering::SeqCst);
                let Some(target) = read_target(&stream) else {
                    continue;
                };
                thread_seen.lock().expect("seen lock").push(target.clone());
                let reply = route(&target, n);
                let _ = write_reply(stream, &reply);
            }
        });

        Self { addr, seen }
    }

    /// The origin to point a client at, e.g. `http://127.0.0.1:34567`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every request target served so far, in order.
    pub fn requests(&self) -> Vec<String> {
        self.seen.lock().expect("seen lock").clone()
    }
}

/// Pull the request target out of the request line, then drain the headers.
fn read_target(stream: &TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None;
    }
    let target = request_line.split_whitespace().nth(1)?.to_string();
    // Drain headers so the client is not left writing into a full buffer.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    Some(target)
}

fn write_reply(mut stream: TcpStream, reply: &Reply) -> std::io::Result<()> {
    let reason = match reply.status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    };
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        reply.status,
        reply.body.len(),
    )?;
    stream.write_all(reply.body.as_bytes())?;
    stream.flush()
}
