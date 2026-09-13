use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use engine::database::{BracketCardClass, BracketLists};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_lists() -> BracketLists {
    BracketLists::from_json_path(&repository_root().join("data/bracket_lists.json"))
        .expect("tracked bracket list data must parse")
}

fn date_is_shaped(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

#[test]
fn data_file_parses_under_the_new_schema() {
    assert_eq!(load_lists().version, "2026-02-09-wotc");
}

#[test]
fn every_list_has_its_own_dated_source() {
    let lists = load_lists();
    for class in BracketCardClass::ALL {
        let source = &lists.get(class).expect("all six classes must exist").source;
        assert!(!source.url.is_empty(), "{class:?} source URL");
        assert!(date_is_shaped(&source.published), "{class:?} published");
        assert!(date_is_shaped(&source.retrieved), "{class:?} retrieved");
    }
}

#[test]
fn every_wotc_source_has_a_readable_local_copy() {
    let root = repository_root();
    let lists = load_lists();
    for class in BracketCardClass::ALL {
        let source = &lists.get(class).expect("all six classes must exist").source;
        match class {
            BracketCardClass::ExtraCombats => assert!(source.local_copy.is_none()),
            BracketCardClass::GameChangers
            | BracketCardClass::MassLandSweepers
            | BracketCardClass::MassManaDenial
            | BracketCardClass::ExtraTurns
            | BracketCardClass::EfficientTutors => {
                let path = source
                    .local_copy
                    .as_deref()
                    .expect("WotC sources must have an archived local copy");
                assert!(root.join(path).exists(), "missing local copy {path}");
            }
        }
    }
}

#[test]
fn every_curated_name_resolves_in_the_full_card_export() {
    let export_path = repository_root().join("client/public/card-data.json");
    if !export_path.exists() {
        eprintln!(
            "skipping: full card export is missing at {}; generate it to validate every curated bracket-list name",
            export_path.display()
        );
        return;
    }

    // The committed integration fixture contains only 33 of the 94 curated
    // names, so this contract deliberately reads the full gitignored export.
    let export: serde_json::Map<String, serde_json::Value> = serde_json::from_reader(
        std::fs::File::open(&export_path).expect("full card export should be readable"),
    )
    .expect("full card export should contain a JSON object");
    let export_names: BTreeSet<String> = export.keys().map(|name| name.to_lowercase()).collect();
    let lists = load_lists();
    let curated_names: Vec<&str> = lists.all_names().collect();
    assert_eq!(curated_names.len(), 94, "curated-name census changed");

    // Curated MDFCs use their front-face card identity. Thus Tergrid resolves
    // against the export's lowercased front-face key; its Lantern face may also
    // be present, but is not a second curated card.
    let unresolved: Vec<&str> = curated_names
        .into_iter()
        .filter(|name| !export_names.contains(*name))
        .collect();
    assert!(
        unresolved.is_empty(),
        "curated bracket-list names missing from {}: {unresolved:#?}",
        export_path.display()
    );
}

#[test]
fn correction_a_extra_combats_are_not_extra_turns() {
    let lists = load_lists();
    let combat_names = BTreeSet::from(["aggravated assault", "hellkite charger", "savage beating"]);
    let turns = &lists.get(BracketCardClass::ExtraTurns).unwrap().names;
    let combats = &lists.get(BracketCardClass::ExtraCombats).unwrap().names;
    assert_eq!(turns.len(), 11);
    assert!(combat_names.iter().all(|name| !turns.contains(*name)));
    assert_eq!(
        combats.iter().map(String::as_str).collect::<BTreeSet<_>>(),
        combat_names
    );
    assert!(lists.signals_for("Aggravated Assault").is_clean());
}

#[test]
fn correction_b_sunder_is_a_sweeper_and_the_mana_class_exists() {
    let lists = load_lists();
    let sweepers = &lists.get(BracketCardClass::MassLandSweepers).unwrap().names;
    let denial = &lists.get(BracketCardClass::MassManaDenial).unwrap().names;
    assert_eq!(sweepers.len(), 10);
    assert!(sweepers.contains("sunder"));
    assert_eq!(denial.len(), 10);
    assert!(!denial.contains("sunder"));
    for name in ["winter orb", "blood moon", "stasis"] {
        assert!(denial.contains(name));
    }
}

#[test]
fn correction_b_records_its_own_incompleteness() {
    let lists = load_lists();
    let note = lists
        .get(BracketCardClass::MassManaDenial)
        .unwrap()
        .note
        .as_deref()
        .expect("mass mana denial needs an incompleteness note");
    assert!(note.contains("derivation did not converge"));
}

#[test]
fn correction_c_tutors_are_superseded() {
    let lists = load_lists();
    let superseded = lists
        .get(BracketCardClass::EfficientTutors)
        .unwrap()
        .superseded_by
        .as_ref()
        .expect("tutor guidance must be marked superseded");
    assert_eq!(superseded.published, "2025-10-21");
}

#[test]
fn correction_c_records_the_game_changer_overlap() {
    let lists = load_lists();
    assert_eq!(
        lists.overlap(
            BracketCardClass::GameChangers,
            BracketCardClass::EfficientTutors
        ),
        BTreeSet::from([
            "demonic tutor",
            "enlightened tutor",
            "imperial seal",
            "mystical tutor",
            "vampiric tutor",
            "worldly tutor",
        ])
    );
}

#[test]
fn game_changers_list_is_the_verified_53() {
    let lists = load_lists();
    let names = &lists.get(BracketCardClass::GameChangers).unwrap().names;
    assert_eq!(names.len(), 53);
    for name in ["farewell", "biorhythm", "tergrid, god of fright"] {
        assert!(names.contains(name));
    }
    assert!(!names.contains("tergrid, god of fright // tergrid's lantern"));
}

#[test]
fn mass_and_extra_lists_do_not_overlap_game_changers() {
    let lists = load_lists();
    assert!(lists
        .overlap(
            BracketCardClass::GameChangers,
            BracketCardClass::MassLandSweepers
        )
        .is_empty());
    assert!(lists
        .overlap(BracketCardClass::GameChangers, BracketCardClass::ExtraTurns)
        .is_empty());
}
