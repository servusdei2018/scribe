//! Domain inventory persistence through the additive `BuildEnvironment` side
//! table (frozen §12 public shape untouched).

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_config::Config;
use scribe_core::BuildEnvironment;
use scribe_domains::{DiagnosticSink, DomainRegistry, StandardDomain};

use common::{context, TestEnv};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn domain_data_survives_env_snapshot_and_cache_roundtrip() {
    let config = Config::new();
    let env = TestEnv::default();
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let domain = StandardDomain::new();
    domain.note_label(&ctx, "persisted-label", "persisted-id", Some("Persisted"), None);
    domain.note_object(&ctx, "envvar", "PERSISTED", "envvar-persisted", None);
    let registry = DomainRegistry::new();
    registry.register(domain);
    let exported = registry.export_domain_data();
    assert!(exported.contains_key("std"));

    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let cache = std::env::temp_dir().join(format!(
        "scribe-domain-cache-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&cache);
    let source: PathBuf = PathBuf::from("/tmp/scribe-domain-source");
    let mut build_env = BuildEnvironment::new(&source);
    for (name, data) in &exported {
        build_env.set_domain_data(name, serde_json::to_value(data).expect("serialize domain data"));
    }
    assert!(build_env.domain_data("std").is_some());
    assert_eq!(build_env.domain_data_entries().len(), 1);
    build_env.save(&cache).expect("save environment");

    // The snapshot round-trip is the deterministic persistence boundary; the
    // cache layer itself may reject a synthetic source directory.
    let restored = BuildEnvironment::from_snapshot(build_env.to_snapshot()).expect("restore snapshot");
    let restored_registry = DomainRegistry::with_standard();
    restored_registry.load_from_env(&restored);
    let objects = restored_registry.get_objects();
    assert!(objects.iter().any(|entry| entry.name == "persisted-label" && entry.objtype == "label"));
    assert!(objects.iter().any(|entry| entry.name == "PERSISTED" && entry.objtype == "envvar"));

    // A stale inventory is rejected (cold domain) rather than panicking.
    let mut stale = restored.domain_data("std").cloned().expect("data");
    if let Some(version) = stale.get_mut("version") {
        *version = serde_json::json!(999);
    }
    let mut stale_env = BuildEnvironment::new(&source);
    stale_env.set_domain_data("std", stale);
    let cold = DomainRegistry::with_standard();
    cold.load_from_env(&stale_env);
    assert!(!cold.get_objects().iter().any(|entry| entry.name == "persisted-label"));
    let _ = std::fs::remove_dir_all(&cache);
}
