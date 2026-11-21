//! Client-side full-text search index (Phase 18).
//!
//! This module is the Rust counterpart of Sphinx's `sphinx.search` package and
//! the `html` builder's `IndexBuilder`. It builds the exact JSON payload that
//! the vendored `searchtools.js` consumes via `Search.setIndex({...})`.
//!
//! # Upstream contract (inspected, not guessed)
//!
//! * `sphinx/search/__init__.py` — `IndexBuilder.feed` / `freeze`, `WordCollector`,
//!   `_JavaScriptIndex` (`PREFIX = "Search.setIndex("`, `SUFFIX = ")"`).
//! * `sphinx/search/en.py` — `SearchEnglish` (Snowball English stemmer, English
//!   stop-word set).
//! * `sphinx/search/_stopwords/en.txt` — the 174-word Snowball English list,
//!   vendored below with attribution.
//! * `sphinx/themes/basic/static/searchtools.js` — the client-side query/ranking
//!   algorithm; its `terms` / `titleterms` maps map a stemmed token to a single
//!   document index (when unique) or a sorted array of indices.
//!
//! # Deviations (documented in `ARCHITECTURE.md` §22)
//!
//! * The pinned oracle (Sphinx 9.3.0) emits `Search.setIndex({...})` with **no**
//!   trailing semicolon; scribe matches those bytes exactly.
//! * `envversion` keeps the module→integer shape but uses scribe-specific
//!   values; the stock JavaScript only relies on it being stable across builds.
//! * Non-English languages stem through `rust-stemmers` where a Snowball
//!   algorithm exists; otherwise (e.g. `ja`, `zh`) tokenization and identity
//!   stemming degrade gracefully. Only English has a vendored stop-word list.

use std::collections::{BTreeMap, BTreeSet};

use once_cell::sync::Lazy;
use regex::Regex;
use rust_stemmers::{Algorithm, Stemmer};
use scribe_ast::{Document, NodeId, NodeKind};
use scribe_domains::{DomainRegistry, ObjectEntry};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};

/// Snowball English stop words, vendored verbatim from Sphinx's
/// `sphinx/search/_stopwords/en.txt` (which is itself generated from the
/// Snowball project's `algorithms/english/stop.txt`).
///
/// Attribution: the Snowball stop-word list is in the public domain; see
/// <https://github.com/snowballstem/snowball-website>. Sphinx distributes it
/// under its BSD-3-Clause license.
pub const ENGLISH_STOPWORDS: &[&str] = &[
    "a", "about", "above", "after", "again", "against", "all", "am", "an", "and", "any", "are",
    "aren't", "as", "at", "be", "because", "been", "before", "being", "below", "between", "both",
    "but", "by", "can't", "cannot", "could", "couldn't", "did", "didn't", "do", "does", "doesn't",
    "doing", "don't", "down", "during", "each", "few", "for", "from", "further", "had", "hadn't",
    "has", "hasn't", "have", "haven't", "having", "he", "he'd", "he'll", "he's", "her", "here",
    "here's", "hers", "herself", "him", "himself", "his", "how", "how's", "i", "i'd", "i'll",
    "i'm", "i've", "if", "in", "into", "is", "isn't", "it", "it's", "its", "itself", "let's",
    "me", "more", "most", "mustn't", "my", "myself", "no", "nor", "not", "of", "off", "on",
    "once", "only", "or", "other", "ought", "our", "ours", "ourselves", "out", "over", "own",
    "same", "shan't", "she", "she'd", "she'll", "she's", "should", "shouldn't", "so", "some",
    "such", "than", "that", "that's", "the", "their", "theirs", "them", "themselves", "then",
    "there", "there's", "these", "they", "they'd", "they'll", "they're", "they've", "this",
    "those", "through", "to", "too", "under", "until", "up", "very", "was", "wasn't", "we",
    "we'd", "we'll", "we're", "we've", "were", "weren't", "what", "what's", "when", "when's",
    "where", "where's", "which", "while", "who", "who's", "whom", "why", "why's", "with",
    "won't", "would", "wouldn't", "you", "you'd", "you'll", "you're", "you've", "your", "yours",
    "yourself", "yourselves",
];

/// Languages with a full (Snowball stemmer + stop words) scribe implementation.
pub const FULLY_SUPPORTED_LANGUAGES: &[&str] = &["en"];

/// Language codes whose Snowball algorithm is available through
/// `rust-stemmers`. Other configured languages (notably `ja`/`zh`) fall back to
/// a simple word tokenizer with identity stemming.
pub const STEMMABLE_LANGUAGES: &[&str] = &[
    "ar", "da", "de", "el", "en", "es", "fi", "fr", "hu", "it", "nl", "no", "pt", "ro", "ru",
    "sv", "ta", "tr",
];

/// The prefix/suffix of a Sphinx JavaScript search index.
pub const JS_INDEX_PREFIX: &str = "Search.setIndex(";
pub const JS_INDEX_SUFFIX: &str = ")";

// Regexes used to sanitize raw HTML nodes exactly as upstream's
// `WordCollector` does. These are hard-coded, well-formed literals, so the
// `expect` calls below are provably safe (a malformed literal is a programming
// error caught by the unit tests, never a runtime input).
static STYLE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?is)<style.*?</style>").expect("hard-coded style regex is valid")
});
static SCRIPT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?is)<script.*?</script>").expect("hard-coded script regex is valid")
});
static TAG_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<[^<]+?>").expect("hard-coded tag regex is valid"));

/// Split `input` into words using Sphinx's Unicode-aware `word_re = r'\w+'`.
///
/// Rust's [`char::is_alphanumeric`] plus `_` is a close analogue of Python's
/// Unicode `\w` (letters, numbers and underscore). Neither lowercases: Sphinx
/// lowercases only inside the stemmer and keeps the original token as a
/// fallback when the stemmed form is a stop word (which is why e.g. `"It"`
/// can legitimately appear in the index).
#[must_use]
pub fn tokenize(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in input.chars() {
        if ch == '_' || ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Map a Sphinx language code to a Snowball algorithm available in
/// `rust-stemmers`. Returns `None` for languages without a Rust algorithm
/// (e.g. `ja`, `zh`), where scribe degrades to identity stemming.
#[must_use]
pub fn algorithm_for(language: &str) -> Option<Algorithm> {
    let lang = language.split('_').next().unwrap_or(language).to_lowercase();
    match lang.as_str() {
        "ar" => Some(Algorithm::Arabic),
        "da" => Some(Algorithm::Danish),
        "de" => Some(Algorithm::German),
        "el" => Some(Algorithm::Greek),
        "en" => Some(Algorithm::English),
        "es" => Some(Algorithm::Spanish),
        "fi" => Some(Algorithm::Finnish),
        "fr" => Some(Algorithm::French),
        "hu" => Some(Algorithm::Hungarian),
        "it" => Some(Algorithm::Italian),
        "nl" => Some(Algorithm::Dutch),
        "no" | "nb" | "nn" => Some(Algorithm::Norwegian),
        "pt" => Some(Algorithm::Portuguese),
        "ro" => Some(Algorithm::Romanian),
        "ru" => Some(Algorithm::Russian),
        "sv" => Some(Algorithm::Swedish),
        "ta" => Some(Algorithm::Tamil),
        "tr" => Some(Algorithm::Turkish),
        _ => None,
    }
}

/// Stem one lowercased word for `language`, mirroring Sphinx's
/// `stem(word).lower()`. Languages without a Snowball backend return the
/// lowercased word unchanged (graceful degradation).
#[must_use]
pub fn stem_word(word: &str, language: &str) -> String {
    let lower = word.to_lowercase();
    match algorithm_for(language) {
        Some(algorithm) => {
            let stemmer = Stemmer::create(algorithm);
            stemmer.stem(&lower).to_lowercase()
        }
        None => lower,
    }
}

/// The stop-word set used for `language`. Only English is vendored; other
/// languages fall back to an empty set (documented divergence).
#[must_use]
pub fn stopwords_for(language: &str) -> &'static [&'static str] {
    let lang = language.split('_').next().unwrap_or(language).to_lowercase();
    if lang == "en" {
        ENGLISH_STOPWORDS
    } else {
        &[]
    }
}

/// Return `true` when a word should be filtered out of the index, matching
/// Sphinx's `SearchLanguage.word_filter` (`not word.isdigit() and word not in
/// stopwords`).
#[must_use]
pub fn is_filtered(word: &str, language: &str) -> bool {
    if !word.is_empty() && word.chars().all(char::is_numeric) {
        return true;
    }
    stopwords_for(language).contains(&word)
}

/// Deterministic, scribe-specific `envversion` map.
///
/// Upstream stores a module→integer map that changes whenever a domain's data
/// format changes. The stock `searchtools.js` never inspects it; only Sphinx's
/// Python `IndexBuilder.load` does, for incremental builds. scribe keeps the
/// shape with stable, internal values.
#[must_use]
pub fn scribe_envversion() -> BTreeMap<String, u64> {
    BTreeMap::from([
        ("scribe".to_owned(), 1),
        ("scribe.builders".to_owned(), 1),
        ("scribe.domains".to_owned(), 1),
        ("scribe.search".to_owned(), 1),
    ])
}

/// An object collected from a domain, before serialization.
#[derive(Clone, Debug)]
struct DomainObject {
    domain: String,
    name: String,
    display_name: String,
    objtype: String,
    type_name: String,
    docname: String,
    anchor: String,
    priority: i8,
}

/// Words collected from one doctree, mirroring upstream's `WordStore`.
#[derive(Default)]
struct WordStore {
    words: Vec<String>,
    titles: Vec<(String, Option<String>)>,
    title_words: Vec<String>,
}

/// A per-document contribution to the global [`SearchIndex`].
///
/// This is the "delta chunk" persisted in `scribe-cache` by incremental
/// builds. It stores the *already stemmed and filtered* terms for one document
/// so that reassembling the global index is a pure `BTreeMap` merge (no
/// re-tokenising or re-stemming of unchanged documents).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchIndexFragment {
    /// Sphinx docname this fragment belongs to.
    pub docname: String,
    /// Document title (empty string for untitled pages).
    pub title: String,
    /// Source filename including its suffix (`.rst`, `.txt`, …).
    pub filename: String,
    /// Section titles: `(title, optional anchor)`.
    pub all_titles: Vec<(String, Option<String>)>,
    /// Stemmed, filtered body terms (title terms already removed).
    pub body_terms: Vec<String>,
    /// Stemmed, filtered title terms.
    pub title_terms: Vec<String>,
    /// Explicit `.. index::` entries: `(lower-cased entry, anchor, "main"|"")`.
    pub index_entries: Vec<(String, String, String)>,
}

/// The client-side search index.
///
/// Construct with [`SearchIndex::new`], feed every document with
/// [`feed`](Self::feed), add domain objects with
/// [`add_domain_objects`](Self::add_domain_objects), then emit with
/// [`to_js`](Self::to_js). All internal storage is ordered (`BTreeMap` /
/// `BTreeSet`) so emitted bytes are reproducible across runs and machines.
#[derive(Clone, Debug)]
pub struct SearchIndex {
    language: String,
    envversion: BTreeMap<String, u64>,
    titles: BTreeMap<String, String>,
    filenames: BTreeMap<String, String>,
    mapping: BTreeMap<String, BTreeSet<String>>,
    title_mapping: BTreeMap<String, BTreeSet<String>>,
    all_titles: BTreeMap<String, Vec<(String, Option<String>)>>,
    index_entries: BTreeMap<String, Vec<(String, String, String)>>,
    objects: Vec<DomainObject>,
}

impl SearchIndex {
    /// Create an empty index for `language` with the given `envversion`.
    #[must_use]
    pub fn new(language: impl Into<String>, envversion: BTreeMap<String, u64>) -> Self {
        Self {
            language: language.into(),
            envversion,
            titles: BTreeMap::new(),
            filenames: BTreeMap::new(),
            mapping: BTreeMap::new(),
            title_mapping: BTreeMap::new(),
            all_titles: BTreeMap::new(),
            index_entries: BTreeMap::new(),
            objects: Vec::new(),
        }
    }

    /// Feed a document using the default filename `<docname>.rst`.
    pub fn feed(&mut self, docname: &str, title: &str, document: &Document) {
        let filename = format!("{docname}.rst");
        self.feed_with_filename(docname, &filename, title, document);
    }

    /// Feed a document with an explicit source filename (as upstream's
    /// `IndexBuilder.feed` does), so `filenames` reflects the real source
    /// suffix (`.rst`, `.txt`, …).
    pub fn feed_with_filename(
        &mut self,
        docname: &str,
        filename: &str,
        title: &str,
        document: &Document,
    ) {
        let fragment = Self::build_fragment(docname, filename, title, document, &self.language);
        self.apply_fragment(fragment);
    }

    /// Compute one document's search-index contribution without touching the
    /// global index. The returned fragment is byte-identical to what
    /// [`feed_with_filename`](Self::feed_with_filename) would have produced,
    /// so it can be cached and later merged with [`apply_fragment`](Self::apply_fragment)
    /// on an incremental build without re-tokenising the source.
    #[must_use]
    pub fn build_fragment(
        docname: &str,
        filename: &str,
        title: &str,
        document: &Document,
        language: &str,
    ) -> SearchIndexFragment {
        let mut store = WordStore::default();
        collect_words(document, document.root, &mut store);
        let all_titles = store.titles;

        // Title words: stem and, if the stem is filtered, fall back to the
        // original token (upstream's "stemmer must not remove words" rule).
        let mut title_terms = BTreeSet::new();
        for word in store.title_words {
            let stemmed = stem_word(&word, language);
            if !is_filtered(&stemmed, language) {
                title_terms.insert(stemmed);
            } else if !is_filtered(&word, language) {
                title_terms.insert(word);
            }
        }

        // Body words: a token already present in this document's title index
        // is not repeated in the body index. The check is scoped to *this*
        // document, which is exactly what the global `feed_with_filename`
        // achieved via `title_mapping.get(stemmed).contains(docname)`.
        let mut body_terms = BTreeSet::new();
        for word in store.words {
            let mut stemmed = stem_word(&word, language);
            if is_filtered(&stemmed, language) && !is_filtered(&word, language) {
                stemmed = word;
            }
            if !is_filtered(&stemmed, language) && !title_terms.contains(&stemmed) {
                body_terms.insert(stemmed);
            }
        }

        SearchIndexFragment {
            docname: docname.to_owned(),
            title: title.to_owned(),
            filename: filename.to_owned(),
            all_titles,
            body_terms: body_terms.into_iter().collect(),
            title_terms: title_terms.into_iter().collect(),
            index_entries: collect_index_entries_from(docname, document),
        }
    }

    /// Merge a precomputed fragment into the global index.
    pub fn apply_fragment(&mut self, fragment: SearchIndexFragment) {
        let docname = fragment.docname;
        self.titles.insert(docname.clone(), fragment.title);
        self.filenames.insert(docname.clone(), fragment.filename);
        self.all_titles.insert(docname.clone(), fragment.all_titles);
        for term in fragment.body_terms {
            self.mapping.entry(term).or_default().insert(docname.clone());
        }
        for term in fragment.title_terms {
            self.title_mapping.entry(term).or_default().insert(docname.clone());
        }
        for (entry, id, main) in fragment.index_entries {
            self.index_entries.entry(entry).or_default().push((docname.clone(), id, main));
        }
    }

    /// Add a single object without domain metadata. The domain is inferred
    /// from the object type key (falling back to `"py"`); prefer
    /// [`add_domain_objects`](Self::add_domain_objects) when a
    /// [`DomainRegistry`] is available for exact type names.
    pub fn add_object(&mut self, entry: &ObjectEntry) {
        let domain = infer_domain(&entry.objtype).to_owned();
        let type_name = entry.objtype.clone();
        self.push_object(domain, type_name, entry);
    }

    /// Add one object with explicit domain and localized type name.
    pub fn add_object_for_domain(&mut self, domain: &str, type_name: &str, entry: &ObjectEntry) {
        self.push_object(domain.to_owned(), type_name.to_owned(), entry);
    }

    /// Add all objects from every registered domain, using each domain's own
    /// name and localized type names (the faithful upstream path).
    pub fn add_domain_objects(&mut self, registry: &DomainRegistry) {
        for domain_name in registry.domain_names() {
            let Some(shared) = registry.domain(&domain_name) else {
                continue;
            };
            let guard = shared.lock();
            for entry in guard.get_objects() {
                let type_name = guard.get_type_name(&entry.objtype, true);
                self.push_object(domain_name.clone(), type_name, &entry);
            }
        }
    }

    /// Add an explicit index entry (`.. index::`), mirroring upstream's
    /// `IndexBuilder.feed` extraction from `addnodes.index`.
    pub fn add_index_entry(&mut self, docname: &str, entry: &str, id: &str, is_main: bool) {
        self.index_entries
            .entry(entry.to_lowercase())
            .or_default()
            .push((docname.to_owned(), id.to_owned(), if is_main { "main" } else { "" }.to_owned()));
    }

    /// Serialize the index to a deterministic `serde_json::Value`.
    ///
    /// Keys are inserted into `BTreeMap`s, and `serde_json`'s default map is
    /// also ordered, so the resulting byte stream is stable (matching Python's
    /// `json.dumps(..., sort_keys=True, separators=(',', ':'))`).
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let docnames: Vec<String> = self.titles.keys().cloned().collect();
        let fn2index: BTreeMap<&str, usize> = docnames
            .iter()
            .enumerate()
            .map(|(i, name)| (name.as_str(), i))
            .collect();

        let titles: Vec<String> = docnames
            .iter()
            .map(|name| self.titles.get(name).cloned().unwrap_or_default())
            .collect();
        let filenames: Vec<Value> = docnames
            .iter()
            .map(|name| match self.filenames.get(name) {
                Some(filename) => Value::String(filename.clone()),
                None => Value::Null,
            })
            .collect();

        let (terms, title_terms) = self.get_terms(&fn2index);
        let (objects, objtypes, objnames) = self.build_object_data(&fn2index);

        // alltitles: title -> [[doc_index, anchor_or_null], ...]
        let mut alltitles: BTreeMap<String, Value> = BTreeMap::new();
        for docname in &docnames {
            let Some(index) = fn2index.get(docname.as_str()) else {
                continue;
            };
            if let Some(entries) = self.all_titles.get(docname) {
                for (title, anchor) in entries {
                    let anchor_value = match anchor {
                        Some(id) => Value::String(id.clone()),
                        None => Value::Null,
                    };
                    let row = alltitles.entry(title.clone()).or_insert_with(|| Value::Array(Vec::new()));
                    if let Some(array) = row.as_array_mut() {
                        array.push(Value::Array(vec![Value::from(*index), anchor_value]));
                    }
                }
            }
        }

        // indexentries: entry.lower() -> [[doc_index, id, is_main], ...]
        let mut index_entries: BTreeMap<String, Value> = BTreeMap::new();
        for (entry, found) in &self.index_entries {
            let mut rows = Vec::new();
            for (docname, id, main) in found {
                if let Some(index) = fn2index.get(docname.as_str()) {
                    rows.push(Value::Array(vec![
                        Value::from(*index),
                        Value::String(id.clone()),
                        Value::Bool(main == "main"),
                    ]));
                }
            }
            index_entries.insert(entry.clone(), Value::Array(rows));
        }

        // Assemble with sorted keys (BTreeMap -> serde_json::Map).
        let mut root: BTreeMap<String, Value> = BTreeMap::new();
        root.insert("alltitles".to_owned(), Value::Object(alltitles.into_iter().collect::<JsonMap<_, _>>()));
        root.insert("docnames".to_owned(), Value::Array(docnames.into_iter().map(Value::String).collect()));
        root.insert("envversion".to_owned(), serde_json::to_value(&self.envversion).unwrap_or(Value::Null));
        root.insert("filenames".to_owned(), Value::Array(filenames));
        root.insert("indexentries".to_owned(), Value::Object(index_entries.into_iter().collect::<JsonMap<_, _>>()));
        root.insert(
            "objects".to_owned(),
            Value::Object(
                objects
                    .into_iter()
                    .map(|(prefix, rows)| (prefix, Value::Array(rows)))
                    .collect::<JsonMap<_, _>>(),
            ),
        );
        root.insert(
            "objnames".to_owned(),
            Value::Object(
                objnames
                    .into_iter()
                    .map(|(index, (domain, objtype, name))| {
                        (
                            index.to_string(),
                            Value::Array(vec![
                                Value::String(domain),
                                Value::String(objtype),
                                Value::String(name),
                            ]),
                        )
                    })
                    .collect::<JsonMap<_, _>>(),
            ),
        );
        root.insert(
            "objtypes".to_owned(),
            Value::Object(
                objtypes
                    .into_iter()
                    .map(|((domain, objtype), index)| {
                        (index.to_string(), Value::String(format!("{domain}:{objtype}")))
                    })
                    .collect::<JsonMap<_, _>>(),
            ),
        );
        root.insert("terms".to_owned(), Value::Object(terms.into_iter().collect::<JsonMap<_, _>>()));
        root.insert("titles".to_owned(), Value::Array(titles.into_iter().map(Value::String).collect()));
        root.insert("titleterms".to_owned(), Value::Object(title_terms.into_iter().collect::<JsonMap<_, _>>()));
        Value::Object(root.into_iter().collect::<JsonMap<_, _>>())
    }

    /// Emit the JavaScript index exactly as Sphinx's `_JavaScriptIndex.dumps`
    /// does: `Search.setIndex(` + compact sorted JSON + `)`.
    #[must_use]
    pub fn to_js(&self) -> String {
        // `to_json_value()` only contains `Value`s, whose serializer is
        // infallible; the fallback is unreachable but keeps this method total.
        let json = serde_json::to_string(&self.to_json_value()).unwrap_or_else(|_| "{}".to_owned());
        format!("{JS_INDEX_PREFIX}{json}{JS_INDEX_SUFFIX}")
    }

    /// Number of documents fed so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.titles.len()
    }

    /// True when no documents have been fed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.titles.is_empty()
    }

    /// The configured language code.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    // ---- internal helpers -------------------------------------------------

    fn push_object(&mut self, domain: String, type_name: String, entry: &ObjectEntry) {
        self.objects.push(DomainObject {
            domain,
            name: entry.name.clone(),
            display_name: entry.display_name.clone(),
            objtype: entry.objtype.clone(),
            type_name,
            docname: entry.docname.clone(),
            anchor: entry.anchor.clone(),
            priority: entry.priority,
        });
    }

    /// Build `terms` / `titleterms`: a single doc index becomes an int, several
    /// become a sorted list (upstream `IndexBuilder.get_terms`).
    fn get_terms(
        &self,
        fn2index: &BTreeMap<&str, usize>,
    ) -> (BTreeMap<String, Value>, BTreeMap<String, Value>) {
        fn convert(
            mapping: &BTreeMap<String, BTreeSet<String>>,
            fn2index: &BTreeMap<&str, usize>,
        ) -> BTreeMap<String, Value> {
            let mut out = BTreeMap::new();
            for (term, docs) in mapping {
                if docs.len() == 1 {
                    if let Some(docname) = docs.iter().next() {
                        if let Some(index) = fn2index.get(docname.as_str()) {
                            out.insert(term.clone(), Value::from(*index));
                        }
                    }
                } else {
                    let indices: Vec<Value> = docs
                        .iter()
                        .filter_map(|docname| fn2index.get(docname.as_str()).map(|index| Value::from(*index)))
                        .collect();
                    out.insert(term.clone(), Value::Array(indices));
                }
            }
            out
        }
        (convert(&self.mapping, fn2index), convert(&self.title_mapping, fn2index))
    }

    /// Build the `objects` map plus the `objtypes` / `objnames` lookups, in the
    /// same order and with the same short-anchor rules as upstream.
    #[allow(clippy::type_complexity)]
    fn build_object_data(
        &self,
        fn2index: &BTreeMap<&str, usize>,
    ) -> (
        BTreeMap<String, Vec<Value>>,
        BTreeMap<(String, String), usize>,
        BTreeMap<usize, (String, String, String)>,
    ) {
        let mut sorted: Vec<&DomainObject> = self.objects.iter().collect();
        sorted.sort_by(|a, b| {
            (&a.domain, &a.name, &a.objtype, &a.docname, &a.anchor).cmp(&(
                &b.domain,
                &b.name,
                &b.objtype,
                &b.docname,
                &b.anchor,
            ))
        });

        let mut objects: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut objtypes: BTreeMap<(String, String), usize> = BTreeMap::new();
        let mut objnames: BTreeMap<usize, (String, String, String)> = BTreeMap::new();

        for object in sorted {
            let Some(index) = fn2index.get(object.docname.as_str()) else {
                continue;
            };
            if object.priority < 0 {
                continue;
            }
            let fullname = html_escape(&object.name);
            let display = html_escape(&object.display_name);
            let (prefix, name) = match display.rsplit_once('.') {
                Some((prefix, name)) => (prefix.to_owned(), name.to_owned()),
                None => (String::new(), display.clone()),
            };

            let key = (object.domain.clone(), object.objtype.clone());
            let typeindex = if let Some(existing) = objtypes.get(&key) {
                *existing
            } else {
                let next = objtypes.len();
                objtypes.insert(key.clone(), next);
                objnames.insert(
                    next,
                    (object.domain.clone(), object.objtype.clone(), object.type_name.clone()),
                );
                next
            };

            let shortanchor = if object.anchor == fullname {
                String::new()
            } else if object.anchor == format!("{}-{}", object.objtype, fullname) {
                "-".to_owned()
            } else {
                object.anchor.clone()
            };

            objects.entry(prefix).or_default().push(Value::Array(vec![
                Value::from(*index),
                Value::from(typeindex),
                Value::from(object.priority),
                Value::String(shortanchor),
                Value::String(name),
            ]));
        }
        (objects, objtypes, objnames)
    }

}

/// Extract explicit `.. index::` entries from a doctree as fragment rows.
///
/// This is the shared implementation behind both [`SearchIndex::feed`] (full
/// builds) and [`SearchIndex::build_fragment`] (cached incremental chunks).
fn collect_index_entries_from(docname: &str, document: &Document) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for id in document.preorder_ids() {
        let Ok(node) = document.get(id) else { continue };
        if node.kind != NodeKind::Index {
            continue;
        }
        let raw = node.attrs.get("entries");
        let entries: Vec<String> = match raw {
            Some(scribe_ast::AttributeValue::List(values)) => values.clone(),
            Some(scribe_ast::AttributeValue::Str(text)) => {
                serde_json::from_str(text).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        for entry in entries {
            // Upstream tuple: (entry_type, value, target_id, main, category).
            // scribe stores these as `;`-separated five-column strings.
            let fields: Vec<&str> = entry.split(';').collect();
            if fields.len() < 2 {
                continue;
            }
            let value = fields[1].trim();
            if value.is_empty() {
                continue;
            }
            let entry_id = fields.get(2).map(|s| s.trim()).unwrap_or("");
            let main = fields.get(3).is_some_and(|s| s.trim() == "main");
            out.push((
                value.to_lowercase(),
                entry_id.to_owned(),
                if main { "main" } else { "" }.to_owned(),
            ));
        }
    }
    let _ = docname;
    out
}

/// Recursively collect words and titles from a doctree, honoring upstream's
/// skip rules (comments, `no-search` class, non-HTML raw nodes).
fn collect_words(document: &Document, id: NodeId, store: &mut WordStore) {
    let Ok(node) = document.get(id) else { return };

    if node.kind == NodeKind::Comment {
        return;
    }
    if node.classes().is_ok_and(|classes| classes.iter().any(|class| class == "no-search")) {
        return;
    }
    if node.kind == NodeKind::Raw {
        // Upstream indexes the text of raw HTML nodes after stripping
        // `<style>`, `<script>` and remaining tags.
        let format = node.attrs.get_str("format").ok().flatten().unwrap_or("");
        if format.split_whitespace().any(|part| part == "html") {
            if let Some(text) = node.text_content() {
                let without_style = STYLE_RE.replace_all(text, "");
                let without_script = SCRIPT_RE.replace_all(&without_style, "");
                let plain = TAG_RE.replace_all(&without_script, "");
                store.words.extend(tokenize(&plain));
            }
        }
        return;
    }

    if node.kind == NodeKind::Text {
        if let Some(text) = node.text_content() {
            store.words.extend(tokenize(text));
        }
    } else if node.kind == NodeKind::Title {
        let title = node.astext(document);
        let anchor = if store.titles.is_empty() {
            // Main document title has no anchor upstream.
            None
        } else {
            document
                .parent(id)
                .ok()
                .flatten()
                .and_then(|parent| document.get(parent).ok())
                .and_then(|parent| parent.ids().ok())
                .and_then(|ids| ids.into_iter().next())
                .filter(|id| !id.is_empty())
        };
        store.titles.push((title.clone(), anchor));
        store.title_words.extend(tokenize(&title));
        // Deliberately fall through to the children: upstream's visitor also
        // visits a title's text children as ordinary words. The body/title
        // de-duplication in `feed` prevents double-indexing.
    }

    for &child in &node.children {
        collect_words(document, child, store);
    }
}

/// Python `html.escape(..., quote=True)` for object names, matching upstream
/// `IndexBuilder.get_objects`.
fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Best-effort domain inference for [`SearchIndex::add_object`]. Object type
/// keys are only used to build `objtypes`/`objnames`; callers with a real
/// registry should use [`SearchIndex::add_domain_objects`] instead.
fn infer_domain(objtype: &str) -> &'static str {
    match objtype {
        "module" | "func" | "class" | "meth" | "attr" | "data" | "exc" | "obj" | "attribute"
        | "method" | "function" => "py",
        "function_c" | "var" | "type_c" | "macro" | "member" | "struct" | "union" | "enum"
        | "enumerator" | "typedef" => "c",
        "label" | "term" | "doc" => "std",
        _ => "py",
    }
}

/// Minified Snowball base stemmer, vendored from Sphinx's
/// `sphinx/search/minified-js/`. Embedded into `language_data.js` exactly as
/// upstream does, so `searchtools.js` gets a `window.Stemmer` class.
pub const BASE_STEMMER_JS: &str = include_str!("html/js/base-stemmer.js");
/// Minified Snowball English stemmer, vendored from Sphinx's
/// `sphinx/search/minified-js/`.
pub const ENGLISH_STEMMER_JS: &str = include_str!("html/js/english-stemmer.js");

/// Identity stemmer stub used for languages without a vendored JS stemmer.
/// This mirrors Sphinx's `SearchLanguage.js_stemmer_code` default.
pub const IDENTITY_STEMMER_JS: &str = "\
var Stemmer = function () {\n\
  this.stemWord = function (w) {\n\
    return w;\n\
  };\n\
};\n";

/// The `language_data.js` `search_language_stemming_code` value for `language`.
///
/// English (and `zh`, which upstream also serves with the English stemmer)
/// gets the real minified Snowball code; every other language degrades to the
/// identity stemmer, which is still structurally valid for `searchtools.js`.
#[must_use]
pub fn search_language_stemming_code(language: &str) -> String {
    let lang = language.split('_').next().unwrap_or(language).to_lowercase();
    if lang == "en" || lang == "zh" {
        format!("{BASE_STEMMER_JS}\n{ENGLISH_STEMMER_JS}\nwindow.Stemmer = EnglishStemmer;")
    } else {
        IDENTITY_STEMMER_JS.to_owned()
    }
}

/// The `language_data.js` `search_language_stop_words` value for `language`,
/// as a sorted JSON array string (matching upstream's `json.dumps(sorted(...))`).
#[must_use]
pub fn search_language_stop_words_json(language: &str) -> String {
    let mut words: Vec<&str> = stopwords_for(language).to_vec();
    words.sort_unstable();
    serde_json::to_string(&words).unwrap_or_else(|_| "[]".to_owned())
}

/// Parse a `Search.setIndex(...)` payload into a JSON value.
///
/// Used by the differential tests (and available to downstream tooling); it
/// tolerates both the oracle's `...)` form and a `...);` form.
#[must_use]
pub fn parse_js_index(source: &str) -> Option<Value> {
    let body = source.strip_prefix(JS_INDEX_PREFIX)?;
    let body = body.strip_suffix(JS_INDEX_SUFFIX)?;
    serde_json::from_str(body).ok()
}

#[cfg(test)]
mod fragment_tests {
    use super::{scribe_envversion, SearchIndex, SearchIndexFragment};
    use scribe_ast::{Document, NodeKind};

    fn doc(title: &str, section: &str, body: &str) -> Document {
        let mut document = Document::new(Some("doc".to_owned()));
        let main = document.add(NodeKind::Title, Some(document.root)).expect("title");
        document.new_text(title, Some(main)).expect("title text");
        let section_id = document.add(NodeKind::Section, Some(document.root)).expect("section");
        if let Ok(node) = document.get_mut(section_id) {
            node.attrs.set_list("ids", vec!["sec-1".to_owned()]);
        }
        let sec_title = document.add(NodeKind::Title, Some(section_id)).expect("section title");
        document.new_text(section, Some(sec_title)).expect("section title text");
        let para = document.add(NodeKind::Paragraph, Some(section_id)).expect("paragraph");
        document.new_text(body, Some(para)).expect("body text");
        document.metadata.title = Some(title.to_owned());
        document
    }

    #[test]
    fn fragment_merge_is_byte_identical_to_direct_feed() {
        let documents: Vec<(String, Document)> = vec![
            ("alpha".to_owned(), doc("Alpha", "Intro", "the quick brown fox jumps")),
            ("beta".to_owned(), doc("Beta", "Details", "alpha beta gamma delta")),
            ("gamma".to_owned(), doc("Gamma", "More", "repeated repeated words words")),
        ];

        // Reference: the original inline feed path.
        let mut reference = SearchIndex::new("en", scribe_envversion());
        for (name, document) in &documents {
            let title = document.metadata.title.clone().unwrap_or_default();
            reference.feed_with_filename(name, &format!("{name}.rst"), &title, document);
        }

        // Phase 67: precomputed fragments merged in docname order.
        let fragments: Vec<SearchIndexFragment> = documents
            .iter()
            .map(|(name, document)| {
                let title = document.metadata.title.clone().unwrap_or_default();
                SearchIndex::build_fragment(
                    name,
                    &format!("{name}.rst"),
                    &title,
                    document,
                    "en",
                )
            })
            .collect();
        let mut merged = SearchIndex::new("en", scribe_envversion());
        for fragment in fragments {
            merged.apply_fragment(fragment);
        }

        assert_eq!(merged.to_js(), reference.to_js());
    }
}
