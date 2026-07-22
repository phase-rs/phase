use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

const AUTHORITY_MATRIX: &str = include_str!("../fixtures/cr733/authority_matrix.json");

#[test]
fn cr733_authority_matrix_covers_the_fresh_write_census() {
    let matrix: Value = serde_json::from_str(AUTHORITY_MATRIX)
        .expect("CR733 authority matrix fixture must be valid JSON");
    let matrix_fields = matrix["fields"]
        .as_array()
        .expect("CR733 authority matrix must contain a fields array");

    let mut matrix_field_counts = BTreeMap::new();
    for entry in matrix_fields {
        let field = entry["field"]
            .as_str()
            .expect("each CR733 authority-matrix entry must name a field");
        *matrix_field_counts
            .entry(field.to_owned())
            .or_insert(0_usize) += 1;
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .expect("engine crate must be nested below the repository root");
    let output_path =
        std::env::temp_dir().join(format!("cr733-mutation-census-{}.json", std::process::id()));
    let _ = fs::remove_file(&output_path);

    let output = Command::new("python3")
        .arg(repo_root.join("scripts/cr733_mutation_census.py"))
        .arg("--json")
        .arg(&output_path)
        .current_dir(repo_root)
        .output()
        .expect("python3 must start the CR733 census generator");
    assert!(
        output.status.success(),
        "CR733 census generation failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let census_text = fs::read_to_string(&output_path)
        .expect("successful CR733 census generation must produce its JSON output");
    fs::remove_file(&output_path).expect("remove temporary CR733 census output");
    let census: Value =
        serde_json::from_str(&census_text).expect("fresh CR733 census must be valid JSON");

    let mut census_fields = BTreeSet::new();
    let mut reachable_fields = BTreeSet::new();
    for site in census["sites"]
        .as_array()
        .expect("fresh CR733 census must contain a sites array")
    {
        if site["family"].as_str() != Some("write") {
            continue;
        }
        let Some(field) = site["field"].as_str() else {
            continue;
        };
        census_fields.insert(field.to_owned());
        if site["reachable"].as_bool() == Some(true) {
            reachable_fields.insert(field.to_owned());
        }
    }

    let matrix_fields: BTreeSet<_> = matrix_field_counts.keys().cloned().collect();
    let missing: Vec<_> = census_fields.difference(&matrix_fields).cloned().collect();
    let nonexistent: Vec<_> = matrix_fields.difference(&census_fields).cloned().collect();
    assert!(
        missing.is_empty(),
        "fresh CR733 write-family fields missing from the authority matrix: {missing:?}"
    );
    assert!(
        nonexistent.is_empty(),
        "CR733 authority matrix references fields absent from the fresh census: {nonexistent:?}"
    );

    for field in &census_fields {
        assert_eq!(
            matrix_field_counts.get(field),
            Some(&1),
            "CR733 authority matrix must map write-family field {field:?} exactly once"
        );
    }
    for field in &reachable_fields {
        assert!(
            matrix_field_counts.contains_key(field),
            "new reachable CR733 write-family field {field:?} is unmapped"
        );
    }
}
