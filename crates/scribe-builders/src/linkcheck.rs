//! External/internal link checker builder (Phase 25).
//!
//! Upstream `sphinx/builders/linkcheck.py` collects `reference` / `image` /
//! `raw` URIs in a post-transform, then checks them with a pool of threads:
//! it tries HEAD first (GET when the response body is needed for an anchor),
//! follows redirects, honours `Retry-After`, and writes `output.txt` and
//! `output.json`. Scribe keeps the same observable report format but checks
//! links concurrently with `tokio` + `reqwest` (one task per URI, a semaphore
//! limiting concurrency per domain) and writes results in deterministic
//! `(docname, line, uri)` order. No external network is used by the test
//! suite: a local tokio HTTP server supplies the fixtures.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT};
use reqwest::{Client, StatusCode, Url};
use scribe_ast::{Document, NodeKind};
use scribe_config::Config;
use scribe_core::BuildEnvironment;
use serde::Serialize;
use tokio::sync::{Mutex, Semaphore};

use crate::build_common::prepare;
use crate::error::{OutputError, OutputResult};
use crate::html::BuildOptions;

/// Default `Accept` header from upstream's `DEFAULT_REQUEST_HEADERS`.
pub const DEFAULT_ACCEPT: &str = "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8";
/// Default delay (seconds) for a 429 response without `Retry-After`.
pub const DEFAULT_DELAY_SECS: f64 = 60.0;

/// Status values from upstream's `_Status` enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkStatus {
    Broken,
    Ignored,
    RateLimited,
    Redirected,
    Timeout,
    Unchecked,
    Unknown,
    Working,
}

impl LinkStatus {
    /// Stable string used in `output.json` and report summaries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Broken => "broken",
            Self::Ignored => "ignored",
            Self::RateLimited => "rate-limited",
            Self::Redirected => "redirected",
            Self::Timeout => "timeout",
            Self::Unchecked => "unchecked",
            Self::Unknown => "unknown",
            Self::Working => "working",
        }
    }
}

/// A URI discovered in a doctree, with its source location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    /// URI as written in the source.
    pub uri: String,
    /// Document containing the link.
    pub docname: String,
    /// Absolute path of the source document.
    pub docpath: PathBuf,
    /// 1-based line number, or -1 when unavailable.
    pub lineno: i64,
}

/// One result line (the structure upstream writes to `output.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckResult {
    /// Source path relative to the source directory.
    pub filename: String,
    /// 1-based line number.
    pub lineno: i64,
    /// Link status.
    pub status: LinkStatus,
    /// HTTP status code (0 when not applicable).
    pub code: i64,
    /// URI that was checked.
    pub uri: String,
    /// Human-readable detail.
    pub info: String,
}

/// Aggregate outcome of a linkcheck build.
#[derive(Clone, Debug, Default)]
pub struct LinkcheckReport {
    /// Every checked link, in deterministic order.
    pub results: Vec<CheckResult>,
    /// Source documents processed.
    pub built_docnames: Vec<String>,
    /// Non-fatal diagnostics from the read phase.
    pub warnings: Vec<String>,
}

impl LinkcheckReport {
    /// Number of broken links.
    pub fn broken_count(&self) -> usize {
        self.results.iter().filter(|r| r.status == LinkStatus::Broken).count()
    }
    /// Number of timed-out links (or broken links when timeouts are reported
    /// as broken).
    pub fn timeout_count(&self) -> usize {
        self.results.iter().filter(|r| r.status == LinkStatus::Timeout).count()
    }
    /// `true` when any link was broken or timed out.
    pub fn has_broken(&self) -> bool {
        self.broken_count() + self.timeout_count() > 0
    }
}

/// Builder for `-b linkcheck`.
pub struct LinkcheckBuilder;

impl LinkcheckBuilder {
    /// Collect, check and report every hyperlink in the project.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut BuildEnvironment,
    ) -> OutputResult<LinkcheckReport> {
        let prepared = prepare(options, config, env, "linkcheck")?;
        let settings = LinkcheckConfig::from_config(config);
        let links = collect_hyperlinks(&prepared.parsed, env);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(settings.workers.max(1))
            .enable_all()
            .build()
            .map_err(|error| OutputError::config("linkcheck", "workers", error.to_string()))?;
        let local = LocalLinkIndex::from_documents(&prepared.parsed);
        let checker = Arc::new(AsyncChecker::new(settings.clone(), local)?);
        let results = runtime.block_on(check_all(Arc::clone(&checker), links, env));

        let mut results = results;
        results.sort_by(|a, b| {
            (&a.filename, a.lineno, &a.uri).cmp(&(&b.filename, b.lineno, &b.uri))
        });
        let report = LinkcheckReport {
            results,
            built_docnames: prepared.docnames,
            warnings: prepared.warnings,
        };
        write_report(&report, options, env)?;
        Ok(report)
    }
}

/// Collect URIs from reference, image and raw nodes, plus internal `refid`
/// links that can be verified against the build environment without network.
///
/// Upstream deduplicates by URI and keeps the first location; we mirror that
/// with a `BTreeSet` so the output is deterministic.
pub fn collect_hyperlinks(
    parsed: &BTreeMap<String, Document>,
    env: &BuildEnvironment,
) -> Vec<Hyperlink> {
    let mut seen = BTreeSet::new();
    let mut links = Vec::new();
    for (docname, document) in parsed {
        let docpath = env.doc2path(docname).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(docname));
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            let lineno = node.source.as_ref().and_then(|s| s.line).map(i64::from).unwrap_or(-1);
            let mut add_uri = |uri: &str, links: &mut Vec<Hyperlink>| {
                if uri.is_empty() || !seen.insert(uri.to_owned()) {
                    return;
                }
                links.push(Hyperlink {
                    uri: uri.to_owned(),
                    docname: docname.clone(),
                    docpath: docpath.clone(),
                    lineno,
                });
            };
            match node.kind {
                NodeKind::Reference | NodeKind::DownloadReference | NodeKind::NumberReference => {
                    if let Some(uri) = node.attrs.get_str("refuri").unwrap_or_default() {
                        if !uri.is_empty() {
                            add_uri(uri, &mut links);
                        }
                    } else if let Some(refid) = node.attrs.get_str("refid").unwrap_or_default() {
                        // Internal anchors are encoded as `#id` so the checker
                        // can validate the target document/anchor locally.
                        let uri = format!("#{refid}");
                        add_uri(&uri, &mut links);
                    }
                }
                NodeKind::Image => {
                    if let Some(values) = node.attrs.get_list("candidates").unwrap_or_default() {
                        for uri in values {
                            if uri.contains("://") {
                                add_uri(&uri, &mut links);
                            }
                        }
                    }
                }
                NodeKind::Raw => {
                    if let Some(uri) = node.attrs.get_str("source").unwrap_or_default() {
                        if uri.contains("://") {
                            add_uri(uri, &mut links);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    links
}

/// Index of internal document anchors, built from the parsed doctrees so
/// scheme-less local links and `#fragment` links can be verified without any
/// network access.
#[derive(Clone, Debug, Default)]
pub struct LocalLinkIndex {
    /// docname → set of `ids` / `names` present in its doctree.
    pub docs: BTreeMap<String, BTreeSet<String>>,
}

impl LocalLinkIndex {
    /// Build the index from the prepared doctrees.
    pub fn from_documents(parsed: &BTreeMap<String, Document>) -> Self {
        let mut docs = BTreeMap::new();
        for (docname, document) in parsed {
            let mut anchors = BTreeSet::new();
            for id in document.preorder_ids() {
                let Ok(node) = document.get(id) else {
                    continue;
                };
                if let Some(ids) = node.attrs.get_list("ids").unwrap_or_default() {
                    anchors.extend(ids.iter().cloned());
                }
                if let Some(names) = node.attrs.get_list("names").unwrap_or_default() {
                    anchors.extend(names.iter().cloned());
                }
            }
            docs.insert(docname.clone(), anchors);
        }
        Self { docs }
    }

    /// `true` when the document name is known.
    pub fn contains_doc(&self, docname: &str) -> bool {
        self.docs.contains_key(docname)
    }

    /// `true` when the document contains the given section/target anchor.
    pub fn contains_anchor(&self, docname: &str, anchor: &str) -> bool {
        self.docs.get(docname).is_some_and(|anchors| anchors.contains(anchor))
    }
}

/// Configuration snapshot used by the checker (extracted once from `Config`).
#[derive(Clone, Debug)]
pub struct LinkcheckConfig {
    pub ignore: Vec<Regex>,
    pub exclude_documents: Vec<Regex>,
    pub retries: usize,
    pub timeout: Duration,
    pub workers: usize,
    pub check_anchors: bool,
    pub anchors_ignore: Vec<Regex>,
    pub anchors_ignore_for_url: Vec<Regex>,
    pub allowed_redirects: Vec<(Regex, Regex)>,
    pub rate_limit_timeout: f64,
    pub auth: Vec<(Regex, String, Option<String>)>,
    pub request_headers: BTreeMap<String, BTreeMap<String, String>>,
    pub report_timeouts_as_broken: bool,
    pub case_insensitive_urls: Vec<Regex>,
    pub user_agent: Option<String>,
    pub tls_verify: bool,
}

impl LinkcheckConfig {
    /// Extract and compile all `linkcheck_*` settings from the configuration.
    pub fn from_config(config: &Config) -> Self {
        let string_patterns = |key: &str| -> Vec<Regex> {
            config.get_strings(key).unwrap_or_default().into_iter().filter_map(|p| Regex::new(&p).ok()).collect()
        };
        let timeout_secs = config
            .get_float("linkcheck_timeout")
            .or_else(|| config.get_int("linkcheck_timeout").map(|v| v as f64))
            .unwrap_or(30.0);
        let rate_limit_timeout = config
            .get_float("linkcheck_rate_limit_timeout")
            .or_else(|| config.get_int("linkcheck_rate_limit_timeout").map(|v| v as f64))
            .unwrap_or(300.0);
        let retries = config.get_int("linkcheck_retries").unwrap_or(1).max(0) as usize;
        let workers = config.get_int("linkcheck_workers").unwrap_or(5).max(1) as usize;

        let mut auth = Vec::new();
        if let Some(entries) = config.get_list("linkcheck_auth") {
            for entry in entries {
                if let Some(fields) = entry.get_list() {
                    if fields.len() < 2 {
                        continue;
                    }
                    let (Some(pattern), Some(user)) = (fields[0].get_str(), fields[1].get_str()) else {
                        continue;
                    };
                    if let Ok(re) = Regex::new(pattern) {
                        let password = fields.get(2).and_then(|v| v.get_str()).map(str::to_owned);
                        auth.push((re, user.to_owned(), password));
                    }
                }
            }
        }

        let mut request_headers = BTreeMap::new();
        if let Some(groups) = config.get_dict("linkcheck_request_headers") {
            for (url, value) in groups {
                if let Some(headers) = value.get_dict() {
                    let mut group = BTreeMap::new();
                    for (name, header) in headers {
                        if let Some(text) = header.get_str() {
                            group.insert(name.clone(), text.to_owned());
                        }
                    }
                    request_headers.insert(url.clone(), group);
                }
            }
        }

        let mut allowed_redirects = Vec::new();
        if let Some(redirects) = config.get_dict("linkcheck_allowed_redirects") {
            for (from, to) in redirects {
                if let (Ok(from_re), Some(to_re)) = (Regex::new(from), to.get_str().and_then(|s| Regex::new(s).ok())) {
                    allowed_redirects.push((from_re, to_re));
                }
            }
        }

        Self {
            ignore: string_patterns("linkcheck_ignore"),
            exclude_documents: string_patterns("linkcheck_exclude_documents"),
            retries,
            timeout: Duration::from_secs_f64(timeout_secs.max(0.0)),
            workers,
            check_anchors: config.get_bool("linkcheck_anchors").unwrap_or(true),
            anchors_ignore: string_patterns("linkcheck_anchors_ignore"),
            anchors_ignore_for_url: string_patterns("linkcheck_anchors_ignore_for_url"),
            allowed_redirects,
            rate_limit_timeout,
            auth,
            request_headers,
            report_timeouts_as_broken: config.get_bool("linkcheck_report_timeouts_as_broken").unwrap_or(false),
            case_insensitive_urls: string_patterns("linkcheck_case_insensitive_urls"),
            user_agent: config.get_str("user_agent").map(str::to_owned),
            tls_verify: config.get_bool("tls_verify").unwrap_or(true),
        }
    }
}

/// Python's `re.Pattern.match` anchors at the **start** of the string;
/// `Regex::is_match` searches anywhere. All upstream linkcheck patterns use
/// `.match()`, so every call site goes through this helper.
pub fn prefix_match(pattern: &Regex, text: &str) -> bool {
    pattern.find(text).is_some_and(|m| m.start() == 0)
}

/// `true` when a URI matches any `linkcheck_ignore` regex (prefix match,
/// mirroring Python's `re.Pattern.match`).
pub fn is_ignored(uri: &str, patterns: &[Regex]) -> bool {
    patterns.iter().any(|pattern| prefix_match(pattern, uri))
}

/// Decode `%XX` escapes in a URI fragment (upstream `urllib.parse.unquote`).
pub fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Conservative anchor detection: upstream uses an HTML parser looking at
/// `id` / `name` attributes of start tags. We use an equivalent attribute
/// scanner; it may accept anchors that appear only in text, but never misses
/// a genuine `id=`/`name=` attribute (documented in `ARCHITECTURE.md` §31).
pub fn contains_anchor(html: &str, anchor: &str) -> bool {
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Skip the tag name and comments/declarations.
        i += 1;
        if i < bytes.len() && (bytes[i] == b'!' || bytes[i] == b'?') {
            while i < bytes.len() && bytes[i] != b'>' {
                i += 1;
            }
            i += 1;
            continue;
        }
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' && bytes[i] != b'/' {
            i += 1;
        }
        // Scan attributes until the end of this start tag.
        while i < bytes.len() && bytes[i] != b'>' {
            while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
                i += 1;
            }
            let name_start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b'_' | b':' | b'.')) {
                i += 1;
            }
            if name_start == i {
                i += 1;
                continue;
            }
            let name = html[name_start..i].to_ascii_lowercase();
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'=' {
                continue;
            }
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            let value = if bytes[i] == b'"' || bytes[i] == b'\'' {
                let quote = bytes[i];
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                let value = &html[start..i.min(html.len())];
                if i < bytes.len() {
                    i += 1;
                }
                value
            } else {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                &html[start..i]
            };
            if (name == "id" || name == "name") && value == anchor {
                return true;
            }
        }
    }
    false
}

struct AsyncChecker {
    settings: LinkcheckConfig,
    local: LocalLinkIndex,
    client: Client,
    semaphores: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
    rate_limits: Arc<Mutex<HashMap<String, tokio::time::Instant>>>,
}

impl AsyncChecker {
    fn new(settings: LinkcheckConfig, local: LocalLinkIndex) -> OutputResult<Self> {
        let client = Client::builder()
            .timeout(settings.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(!settings.tls_verify)
            .build()
            .map_err(|error| OutputError::config("linkcheck", "http_client", error.to_string()))?;
        Ok(Self {
            settings,
            local,
            client,
            semaphores: Arc::new(Mutex::new(HashMap::new())),
            rate_limits: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    async fn domain_permit(&self, domain: &str) -> Arc<Semaphore> {
        let mut map = self.semaphores.lock().await;
        map.entry(domain.to_owned()).or_insert_with(|| Arc::new(Semaphore::new(self.settings.workers))).clone()
    }

    async fn check(&self, filename: String, link: Hyperlink) -> CheckResult {
        let uri = link.uri.clone();
        let lineno = link.lineno;
        let result = self.check_inner(&link).await;
        CheckResult { filename, lineno, status: result.0, code: result.1, uri, info: result.2 }
    }

    async fn check_inner(&self, link: &Hyperlink) -> (LinkStatus, i64, String) {
        let uri = &link.uri;
        if self.settings.exclude_documents.iter().any(|p| prefix_match(p, &link.docname)) {
            return (LinkStatus::Ignored, 0, String::new());
        }
        if is_ignored(uri, &self.settings.ignore) {
            return (LinkStatus::Ignored, 0, String::new());
        }
        if uri.is_empty() || uri.starts_with("mailto:") || uri.starts_with("tel:") {
            return (LinkStatus::Unchecked, 0, String::new());
        }
        if uri.starts_with('#') {
            return self.check_local_anchor(link);
        }
        if !uri.starts_with("http:") && !uri.starts_with("https:") {
            if uri.contains("://") || uri.starts_with("//") {
                return (LinkStatus::Unchecked, 0, String::new());
            }
            return self.check_local_file(link);
        }

        let mut last = (LinkStatus::Broken, 0, String::new());
        for _ in 0..=self.settings.retries {
            last = self.check_uri(link).await;
            if last.0 == LinkStatus::RateLimited {
                // Upstream re-queues a rate-limited URI after the recorded
                // back-off. Wait for that instant and try again so the final
                // report reflects the actual link state.
                let domain = Url::parse(uri).ok().and_then(|url| url.host_str().map(str::to_owned)).unwrap_or_default();
                let next = self.rate_limits.lock().await.get(&domain).copied();
                if let Some(next) = next {
                    let now = tokio::time::Instant::now();
                    if next > now {
                        tokio::time::sleep(next - now).await;
                    }
                }
                continue;
            }
            if last.0 != LinkStatus::Broken {
                break;
            }
        }
        last
    }

    fn check_local_anchor(&self, link: &Hyperlink) -> (LinkStatus, i64, String) {
        let anchor = unquote(&link.uri[1..]);
        if self.local.contains_anchor(&link.docname, &anchor) {
            (LinkStatus::Working, 0, String::new())
        } else {
            (LinkStatus::Broken, 0, format!("Anchor '{}' not found", anchor))
        }
    }

    fn check_local_file(&self, link: &Hyperlink) -> (LinkStatus, i64, String) {
        let (path, fragment) = match link.uri.split_once('#') {
            Some((p, f)) => (p, Some(unquote(f))),
            None => (link.uri.as_str(), None),
        };
        // A link to another source document (`other.rst`, `other.html`) can
        // be resolved against the doctree index, including its anchors.
        let docname = Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_owned);
        if let Some(docname) = docname {
            if self.local.contains_doc(&docname) {
                return match fragment {
                    Some(anchor) if !self.local.contains_anchor(&docname, &anchor) => {
                        (LinkStatus::Broken, 0, format!("Anchor '{}' not found", anchor))
                    }
                    _ => (LinkStatus::Working, 0, String::new()),
                };
            }
        }
        let candidate = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            link.docpath.parent().unwrap_or(Path::new(".")).join(path)
        };
        if candidate.exists() { (LinkStatus::Working, 0, String::new()) }
        else { (LinkStatus::Broken, 0, String::new()) }
    }

    async fn check_uri(&self, link: &Hyperlink) -> (LinkStatus, i64, String) {
        let (req_url, anchor) = match link.uri.split_once('#') {
            Some((base, fragment)) if !fragment.is_empty() => {
                let anchor = unquote(fragment);
                let mut skip = false;
                for pattern in &self.settings.anchors_ignore {
                    if prefix_match(pattern, &anchor) { skip = true; break; }
                }
                if !skip {
                    for pattern in &self.settings.anchors_ignore_for_url {
                        if prefix_match(pattern, base) { skip = true; break; }
                    }
                }
                (base, if skip { String::new() } else { anchor })
            }
            _ => (link.uri.as_str(), String::new()),
        };

        let url = match Url::parse(req_url) {
            Ok(url) => url,
            Err(error) => return (LinkStatus::Broken, 0, error.to_string()),
        };
        let domain = url.host_str().unwrap_or("").to_owned();
        let permit = self.domain_permit(&domain).await;
        let _permit = permit.acquire().await;

        // Honour a recorded rate-limit back-off for this domain.
        if let Some(next) = self.rate_limits.lock().await.get(&domain).copied() {
            let now = tokio::time::Instant::now();
            if next > now {
                tokio::time::sleep(next - now).await;
            }
        }

        let headers = self.headers_for(&url);
        let auth = self.settings.auth.iter().find(|(pattern, _, _)| prefix_match(pattern, &link.uri));

        let mut current = url.clone();
        let mut redirects: Vec<u16> = Vec::new();
        let mut last_error = String::new();
        let mut hop = 0;
        let mut force_get = false;
        let need_body = self.settings.check_anchors && !anchor.is_empty();

        while hop < 10 {
            hop += 1;
            let use_get = need_body || force_get;
            let mut request = if use_get { self.client.get(current.clone()) } else { self.client.head(current.clone()) };
            request = request.headers(headers.clone());
            if let Some((_, user, password)) = auth {
                request = request.basic_auth(user, password.as_deref());
            }
            if let Some(agent) = &self.settings.user_agent {
                request = request.header(reqwest::header::USER_AGENT, agent.as_str());
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => {
                    last_error = error.to_string();
                    if !use_get {
                        // Servers that drop HEAD connections: fall back to GET.
                        force_get = true;
                        continue;
                    }
                    return (self.timeout_status_for_error(&last_error), 0, last_error);
                }
            };
            let status = response.status();

            if status == StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
                let delay = self.rate_limit_delay(&domain, &retry_after);
                if let Some(delay) = delay {
                    self.rate_limits.lock().await.insert(domain.clone(), tokio::time::Instant::now() + delay);
                    return (LinkStatus::RateLimited, 0, String::new());
                }
                return (LinkStatus::Broken, status.as_u16() as i64, last_error);
            }
            if status == StatusCode::UNAUTHORIZED {
                // `linkcheck_allow_unauthorized` defaults to false upstream;
                // a 401 is therefore a broken link unless explicitly allowed.
                return (LinkStatus::Broken, 0, "unauthorized".to_owned());
            }
            if status == StatusCode::SERVICE_UNAVAILABLE {
                return (LinkStatus::Ignored, 0, "service unavailable".to_owned());
            }
            if status.is_redirection() {
                if let Some(location) = response.headers().get("location").and_then(|v| v.to_str().ok()) {
                    match current.join(location) {
                        Ok(next) => {
                            redirects.push(status.as_u16());
                            current = next;
                            continue;
                        }
                        Err(error) => return (LinkStatus::Broken, status.as_u16() as i64, error.to_string()),
                    }
                }
            }
            if !status.is_success() {
                last_error = format!("{} {}", status.as_u16(), status.canonical_reason().unwrap_or(""));
                if !use_get {
                    continue;
                }
                return (LinkStatus::Broken, status.as_u16() as i64, last_error);
            }

            // Success. Check the anchor against the final response body when
            // required, using GET if we only issued a HEAD request.
            if !anchor.is_empty() {
                let body = if use_get {
                    response.text().await.unwrap_or_default()
                } else {
                    match self.client.get(current.clone()).headers(headers.clone()).send().await {
                        Ok(response) => response.text().await.unwrap_or_default(),
                        Err(error) => return (LinkStatus::Broken, 0, error.to_string()),
                    }
                };
                if !contains_anchor(&body, &anchor) {
                    return (LinkStatus::Broken, 0, format!("Anchor '{}' not found", anchor));
                }
            }
            self.rate_limits.lock().await.remove(&domain);

            let final_url = current.as_str().trim_end_matches('/').to_owned();
            let req_normalized = req_url.trim_end_matches('/');
            if final_url == req_normalized || self.allowed_redirect(req_url, current.as_str()) {
                return (LinkStatus::Working, 0, String::new());
            }
            let code = redirects.last().copied().unwrap_or(0) as i64;
            return (LinkStatus::Redirected, code, current.to_string());
        }
        (LinkStatus::Broken, 0, last_error)
    }

    fn timeout_status_for_error(&self, error: &str) -> LinkStatus {
        let lower = error.to_ascii_lowercase();
        if lower.contains("timed out") || lower.contains("timeout") {
            if self.settings.report_timeouts_as_broken { LinkStatus::Broken } else { LinkStatus::Timeout }
        } else {
            LinkStatus::Broken
        }
    }

    fn headers_for(&self, url: &Url) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(value) = HeaderValue::from_str(DEFAULT_ACCEPT) {
            headers.insert(ACCEPT, value);
        }
        let origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
        let origin_slash = format!("{origin}/");
        let candidates = [origin.as_str(), origin_slash.as_str(), url.as_str(), "*"];
        for candidate in candidates {
            if let Some(group) = self.settings.request_headers.get(candidate) {
                for (name, value) in group {
                    if let (Ok(name), Ok(value)) = (
                        HeaderName::from_bytes(name.as_bytes()),
                        HeaderValue::from_str(value),
                    ) {
                        headers.insert(name, value);
                    }
                }
                break;
            }
        }
        headers
    }

    fn allowed_redirect(&self, from: &str, to: &str) -> bool {
        self.settings.allowed_redirects.iter().any(|(from_re, to_re)| prefix_match(from_re, from) && prefix_match(to_re, to))
    }

    fn rate_limit_delay(&self, _domain: &str, retry_after: &str) -> Option<Duration> {
        retry_after_delay(retry_after, self.settings.rate_limit_timeout, chrono::Utc::now().timestamp_millis())
    }
}

/// Parse an upstream `Retry-After` header (integer seconds or RFC 1123 date)
/// into a bounded back-off delay. `None` means "exceeds the configured
/// `linkcheck_rate_limit_timeout`" and the link should be reported broken.
pub fn retry_after_delay(retry_after: &str, max_timeout: f64, now_epoch_ms: i64) -> Option<Duration> {
    let mut delay = DEFAULT_DELAY_SECS;
    if !retry_after.is_empty() {
        if let Ok(secs) = retry_after.parse::<f64>() {
            delay = secs;
        } else if let Ok(date) = chrono::DateTime::parse_from_rfc2822(retry_after) {
            delay = (date.timestamp_millis() - now_epoch_ms) as f64 / 1000.0;
        }
    }
    if delay < 0.0 || delay > max_timeout {
        None
    } else {
        Some(Duration::from_secs_f64(delay))
    }
}

async fn check_all(
    checker: Arc<AsyncChecker>,
    links: Vec<Hyperlink>,
    env: &BuildEnvironment,
) -> Vec<CheckResult> {
    let mut handles = Vec::with_capacity(links.len());
    for link in links {
        let checker = Arc::clone(&checker);
        let source_dir = env.source_dir().to_path_buf();
        let filename = link
            .docpath
            .strip_prefix(&source_dir)
            .unwrap_or(&link.docpath)
            .to_string_lossy()
            .into_owned();
        handles.push(tokio::spawn(async move { checker.check(filename, link).await }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(error) => results.push(CheckResult {
                filename: String::new(),
                lineno: -1,
                status: LinkStatus::Broken,
                code: 0,
                uri: String::new(),
                info: format!("link check task failed: {error}"),
            }),
        }
    }
    results
}

/// Write Sphinx-compatible `output.txt` and `output.json`.
pub fn write_report(
    report: &LinkcheckReport,
    options: &BuildOptions,
    _env: &BuildEnvironment,
) -> OutputResult<()> {
    fs::create_dir_all(&options.output_dir)
        .map_err(|error| OutputError::io("linkcheck", &options.output_dir, error))?;
    let mut text = String::new();
    let mut json = String::new();
    for result in &report.results {
        let line = serde_json::to_string(result)
            .map_err(|error| OutputError::config("linkcheck", "output.json", error.to_string()))?;
        json.push_str(&line);
        json.push('\n');
        match result.status {
            LinkStatus::Broken | LinkStatus::Timeout => {
                text.push_str(&format!(
                    "{}:{}: [{}] {}: {}\n",
                    result.filename, result.lineno, result.status.as_str(), result.uri, result.info
                ));
            }
            LinkStatus::Redirected => {
                let what = "redirected with unknown code".to_owned();
                text.push_str(&format!(
                    "{}:{}: [{}] {} to {}\n",
                    result.filename, result.lineno, what, result.uri, result.info
                ));
            }
            _ => {}
        }
    }
    let text_path = options.output_dir.join("output.txt");
    fs::write(&text_path, text).map_err(|error| OutputError::io("linkcheck", &text_path, error))?;
    let json_path = options.output_dir.join("output.json");
    fs::write(&json_path, json).map_err(|error| OutputError::io("linkcheck", &json_path, error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_strings_match_upstream() {
        assert_eq!(LinkStatus::Broken.as_str(), "broken");
        assert_eq!(LinkStatus::RateLimited.as_str(), "rate-limited");
        assert_eq!(LinkStatus::Working.as_str(), "working");
    }

    #[test]
    fn ignore_patterns_use_prefix_matching() {
        let patterns = vec![Regex::new(r"^https://example\.com/").expect("valid")];
        assert!(is_ignored("https://example.com/foo", &patterns));
        assert!(!is_ignored("http://example.com/foo", &patterns));
    }

    #[test]
    fn unquote_decodes_percent_escapes() {
        assert_eq!(unquote("hello%20world"), "hello world");
        assert_eq!(unquote("a%2Fb%ZZ"), "a/b%ZZ");
    }

    #[test]
    fn anchor_scanning_finds_id_and_name_attributes() {
        let html = r#"<html><body><div id="target">x</div><a name='other'>y</a></body></html>"#;
        assert!(contains_anchor(html, "target"));
        assert!(contains_anchor(html, "other"));
        assert!(!contains_anchor(html, "missing"));
    }

    #[test]
    fn anchor_scanning_ignores_unquoted_and_uppercase_tags() {
        let html = r#"<H1 ID=top>Title</H1>"#;
        assert!(contains_anchor(html, "top"));
    }

    #[test]
    fn retry_after_delay_parses_seconds_and_dates() {
        assert_eq!(retry_after_delay("2", 300.0, 0), Some(Duration::from_secs(2)));
        assert_eq!(retry_after_delay("", 300.0, 0), Some(Duration::from_secs(60)));
        assert_eq!(retry_after_delay("500", 300.0, 0), None);
        let epoch = 1_700_000_000_000i64;
        let date = chrono::DateTime::from_timestamp_millis(epoch + 5_000)
            .expect("valid timestamp")
            .to_rfc2822();
        assert_eq!(retry_after_delay(&date, 300.0, epoch), Some(Duration::from_secs(5)));
    }

    #[test]
    fn check_result_serialization_matches_upstream_field_order() {
        let result = CheckResult {
            filename: "index.rst".to_owned(), lineno: 7, status: LinkStatus::Broken,
            code: 0, uri: "http://example.test/".to_owned(), info: "error".to_owned(),
        };
        let json = serde_json::to_string(&result).expect("serializable");
        assert_eq!(
            json,
            r#"{"filename":"index.rst","lineno":7,"status":"broken","code":0,"uri":"http://example.test/","info":"error"}"#
        );
    }

    #[test]
    fn domain_permit_is_shared_per_domain() {
        let checker = AsyncChecker::new(
            LinkcheckConfig::from_config(&scribe_config::Config::new()),
            LocalLinkIndex::default(),
        )
        .expect("client");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        runtime.block_on(async {
            let a = checker.domain_permit("example.test").await;
            let b = checker.domain_permit("example.test").await;
            let c = checker.domain_permit("other.test").await;
            assert!(Arc::ptr_eq(&a, &b));
            assert!(!Arc::ptr_eq(&a, &c));
        });
    }
}
