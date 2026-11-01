//! Guard: the engine's integration tests compile into a fixed set of shard
//! targets, and every test file in `tests/integration/` runs in exactly one.
//!
//! Cargo autotest discovery compiles every `tests/*.rs` file into its own
//! test binary, and each such binary statically links the full engine.
//! At one point 206 files had accumulated there, adding ~30 minutes of link
//! time to every engine change. Integration tests instead live in
//! `tests/integration/` and are registered with a `mod` line in one shard
//! root: `tests/integration/main.rs` (the auto-discovered `integration`
//! target) or one of the `[[test]]` roots `crates/engine/Cargo.toml` declares
//! in that directory. nextest still runs every test in its own process, so
//! the shard boundary adds no isolation.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Modules several shard roots may declare (as `pub mod`). Each defines no
/// test, so declaring it in more than one root runs nothing twice.
const SHARED_HELPERS: &[&str] = &["support.rs"];

fn engine_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn tests_dir_has_no_top_level_rs_files() {
    let tests_dir = engine_dir().join("tests");
    let mut offenders: Vec<String> = std::fs::read_dir(&tests_dir)
        .expect("read crates/engine/tests/")
        .filter_map(|entry| {
            let entry = entry.expect("read dir entry");
            let path = entry.path();
            (path.is_file() && path.extension().is_some_and(|ext| ext == "rs"))
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect();
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "Found top-level test file(s) in crates/engine/tests/: {offenders:?}.\n\
         Each becomes its own test binary linking the full engine. Move the file \
         into crates/engine/tests/integration/ and add a `mod` line to \
         tests/integration/main.rs (or to another shard root) instead."
    );
}

/// The shard roots, relative to `tests/integration/`: `main.rs`, which autotest
/// discovery builds as the `integration` target, plus every `[[test]]` path the
/// manifest declares in that directory. Panics if `autotests` is turned off,
/// because `main.rs` would then compile into nothing while every other shard
/// stays green.
fn shard_roots(manifest: &str) -> Vec<String> {
    let manifest: toml::Table = manifest.parse().expect("crates/engine/Cargo.toml parses");
    let autotests = manifest
        .get("package")
        .and_then(|package| package.get("autotests"))
        .and_then(toml::Value::as_bool);
    assert_ne!(
        autotests,
        Some(false),
        "crates/engine/Cargo.toml sets `autotests = false`, so tests/integration/main.rs \
         is no longer built as the `integration` shard"
    );
    let mut roots = vec!["main.rs".to_string()];
    for target in manifest
        .get("test")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(root) = target
            .get("path")
            .and_then(toml::Value::as_str)
            .and_then(|path| path.strip_prefix("tests/integration/"))
        {
            roots.push(root.to_string());
        }
    }
    roots
}

/// The files a shard root registers, relative to `tests/integration/`: one per
/// `mod x;` line (`x.rs`), or the `#[path = "…"]` value directly above it.
fn registrations(root_src: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut path_attr: Option<&str> = None;
    for line in root_src.lines().map(str::trim) {
        if let Some(path) = line
            .strip_prefix("#[path = \"")
            .and_then(|rest| rest.strip_suffix("\"]"))
        {
            path_attr = Some(path);
            continue;
        }
        let declaration = line.strip_prefix("pub ").unwrap_or(line);
        if let Some(name) = declaration
            .strip_prefix("mod ")
            .and_then(|rest| rest.strip_suffix(';'))
        {
            files.push(path_attr.map_or_else(|| format!("{name}.rs"), str::to_string));
        }
        if !line.starts_with("#[") {
            path_attr = None;
        }
    }
    files
}

/// Every way the registration rule can be broken. `roots` maps a root file to
/// its source; `files` maps every other `.rs` file in `tests/integration/` to
/// its source.
fn registration_offenders(
    roots: &BTreeMap<String, String>,
    files: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut registered_in: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (root, src) in roots {
        for file in registrations(src) {
            registered_in.entry(file).or_default().push(root);
        }
    }
    let mut offenders: Vec<String> = files
        .keys()
        .filter(|file| !registered_in.contains_key(*file))
        .map(|file| format!("{file}: registered in no shard root, so it is never compiled or run"))
        .collect();
    for (file, roots_of) in &registered_in {
        if roots_of.len() > 1 && !SHARED_HELPERS.contains(&file.as_str()) {
            offenders.push(format!(
                "{file}: registered in {roots_of:?}, so its tests run more than once"
            ));
        }
    }
    for helper in SHARED_HELPERS {
        match files.get(*helper) {
            None => offenders.push(format!("{helper}: declared shared helper has no file")),
            Some(src) if src.contains("#[test]") => offenders.push(format!(
                "{helper}: a shared helper defines a test, which would run in every root that declares it"
            )),
            Some(_) => {}
        }
    }
    offenders
}

#[test]
fn every_integration_file_is_registered_in_exactly_one_shard_root() {
    let dir = engine_dir().join("tests").join("integration");
    let manifest = std::fs::read_to_string(engine_dir().join("Cargo.toml"))
        .expect("read crates/engine/Cargo.toml");
    let read = |name: &str| {
        let path = dir.join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
    };
    let roots: BTreeMap<String, String> = shard_roots(&manifest)
        .into_iter()
        .map(|root| (root.clone(), read(&root)))
        .collect();
    let files: BTreeMap<String, String> = std::fs::read_dir(&dir)
        .expect("read crates/engine/tests/integration/")
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "rs"))
        .map(|path| {
            path.file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| !roots.contains_key(name))
        .map(|name| {
            let src = read(&name);
            (name, src)
        })
        .collect();
    assert!(
        files.len() > 1000,
        "census reached {} integration files",
        files.len()
    );
    let offenders = registration_offenders(&roots, &files);
    assert!(
        offenders.is_empty(),
        "integration test registration is broken (shard roots: {:?}):\n{}",
        roots.keys().collect::<Vec<_>>(),
        offenders.join("\n")
    );
}

#[test]
fn the_registration_census_flags_every_offense_class() {
    let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let roots = map(&[
        ("main.rs", "#[path = \"../../src/x.rs\"]\nmod x;\npub mod support;\nmod a;\nmod twice;"),
        ("shard_2.rs", "pub mod support;\n#[cfg(feature = \"f\")]\nmod b;\nmod twice;\n#[path = \"../../src/x.rs\"]\n#[cfg(test)]\nmod x;"),
    ]);
    let clean = map(&[("support.rs", "pub fn s() {}"), ("a.rs", ""), ("b.rs", "")]);
    assert_eq!(
        registration_offenders(
            &map(&[
                ("main.rs", "pub mod support;\nmod a;"),
                ("shard_2.rs", "pub mod support;\nmod b;")
            ]),
            &clean
        ),
        Vec::<String>::new(),
        "positive control: one registration per file, the shared helper in both roots"
    );
    let mut files = clean.clone();
    files.insert("twice.rs".into(), String::new());
    files.insert("orphan.rs".into(), String::new());
    files.insert("support.rs".into(), "#[test]\nfn t() {}".into());
    let offenders = registration_offenders(&roots, &files);
    for needle in [
        "orphan.rs: registered in no shard root",
        "twice.rs: registered in [\"main.rs\", \"shard_2.rs\"]",
        "../../src/x.rs: registered in [\"main.rs\", \"shard_2.rs\"]",
        "support.rs: a shared helper defines a test",
    ] {
        assert!(
            offenders.iter().any(|o| o.starts_with(needle)),
            "missing `{needle}` in {offenders:?}"
        );
    }
    assert_eq!(offenders.len(), 4, "{offenders:?}");
}
