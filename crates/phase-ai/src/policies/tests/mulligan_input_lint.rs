//! Architectural lint: intentionally unused mulligan inputs must explain why.
//!
//! New mulligan policies receive `plan`, `turn_order`, and `mulligans_taken`.
//! Binding any of those as `_plan`, `_turn_order`, or `_mulligans_taken`
//! requires a same-line `// input-unused:` marker so reviewers can tell
//! intentional non-use from cargo-culted signatures.

use std::fs;
use std::path::Path;

const MARKER: &str = "// input-unused:";
const BINDINGS: &[&str] = &["_plan:", "_turn_order:", "_mulligans_taken:"];

#[test]
fn unused_mulligan_inputs_carry_marker() {
    let root = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/policies/mulligan"
    ));
    let mut violations = Vec::new();
    walk(root, &mut violations);

    assert!(
        violations.is_empty(),
        "unused mulligan inputs need `{MARKER}` markers:\n{}",
        violations.join("\n")
    );
}

fn walk(dir: &Path, violations: &mut Vec<String>) {
    let entries = fs::read_dir(dir).expect("mulligan policy dir");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, violations);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("mod.rs") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        for (idx, line) in contents.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if BINDINGS.iter().any(|binding| code.contains(binding)) && !line.contains(MARKER) {
                violations.push(format!(
                    "{}:{}: unused mulligan input without marker",
                    path.display(),
                    idx + 1
                ));
            }
        }
    }
}
