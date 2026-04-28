//! `mtgish-diff` — structural-diff measurement tool.
//!
//! Loads two card-data JSON files (the native `oracle_nom` parser output
//! and the mtgish-import output), canonicalizes both, and emits a
//! divergence report classifying each disagreement.
//!
//! Usage:
//! ```text
//!   mtgish-diff <native_card_data.json> <mtgish_card_data.json> [report_path]
//! ```
//!
//! Both inputs are expected to be top-level JSON objects keyed by card
//! name. Cards present in only one side are reported as "card-missing"
//! rather than diffed (no canonical pairing exists).
//!
//! Output JSON shape:
//! ```json
//! {
//!   "cards_compared": N,
//!   "cards_match": N,
//!   "cards_diverge": N,
//!   "cards_native_only": N,
//!   "cards_mtgish_only": N,
//!   "by_severity": { "SemanticDivergence": N, "ScopeDivergence": N, "Cosmetic": N },
//!   "top_divergences": [
//!     { "card_name": "...", "path": "...", "severity": "...", "native": ..., "mtgish": ... }
//!   ]
//! }
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use mtgish_import::diff::{canonicalize, classify_value, Severity};
use serde_json::{json, Value};

const TOP_DIVERGENCES_LIMIT: usize = 50;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: {} <native_card_data.json> <mtgish_card_data.json> [report_path]",
            args.first().map(String::as_str).unwrap_or("mtgish-diff")
        );
        return ExitCode::from(2);
    }

    let native_path = &args[1];
    let mtgish_path = &args[2];
    let report_path = args.get(3).cloned();

    let report = match run(native_path, mtgish_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mtgish-diff: {e}");
            return ExitCode::from(1);
        }
    };

    let serialized = serde_json::to_string_pretty(&report).expect("report serializes");
    match report_path {
        Some(path) => {
            if let Err(e) = fs::write(&path, &serialized) {
                eprintln!("mtgish-diff: failed to write {path}: {e}");
                return ExitCode::from(1);
            }
            println!("wrote diff report → {path}");
        }
        None => {
            println!("{serialized}");
        }
    }
    ExitCode::SUCCESS
}

fn run(native_path: &str, mtgish_path: &str) -> anyhow::Result<Value> {
    let native = load_card_map(native_path)?;
    let mtgish = load_card_map(mtgish_path)?;

    let mut cards_compared = 0usize;
    let mut cards_match = 0usize;
    let mut cards_diverge = 0usize;
    let mut by_severity: BTreeMap<&'static str, usize> = BTreeMap::new();
    by_severity.insert("SemanticDivergence", 0);
    by_severity.insert("ScopeDivergence", 0);
    by_severity.insert("Cosmetic", 0);

    let mut top_divergences: Vec<Value> = Vec::new();

    let native_keys: std::collections::BTreeSet<&String> = native.keys().collect();
    let mtgish_keys: std::collections::BTreeSet<&String> = mtgish.keys().collect();

    let cards_native_only = native_keys.difference(&mtgish_keys).count();
    let cards_mtgish_only = mtgish_keys.difference(&native_keys).count();

    for name in native_keys.intersection(&mtgish_keys) {
        cards_compared += 1;
        let n = canonicalize(native[name.as_str()].clone());
        let m = canonicalize(mtgish[name.as_str()].clone());
        if n == m {
            cards_match += 1;
            continue;
        }
        cards_diverge += 1;
        let divs = classify_value(&n, &m);
        for d in divs {
            *by_severity.entry(severity_name(d.severity)).or_insert(0) += 1;
            if top_divergences.len() < TOP_DIVERGENCES_LIMIT {
                top_divergences.push(json!({
                    "card_name": name,
                    "path": d.path,
                    "severity": severity_name(d.severity),
                    "native": d.native,
                    "mtgish": d.mtgish,
                }));
            }
        }
    }

    Ok(json!({
        "cards_compared": cards_compared,
        "cards_match": cards_match,
        "cards_diverge": cards_diverge,
        "cards_native_only": cards_native_only,
        "cards_mtgish_only": cards_mtgish_only,
        "by_severity": by_severity,
        "top_divergences": top_divergences,
    }))
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::SemanticDivergence => "SemanticDivergence",
        Severity::ScopeDivergence => "ScopeDivergence",
        Severity::Cosmetic => "Cosmetic",
    }
}

fn load_card_map(path: &str) -> anyhow::Result<serde_json::Map<String, Value>> {
    let p = Path::new(path);
    let text = fs::read_to_string(p)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", p.display()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", p.display()))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => anyhow::bail!(
            "{} must be a top-level JSON object keyed by card name",
            p.display()
        ),
    }
}
