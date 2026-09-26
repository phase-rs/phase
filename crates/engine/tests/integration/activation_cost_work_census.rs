//! CR 601.2f + CR 601.2h: every production entry that can begin paying an
//! activation's cost, found MECHANICALLY and classified. An activation whose
//! lock waits for its targets carries an `Open` cost carrier until target
//! settlement; each guarded entry refuses one, so a route that reaches cost
//! work unsettled fails closed instead of paying a price that ignores the
//! target-gated modifiers.
//!
//! This census pins the caller sets of the two primitives that pay activation
//! costs. A new caller fails it until someone decides which class it is in:
//! guarded (it can see an `Open` carrier), loyalty (the fast path carries no
//! carrier), or excluded (it can't be target-gated at all).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::source_census;

/// `(file, enclosing fn)` -> the guard site that fn must check, or why it has none.
enum Class {
    Guarded(&'static str),
    /// CR 606.1: the loyalty fast path builds no cost carrier at all.
    LoyaltyNoCarrier,
    /// CR 702.49a: ninjutsu's return is a cost and its effect has no target, so
    /// no target-gated modifier can qualify, and it never carries a snapshot.
    NinjutsuNoTarget,
    /// CR 605.1a: a mana ability doesn't require a target.
    ManaAbilityNoTarget,
}

const PAY_ABILITY_COST: &[(&str, &str, Class)] = &[
    (
        "game/casting.rs",
        "activate_with_cost_carrier",
        Class::Guarded("DirectPay"),
    ),
    (
        "game/casting_costs.rs",
        "finish_selected_return_to_hand_after_automatic",
        Class::Guarded("ReturnAfterAutomatic"),
    ),
    (
        "game/casting_costs.rs",
        "handle_return_to_hand_for_cost",
        Class::Guarded("ReturnToHand"),
    ),
    (
        "game/casting_costs.rs",
        "handle_remove_counter_for_cost",
        Class::Guarded("RemoveCounter"),
    ),
    (
        "game/casting_costs.rs",
        "handle_remove_counter_distribution_for_cost",
        Class::Guarded("RemoveCounterDistribution"),
    ),
    (
        "game/casting_costs.rs",
        "push_activated_ability_to_stack",
        Class::Guarded("PushToStack"),
    ),
    (
        "game/planeswalker.rs",
        "finalize_loyalty_activation",
        Class::LoyaltyNoCarrier,
    ),
    (
        "game/keywords.rs",
        "activate_ninjutsu",
        Class::NinjutsuNoTarget,
    ),
    (
        "game/mana_abilities.rs",
        "pay_mana_ability_cost_component",
        Class::ManaAbilityNoTarget,
    ),
    (
        "game/mana_abilities.rs",
        "pay_mana_ability_cost_with_choices",
        Class::ManaAbilityNoTarget,
    ),
    (
        "game/mana_abilities.rs",
        "pay_mana_ability_cost_with_choices",
        Class::ManaAbilityNoTarget,
    ),
];

const PAY_ABILITY_MANA_WITH_RESUME: &[(&str, &str, Class)] = &[
    (
        "game/casting_costs.rs",
        "finalize_mana_payment_with_resume",
        Class::Guarded("ManaResume"),
    ),
    (
        "game/casting_costs.rs",
        "finalize_mana_payment_with_phyrexian_choices",
        Class::Guarded("PhyrexianResume"),
    ),
];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Byte offset just past the `}` that closes the `{` at `open`, over code with
/// string and char literals skipped.
fn matching_brace(code: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < code.len() {
        match code[i] {
            b'"' => {
                i += 1;
                while i < code.len() && code[i] != b'"' {
                    if code[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' if i + 2 < code.len() && code[i + 2] == b'\'' => i += 2,
            b'\'' if i + 3 < code.len() && code[i + 1] == b'\\' && code[i + 3] == b'\'' => i += 3,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    code.len()
}

/// The production code of one file: comment halves removed
/// (`source_census::code_lines`) and every `#[cfg(test)]` inline module cut
/// out BRACE-MATCHED. Cutting at the first test module instead drops every
/// production caller after a mid-file test module.
fn production_code(src: &str) -> String {
    let code = source_census::code_lines(src);
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut i = 0;
    while let Some(at) = code[i..].find("#[cfg(test)]") {
        let attr = i + at;
        out.push_str(&code[i..attr]);
        let rest = &code[attr..];
        // Skip further attributes, then look at the item the attribute gates.
        let item = rest
            .lines()
            .skip(1)
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("#["))
            .unwrap_or("");
        let is_inline_mod = (item.starts_with("mod ") || item.starts_with("pub(crate) mod "))
            && item.ends_with('{');
        if is_inline_mod {
            let open = attr + rest.find('{').expect("an inline module opens a brace");
            i = matching_brace(bytes, open);
        } else {
            out.push_str("#[cfg(test)]");
            i = attr + "#[cfg(test)]".len();
        }
    }
    out.push_str(&code[i..]);
    out
}

/// Files declared as `#[cfg(test)] #[path = "x.rs"] mod m;` or
/// `#[cfg(test)] mod m;`: test-only, whatever they contain.
fn test_only_files(src_root: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<&str> = text.lines().map(source_census::code).collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim() != "#[cfg(test)]" {
                continue;
            }
            let mut path_attr = None;
            for next in lines.iter().skip(index + 1).map(|l| l.trim()) {
                if let Some(rest) = next.strip_prefix("#[path = \"") {
                    path_attr = rest.strip_suffix("\"]").map(str::to_string);
                    continue;
                }
                if next.starts_with("#[") || next.is_empty() {
                    continue;
                }
                let module = next
                    .strip_prefix("pub(crate) mod ")
                    .or_else(|| next.strip_prefix("mod "))
                    .and_then(|m| m.strip_suffix(';'));
                if let Some(module) = module {
                    let dir = file.parent().unwrap();
                    let stem_dir = if file
                        .file_name()
                        .is_some_and(|n| n == "mod.rs" || n == "lib.rs")
                    {
                        dir.to_path_buf()
                    } else {
                        dir.join(file.file_stem().unwrap())
                    };
                    match &path_attr {
                        Some(path) => out.push(dir.join(path)),
                        None => {
                            out.push(stem_dir.join(format!("{module}.rs")));
                            out.push(dir.join(format!("{module}.rs")));
                        }
                    }
                }
                break;
            }
        }
    }
    let _ = src_root;
    out
}

/// Every production call of `primitive(`, as `(file relative to src, enclosing fn)`.
fn callers_of(primitive: &str) -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    files.sort();
    assert!(files.len() > 100, "reach guard: the walk found the crate");
    let test_only = test_only_files(&root, &files);
    let mut callers = Vec::new();
    for file in &files {
        if test_only.iter().any(|t| t == file) {
            continue;
        }
        let text = std::fs::read_to_string(file).unwrap();
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        callers.extend(
            calls_in(&production_code(&text), primitive)
                .into_iter()
                .map(|f| (rel.clone(), f)),
        );
    }
    callers
}

/// Each call of `primitive(` in `code` that isn't its definition, named by the
/// nearest preceding `fn`.
fn calls_in(code: &str, primitive: &str) -> Vec<String> {
    let needle = format!("{primitive}(");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = code[from..].find(&needle) {
        let pos = from + at;
        from = pos + needle.len();
        let before = &code[..pos];
        let boundary = before
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if !boundary || before.trim_end().ends_with("fn") {
            continue;
        }
        let enclosing = before
            .rmatch_indices("fn ")
            .find_map(|(i, _)| {
                let name: String = before[i + 3..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                (!name.is_empty()
                    && before[..i]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !(c.is_alphanumeric() || c == '_')))
                .then_some(name)
            })
            .unwrap_or_default();
        out.push(enclosing);
    }
    out
}

fn assert_census(primitive: &str, expected: &[(&str, &str, Class)]) {
    let mut found: BTreeMap<(String, String), usize> = BTreeMap::new();
    for caller in callers_of(primitive) {
        *found.entry(caller).or_default() += 1;
    }
    let mut want: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (file, function, _) in expected {
        *want
            .entry((file.to_string(), function.to_string()))
            .or_default() += 1;
    }
    assert_eq!(
        found, want,
        "the production callers of `{primitive}` changed. Classify the new caller: guard it \
         with `require_locked_activation_cost` if it can see an activation carrier, or record \
         why it can't be target-gated."
    );
    // Every guarded caller's function checks the guard for its own site.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for (file, function, class) in expected {
        let Class::Guarded(site) = class else {
            continue;
        };
        let text = production_code(&std::fs::read_to_string(root.join(file)).unwrap());
        let start = text
            .find(&format!("fn {function}("))
            .unwrap_or_else(|| panic!("{file}: fn {function}"));
        let open = start + text[start..].find('{').unwrap();
        let body = &text[start..matching_brace(text.as_bytes(), open)];
        assert!(
            body.contains("require_locked_activation_cost(")
                && body.contains(&format!("ActivationCostGuardSite::{site}")),
            "{file}: `{function}` pays activation costs but doesn't refuse an unlocked one"
        );
    }
}

#[test]
fn every_activation_cost_payment_entry_is_classified() {
    assert_census("pay_ability_cost_for_activation", PAY_ABILITY_COST);
}

#[test]
fn every_activation_mana_resume_entry_is_classified() {
    assert_census(
        "pay_ability_mana_cost_with_choices_excluding_and_resume",
        PAY_ABILITY_MANA_WITH_RESUME,
    );
}

/// The mistake this census was built after: a census that stops at the first
/// test module misses a production call AFTER a mid-file one.
#[test]
fn a_production_call_after_a_mid_file_test_module_is_counted() {
    let planted = "fn before() { pay_it(); }\n\
                   #[cfg(test)]\n\
                   mod tests {\n    fn in_test() { pay_it(); let s = \"}\"; }\n}\n\
                   fn after() { pay_it(); }\n";
    assert_eq!(
        calls_in(&production_code(planted), "pay_it"),
        vec!["before".to_string(), "after".to_string()]
    );
}
