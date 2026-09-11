//! CR 100.5 / CR 903.5a: a persisted `format_config.deck_size` must be the rule its own
//! format defines — CR 100.5 for the minimum-deck-size axis, CR 903.5a for Commander's
//! "the minimum deck size and the maximum deck size are both 100".
//!
//! `scripts/migrate-dump-fixture.sh` takes that variant as an operator argument
//! (`--deck-size <Minimum|Exactly>:<count>`) and validates its SHAPE only, so a typo
//! produces a well-formed artifact the bash gate happily emits. This row is what holds
//! the operator to the argument, the way the row beside `load_dellian_dump` holds one to
//! `--effect-kind`.
//!
//! Two independent legs, because neither subsumes the other. The decode leg is the
//! authority for a tag naming no live variant and for a `Custom` payload, whose runtime
//! fields `FormatConfig`'s `Deserialize` re-derives from `custom_rules` and demands
//! equality of. The comparison leg is the authority for a well-formed variant the
//! declared format does not define, which decodes cleanly. Deliberately not written
//! against `DeckSizeRule::min_cards`: that accessor returns the payload for both
//! variants, so it cannot tell `Minimum(100)` from `Exactly(100)` — the exact
//! discrimination this row exists to make.

use std::path::{Path, PathBuf};

use engine::types::format::{FormatConfig, GameFormat};

fn gunzip(path: &Path) -> String {
    use std::io::Read;

    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let mut json = String::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_string(&mut json)
        .unwrap_or_else(|error| panic!("{} must inflate to UTF-8 JSON: {error}", path.display()));
    json
}

fn collect_gz(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("read dir entry").path();
        if path.is_dir() {
            collect_gz(&path, out);
        } else if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".json.gz"))
        {
            out.push(path);
        }
    }
}

/// Every object reached at a key named `format_config`, paired with its JSON path so a
/// failure names the offending site rather than only the file.
fn collect_format_configs(
    value: &serde_json::Value,
    path: &mut Vec<String>,
    out: &mut Vec<(String, serde_json::Value)>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                path.push(key.clone());
                if key == "format_config" {
                    out.push((path.join("."), child.clone()));
                }
                collect_format_configs(child, path, out);
                path.pop();
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(index.to_string());
                collect_format_configs(child, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

#[test]
fn every_persisted_deck_size_is_the_rule_its_format_defines() {
    // `crates/` rather than the engine's own fixture directory: `regenerate` in
    // `migrate-dump-fixture.sh` does `mkdir -p "$(dirname "$dest")"`, so `--out` reaches
    // every gzipped fixture directory in the workspace, and `git ls-files '*.json.gz'`
    // shows all of them live under this root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut fixtures = Vec::new();
    collect_gz(&root, &mut fixtures);
    fixtures.sort();

    let mut compared = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for fixture in &fixtures {
        let value: serde_json::Value = serde_json::from_str(&gunzip(fixture))
            .unwrap_or_else(|error| panic!("{} parses as JSON: {error}", fixture.display()));
        let mut configs = Vec::new();
        collect_format_configs(&value, &mut Vec::new(), &mut configs);

        let name = fixture.strip_prefix(&root).unwrap_or(fixture).display();
        for (at, object) in configs {
            let config = match serde_json::from_value::<FormatConfig>(object) {
                Ok(config) => config,
                Err(error) => {
                    failures.push(format!(
                        "{name} at {at}: does not decode through FormatConfig: {error}"
                    ));
                    continue;
                }
            };
            // A `Custom` config's authority is `for_custom_rules`, which the decode above
            // already applied; `for_format` cannot answer for it at all.
            if matches!(config.format, GameFormat::Custom(_)) {
                continue;
            }
            let expected = match FormatConfig::for_format(config.format) {
                Ok(expected) => expected,
                Err(error) => {
                    failures.push(format!(
                        "{name} at {at}: {} has no config: {error}",
                        config.format
                    ));
                    continue;
                }
            };
            compared += 1;
            if config.deck_size != expected.deck_size {
                failures.push(format!(
                    "{name} at {at}: persisted {:?}, but {} defines {:?}",
                    config.deck_size, config.format, expected.deck_size
                ));
            }
        }
    }

    assert!(
        compared > 0,
        "reach-guard: the walk of {} compared no deck_size at all, so an empty verdict \
         below would be a walk that visited nothing rather than a corpus that conforms",
        root.display()
    );
    assert!(
        failures.is_empty(),
        "persisted deck_size disagrees with the format's own rule ({compared} compared):\n{}",
        failures.join("\n")
    );
}
