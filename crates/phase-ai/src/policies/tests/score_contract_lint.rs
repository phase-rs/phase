//! Architectural lint: new tactical policies must use the scoring contract.
//!
//! The current policy corpus still contains direct `PolicyVerdict::Score`
//! construction from before the band-helper contract existed. Those production
//! sites are counted in `LEGACY_SCORE_LITERAL_COUNTS`; new sites in old or new
//! files fail this lint.

use std::fs;
use std::path::Path;

const LEGACY_SCORE_LITERAL_COUNTS: &[(&str, usize)] = &[
    ("blight_value.rs", 1),
    ("board_development.rs", 1),
    ("chalice_avoidance.rs", 1),
    ("combat_tax.rs", 3),
    ("combo_line.rs", 3),
    ("condition_gated_activation.rs", 1),
    ("control_change_awareness.rs", 7),
    ("copy_value.rs", 1),
    ("equipment_priority.rs", 1),
    ("etb_value.rs", 1),
    ("evasion_removal_priority.rs", 1),
    ("free_outlet_activation.rs", 8),
    ("interaction_reservation.rs", 1),
    ("land_animation.rs", 7),
    ("land_sequencing.rs", 1),
    ("landfall_timing.rs", 5),
    ("mill_targeting.rs", 4),
    ("planeswalker_loyalty.rs", 2),
    ("plus_one_counters.rs", 10),
    ("reactive_self_protection.rs", 0),
    ("recursion_awareness.rs", 1),
    ("sacrifice_value.rs", 1),
    ("spellskite_priority.rs", 1),
    ("stack_awareness.rs", 1),
    ("x_value.rs", 1),
];

const SKIP_FILES: &[&str] = &[
    "activation.rs",
    "context.rs",
    "effect_classify.rs",
    "mod.rs",
    "registry.rs",
    "strategy_helpers.rs",
];

#[test]
fn new_policy_files_use_score_contract_helpers() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/policies"));
    let mut violations = Vec::new();

    for entry in fs::read_dir(root).expect("policies dir").flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if SKIP_FILES.contains(&file_name) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        let production = contents.split("#[cfg(test)]").next().unwrap_or(&contents);
        let direct_score_count = production.matches("PolicyVerdict::Score {").count();
        let allowed_count = legacy_score_literal_count(file_name);
        if direct_score_count != allowed_count {
            violations.push(format!(
                "{}: direct `PolicyVerdict::Score` count changed: found {}, expected {}",
                path.display(),
                direct_score_count,
                allowed_count
            ));
        }
        for (idx, line) in production.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if band_helper_uses_numeric_literal(code) {
                violations.push(format!(
                    "{}:{}: band helper must take a config-routed field, not a numeric literal",
                    path.display(),
                    idx + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "score contract lint violations:\n{}",
        violations.join("\n")
    );
}

fn band_helper_uses_numeric_literal(code: &str) -> bool {
    ["nudge", "preference", "strong", "critical"]
        .iter()
        .any(|helper| {
            let needle = format!("PolicyVerdict::{helper}(");
            code.find(&needle).is_some_and(|start| {
                let rest = code[start + needle.len()..].trim_start();
                rest.starts_with(|ch: char| ch.is_ascii_digit() || ch == '-' || ch == '.')
            })
        })
}

fn legacy_score_literal_count(file_name: &str) -> usize {
    LEGACY_SCORE_LITERAL_COUNTS
        .iter()
        .find_map(|(name, count)| (*name == file_name).then_some(*count))
        .unwrap_or(0)
}
