//! Minimal asynchronous static HTTP server with Server-Sent Events (Phase 28).
//!
//! Implemented directly on `tokio::net::TcpListener` rather than pulling a
//! full framework: the surface is small (GET/HEAD, correct MIME types,
//! directory index, 404, SSE reload stream) and keeping it local makes the
//! behavior deterministic and easy to test end-to-end.

use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::time::timeout;

use super::inject::{maybe_inject, LIVE_SCRIPT, LIVE_SCRIPT_URL};
use super::mime::{is_html, mime_for_path};

/// Health-check endpoint used by integration tests and orchestration tools.
pub const HEALTH_PATH: &str = "/_scribe_health";
/// SSE endpoint opened by the injected client script.
pub const EVENTS_PATH: &str = "/events";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// The embedded HTTP server. Clone-free: all shared state lives in the struct
/// and the accept loop destructures it into task-local values.
pub struct HttpServer {
    listener: TcpListener,
    addr: SocketAddr,
    root: PathBuf,
    reload_enabled: bool,
    reload_tx: broadcast::Sender<Vec<String>>,
}

impl HttpServer {
    /// Bind `(host, port)`. Passing port `0` asks the OS for an ephemeral port;
    /// the chosen address is available through [`Self::local_addr`].
    pub async fn bind(host: &str, port: u16, root: PathBuf, reload_enabled: bool) -> std::io::Result<Self> {
        let listener = TcpListener::bind((host, port)).await?;
        let addr = listener.local_addr()?;
        let (reload_tx, _) = broadcast::channel(32);
        Ok(Self { listener, addr, root, reload_enabled, reload_tx })
    }

    /// Address the server is listening on (resolved port, even for `:0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Sender used by the rebuild loop to notify browsers after a successful
    /// rebuild. The payload lists HTML pages (e.g. `["index.html"]`) that
    /// changed.
    pub fn reload_sender(&self) -> broadcast::Sender<Vec<String>> {
        self.reload_tx.clone()
    }

    /// Run the accept loop until a shutdown signal is received.
    pub async fn run(self, mut shutdown: broadcast::Receiver<()>) {
        let Self { listener, root, reload_enabled, reload_tx, .. } = self;
        loop {
            tokio::select! {
                _ = shutdown.recv() => break,
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _peer)) => {
                            let root = root.clone();
                            let tx = reload_tx.clone();
                            tokio::spawn(async move {
                                handle_connection(stream, root, reload_enabled, tx).await;
                            });
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    root: PathBuf,
    reload_enabled: bool,
    reload_tx: broadcast::Sender<Vec<String>>,
) {
    let mut buffer = vec![0_u8; MAX_REQUEST_BYTES];
    let read = match timeout(REQUEST_TIMEOUT, stream.read(&mut buffer)).await {
        Ok(Ok(count)) => count,
        _ => return,
    };
    if read == 0 {
        return;
    }
    let request = String::from_utf8_lossy(&buffer[..read]);
    let request_line = request.lines().next().unwrap_or_default();
    let mut fields = request_line.split_whitespace();
    let method = fields.next().unwrap_or_default();
    let raw_target = fields.next().unwrap_or("/");
    if method != "GET" && method != "HEAD" {
        write_all(&mut stream, response("405 Method Not Allowed", "text/plain; charset=utf-8", b"method not allowed", method == "HEAD")).await;
        return;
    }
    let head_only = method == "HEAD";
    let target = raw_target.split('?').next().unwrap_or("/");
    let path = percent_decode(target);

    if path == EVENTS_PATH {
        if reload_enabled {
            serve_events(&mut stream, reload_tx).await;
        } else {
            write_all(&mut stream, not_found(head_only)).await;
        }
        return;
    }
    if path == HEALTH_PATH {
        write_all(&mut stream, response("200 OK", "text/plain; charset=utf-8", b"ok", head_only)).await;
        return;
    }
    if path == LIVE_SCRIPT_URL {
        if reload_enabled {
            write_all(&mut stream, response("200 OK", "text/javascript; charset=utf-8", LIVE_SCRIPT.as_bytes(), head_only)).await;
        } else {
            write_all(&mut stream, not_found(head_only)).await;
        }
        return;
    }

    let relative = path.trim_start_matches('/');
    let mut requested = if relative.is_empty() {
        root.join("index.html")
    } else {
        root.join(relative)
    };
    if !path_is_safe(&root, &requested) {
        write_all(&mut stream, response("403 Forbidden", "text/plain; charset=utf-8", b"forbidden", head_only)).await;
        return;
    }
    if requested.is_dir() {
        requested = requested.join("index.html");
    }
    let bytes = match tokio::fs::read(&requested).await {
        Ok(bytes) => bytes,
        Err(_) => {
            write_all(&mut stream, not_found(head_only)).await;
            return;
        }
    };
    let mime = mime_for_path(&requested);
    let body = if reload_enabled && is_html(mime) {
        let text = String::from_utf8_lossy(&bytes);
        maybe_inject(mime, &text, LIVE_SCRIPT_URL).0.into_bytes()
    } else {
        bytes
    };
    write_all(&mut stream, response("200 OK", mime, &body, head_only)).await;
}

async fn serve_events(stream: &mut TcpStream, reload_tx: broadcast::Sender<Vec<String>>) {
    let mut receiver = reload_tx.subscribe();
    let headers = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\n\r\n";
    if stream.write_all(headers).await.is_err() || stream.flush().await.is_err() {
        return;
    }
    loop {
        match receiver.recv().await {
            Ok(pages) => {
                let data = serde_json::to_string(&pages).unwrap_or_else(|_| "[]".to_owned());
                let message = format!("event: scribe-reload\ndata: {data}\n\n");
                if stream.write_all(message.as_bytes()).await.is_err() || stream.flush().await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

fn response(status: &str, content_type: &str, body: &[u8], head_only: bool) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    if !head_only {
        out.extend_from_slice(body);
    }
    out
}

fn not_found(head_only: bool) -> Vec<u8> {
    response(
        "404 Not Found",
        "text/html; charset=utf-8",
        b"<!doctype html><title>404 Not Found</title><h1>404 Not Found</h1><p>scribe dev server</p>",
        head_only,
    )
}

async fn write_all(stream: &mut TcpStream, bytes: Vec<u8>) {
    let _ = timeout(REQUEST_TIMEOUT, stream.write_all(&bytes)).await;
    let _ = stream.flush().await;
}

/// Reject paths that escape the served root via `..` segments. Both paths are
/// normalized lexically so a decoded `%2e%2e` cannot slip through.
fn path_is_safe(root: &Path, requested: &Path) -> bool {
    if requested.components().any(|component| matches!(component, Component::ParentDir)) {
        return false;
    }
    super::mapping::lexical_normalize(requested).starts_with(super::mapping::lexical_normalize(root))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{hex_value, path_is_safe, percent_decode, response};
    use std::path::Path;

    #[test]
    fn percent_decodes_escapes_and_leaves_other_bytes() {
        assert_eq!(percent_decode("/a%20b/index.html"), "/a b/index.html");
        assert_eq!(percent_decode("/100%25.html"), "/100%.html");
        assert_eq!(percent_decode("/plain"), "/plain");
    }

    #[test]
    fn rejects_traversal_paths() {
        let root = Path::new("/tmp/site");
        assert!(path_is_safe(root, Path::new("/tmp/site/_static/app.css")));
        assert!(!path_is_safe(root, Path::new("/tmp/site/../secret")));
        assert!(!path_is_safe(root, Path::new("/tmp/site/a/../../secret")));
    }

    #[test]
    fn hex_values_are_case_insensitive() {
        assert_eq!(hex_value(b'0'), Some(0));
        assert_eq!(hex_value(b'f'), Some(15));
        assert_eq!(hex_value(b'F'), Some(15));
        assert_eq!(hex_value(b'z'), None);
    }

    #[test]
    fn head_requests_omit_the_body() {
        let full = response("200 OK", "text/plain", b"hello", false);
        let head = response("200 OK", "text/plain", b"hello", true);
        assert!(full.ends_with(b"hello"));
        assert!(head.ends_with(b"\r\n\r\n"));
        assert!(String::from_utf8_lossy(&head).contains("Content-Length: 5"));
    }
}
