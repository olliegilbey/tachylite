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
        let mut child = Command::new(env!("CARGO_BIN_EXE_tachylite"))
            .arg("serve")
            .arg(vault)
            .arg(out)
            .args(["--port", "0"])
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
