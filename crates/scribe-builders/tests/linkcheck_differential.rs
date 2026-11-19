//! Differential tests for the Phase 25 linkcheck builder.
//!
//! A tiny tokio HTTP server serves deterministic good/broken/redirect/anchor
//! pages on an ephemeral localhost port. Both scribe and the pinned Python
//! Sphinx oracle check the same source tree against that server; their
//! `output.json` statuses are compared. **No external network is used**, so
//! the test is fully deterministic and offline.

mod phase25_common;

use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::process::Command;
use std::sync::mpsc;

use phase25_common::{config, options, temp_dir, workspace_root};
use scribe_builders::{LinkcheckBuilder, LinkStatus};
use scribe_core::BuildEnvironment;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

struct TestServer {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
    }
}

fn spawn_server() -> TestServer {
    let (addr_tx, addr_rx) = mpsc::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind local server");
            addr_tx.send(listener.local_addr().expect("local addr")).expect("send addr");
            let mut shutdown = shutdown_rx;
            loop {
                tokio::select! {
                    _ = &mut shutdown => break,
                    accepted = listener.accept() => {
                        if let Ok((stream, _)) = accepted {
                            tokio::spawn(handle_connection(stream));
                        }
                    }
                }
            }
        });
    });
    let addr = addr_rx.recv().expect("server must publish its address");
    TestServer { addr, shutdown: Some(shutdown_tx) }
}

async fn handle_connection(mut stream: TcpStream) {
    let mut buffer = vec![0u8; 8192];
    let read = stream.read(&mut buffer).await.unwrap_or(0);
    let request = String::from_utf8_lossy(&buffer[..read]);
    let line = request.lines().next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/").to_owned();

    let (status, reason, headers, full_body) = match path.as_str() {
        "/good" => (200, "OK", "", "ok\n"),
        "/anchor" => (200, "OK", "", "<!DOCTYPE html><html><body><a id=\"target\">x</a></body></html>"),
        "/redirect" => (302, "Found", "Location: /good\r\n", ""),
        "/broken" => (404, "Not Found", "", ""),
        "/ignored" => (404, "Not Found", "", ""),
        _ => (404, "Not Found", "", ""),
    };
    let body = if method == "HEAD" { "" } else { full_body };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn fixture(addr: SocketAddr) -> std::path::PathBuf {
    let dir = temp_dir("linkcheck-src");
    fs::write(
        dir.join("conf.py"),
        "version = '1.0'\nlinkcheck_timeout = 2\nlinkcheck_ignore = ['.*ignored']\n",
    )
    .expect("write conf.py");
    fs::write(
        dir.join("index.rst"),
        format!(
            "Linkcheck fixture\n=================\n\n\
             `good <http://{addr}/good>`_\n\n\
             `anchor <http://{addr}/anchor#target>`_\n\n\
             `bad anchor <http://{addr}/anchor#missing>`_\n\n\
             `redirect <http://{addr}/redirect>`_\n\n\
             `broken <http://{addr}/broken>`_\n\n\
             `ignored <http://{addr}/ignored>`_\n"
        ),
    )
    .expect("write index.rst");
    dir
}

fn statuses(path: &std::path::Path) -> BTreeMap<String, String> {
    let json = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    json.lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("JSON line");
            let uri = value["uri"].as_str().unwrap_or("").to_owned();
            let status = value["status"].as_str().unwrap_or("").to_owned();
            (uri, status)
        })
        .collect()
}

#[test]
fn linkcheck_checks_local_http_server_and_matches_oracle() {
    let server = spawn_server();
    let src = fixture(server.addr);
    let scribe_out = temp_dir("linkcheck-scribe-out");
    let oracle_out = temp_dir("linkcheck-oracle-out");

    let config = config(&src, &[]);
    let opts = options(&src, &scribe_out, "linkcheck");
    let mut env = BuildEnvironment::with_config(&src, &config);
    let report = LinkcheckBuilder::build(&opts, &config, &mut env).expect("linkcheck build");

    // Our own expectations (independent of the oracle).
    assert_eq!(report.broken_count(), 2, "{:#?}", report.results);
    assert!(report.results.iter().any(|r| r.status == LinkStatus::Working && r.uri.ends_with("/good")));
    assert!(report.results.iter().any(|r| r.status == LinkStatus::Working && r.uri.ends_with("/anchor#target")));
    assert!(report.results.iter().any(|r| r.status == LinkStatus::Redirected && r.uri.ends_with("/redirect")));
    assert!(report.results.iter().any(|r| r.status == LinkStatus::Ignored && r.uri.ends_with("/ignored")));
    assert!(report.results.iter().any(|r| r.status == LinkStatus::Broken && r.info.contains("Anchor 'missing' not found")));

    // Deterministic report ordering: sorted by filename, line and URI.
    let keys: Vec<(&str, i64, &str)> = report
        .results
        .iter()
        .map(|r| (r.filename.as_str(), r.lineno, r.uri.as_str()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);

    let binary = workspace_root().join(".oracle-venv/bin/sphinx-build");
    let status = Command::new(binary)
        .args(["-b", "linkcheck", "-E", "-q"])
        .arg(&src)
        .arg(&oracle_out)
        .status()
        .expect("oracle must be executable");
    // Upstream sets exit status 1 when broken links are found; that is the
    // expected outcome for this fixture.
    assert!(!status.success());

    let scribe_statuses = statuses(&scribe_out.join("output.json"));
    let oracle_statuses = statuses(&oracle_out.join("output.json"));
    assert_eq!(scribe_statuses.len(), oracle_statuses.len(), "status maps: {scribe_statuses:?} vs {oracle_statuses:?}");
    for (uri, expected) in &oracle_statuses {
        let actual = scribe_statuses.get(uri).unwrap_or_else(|| panic!("missing {uri} in scribe report: {scribe_statuses:?}"));
        assert_eq!(actual, expected, "status mismatch for {uri}");
    }
}

#[test]
fn malformed_url_and_network_error_do_not_panic() {
    let server = spawn_server();
    let src = temp_dir("linkcheck-malformed-src");
    fs::write(dir_conf(&src), "version = '1.0'\nlinkcheck_timeout = 1\n").expect("conf.py");
    fs::write(
        src.join("index.rst"),
        format!(
            "Malformed links\n===============\n\n\
             `bad <http://[::1`_\n\n\
             `closed port <http://127.0.0.1:{}/nope>`_\n\n\
             `mailto <mailto:someone@example.com>`_\n",
            server.addr.port()
        ),
    )
    .expect("index.rst");
    let out = temp_dir("linkcheck-malformed-out");
    let config = config(&src, &[]);
    let opts = options(&src, &out, "linkcheck");
    let mut env = BuildEnvironment::with_config(&src, &config);
    let report = LinkcheckBuilder::build(&opts, &config, &mut env).expect("no panic on malformed URIs");
    assert!(report.results.len() >= 2, "{:#?}", report.results);
    assert!(report.results.iter().any(|r| r.status == LinkStatus::Unchecked && r.uri.starts_with("mailto:")));
}

fn dir_conf(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("conf.py")
}

#[test]
fn internal_links_and_anchors_are_verified_without_network() {
    let src = temp_dir("linkcheck-local-src");
    fs::write(dir_conf(&src), "version = '1.0'\n").expect("conf.py");
    fs::write(
        src.join("index.rst"),
        "Index\n=====\n\n\
         .. _here:\n\n\
         `other anchor <other.rst#target>`_\n\n\
         `other missing anchor <other.rst#nope>`_\n\n\
         `missing doc <missing.rst>`_\n",
    )
    .expect("index.rst");
    fs::write(src.join("other.rst"), "Target\n======\n\nBody.\n").expect("other.rst");
    let out = temp_dir("linkcheck-local-out");
    let config = config(&src, &[]);
    let opts = options(&src, &out, "linkcheck");
    let mut env = BuildEnvironment::with_config(&src, &config);
    let report = LinkcheckBuilder::build(&opts, &config, &mut env).expect("local linkcheck build");

    let by_uri: BTreeMap<&str, LinkStatus> = report
        .results
        .iter()
        .map(|r| (r.uri.as_str(), r.status))
        .collect();
    assert_eq!(by_uri.get("other.rst#target"), Some(&LinkStatus::Working));
    assert_eq!(by_uri.get("other.rst#nope"), Some(&LinkStatus::Broken));
    assert_eq!(by_uri.get("missing.rst"), Some(&LinkStatus::Broken));
}
