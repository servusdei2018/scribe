//! Phase 33: end-to-end remote `objects.inv` fetching against a local mock
//! HTTP server.  No external network and no extra dev-dependency are used: the
//! server is a small `std::net::TcpListener` that speaks just enough HTTP/1.1
//! for `reqwest::blocking`.
//!
//! Every case is non-fatal by contract: malformed inventories, 404s, timeouts
//! and expired caches must degrade to warnings, never panics or build errors.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use scribe_domains::intersphinx::{CacheEntry, FetchOptions};
use scribe_domains::{
    fetch_one, DumpConfig, Inventory, InventoryCache, MappingEntry, ObjectEntry,
};

const BASE_URI: &str = "http://127.0.0.1/";

fn valid_inventory() -> Vec<u8> {
    let mut objects = BTreeMap::new();
    objects.insert(
        "py".to_owned(),
        vec![ObjectEntry::new(
            "remote_func",
            "remote_func",
            "function",
            "api",
            "remote_func",
            1,
        )],
    );
    Inventory::dump(
        &objects,
        &DumpConfig {
            project: "Mock Project",
            version: "1.0",
        },
        |docname| format!("{docname}.html"),
    )
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("scribe-phase33-http-{tag}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn mapping(location: String) -> MappingEntry {
    MappingEntry {
        name: "mock".to_owned(),
        target_uri: BASE_URI.to_owned(),
        locations: vec![Some(location)],
    }
}

/// One-shot HTTP server.  `respond` receives the raw request line and returns
/// the complete HTTP response bytes to send back.
struct MockServer {
    url: String,
    done: mpsc::Receiver<()>,
}

fn serve(respond: impl Fn(&str) -> Vec<u8> + Send + 'static) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().expect("local addr").port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let request = String::from_utf8_lossy(&buf).to_string();
            let first_line = request.lines().next().unwrap_or("");
            let response = respond(first_line);
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
        let _ = tx.send(());
    });
    MockServer {
        url: format!("http://127.0.0.1:{port}/objects.inv"),
        done: rx,
    }
}

impl MockServer {
    fn wait(self) {
        let _ = self.done.recv_timeout(Duration::from_secs(5));
    }
}

fn http_response(status: &str, extra: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len()).into_bytes();
    if !extra.is_empty() {
        out.extend_from_slice(extra.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

#[test]
fn mock_http_success() {
    let inventory = valid_inventory();
    let server = serve(move |_| http_response("200 OK", "Content-Type: application/octet-stream", &inventory));
    let url = server.url.clone();
    let options = FetchOptions {
        retries: 0,
        ..FetchOptions::default()
    };
    let outcome = fetch_one(&mapping(url), &options, None);
    assert!(outcome.inventory.is_some(), "warnings: {:?}", outcome.warnings);
    assert!(!outcome.from_cache);
    assert_eq!(
        outcome.inventory.and_then(|inv| inv.get("py:function", "remote_func").map(|item| item.uri.clone())),
        Some(format!("{BASE_URI}api.html#remote_func"))
    );
    server.wait();
}

#[test]
fn mock_http_follows_redirect() {
    let inventory = valid_inventory();
    // The first request is redirected to /final.inv; a second connection
    // serves the inventory.  `reqwest` follows it via the client's redirect
    // policy configured in `fetch_http`.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect server");
    let port = listener.local_addr().expect("local addr").port();
    let body = inventory.clone();
    let handle = thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut stream, _)) = listener.accept() else { break };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let request = String::from_utf8_lossy(&buf).to_string();
            let first = request.lines().next().unwrap_or("");
            let response = if first.contains("/final.inv") {
                http_response("200 OK", "", &body)
            } else {
                http_response("302 Found", &format!("Location: http://127.0.0.1:{port}/final.inv\r\n"), b"")
            };
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
    });
    let options = FetchOptions {
        retries: 0,
        ..FetchOptions::default()
    };
    let outcome = fetch_one(&mapping(format!("http://127.0.0.1:{port}/objects.inv")), &options, None);
    assert!(outcome.inventory.is_some(), "warnings: {:?}", outcome.warnings);
    let _ = handle.join();
}

#[test]
fn mock_http_404_degrades_to_warning() {
    let server = serve(|_| http_response("404 Not Found", "", b"missing"));
    let url = server.url.clone();
    let options = FetchOptions {
        retries: 0,
        ..FetchOptions::default()
    };
    let outcome = fetch_one(&mapping(url), &options, None);
    assert!(outcome.inventory.is_none());
    assert!(!outcome.warnings.is_empty());
    assert!(outcome.warnings.iter().any(|warning| warning.contains("404")));
    server.wait();
}

#[test]
fn mock_http_timeout_falls_back_to_stale_cache() {
    let dir = temp_dir("timeout");
    let cache_dir = dir.join("cache");
    let cache = InventoryCache::new(cache_dir.clone(), 5);
    let key = "mock\0http://127.0.0.1/";
    // Seed a deliberately expired entry: TTL is bypassed only by the offline
    // fallback path after a fresh fetch fails.
    cache
        .put(key, &CacheEntry {
            name: "mock".to_owned(),
            timestamp: 1_000,
            bytes: valid_inventory(),
        })
        .expect("seed cache");

    // A server that accepts but never answers exercises the read timeout.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind silent server");
    let port = listener.local_addr().expect("local addr").port();
    let handle = thread::spawn(move || {
        if let Ok((_stream, _)) = listener.accept() {
            thread::sleep(Duration::from_millis(400));
        }
    });
    let options = FetchOptions {
        timeout: Some(Duration::from_millis(60)),
        connect_timeout: Some(Duration::from_millis(60)),
        read_timeout: Some(Duration::from_millis(60)),
        retries: 0,
        retry_backoff: Duration::ZERO,
        cache_dir: Some(cache_dir.clone()),
        cache_limit_days: 5,
        now: 2_000_000_000,
        ..FetchOptions::default()
    };
    let outcome = fetch_one(
        &mapping(format!("http://127.0.0.1:{port}/objects.inv")),
        &options,
        Some(&cache),
    );
    assert!(outcome.inventory.is_some(), "warnings: {:?}", outcome.warnings);
    assert!(outcome.from_cache, "expected stale-cache fallback");
    assert!(outcome.warnings.iter().any(|warning| warning.contains("stale")));
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cache_hit_within_ttl_then_miss_after_ttl() {
    let dir = temp_dir("ttl");
    let cache_dir = dir.join("cache");
    let cache = InventoryCache::new(cache_dir.clone(), 5);
    let key = "mock\0http://127.0.0.1/";
    cache
        .put(key, &CacheEntry {
            name: "mock".to_owned(),
            timestamp: 1_700_000_000,
            bytes: valid_inventory(),
        })
        .expect("seed cache");

    let fresh = FetchOptions {
        cache_dir: Some(cache_dir.clone()),
        now: 1_700_000_000 + 4 * 86_400,
        ..FetchOptions::default()
    };
    // Point at a closed port: the fresh cache short-circuits before any fetch.
    let outcome = fetch_one(&mapping("http://127.0.0.1:1/objects.inv".to_owned()), &fresh, Some(&cache));
    assert!(outcome.from_cache);
    assert!(outcome.inventory.is_some());

    let expired = FetchOptions {
        now: 1_700_000_000 + 6 * 86_400,
        // TTL expiry only means "miss" when offline fallback is disabled;
        // otherwise the stale entry is deliberately kept as a fallback.
        offline_fallback: false,
        ..fresh
    };
    let outcome = fetch_one(&mapping("http://127.0.0.1:1/objects.inv".to_owned()), &expired, Some(&cache));
    assert!(!outcome.from_cache);
    assert!(outcome.inventory.is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cached_real_inventory_fixture() {
    // A real inventory captured from the repo's self-host build.  With the
    // source removed, the cached copy is the only way to load it — proving the
    // cache format is compatible with a genuine `objects.inv` payload.
    let bytes = include_bytes!("fixtures/sphinx_docs.inv");
    let dir = temp_dir("real");
    let cache_dir = dir.join("cache");
    let cache = InventoryCache::new(cache_dir.clone(), 5);
    let key = "real\0https://www.sphinx-doc.org/en/master/";
    cache
        .put(key, &CacheEntry {
            name: "real".to_owned(),
            timestamp: 1_700_000_000,
            bytes: bytes.to_vec(),
        })
        .expect("seed real cache");
    let options = FetchOptions {
        now: 1_700_000_000 + 60,
        ..FetchOptions::default()
    };
    let entry = MappingEntry {
        name: "real".to_owned(),
        target_uri: "https://www.sphinx-doc.org/en/master/".to_owned(),
        locations: vec![Some("https://www.sphinx-doc.org/en/master/objects.inv".to_owned())],
    };
    let outcome = fetch_one(&entry, &options, Some(&cache));
    assert!(outcome.from_cache, "warnings: {:?}", outcome.warnings);
    let inventory = outcome.inventory.expect("real inventory loaded");
    assert!(inventory.len() > 10, "fixture should contain real objects");
    let _ = std::fs::remove_dir_all(dir);
}
