//! Live-seam helpers: start `tachylite serve`, and speak HTTP/1.1 and SSE over std sockets.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault")
}

pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for item in fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        let target = to.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_dir(&item.path(), &target);
        } else {
            fs::copy(item.path(), target).unwrap();
        }
    }
}

/// A temp dir with a copy of the fixture vault at `vault/` and an output path `out/`.
pub struct Dirs {
    pub tmp: tempfile::TempDir,
}

impl Dirs {
    pub fn fixture() -> Dirs {
        let tmp = tempfile::tempdir().unwrap();
        copy_dir(&fixture(), &tmp.path().join("vault"));
        Dirs { tmp }
    }
    pub fn empty() -> Dirs {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("vault")).unwrap();
        Dirs { tmp }
    }
    pub fn vault(&self) -> PathBuf {
        self.tmp.path().join("vault")
    }
    pub fn out(&self) -> PathBuf {
        self.tmp.path().join("out")
    }
}

/// A running `tachylite serve`. Dropping it kills the child it spawned, and nothing else.
pub struct Server {
    pub child: Child,
    pub addr: SocketAddr,
    stderr: Receiver<String>,
}

impl Server {
    pub fn start(vault: &Path, out: &Path) -> Server {
        Server::start_with(vault, out, &[])
    }

    /// Starts with extra environment variables.
    pub fn start_with(vault: &Path, out: &Path, envs: &[(&str, &str)]) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tachylite"))
            .arg("serve")
            .arg(vault)
            .arg(out)
            .args(["--port", "0"])
            .envs(envs.iter().copied())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, rx) = channel();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Server {
            child,
            addr: "127.0.0.1:1".parse().unwrap(),
            stderr: rx,
        };
        let line = server
            .wait_line(
                |l| l.starts_with("serving http://"),
                Duration::from_secs(30),
            )
            .expect("server did not start");
        server.addr = line["serving http://".len()..].parse().unwrap();
        server
    }

    /// Waits for a stderr line matching `pred`, returning it, or `None` at the deadline.
    pub fn wait_line(&self, pred: impl Fn(&str) -> bool, within: Duration) -> Option<String> {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match self.stderr.recv_timeout(left) {
                Ok(line) if pred(&line) => return Some(line),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }

    pub fn get(&self, path: &str) -> Response {
        get(self.addr, path, &[])
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

pub fn get(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("no header end");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Response {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    }
}

/// Percent-encodes everything but unreserved characters, as `encodeURIComponent` does.
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The `tachylite-version` meta of a page.
pub fn version_of(html: &str) -> String {
    let marker = "<meta name=\"tachylite-version\" content=\"";
    let start = html.find(marker).expect("no version meta") + marker.len();
    html[start..start + 16].to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub name: String,
    pub id: Option<String>,
    pub data: String,
}

/// One open SSE stream, read with deadlines.
pub struct Events {
    stream: TcpStream,
    raw: Vec<u8>,
    body: Vec<u8>,
}

impl Server {
    /// Subscribes as a page at `pathname` showing `version` would.
    pub fn subscribe(&self, pathname: &str, version: &str, last_event_id: Option<&str>) -> Events {
        let mut stream = TcpStream::connect(self.addr).unwrap();
        let mut request = format!(
            "GET /.tachylite/events?page={}&v={version} HTTP/1.1\r\nHost: test\r\nAccept: text/event-stream\r\n",
            encode(pathname)
        );
        if let Some(id) = last_event_id {
            request.push_str(&format!("Last-Event-ID: {id}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        let mut events = Events {
            stream,
            raw: Vec::new(),
            body: Vec::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let split = loop {
            if let Some(i) = events.raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            assert!(events.fill(deadline), "no SSE response headers");
        };
        let head = String::from_utf8_lossy(&events.raw[..split]).into_owned();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(
            head.to_ascii_lowercase().contains("text/event-stream"),
            "{head}"
        );
        events.raw.drain(..split + 4);
        events
    }
}

impl Events {
    /// Reads more bytes from the socket before `deadline`. False on timeout or close.
    fn fill(&mut self, deadline: Instant) -> bool {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        self.stream
            .set_read_timeout(Some(left.max(Duration::from_millis(1))))
            .unwrap();
        let mut buf = [0u8; 65536];
        match self.stream.read(&mut buf) {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.raw.extend_from_slice(&buf[..n]);
                true
            }
        }
    }

    /// Moves complete chunks of the chunked body from `raw` to `body`.
    fn dechunk(&mut self) {
        loop {
            let Some(eol) = self.raw.windows(2).position(|w| w == b"\r\n") else {
                return;
            };
            let size_text = String::from_utf8_lossy(&self.raw[..eol]).into_owned();
            let size = usize::from_str_radix(size_text.trim(), 16).expect("chunk size");
            if self.raw.len() < eol + 2 + size + 2 {
                return;
            }
            self.body
                .extend_from_slice(&self.raw[eol + 2..eol + 2 + size]);
            self.raw.drain(..eol + 2 + size + 2);
        }
    }

    /// The next named event before the deadline, skipping keep-alive comments.
    pub fn next(&mut self, within: Duration) -> Option<SseEvent> {
        let deadline = Instant::now() + within;
        loop {
            self.dechunk();
            while let Some(end) = self.body.windows(2).position(|w| w == b"\n\n") {
                let block = String::from_utf8_lossy(&self.body[..end]).into_owned();
                self.body.drain(..end + 2);
                let mut event = SseEvent {
                    name: "message".into(),
                    id: None,
                    data: String::new(),
                };
                let mut data = Vec::new();
                let mut any = false;
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event.name = v.trim_start().to_string();
                        any = true;
                    } else if let Some(v) = line.strip_prefix("id:") {
                        event.id = Some(v.trim_start().to_string());
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
                        any = true;
                    }
                }
                if any {
                    event.data = data.join("\n");
                    return Some(event);
                }
            }
            if !self.fill(deadline) {
                return None;
            }
        }
    }

    pub fn expect(&mut self, name: &str) -> SseEvent {
        self.expect_within(name, Duration::from_secs(10))
    }

    /// Like `expect`, for an event behind slow work, such as a debug build's resync.
    pub fn expect_within(&mut self, name: &str, within: Duration) -> SseEvent {
        let event = self
            .next(within)
            .unwrap_or_else(|| panic!("no {name} event"));
        assert_eq!(event.name, name, "{event:?}");
        event
    }
}
