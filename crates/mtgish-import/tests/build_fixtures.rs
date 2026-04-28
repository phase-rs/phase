//! Fixture builder — NOT a regression test. Run with
//! `cargo test -p mtgish-import --test build_fixtures -- --ignored --nocapture`.
//!
//! For each `(archetype, card_name)` pair, extracts the card's `OracleCard`
//! JSON object from `data/mtgish-cards.json`, runs `convert_card` on it,
//! and writes the pair `(input.json, expected.json)` into
//! `tests/golden/structural/<archetype>/`.
//!
//! This is a one-shot helper used by the human author when bootstrapping or
//! intentionally re-blessing a structural golden. The real regression suite
//! is `tests/golden_structural.rs` and never invokes this helper.

use mtgish_import::convert::{convert_card, EngineFaceStub};
use mtgish_import::report::{Ctx, ImportReport};
use mtgish_import::schema::types::OracleCard;

const FIXTURES: &[(&str, &str)] = &[
    ("vanilla_etb_trigger", "Augury Owl"),
    ("etb_tapped", "Alpine Meadow"),
    ("etb_with_counters", "Endless One"),
    ("multi_keyword_protection", "Akroma, Angel of Wrath"),
    ("equipment_equip_cost", "Bonesplitter"),
    ("aura_grants_flying_and_pump", "Arcane Flight"),
    ("etb_replacement_plus_trigger", "Authority of the Consuls"),
    ("madness_alternative_cost", "Reckless Wurm"),
    ("etb_and_ltb_lifegain", "Aven Riftwatcher"),
    ("etb_with_counters_and_trigger", "Belligerent Hatchling"),
];

fn corpus_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("data/mtgish-cards.json")
}

#[test]
#[ignore]
fn build_all_fixtures() {
    let raw = std::fs::read_to_string(corpus_path()).expect("read corpus");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("parse corpus");

    let by_name: std::collections::HashMap<&str, &serde_json::Value> = arr
        .iter()
        .filter_map(|v| v.get("Name").and_then(|n| n.as_str()).map(|n| (n, v)))
        .collect();

    let goldens_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/structural");

    for (archetype, card_name) in FIXTURES {
        let value = by_name
            .get(card_name)
            .copied()
            .unwrap_or_else(|| panic!("card not in corpus: {card_name}"));

        let card: OracleCard =
            serde_json::from_value((*value).clone()).expect("deserialize OracleCard");

        let mut report = ImportReport::default();
        let mut ctx = Ctx::new((*card_name).to_string(), &mut report);
        let stubs: Vec<EngineFaceStub> = convert_card(&card, &mut ctx)
            .unwrap_or_else(|e| panic!("[{archetype}] {card_name} conversion failed: {e}"));
        let saw_gap = ctx.finish();
        if saw_gap {
            panic!("[{archetype}] {card_name} recorded a gap; cannot golden a non-clean card");
        }

        let dir = goldens_root.join(archetype);
        std::fs::create_dir_all(&dir).expect("mkdir golden archetype");

        let input_pretty = serde_json::to_string_pretty(value).expect("re-serialize input");
        std::fs::write(dir.join("input.json"), input_pretty).expect("write input.json");

        let expected = serde_json::to_string_pretty(&stubs).expect("serialize stubs");
        std::fs::write(dir.join("expected.json"), expected).expect("write expected.json");

        println!("wrote {} ({})", archetype, card_name);
    }
}
