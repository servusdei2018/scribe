//! Parallel loading of `intersphinx_mapping` inventories.
//!
//! Upstream uses `concurrent.futures.ThreadPoolExecutor` plus a per-inventory
//! location fallback loop; we use `rayon` for the same effect.  Each project
//! is fetched independently, remote inventories go through `reqwest` (rustls)
//! with a timeout and retry, and successful remote responses are cached under
//! a BLAKE3 key.  Local paths and `file://` URIs work offline.  Every failure
//! becomes a warning string on the outcome, never a panic or a build error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rayon::prelude::*;
use scribe_config::{Config, ConfigValue};

use super::cache::{CacheEntry, InventoryCache};
use super::inventory::{posixpath_join, Inventory};
use super::{IntersphinxError, IntersphinxResult};

/// A single inventory location; `None` means the default `objects.inv` next to
/// the project's target URI (upstream's `None` inventory location).
pub type InventoryLocation = Option<String>;

/// One normalized `intersphinx_mapping` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappingEntry {
    /// Project identifier (mapping key).
    pub name: String,
    /// Base URI used to resolve links in the inventory.
    pub target_uri: String,
    /// Candidate inventory locations, tried in order.
    pub locations: Vec<InventoryLocation>,
}

/// Parsed and validated `intersphinx_mapping`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IntersphinxMapping(pub BTreeMap<String, MappingEntry>);

impl IntersphinxMapping {
    /// Builds a mapping from the resolved configuration.
    ///
    /// Invalid entries are dropped with a warning rather than aborting the
    /// build (upstream raises `ConfigError`, but scribe keeps intersphinx
    /// failures non-fatal by contract).
    pub fn from_config(config: &Config) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let Some(value) = config.get("intersphinx_mapping") else {
            return (Self::default(), warnings);
        };
        let ConfigValue::Dict(entries) = value else {
            warnings.push(format!("invalid intersphinx_mapping: expected a dict, got {value:?}"));
            return (Self::default(), warnings);
        };
        let mut result = BTreeMap::new();
        let mut seen_uris: BTreeMap<String, String> = BTreeMap::new();
        for (name, raw) in entries {
            if name.is_empty() {
                warnings.push("invalid intersphinx_mapping: empty project identifier".to_owned());
                continue;
            }
            let Some((uri, locations)) = parse_mapping_value(raw) else {
                warnings.push(format!("invalid intersphinx_mapping[{name:?}]: expected (uri, locations)"));
                continue;
            };
            if uri.is_empty() {
                warnings.push(format!("invalid intersphinx_mapping[{name:?}]: empty target URI"));
                continue;
            }
            if let Some(other) = seen_uris.get(&uri) {
                warnings.push(format!(
                    "invalid intersphinx_mapping[{name:?}]: duplicate target URI {uri:?} (also used by {other:?})"
                ));
                continue;
            }
            seen_uris.insert(uri.clone(), name.clone());
            result.insert(
                name.clone(),
                MappingEntry { name: name.clone(), target_uri: uri, locations },
            );
        }
        (Self(result), warnings)
    }

    /// Builds a mapping from `(name, uri, locations)` triples (test helper).
    pub fn from_entries(entries: impl IntoIterator<Item = (String, String, Vec<InventoryLocation>)>) -> Self {
        let mut result = BTreeMap::new();
        for (name, target_uri, locations) in entries {
            result.insert(
                name.clone(),
                MappingEntry { name, target_uri, locations },
            );
        }
        Self(result)
    }

    /// True when no inventories are configured.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of configured projects.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// All entries in deterministic (name) order.
    pub fn entries(&self) -> Vec<&MappingEntry> {
        self.0.values().collect()
    }
}

fn parse_mapping_value(raw: &ConfigValue) -> Option<(String, Vec<InventoryLocation>)> {
    let ConfigValue::List(pair) = raw else {
        return None;
    };
    if pair.len() != 2 {
        return None;
    }
    let ConfigValue::Str(uri) = &pair[0] else {
        return None;
    };
    let locations = match &pair[1] {
        ConfigValue::None => vec![None],
        ConfigValue::Str(location) => vec![Some(location.clone())],
        ConfigValue::List(values) => values
            .iter()
            .map(|value| match value {
                ConfigValue::Str(location) if !location.is_empty() => Some(location.clone()),
                ConfigValue::None | ConfigValue::Str(_) => None,
                _ => None,
            })
            .collect(),
        _ => return None,
    };
    let locations = if locations.is_empty() { vec![None] } else { locations };
    Some((uri.clone(), locations))
}

/// Options controlling inventory fetching.
#[derive(Clone, Debug)]
pub struct FetchOptions {
    /// Overall per-request timeout (`None` means no explicit timeout).
    pub timeout: Option<Duration>,
    /// TCP connect timeout, applied in addition to `timeout`.
    pub connect_timeout: Option<Duration>,
    /// Socket read timeout, applied in addition to `timeout`.
    pub read_timeout: Option<Duration>,
    /// Number of retries after the first failed attempt.
    pub retries: usize,
    /// Base delay between retries, doubled after each failure (bounded).
    pub retry_backoff: Duration,
    /// `User-Agent` sent with every HTTP request.
    pub user_agent: String,
    /// Maximum number of redirects followed by the HTTP client.
    pub max_redirects: usize,
    /// Use a stale (expired) cache entry when every fresh fetch fails.
    pub offline_fallback: bool,
    /// Cache lifetime in days (mirrors `intersphinx_cache_limit`).
    pub cache_limit_days: i64,
    /// Cache directory; `None` disables disk caching.
    pub cache_dir: Option<PathBuf>,
    /// Source directory used to resolve plain relative inventory paths.
    pub source_dir: Option<PathBuf>,
    /// "Now" as a UNIX timestamp, injectable for deterministic cache tests.
    pub now: i64,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            timeout: Some(Duration::from_secs(15)),
            connect_timeout: Some(Duration::from_secs(5)),
            read_timeout: Some(Duration::from_secs(10)),
            retries: 2,
            retry_backoff: Duration::from_millis(250),
            user_agent: format!("scribe/{} (+https://github.com/credo/scribe)", env!("CARGO_PKG_VERSION")),
            max_redirects: 10,
            offline_fallback: true,
            cache_limit_days: 5,
            cache_dir: None,
            source_dir: None,
            now: InventoryCache::now(),
        }
    }
}

/// Result of loading one project's inventory.
#[derive(Clone, Debug)]
pub struct FetchOutcome {
    /// Project name from the mapping.
    pub name: String,
    /// Project target URI.
    pub target_uri: String,
    /// Loaded inventory, or `None` when every location failed.
    pub inventory: Option<Inventory>,
    /// Non-fatal warnings collected while trying locations.
    pub warnings: Vec<String>,
    /// True when the bytes came from the on-disk cache.
    pub from_cache: bool,
}

/// Fetches every configured inventory in parallel (rayon).
///
/// Outcomes are sorted by project name so the result is deterministic
/// independent of thread scheduling.
pub fn fetch_all(mapping: &IntersphinxMapping, options: &FetchOptions) -> Vec<FetchOutcome> {
    let cache = options.cache_dir.as_ref().map(|root| InventoryCache::new(root.clone(), options.cache_limit_days));
    let mut outcomes: Vec<FetchOutcome> = mapping
        .entries()
        .par_iter()
        .map(|entry| fetch_one(entry, options, cache.as_ref()))
        .collect();
    outcomes.sort_by(|a, b| a.name.cmp(&b.name));
    outcomes
}

/// Async wrapper around [`fetch_all`].
///
/// The blocking fetches (HTTP with `reqwest`, local files and rayon-parallel
/// projects) run on a `tokio::task::spawn_blocking` worker so callers with an
/// async runtime never block their executor.  A joined-task failure is
/// converted into a structured fetch error, not a panic.
pub async fn fetch_all_async(
    mapping: &IntersphinxMapping,
    options: &FetchOptions,
) -> IntersphinxResult<Vec<FetchOutcome>> {
    let mapping = mapping.clone();
    let options = options.clone();
    tokio::task::spawn_blocking(move || fetch_all(&mapping, &options))
        .await
        .map_err(|error| IntersphinxError::Fetch {
            url: "intersphinx worker".to_owned(),
            detail: error.to_string(),
        })
}

/// Convenience for synchronous callers that own a multi-thread tokio runtime.
pub fn fetch_all_with_runtime(
    runtime: &tokio::runtime::Runtime,
    mapping: &IntersphinxMapping,
    options: &FetchOptions,
) -> IntersphinxResult<Vec<FetchOutcome>> {
    runtime.block_on(fetch_all_async(mapping, options))
}

/// Fetches a single project's inventory, trying its locations in order.
pub fn fetch_one(
    entry: &MappingEntry,
    options: &FetchOptions,
    cache: Option<&InventoryCache>,
) -> FetchOutcome {
    let mut warnings = Vec::new();
    let locations = if entry.locations.is_empty() {
        vec![None]
    } else {
        entry.locations.clone()
    };
    let cache_key = format!("{}\u{0}{}", entry.name, entry.target_uri);

    for location in locations {
        let inv_location = match location {
            Some(ref path) => path.clone(),
            None => posixpath_join(&entry.target_uri, "objects.inv"),
        };
        // Upstream caches any location containing `://` (including `file://`
        // URLs); plain relative paths are always read from disk.  scribe also
        // supports reading `file://` directly, so caching it is safe and makes
        // offline cache tests deterministic.
        let cacheable = inv_location.contains("://");

        // A fresh cached inventory short-circuits remote fetching.
        if cacheable {
            if let Some(cache) = cache {
                if let Some(cached) = cache.get(&cache_key, options.now) {
                    match Inventory::loads(&cached.bytes, &entry.target_uri) {
                        Ok(inventory) => {
                            return success(entry, inventory, warnings, true);
                        }
                        Err(error) => warnings.push(format!("cached inventory for {:?}: {error}", entry.name)),
                    }
                }
            }
        }

        let (raw, final_location) = match fetch_bytes(&inv_location, options) {
            Ok(pair) => pair,
            Err(error) => {
                warnings.push(error.to_string());
                // Offline fallback: a stale inventory beats no inventory.
                // This keeps builds usable when the network is unavailable
                // and never turns a fetch failure into a build error.
                if options.offline_fallback {
                    if let Some(cache) = cache {
                        if let Some(stale) = cache.get_stale(&cache_key) {
                            match Inventory::loads(&stale.bytes, &entry.target_uri) {
                                Ok(inventory) => {
                                    warnings.push(format!(
                                        "using stale cached inventory for {:?}",
                                        entry.name
                                    ));
                                    return success(entry, inventory, warnings, true);
                                }
                                Err(decode_error) => warnings.push(format!(
                                    "cached inventory for {:?}: {decode_error}",
                                    entry.name
                                )),
                            }
                        }
                    }
                }
                continue;
            }
        };
        if cacheable {
            if let Some(cache) = cache {
                let stored = CacheEntry {
                    name: entry.name.clone(),
                    timestamp: options.now,
                    bytes: raw.clone(),
                };
                if let Err(error) = cache.put(&cache_key, &stored) {
                    warnings.push(error.to_string());
                }
            }
        }
        // Upstream `_fetch_inventory_url` rewrites the *base* URI of the
        // generated links when the inventory download was redirected and the
        // configured target still points at the original location.  Mirror
        // that so links from moved inventories (notably readthedocs) keep
        // working.
        let effective_uri = rewrite_target_uri(&entry.target_uri, &inv_location, &final_location);
        match Inventory::loads(&raw, &effective_uri) {
            Ok(inventory) => return success(entry, inventory, warnings, false),
            Err(error) => warnings.push(format!(
                "inventory {} not readable due to {error}",
                safe_location(&inv_location)
            )),
        }
    }

    FetchOutcome {
        name: entry.name.clone(),
        target_uri: entry.target_uri.clone(),
        inventory: None,
        warnings,
        from_cache: false,
    }
}

fn success(
    entry: &MappingEntry,
    inventory: Inventory,
    warnings: Vec<String>,
    from_cache: bool,
) -> FetchOutcome {
    FetchOutcome {
        name: entry.name.clone(),
        target_uri: entry.target_uri.clone(),
        inventory: Some(inventory),
        warnings,
        from_cache,
    }
}

fn is_http(location: &str) -> bool {
    location.starts_with("http://") || location.starts_with("https://")
}

/// Fetches the raw bytes for one location (HTTP, `file://` or plain path).
///
/// The returned string is the *final* location: for HTTP it is the URL after
/// redirects, which upstream uses to rewrite the base URI of generated links.
fn fetch_bytes(location: &str, options: &FetchOptions) -> IntersphinxResult<(Vec<u8>, String)> {
    if is_http(location) {
        return fetch_http(location, options);
    }
    let path = if let Some(rest) = location.strip_prefix("file://") {
        decode_file_url(rest)
    } else {
        PathBuf::from(location)
    };
    let path = resolve_local_path(&path, options.source_dir.as_deref());
    fs_read(&path).map(|bytes| (bytes, location.to_owned()))
}

/// Mirrors upstream `_fetch_inventory_url`'s target-URI rewrite.  When the
/// download was redirected and the configured base URI still points at the
/// original inventory (or its directory), use the redirected directory as the
/// new base URI so relative links in the inventory remain correct.
fn rewrite_target_uri(target_uri: &str, inv_location: &str, final_location: &str) -> String {
    if inv_location == final_location {
        return target_uri.to_owned();
    }
    let dirname = |uri: &str| -> String {
        uri.rfind('/').map_or_else(String::new, |index| uri[..index].to_owned())
    };
    let inv_dir = dirname(inv_location);
    if target_uri == inv_location || target_uri == inv_dir || target_uri == format!("{inv_dir}/") {
        return dirname(final_location);
    }
    target_uri.to_owned()
}

fn decode_file_url(rest: &str) -> PathBuf {
    // A `file:///abs/path` begins with '/'; a relative `file://path` is
    // normalized to `file:///path` for offline tests.
    let candidate = if rest.starts_with('/') {
        format!("file://{rest}")
    } else {
        format!("file:///{rest}")
    };
    url::Url::parse(&candidate)
        .ok()
        .and_then(|parsed| parsed.to_file_path().ok())
        .unwrap_or_else(|| PathBuf::from(rest))
}

fn resolve_local_path(path: &Path, source_dir: Option<&Path>) -> PathBuf {
    let mut resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        source_dir.unwrap_or_else(|| Path::new(".")).join(path)
    };
    if resolved.is_dir() {
        resolved = resolved.join("objects.inv");
    }
    resolved
}

fn fs_read(path: &Path) -> IntersphinxResult<Vec<u8>> {
    std::fs::read(path).map_err(|error| IntersphinxError::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    })
}

fn fetch_http(location: &str, options: &FetchOptions) -> IntersphinxResult<(Vec<u8>, String)> {
    let url = strip_basic_auth(location);
    let mut client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(options.max_redirects));
    if let Some(timeout) = options.timeout {
        client = client.timeout(timeout);
    }
    if let Some(connect_timeout) = options.connect_timeout {
        client = client.connect_timeout(connect_timeout);
    }
    if !options.user_agent.is_empty() {
        let user_agent = reqwest::header::HeaderValue::from_str(&options.user_agent)
            .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("scribe"));
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::USER_AGENT, user_agent);
        client = client.default_headers(headers);
    }
    let client = client.build().map_err(|error| IntersphinxError::Fetch {
        url: url.clone(),
        detail: error.to_string(),
    })?;
    let mut last_error = String::new();
    for attempt in 0..=options.retries {
        // `read_timeout` bounds a stalled body read even when the overall
        // request timeout is disabled; reqwest applies it per socket read.
        let request = client.get(&url);
        let outcome = match options.read_timeout {
            Some(read_timeout) => request.timeout(read_timeout).send(),
            None => request.send(),
        };
        match outcome.and_then(|response| response.error_for_status()) {
            Ok(response) => {
                // `Response::url` is the URL after all redirects; feed it back
                // so `rewrite_target_uri` can update the base URI when needed.
                let final_url = response.url().to_string();
                return response.bytes().map(|bytes| (bytes.to_vec(), final_url)).map_err(|error| {
                    IntersphinxError::Fetch { url: url.clone(), detail: error.to_string() }
                });
            }
            Err(error) => {
                let mut detail = error.to_string();
                let mut source = std::error::Error::source(&error);
                while let Some(cause) = source {
                    detail.push_str(&format!(" [{cause}]"));
                    source = cause.source();
                }
                last_error = format!("attempt {}: {detail}", attempt + 1);
            }
        }
        if attempt < options.retries && !options.retry_backoff.is_zero() {
            // Exponential backoff, capped so a large `retries` cannot stall a
            // build for minutes.  A failed fetch always degrades to a warning.
            let shift = attempt.min(6);
            let delay = options.retry_backoff.saturating_mul(1u32 << shift);
            std::thread::sleep(delay.min(Duration::from_secs(8)));
        }
    }
    Err(IntersphinxError::Fetch { url, detail: last_error })
}

/// Removes basic-auth credentials from a URL (upstream `_strip_basic_auth`).
fn strip_basic_auth(uri: &str) -> String {
    match url::Url::parse(uri) {
        Ok(mut parsed) if !parsed.username().is_empty() || parsed.password().is_some() => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        _ => uri.to_owned(),
    }
}

/// Obscures basic-auth passwords for warning messages (upstream `_get_safe_url`).
fn safe_location(location: &str) -> String {
    match url::Url::parse(location) {
        Ok(mut parsed) if parsed.password().is_some() => {
            let _ = parsed.set_password(Some("***"));
            parsed.to_string()
        }
        _ => location.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mapping_shapes() {
        let uri = ConfigValue::from("https://example.com/docs");
        let none = ConfigValue::List(vec![uri.clone(), ConfigValue::None]);
        assert_eq!(
            parse_mapping_value(&none),
            Some(("https://example.com/docs".into(), vec![None]))
        );
        let single = ConfigValue::List(vec![uri.clone(), ConfigValue::from("inv.objects.inv")]);
        assert_eq!(
            parse_mapping_value(&single),
            Some(("https://example.com/docs".into(), vec![Some("inv.objects.inv".into())]))
        );
    }

    #[test]
    fn redirect_rewrites_target_uri_only_for_matching_base() {
        // Target equal to the original inventory's directory is rewritten to
        // the redirected directory, mirroring upstream `_fetch_inventory_url`.
        assert_eq!(
            rewrite_target_uri(
                "https://docs.readthedocs.io/en/stable",
                "https://docs.readthedocs.io/en/stable/objects.inv",
                "https://docs.readthedocs.io/en/stable/objects.inv",
            ),
            "https://docs.readthedocs.io/en/stable"
        );
        assert_eq!(
            rewrite_target_uri(
                "https://example.com/docs/",
                "https://example.com/docs/objects.inv",
                "https://cdn.example.com/mirror/objects.inv",
            ),
            "https://cdn.example.com/mirror"
        );
        // Unrelated base URIs are left untouched.
        assert_eq!(
            rewrite_target_uri(
                "https://example.com/base",
                "https://example.com/other/objects.inv",
                "https://cdn.example.com/mirror/objects.inv",
            ),
            "https://example.com/base"
        );
    }

    #[test]
    fn strip_basic_auth_removes_credentials() {
        assert_eq!(
            strip_basic_auth("https://user:pass@example.com/docs/objects.inv"),
            "https://example.com/docs/objects.inv"
        );
        assert_eq!(strip_basic_auth("https://example.com/x"), "https://example.com/x");
    }
}
