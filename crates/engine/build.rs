//! Converts `data/known-tokens.toml` (the human-reviewable catalog generated
//! by the `tokens-gen` bin) into JSON at build time for the runtime embed in
//! `game/token_presets.rs`.
//!
//! Why: parsing the ~5.9MB catalog with `toml`/`toml_edit` costs ~1s per
//! process in debug builds, and nextest runs every test in its own process —
//! so every token-touching test (any Oracle text naming a Treasure/Food/named
//! token) paid the full parse. `serde_json::from_slice` of the pre-converted
//! JSON is several times faster, and this conversion runs only when the
//! catalog file itself changes.
//!
//! The conversion is purely structural (`toml::Value` → `serde_json::Value`),
//! so it cannot drift from the typed `CatalogFile` schema: the same serde
//! shape that used to decode the TOML decodes the JSON.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // Explicit rerun-if-changed replaces cargo's rerun-on-any-source default,
    // so unrelated engine edits never rerun this script.
    println!("cargo::rerun-if-changed=data/known-tokens.toml");
    println!("cargo::rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let raw = fs::read_to_string(manifest_dir.join("data/known-tokens.toml"))
        .expect("read crates/engine/data/known-tokens.toml");
    let value: toml::Value = toml::from_str(&raw).expect("known-tokens.toml well-formed");
    // Bare TOML datetimes serialize through serde as a private wrapper struct
    // that would corrupt the JSON round-trip. tokens-gen only emits quoted
    // strings, so reject any future drift here rather than mis-decoding later.
    assert_no_bare_datetimes(&value);

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("known-tokens.json");
    let json = serde_json::to_vec(&value).expect("known-tokens.toml converts to JSON");
    fs::write(&out, json).expect("write known-tokens.json to OUT_DIR");
}

fn assert_no_bare_datetimes(value: &toml::Value) {
    match value {
        toml::Value::Datetime(dt) => panic!(
            "known-tokens.toml contains a bare TOML datetime ({dt}); quote it as a \
             string in tokens-gen — bare datetimes do not survive JSON conversion"
        ),
        toml::Value::Array(items) => items.iter().for_each(assert_no_bare_datetimes),
        toml::Value::Table(table) => table.values().for_each(assert_no_bare_datetimes),
        _ => {}
    }
}
