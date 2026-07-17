//! Architectural lint: new tactical policies must use the scoring contract.
//!
//! The current policy corpus still contains direct `PolicyVerdict::Score`
//! construction from before the band-helper contract existed. Those production
//! sites are counted in `LEGACY_SCORE_LITERAL_COUNTS`; new sites in old or new
//! files fail this lint.

use std::fs;
use std::path::Path;

const LEGACY_SCORE_LITERAL_COUNTS: &[(&str, usize)] = &[
    ("aggro_pressure.rs", 10),
    ("anthem_priority.rs", 5),
    ("board_development.rs", 1),
    ("board_wipe_telegraph.rs", 1),
    ("card_advantage.rs", 1),
    ("chalice_avoidance.rs", 1),
    ("combat_tax.rs", 3),
    ("combo_line.rs", 3),
    ("condition_gated_activation.rs", 1),
    ("control_change_awareness.rs", 6),
    ("effect_timing.rs", 1),
    ("equipment_priority.rs", 1),
    ("etb_value.rs", 1),
    ("evasion_removal_priority.rs", 0),
    ("free_outlet_activation.rs", 8),
    ("hand_disruption.rs", 1),
    ("hold_mana_up.rs", 3),
    ("interaction_reservation.rs", 1),
    ("land_animation.rs", 6),
    ("land_sequencing.rs", 1),
    ("landfall_timing.rs", 5),
    ("lethality_awareness.rs", 1),
    ("life_total_resource.rs", 1),
    ("mana_efficiency.rs", 1),
    ("mill_targeting.rs", 4),
    ("planeswalker_loyalty.rs", 2),
    ("plus_one_counters.rs", 10),
    ("ramp_timing.rs", 4),
    ("reactive_self_protection.rs", 0),
    ("recursion_awareness.rs", 1),
    ("redundancy_avoidance.rs", 1),
    ("spellskite_priority.rs", 1),
    ("spellslinger_casting.rs", 4),
    ("stack_awareness.rs", 1),
    ("sweeper_timing.rs", 3),
    ("synergy_casting.rs", 1),
    ("tempo_curve.rs", 1),
    ("tokens_wide.rs", 7),
    ("tribal_lord_priority.rs", 7),
    ("tutor.rs", 1),
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
        let production = production_source(&contents);
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
    // `score` is the lowercase dispatcher `PolicyVerdict::score(delta, reason)`;
    // it must receive a computed (config-routed) delta, never a numeric literal —
    // otherwise a raw magnitude evades the band-helper contract that the four
    // banded helpers already enforce.
    ["score", "nudge", "preference", "strong", "critical"]
        .iter()
        .any(|helper| {
            let needle = format!("PolicyVerdict::{helper}(");
            code.find(&needle).is_some_and(|start| {
                let rest = code[start + needle.len()..].trim_start();
                rest.starts_with(|ch: char| ch.is_ascii_digit() || ch == '-' || ch == '.')
            })
        })
}

/// Guards the loophole this lint closes: a numeric-literal first argument to
/// `PolicyVerdict::score(...)` must be flagged, while a computed first argument
/// must not. Both the `-8.0` (leading `-`) and `8.0` (leading digit) shapes,
/// plus a `.5`-style leading dot, are covered.
#[test]
fn score_dispatcher_literal_is_flagged() {
    assert!(band_helper_uses_numeric_literal(
        "        return PolicyVerdict::score(-8.0, reason);"
    ));
    assert!(band_helper_uses_numeric_literal(
        "        return PolicyVerdict::score(8.0, reason);"
    ));
    assert!(band_helper_uses_numeric_literal(
        "        PolicyVerdict::score(.5, reason)"
    ));
    // Computed / config-routed deltas must pass.
    assert!(!band_helper_uses_numeric_literal(
        "        PolicyVerdict::score(self.score(ctx).clamp(-15.0, 15.0), reason)"
    ));
    assert!(!band_helper_uses_numeric_literal(
        "        return PolicyVerdict::score(delta, reason);"
    ));
    assert!(!band_helper_uses_numeric_literal(
        "        PolicyVerdict::score(ctx.penalties().mill_cast_bonus * urgency, reason)"
    ));
}

fn legacy_score_literal_count(file_name: &str) -> usize {
    LEGACY_SCORE_LITERAL_COUNTS
        .iter()
        .find_map(|(name, count)| (*name == file_name).then_some(*count))
        .unwrap_or(0)
}

/// Returns the production portion of a policy source file: everything before the
/// `#[cfg(test)] mod ...` test module.
///
/// A naive `split("#[cfg(test)]").next()` truncates at the FIRST `#[cfg(test)]`
/// attribute — but many policy files carry a test-only `use` import
/// (`#[cfg(test)] use ...CastPaymentMode;`) in their top import block. Splitting
/// there hid the entire impl below it, so real production emit sites evaded this
/// lint (issue #5473: `redundancy_avoidance` / `downside_awareness` shipped raw
/// out-of-band `Score` literals undetected). The module boundary is the only
/// correct truncation point: a `#[cfg(test)]` line whose next non-blank line
/// opens a `mod`. Attributes on individual test-only items are skipped.
fn production_source(contents: &str) -> &str {
    for (idx, _) in contents.match_indices("#[cfg(test)]") {
        // Consider only a `#[cfg(test)]` that begins its line (column 0), i.e. a
        // top-level attribute — the test module. Attributes on indented items or
        // occurrences inside strings are not module boundaries.
        let at_line_start = idx == 0 || contents[..idx].ends_with('\n');
        if !at_line_start {
            continue;
        }
        // Skip to the end of the `#[cfg(test)]` line first, so a trailing comment
        // on that line (`#[cfg(test)] // note`) can't be mistaken for the next
        // item. The module boundary is the first non-blank line *after* it.
        let line_end = contents[idx..]
            .find('\n')
            .map_or(contents.len(), |i| idx + i);
        let after_line = &contents[line_end..];
        let next_non_blank = after_line
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        if next_non_blank.trim_start().starts_with("mod ") {
            return &contents[..idx];
        }
    }
    contents
}

/// A test-only `use` (or any `#[cfg(test)]` item that is not the test module)
/// must NOT truncate the production scan; only `#[cfg(test)] mod ...` does. This
/// guards the blind spot that hid issue #5473's offenders.
#[test]
fn production_source_stops_only_at_the_test_module() {
    let src = "\
use foo::Bar;
#[cfg(test)]
use foo::TestOnly;

fn verdict() -> u8 {
    PolicyVerdict::Score { delta: 0 }
}

#[cfg(test)]
mod tests {
    fn t() { PolicyVerdict::Score { delta: 9 } }
}
";
    let production = production_source(src);
    assert_eq!(
        production.matches("PolicyVerdict::Score {").count(),
        1,
        "the production `verdict` emit must be visible; the test-module emit must not"
    );
    assert!(
        production.contains("fn verdict"),
        "the impl below a test-only `use` must remain in the production slice"
    );
    assert!(
        !production.contains("mod tests"),
        "the test module must be excluded"
    );
}

/// A trailing comment on the `#[cfg(test)]` module attribute must not fool the
/// boundary scan (the scan starts on the *next* line). Guards the gemini review
/// nit on #5478 — the boundary is still detected and the module still excluded.
#[test]
fn production_source_ignores_trailing_comment_on_cfg_test_line() {
    let src = "\
fn verdict() -> u8 {
    PolicyVerdict::Score { delta: 0 }
}

#[cfg(test)] // module below
mod tests {
    fn t() { PolicyVerdict::Score { delta: 9 } }
}
";
    let production = production_source(src);
    assert_eq!(
        production.matches("PolicyVerdict::Score {").count(),
        1,
        "the test module must still be excluded despite the trailing comment"
    );
    assert!(!production.contains("mod tests"));
}
