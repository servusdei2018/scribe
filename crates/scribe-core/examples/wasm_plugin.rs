//! Example third-party Wasm plugin source (Phase 27 ABI v1).
//!
//! This file documents, in Rust/C-compatible terms, the minimum a plugin must
//! export to interoperate with `scribe_core::plugin::WasmPluginHost`. It is a
//! compilable Rust example (run `cargo run -p scribe-core --example wasm_plugin`)
//! that shows the *shape* of a plugin; cross-compiling to `wasm32-unknown-unknown`
//! and wiring the real host functions is a documented future backend (see
//! `ARCHITECTURE.md` §33).
//!
//! The on-wire convention for every `(ptr, len)` string is UTF-8 JSON. A
//! callback returns a packed `i64` = `(pointer as u64) << 32 | length`, or `0`
//! to signal "None" (upstream's `None` return). Host functions use the same
//! packing for returned strings.

/// Packed `(pointer, length)` JSON slice — `0` means "None".
pub type PackedSlice = i64;

/// Encode a `(pointer, length)` pair for return to the host.
#[must_use]
pub const fn pack(ptr: u32, len: u32) -> PackedSlice {
    (((ptr as u64) << 32) | len as u64) as i64
}

/// Decode a packed slice received from the host.
#[must_use]
pub const fn unpack(value: PackedSlice) -> (u32, u32) {
    (((value as u64) >> 32) as u32, value as u32)
}

/// Host function imports (module name `scribe`). A real plugin declares these
/// with `extern "C"` and `#[link(wasm_import_module = "scribe")]`.
pub const HOST_FUNCTIONS: &[&str] = &[
    "scribe.log",
    "scribe.diagnostic",
    "scribe.config_get",
    "scribe.ast_get",
    "scribe.add_directive",
    "scribe.add_role",
    "scribe.connect_event",
];

/// Manifest that must be embedded in the `scribe.plugin` custom section.
pub const MANIFEST: &str = r#"{
  "abi_version": 1,
  "name": "example_echo",
  "version": "0.1.0",
  "description": "Appends a marker to every source document",
  "capabilities": { "events": ["source-read"], "directives": false },
  "callbacks": ["on_source_read"]
}"#;

/// `on_source_read(app_json, docname, source) -> { "source": ... }`.
///
/// In a real Wasm build this is `#[no_mangle] pub extern "C" fn on_source_read(ptr: i32, len: i32) -> i64`.
#[must_use]
pub fn on_source_read(source: &str) -> String {
    format!("{source}\n\n.. note:: Processed by example_echo plugin")
}

fn main() {
    println!("scribe Wasm plugin ABI v1");
    println!("manifest: {MANIFEST}");
    println!("host functions: {}", HOST_FUNCTIONS.join(", "));
    println!("pack/unpack round-trip: {:?}", unpack(pack(64, 5)));
    println!("example source-read result:\n{}", on_source_read("Hello"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_slices_roundtrip() {
        assert_eq!(unpack(pack(1024, 42)), (1024, 42));
        let manifest: serde_json::Value = serde_json::from_str(MANIFEST).expect("valid manifest");
        assert_eq!(manifest["abi_version"], 1);
        assert_eq!(manifest["capabilities"]["events"][0], "source-read");
    }
}
