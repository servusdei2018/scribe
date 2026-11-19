//! Shared helpers for Phase 08 integration tests.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A process-unique temporary project directory with automatic cleanup.
pub struct Fixture {
    root: PathBuf,
    source: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    pub fn new(tag: &str) -> Self {
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scribe-env-it-{tag}-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("src");
        let cache = root.join("cache");
        std::fs::create_dir_all(&source).expect("create source dir");
        std::fs::create_dir_all(&cache).expect("create cache dir");
        Self { root, source, cache }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn src(&self) -> &Path {
        &self.source
    }

    pub fn cache(&self) -> &Path {
        &self.cache
    }

    pub fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.source.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, content).expect("write fixture file");
        path
    }

    pub fn append(&self, relative: &str, content: &str) {
        use std::io::Write;
        let path = self.source.join(relative);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open fixture file for append");
        file.write_all(content.as_bytes()).expect("append content");
    }

    pub fn remove(&self, relative: &str) {
        let path = self.source.join(relative);
        if path.is_file() {
            std::fs::remove_file(path).expect("remove fixture file");
        }
    }

    /// Write a fresh configuration with the given key/value lines.
    pub fn write_conf(&self, lines: &[&str]) {
        std::fs::write(self.source.join("conf.py"), lines.join("\n"))
            .expect("write conf.py");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Load a configuration from a `conf.py` fixture via the static bridge, so
/// tests stay independent of the Python interpreter.
pub fn load_conf(source: &Path) -> scribe_config::Config {
    let request = scribe_config::LoadRequest {
        conf_dir: source.to_path_buf(),
        strategy: scribe_config::BridgeStrategy::Static,
        ..scribe_config::LoadRequest::default()
    };
    scribe_config::load_config_with(request)
        .expect("load fixture config")
        .0
}

/// Default configuration (no conf.py) for fixtures that only need suffixes.
pub fn default_config() -> scribe_config::Config {
    scribe_config::Config::new()
}
