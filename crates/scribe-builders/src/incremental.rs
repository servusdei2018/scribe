//! Phase 67 incremental build artifacts.
//!
//! This module persists the small, per-document "delta chunks" that let a
//! warm build avoid re-parsing and re-rendering the entire project:
//!
//! * **search-index fragments** — already tokenised / stemmed contributions
//!   to the global `searchindex.js` (see [`SearchIndexFragment`]),
//! * **domain inventory deltas** — the serialized domain inventories already
//!   live in `BuildEnvironment::domain_data` (Phase 11), which the cache
//!   restores on an incremental build,
//! * **toctree fragments** — the resolved toctree edges live in
//!   `BuildEnvironment::toctree_includes` and are likewise restored from the
//!   environment cache.
//!
//! Storage goes through [`scribe_cache::CacheStore`], the versioned,
//! content-verified typed store provided by the `scribe-cache` crate. A
//! missing or corrupt fragment artifact is simply a cold cache: the builder
//! recomputes every fragment and repersists them, so no build ever aborts on a
//! bad cache file.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::search::SearchIndexFragment;

/// Cache key for the search index fragment bundle.
pub const SEARCH_FRAGMENTS_KEY: &str = "search-fragments";

/// A bundle of every document's search-index contribution.
///
/// One file is written per build; merging is a single bincode decode followed
/// by `BTreeMap` inserts, which is comfortably inside the <5 ms target even
/// for a few hundred pages.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FragmentCache {
    /// Schema marker, bumped together with the fragment layout.
    pub format_version: u32,
    /// Language the stemmed terms were computed with. A change forces a full
    /// rebuild so cached terms are never mixed across stemmers.
    pub search_language: String,
    /// `docname → per-document search contribution`.
    pub search_fragments: BTreeMap<String, SearchIndexFragment>,
}

impl FragmentCache {
    /// Version of the serialized fragment bundle.
    pub const FORMAT_VERSION: u32 = 2;

    /// Create an empty cache for `search_language`.
    #[must_use]
    pub fn new(search_language: &str) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            search_language: search_language.to_owned(),
            search_fragments: BTreeMap::new(),
        }
    }

    /// Load the fragment bundle from `cache_dir`, returning `None` when it is
    /// absent, stale or corrupt.
    #[must_use]
    pub fn load(cache_dir: &Path) -> Option<Self> {
        let store = scribe_cache::CacheStore::<Self>::new(cache_dir);
        let cache = store.load(SEARCH_FRAGMENTS_KEY).ok()??;
        (cache.format_version == Self::FORMAT_VERSION).then_some(cache)
    }

    /// Persist the fragment bundle to `cache_dir`.
    pub fn save(&self, cache_dir: &Path) -> Result<(), scribe_cache::CacheError> {
        let store = scribe_cache::CacheStore::<Self>::new(cache_dir);
        store.save(SEARCH_FRAGMENTS_KEY, self).map(|_| ())
    }
}

impl Default for FragmentCache {
    fn default() -> Self {
        Self::new("")
    }
}

impl FragmentCache {
    /// Whether this bundle has a fragment for every requested document.
    #[must_use]
    pub fn covers<'a, I>(&self, docnames: I) -> bool
    where
        I: IntoIterator<Item = &'a str>,
    {
        docnames
            .into_iter()
            .all(|docname| self.search_fragments.contains_key(docname))
    }
}

#[cfg(test)]
mod tests {
    use super::{FragmentCache, SEARCH_FRAGMENTS_KEY};
    use crate::search::SearchIndexFragment;

    fn sample(name: &str) -> SearchIndexFragment {
        SearchIndexFragment {
            docname: name.to_owned(),
            title: "Title".to_owned(),
            filename: format!("{name}.rst"),
            all_titles: vec![("Title".to_owned(), None)],
            body_terms: vec!["term".to_owned()],
            title_terms: vec!["titl".to_owned()],
            index_entries: Vec::new(),
        }
    }

    #[test]
    fn fragment_cache_round_trip_and_coverage() {
        let root = std::env::temp_dir().join(format!(
            "scribe-fragment-cache-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut cache = FragmentCache::new("en");
        assert!(!cache.covers(["a", "b"]));
        cache.search_fragments.insert("a".to_owned(), sample("a"));
        cache.search_fragments.insert("b".to_owned(), sample("b"));
        assert!(cache.covers(["a", "b"]));
        cache.save(&root).expect("save fragment cache");
        let loaded = FragmentCache::load(&root).expect("load fragment cache");
        assert_eq!(loaded.search_fragments.len(), 2);
        assert_eq!(loaded.search_fragments["a"].filename, "a.rst");
        assert_eq!(loaded.search_fragments["b"].body_terms, vec!["term".to_owned()]);
        assert!(std::path::Path::new(&root)
            .join(format!("cache-{SEARCH_FRAGMENTS_KEY}.bin"))
            .is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}
