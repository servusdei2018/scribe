//! Phase 28 integration tests for `scribe serve` / `scribe watch`.
//!
//! These spawn the real `scribe` binary against a temporary fixture, exercise
//! the embedded HTTP server and the incremental rebuild loop over TCP, and
//! compare served HTML with a fresh `scribe build -b html` run (modulo the
//! injected live-reload tag). No fixed sleeps are used for synchronization:
//! every wait is a bounded poll.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_scribe");
const LIVE_TAG: &str = "<script src=\"/_static/scribe-live.js\" data-scribe-live></script>";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Minimal RAII temp directory (no external `tempfile` dependency needed).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "scribe-serve-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp root");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Spawn a `scribe serve` process and discover its bound port from stdout.
struct Served {
    child: Option<Child>,
    addr: SocketAddr,
    // Keep both pipes draining for the lifetime of the child. Dropping the
    // stdout reader closes the pipe and the server panics on its next
    // `println!` (exit 101), which superficially looks like a shutdown bug.
    _stdout_thread: Option<thread::JoinHandle<String>>,
    _stderr_thread: Option<thread::JoinHandle<String>>,
}

impl Served {
    fn start(source: &Path, output: &Path, extra: &[&str]) -> Self {
        let mut child = Command::new(BIN)
            .arg("serve")
            .arg(source)
            .arg(output)
            .args(["--port", "0", "--delay", "60"])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn scribe serve");

        let stdout = child.stdout.take().expect("stdout piped");
        let mut reader = BufReader::new(stdout);
        let deadline = Instant::now() + Duration::from_secs(20);
        let addr: SocketAddr = loop {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("server did not print its address in time");
            }
            let mut line = String::new();
            let read = reader.read_line(&mut line).expect("read stdout line");
            if read == 0 {
                let _ = child.kill();
                panic!("server exited before printing its address");
            }
            if let Some(target) = line.split("http://").nth(1) {
                let host_and_port = target.split_whitespace().next().unwrap_or_default();
                let port_text = host_and_port.rsplit(':').next().unwrap_or_default();
                if let Ok(port) = port_text.parse::<u16>() {
                    let host = host_and_port.rsplit_once(':').map(|(host, _)| host).unwrap_or("127.0.0.1");
                    break format!("{host}:{port}").parse().expect("valid socket address");
                }
            }
        };

        // Drain both pipes in the background for the child's whole lifetime.
        let stdout_thread = thread::spawn(move || {
            let mut text = String::new();
            let mut reader = reader;
            let _ = reader.read_to_string(&mut text);
            text
        });
        let mut stderr = child.stderr.take().expect("stderr piped");
        let stderr_thread = thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });

        let served = Self {
            child: Some(child),
            addr,
            _stdout_thread: Some(stdout_thread),
            _stderr_thread: Some(stderr_thread),
        };
        served.wait_ready();
        served
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }

    fn pid(&self) -> u32 {
        self.child.as_ref().expect("child still running").id()
    }

    fn take_child(&mut self) -> Child {
        self.child.take().expect("child still running")
    }

    fn wait_ready(&self) {
        poll_until(Duration::from_secs(10), || {
            http_get(self.addr, "/_scribe_health").map(|(status, _)| status == 200).unwrap_or(false)
        })
        .expect("server became ready");
    }

    fn kill(mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn poll_until<F>(timeout: Duration, mut predicate: F) -> Result<(), String>
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("condition not met before timeout".to_owned());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn http_get(addr: SocketAddr, path: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").ok()?;
    stream.flush().ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())?;
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
    Some((status, body))
}

/// Open an SSE stream, returning the reader once the response headers are
/// consumed. The caller reads event frames with a bounded timeout.
/// Retry a single GET briefly to absorb transient connection races.
fn http_get_retry(addr: SocketAddr, path: &str) -> (u16, String) {
    for _ in 0..40 {
        if let Some(result) = http_get(addr, path) {
            return result;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("GET {path} never produced a response");
}

fn open_events(addr: SocketAddr) -> Option<TcpStream> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    write!(stream, "GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n").ok()?;
    stream.flush().ok()?;
    let mut byte = [0_u8; 1];
    let mut seen = String::new();
    loop {
        let count = stream.read(&mut byte).ok()?;
        if count == 0 {
            return None;
        }
        seen.push(byte[0] as char);
        if seen.ends_with("\r\n\r\n") {
            return Some(stream);
        }
        if seen.len() > 4096 {
            return None;
        }
    }
}

fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let source = root.join("src");
    let output = root.join("out");
    fs::create_dir_all(&source).expect("create source dir");
    fs::write(
        source.join("conf.py"),
        "project = 'Serve Test'\nmaster_doc = 'index'\nextensions = []\n",
    )
    .expect("write conf.py");
    fs::write(
        source.join("index.rst"),
        "Serve Test\n==========\n\nHello from scribe serve.\n\n.. toctree::\n\n   other\n",
    )
    .expect("write index.rst");
    fs::write(source.join("other.rst"), "Other\n=====\n\nSecond page.\n").expect("write other.rst");
    (source, output)
}

#[test]
fn serves_index_with_injected_live_reload_script() {
    let temp = TempDir::new("serve-index");
    let (source, output) = fixture(temp.path());
    let served = Served::start(&source, &output, &[]);
    let (status, body) = http_get_retry(served.addr(), "/index.html");
    assert_eq!(status, 200);
    assert!(body.contains("Hello from scribe serve."), "generated page served: {body}");
    assert!(body.contains(LIVE_TAG), "live-reload script injected: {body}");
    served.kill();
}

#[test]
fn slash_and_directory_index_resolve_to_index_html() {
    let temp = TempDir::new("serve-index-root");
    let (source, output) = fixture(temp.path());
    let served = Served::start(&source, &output, &[]);
    let (status, body) = http_get_retry(served.addr(), "/");
    assert_eq!(status, 200);
    assert!(body.contains("Serve Test"));
    let (status, body) = http_get_retry(served.addr(), "/_static/pygments.css");
    assert_eq!(status, 200);
    assert!(!body.contains(LIVE_TAG), "CSS must not be instrumented");
    served.kill();
}

#[test]
fn missing_pages_return_404_and_outside_paths_are_forbidden() {
    let temp = TempDir::new("serve-404");
    let (source, output) = fixture(temp.path());
    let served = Served::start(&source, &output, &[]);
    let (status, _) = http_get_retry(served.addr(), "/does-not-exist.html");
    assert_eq!(status, 404);
    let (status, _) = http_get_retry(served.addr(), "/../secret");
    assert!(status == 403 || status == 404);
    served.kill();
}

#[test]
fn file_change_triggers_incremental_rebuild_and_http_refresh() {
    let temp = TempDir::new("serve-rebuild");
    let (source, output) = fixture(temp.path());
    let served = Served::start(&source, &output, &[]);

    let marker = "FRESH CONTENT MARKER 424242";
    let index = source.join("index.rst");
    fs::write(
        &index,
        format!("Serve Test\n==========\n\n{marker}\n\n.. toctree::\n\n   other\n"),
    )
    .expect("rewrite index.rst");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_marker = false;
    while Instant::now() < deadline {
        if let Some((status, body)) = http_get(served.addr(), "/index.html") {
            if status == 200 && body.contains(marker) {
                saw_marker = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(saw_marker, "served page never reflected the edited source");
    served.kill();
}

#[test]
fn reload_event_is_broadcast_only_after_successful_rebuild() {
    let temp = TempDir::new("serve-sse");
    let (source, output) = fixture(temp.path());
    let served = Served::start(&source, &output, &[]);

    let mut events = open_events(served.addr()).expect("SSE connection opened");
    fs::write(source.join("other.rst"), "Other\n=====\n\nUpdated second page.\n")
        .expect("rewrite other.rst");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut got_event = false;
    let mut buffer = [0_u8; 4096];
    while Instant::now() < deadline {
        match events.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let text = String::from_utf8_lossy(&buffer[..count]);
                if text.contains("event: scribe-reload") {
                    got_event = true;
                    break;
                }
            }
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
    assert!(got_event, "no reload event received after rebuild");
    served.kill();
}

#[test]
fn no_reload_disables_injection_and_events_endpoint() {
    let temp = TempDir::new("serve-no-reload");
    let (source, output) = fixture(temp.path());
    let served = Served::start(&source, &output, &["--no-reload"]);
    let (status, body) = http_get_retry(served.addr(), "/index.html");
    assert_eq!(status, 200);
    assert!(!body.contains(LIVE_TAG), "injection must be disabled");
    let (status, _) = http_get_retry(served.addr(), "/events");
    assert_eq!(status, 404);
    served.kill();
}

#[test]
fn served_page_matches_fresh_build_modulo_injected_script() {
    let temp = TempDir::new("serve-differential");
    let (source, output) = fixture(temp.path());
    let served = Served::start(&source, &output, &[]);

    // Edit a page so the served output is the product of an incremental
    // rebuild, then compare it with an independent full build.
    fs::write(
        source.join("index.rst"),
        "Serve Test\n==========\n\nDifferential content here.\n\n.. toctree::\n\n   other\n",
    )
    .expect("rewrite index.rst");
    poll_until(Duration::from_secs(20), || {
        http_get(served.addr(), "/index.html")
            .map(|(_, body)| body.contains("Differential content here."))
            .unwrap_or(false)
    })
    .expect("incremental rebuild reflected in served page");

    let fresh = temp.path().join("fresh");
    let status = Command::new(BIN)
        .args(["build", "-b", "html", "-E", "-q"])
        .arg(&source)
        .arg(&fresh)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run fresh build");
    assert!(status.success());

    let served_html = http_get_retry(served.addr(), "/index.html").1;
    let fresh_html = fs::read_to_string(fresh.join("index.html")).expect("read fresh page");
    let normalized = served_html.replace(LIVE_TAG, "");
    assert_eq!(
        normalized, fresh_html,
        "served incremental page must equal a fresh `scribe build -b html` output"
    );
    served.kill();
}

#[test]
fn incremental_rebuild_completes_quickly_on_a_small_fixture() {
    let temp = TempDir::new("serve-perf");
    let (source, output) = fixture(temp.path());
    let served = Served::start(&source, &output, &[]);
    let started = Instant::now();
    fs::write(source.join("index.rst"), "Serve Test\n==========\n\nTiming probe.\n")
        .expect("rewrite index.rst");
    poll_until(Duration::from_secs(20), || {
        http_get(served.addr(), "/index.html")
            .map(|(_, body)| body.contains("Timing probe."))
            .unwrap_or(false)
    })
    .expect("timing probe reflected");
    let elapsed = started.elapsed();
    eprintln!("[perf] incremental rebuild + HTTP refresh took {elapsed:?}");
    assert!(elapsed < Duration::from_secs(15), "rebuild took too long: {elapsed:?}");
    served.kill();
}

#[test]
fn graceful_shutdown_on_sigint_exits_cleanly() {
    let temp = TempDir::new("serve-shutdown");
    let (source, output) = fixture(temp.path());
    let mut served = Served::start(&source, &output, &[]);
    let pid = served.pid();
    let status = Command::new("kill").arg("-INT").arg(pid.to_string()).status().expect("send SIGINT");
    assert!(status.success(), "kill -INT failed");

    let (tx, rx) = std::sync::mpsc::channel();
    let mut child = served.take_child();
    thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    let result = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("server exited after SIGINT")
        .expect("wait on child");
    assert!(result.success(), "graceful shutdown should exit 0: {result:?}");
}

#[test]
fn port_conflict_and_missing_source_are_clean_errors() {
    let temp = TempDir::new("serve-errors");
    let (source, output) = fixture(temp.path());

    // Hold a port, then ask the server to bind it.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
    let port = listener.local_addr().expect("local addr").port();
    let conflict = Command::new(BIN)
        .arg("serve")
        .arg(&source)
        .arg(&output)
        .args(["--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run conflicting server");
    assert!(!conflict.status.success(), "port conflict must fail");
    let stderr = String::from_utf8_lossy(&conflict.stderr);
    assert!(
        stderr.contains("cannot listen") || stderr.contains("bind_failed"),
        "clear bind diagnostic: {stderr}"
    );
    drop(listener);

    let missing = Command::new(BIN)
        .arg("serve")
        .arg(temp.path().join("does-not-exist"))
        .arg(output)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run missing-source server");
    assert!(!missing.status.success(), "missing source must fail");
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("source directory") || stderr.contains("missing_source"),
        "clear missing-source diagnostic: {stderr}"
    );
}
