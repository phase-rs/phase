use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

const AUTHORITY_MATRIX: &str = include_str!("../fixtures/cr733/authority_matrix.json");
const WRITE_SITE_CLASSIFICATIONS: &str = include_str!("../fixtures/cr733/blocked_write_sites.json");
const RNG_ALLOCATOR_MAP: &str = include_str!("../fixtures/cr733/rng_allocator_map.json");
const SIDE_EFFECT_MAP: &str = include_str!("../fixtures/cr733/side_effect_map.json");

fn write_site_identity(site: &Value) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        site["field"]
            .as_str()
            .expect("CR733 write site must name its field"),
        site["file"]
            .as_str()
            .expect("CR733 write site must name its file"),
        site["fn"]
            .as_str()
            .expect("CR733 write site must name its enclosing function"),
        site["line"]
            .as_u64()
            .expect("CR733 write site must name its line"),
        site["pattern"]
            .as_str()
            .expect("CR733 write site must name its pattern"),
        site["receiver"]
            .as_str()
            .expect("CR733 write site must name its receiver"),
        site["reachable"]
            .as_bool()
            .expect("CR733 write site must name its reachability"),
    )
}

fn non_write_site_identity(site: &Value) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        site["family"]
            .as_str()
            .expect("CR733 side-effect site must name its family"),
        site["file"]
            .as_str()
            .expect("CR733 side-effect site must name its file"),
        site["fn"]
            .as_str()
            .expect("CR733 side-effect site must name its enclosing function"),
        site["line"]
            .as_u64()
            .expect("CR733 side-effect site must name its line"),
        site["pattern"]
            .as_str()
            .expect("CR733 side-effect site must name its pattern"),
        site["variant"].as_str().unwrap_or(""),
        site["reachable"]
            .as_bool()
            .expect("CR733 side-effect site must name its reachability"),
    )
}

#[test]
fn cr733_authority_matrix_covers_the_fresh_write_census() {
    let matrix: Value = serde_json::from_str(AUTHORITY_MATRIX)
        .expect("CR733 authority matrix fixture must be valid JSON");
    let matrix_fields = matrix["fields"]
        .as_array()
        .expect("CR733 authority matrix must contain a fields array");

    let site_classifications: Value = serde_json::from_str(WRITE_SITE_CLASSIFICATIONS)
        .expect("CR733 write-site classification fixture must be valid JSON");
    let classified_sites = site_classifications["sites"]
        .as_array()
        .expect("CR733 write-site classification fixture must contain a sites array");
    let clone_provenance = site_classifications["clone_provenance"]
        .as_array()
        .expect("CR733 write-site classification fixture must contain clone provenance");
    let clone_site_ids: BTreeSet<_> = clone_provenance
        .iter()
        .flat_map(|entry| {
            entry["site_ids"]
                .as_array()
                .expect("each clone-provenance entry must contain site IDs")
        })
        .map(|site_id| {
            site_id
                .as_str()
                .expect("each clone-provenance site ID must be a string")
                .to_owned()
        })
        .collect();
    let rng_allocator_map: Value = serde_json::from_str(RNG_ALLOCATOR_MAP)
        .expect("CR733 RNG/allocator map fixture must be valid JSON");
    let rng_allocator_sites = rng_allocator_map["sites"]
        .as_array()
        .expect("CR733 RNG/allocator map must contain a sites array");
    let side_effect_map: Value = serde_json::from_str(SIDE_EFFECT_MAP)
        .expect("CR733 side-effect map fixture must be valid JSON");
    let side_effect_sites = side_effect_map["sites"]
        .as_array()
        .expect("CR733 side-effect map must contain a sites array");

    let mut matrix_field_counts = BTreeMap::new();
    for entry in matrix_fields {
        let field = entry["field"]
            .as_str()
            .expect("each CR733 authority-matrix entry must name a field");
        *matrix_field_counts
            .entry(field.to_owned())
            .or_insert(0_usize) += 1;

        match entry["classification"]
            .as_str()
            .expect("each CR733 authority-matrix entry must have a classification")
        {
            "proposed_authority" => {
                assert!(
                    entry["final_authority"].as_str().is_some(),
                    "proposed authority {field:?} must name its final P2 seam"
                );
                assert!(
                    entry["command_family"].as_str().is_some(),
                    "proposed authority {field:?} must name its command family"
                );
                assert!(
                    entry["command_scopes"]
                        .as_array()
                        .is_some_and(|scopes| !scopes.is_empty()),
                    "proposed authority {field:?} must name at least one semantic scope"
                );
                assert!(
                    entry["composition_policy"].as_str().is_some(),
                    "proposed authority {field:?} must state its composition policy"
                );
            }
            "out_of_closure_clone" => {
                assert!(
                    entry["clone_site_ids"]
                        .as_array()
                        .is_some_and(|sites| !sites.is_empty()),
                    "out-of-closure field {field:?} must name its discarded clone sites"
                );
                assert!(
                    entry["provenance_ref"].as_str().is_some(),
                    "out-of-closure field {field:?} must point to clone provenance evidence"
                );
            }
            "derived" => {
                assert!(
                    entry["rebuild_entry"].as_str().is_some(),
                    "derived field {field:?} must name its rebuild entry"
                );
            }
            "blocked" => {
                assert!(
                    entry["blocker_reason"].as_str().is_some(),
                    "blocked field {field:?} must state its narrowed hard-stop reason"
                );
            }
            "out_of_reachable_closure" => {
                assert!(
                    entry["reason"].as_str().is_some(),
                    "out-of-reachable field {field:?} must state why it is outside the P0 closure"
                );
            }
            unexpected => panic!(
                "CR733 authority matrix field {field:?} has unsupported classification {unexpected:?}"
            ),
        }
    }

    for site in classified_sites {
        let field = site["field"]
            .as_str()
            .expect("each CR733 write-site record must name its field");
        match site["classification"]
            .as_str()
            .expect("each CR733 write-site record must have a classification")
        {
            "proposed_authority" => {
                assert!(
                    matrix_field_counts.contains_key(field),
                    "proposed write site for {field:?} must have a matrix field"
                );
            }
            "out_of_closure_clone" => {
                assert_eq!(
                    site["reroute_required"].as_bool(),
                    Some(false),
                    "discarded clone site for {field:?} must not be rerouted by P2"
                );
                let site_id = format!(
                    "{}:{}:{}:{}",
                    site["file"]
                        .as_str()
                        .expect("discarded clone site must name its file"),
                    site["line"]
                        .as_u64()
                        .expect("discarded clone site must name its line"),
                    site["fn"]
                        .as_str()
                        .expect("discarded clone site must name its enclosing function"),
                    site["pattern"]
                        .as_str()
                        .expect("discarded clone site must name its write pattern"),
                );
                assert!(
                    clone_site_ids.contains(&site_id),
                    "discarded clone site {site_id:?} must have source-read provenance"
                );
            }
            "derived" | "blocked" | "out_of_reachable_closure" => {
                assert_eq!(
                    site["reroute_required"].as_bool(),
                    Some(false),
                    "non-rerouted write site for {field:?} must not be sent through P2"
                );
            }
            unexpected => panic!(
                "CR733 write-site record for {field:?} has unsupported classification {unexpected:?}"
            ),
        }
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

    // Everything below pins exact census counts and site identities (including
    // line numbers) for every scanned engine file. Running those pins
    // unconditionally would fail this test on any unrelated engine edit until
    // the fixtures are regenerated, red-locking shared CI. They are therefore
    // opt-in: the CR733 pipeline (and any future dedicated CI gate) sets
    // CR733_CENSUS_STRICT. The field-set ratchet above stays always-on — a new
    // reachable write-family field without a matrix row still fails everywhere.
    if std::env::var_os("CR733_CENSUS_STRICT").is_none() {
        return;
    }

    let summary = &census["summary"];
    assert_eq!(
        matrix["census"]["site_count"].as_u64(),
        Some(
            census["sites"]
                .as_array()
                .expect("fresh CR733 census must contain a sites array")
                .len() as u64
        ),
        "authority-matrix census site-count pin must match a fresh census"
    );
    for family in ["write", "rng", "allocator", "event_emission", "information"] {
        assert_eq!(
            matrix["census"]["family_counts"][family].as_u64(),
            summary["per_family_counts"][family].as_u64(),
            "authority-matrix census pin for {family:?} must match a fresh census"
        );
    }

    let mut expected_write_site_counts = BTreeMap::new();
    let mut classified_write_site_counts = BTreeMap::new();
    for site in census["sites"]
        .as_array()
        .expect("fresh CR733 census must contain a sites array")
        .iter()
        .filter(|site| site["family"].as_str() == Some("write"))
    {
        *expected_write_site_counts
            .entry(write_site_identity(site))
            .or_insert(0_usize) += 1;
    }
    for site in classified_sites {
        *classified_write_site_counts
            .entry(write_site_identity(site))
            .or_insert(0_usize) += 1;
    }
    assert_eq!(
        classified_write_site_counts, expected_write_site_counts,
        "CR733 write-site classifications must mirror every fresh write-family census site exactly"
    );

    let mut expected_rng_allocator_counts = BTreeMap::new();
    let mut mapped_rng_allocator_counts = BTreeMap::new();
    let mut expected_side_effect_counts = BTreeMap::new();
    let mut mapped_side_effect_counts = BTreeMap::new();
    for site in census["sites"]
        .as_array()
        .expect("fresh CR733 census must contain a sites array")
    {
        match site["family"].as_str() {
            Some("rng" | "allocator") => {
                *expected_rng_allocator_counts
                    .entry(non_write_site_identity(site))
                    .or_insert(0_usize) += 1;
            }
            Some("event_emission" | "information") => {
                *expected_side_effect_counts
                    .entry(non_write_site_identity(site))
                    .or_insert(0_usize) += 1;
            }
            Some("write") => {}
            other => panic!("unexpected CR733 census family {other:?}"),
        }
    }
    for entry in rng_allocator_sites {
        *mapped_rng_allocator_counts
            .entry(non_write_site_identity(&entry["site"]))
            .or_insert(0_usize) += 1;
    }
    for entry in side_effect_sites {
        *mapped_side_effect_counts
            .entry(non_write_site_identity(&entry["site"]))
            .or_insert(0_usize) += 1;
    }
    assert_eq!(
        mapped_rng_allocator_counts, expected_rng_allocator_counts,
        "CR733 RNG/allocator receipts must mirror every fresh census site exactly"
    );
    assert_eq!(
        mapped_side_effect_counts, expected_side_effect_counts,
        "CR733 event/information map must mirror every fresh census site exactly"
    );
}
