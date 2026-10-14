//! Offline loopback HTTP server for CLI provider-routing tests.
//!
//! Standalone copy of the `relay-providers` conformance mock pattern,
//! reduced to what the CLI suites need: binds 127.0.0.1 only, serves one
//! canned SSE response per test, and records the requests it received so
//! tests can assert the wire path and headers real invocations produced.
//! No test in this crate ever touches the network.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

/// One HTTP exchange recorded by the mock server.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// Request method.
    pub method: String,
    /// Request path and query.
    pub path: String,
    /// Header names lower-cased, values verbatim.
    pub headers: HashMap<String, String>,
    /// Parsed JSON body (or `Value::Null` when the body was not JSON).
    pub body: Value,
}

impl RecordedRequest {
    /// A header value by lower-cased name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

/// The canned response served for every request: one SSE body.
pub struct CannedResponse {
    /// HTTP status.
    pub status: u16,
    /// The SSE body (already framed `data:` events, or plain text).
    pub body: String,
}

struct MockShared {
    shutdown: AtomicBool,
    requests: Mutex<Vec<RecordedRequest>>,
}

/// A loopback mock HTTP server. Dropping it stops the listener.
pub struct MockServer {
    /// Base URL to point providers at (`http://127.0.0.1:<port>`).
    pub base_url: String,
    shared: Arc<MockShared>,
    worker: Option<JoinHandle<()>>,
}

impl MockServer {
    /// Start a server that answers every request with `response`.
    pub fn start(response: CannedResponse) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        let port = listener.local_addr().expect("bound addr").port();
        let shared = Arc::new(MockShared {
            shutdown: AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("relay-cli-mock-http".into())
            .spawn(move || serve(&listener, worker_shared, response))
            .expect("spawn mock server");

        MockServer {
            base_url: format!("http://127.0.0.1:{port}"),
            shared,
            worker: Some(worker),
        }
    }

    /// The requests the server handled so far.
    pub fn recorded(&self) -> Vec<RecordedRequest> {
        self.shared.requests.lock().expect("request log").clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        // The accept loop polls the shutdown flag, so the worker exits on
        // its own; joining keeps tests hermetic.
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(listener: &TcpListener, shared: Arc<MockShared>, response: CannedResponse) {
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                handle_connection(stream, &shared, &response);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return,
        }
    }
}

fn handle_connection(mut stream: TcpStream, shared: &Arc<MockShared>, response: &CannedResponse) {
    let _ = stream.set_nodelay(true);
    stream.set_nonblocking(false).expect("blocking stream");
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    shared.requests.lock().expect("request log").push(request);

    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        response.status,
        reason(response.status)
    );
    if stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(response.body.as_bytes()))
        .and_then(|_| stream.flush())
        .is_err()
    {}
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

/// Read one HTTP request (head plus content-length body) from the stream.
fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    // Read until the end of the head (blank line) appears anywhere in the
    // buffer; the read may already include part of the body.
    let head_end = loop {
        if let Some(index) = find_head_end(&buffer) {
            break index;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(_) => return None,
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut body_bytes = buffer[head_end..].to_vec();

    let mut lines = head.split("\r\n").flat_map(|line| line.split('\n'));
    let request_line = lines.next()?.to_owned();
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    while body_bytes.len() < content_length {
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(read) => body_bytes.extend_from_slice(&chunk[..read]),
            Err(_) => return None,
        }
    }
    body_bytes.truncate(content_length);
    let body: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    Some(RecordedRequest {
        method,
        path,
        headers,
        body,
    })
}

/// Find the end of the HTTP head: the first `\r\n\r\n` (or bare `\n\n`),
/// returning the index just past it.
fn find_head_end(buffer: &[u8]) -> Option<usize> {
    const PATTERNS: [&[u8]; 2] = [b"\r\n\r\n", b"\n\n"];
    let mut earliest: Option<usize> = None;
    for pattern in PATTERNS {
        if let Some(index) = buffer
            .windows(pattern.len())
            .position(|window| window == pattern)
        {
            let end = index + pattern.len();
            earliest = Some(match earliest {
                Some(current) => current.min(end),
                None => end,
            });
        }
    }
    earliest
}
