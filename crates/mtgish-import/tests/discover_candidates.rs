//! Discovery helper — NOT a real test. Run with
//! `cargo test -p mtgish-import --test discover_candidates -- --ignored --nocapture`.
//!
//! Scans `data/mtgish-cards.json` for cards matching simple text predicates
//! and reports which ones convert cleanly. Used to pick cards for the
//! structural goldens; not part of the regression gate.

use mtgish_import::convert::convert_card;
use mtgish_import::report::{Ctx, ImportReport};
use mtgish_import::schema::types::OracleCard;

fn load_corpus() -> Vec<serde_json::Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("data/mtgish-cards.json");
    let raw = std::fs::read_to_string(&path).expect("read cards.json");
    serde_json::from_str(&raw).expect("parse cards.json")
}

fn try_convert(value: &serde_json::Value) -> bool {
    let Ok(card): Result<OracleCard, _> = serde_json::from_value(value.clone()) else {
        return false;
    };
    let mut report = ImportReport::default();
    let name = value
        .get("Name")
        .and_then(|n| n.as_str())
        .unwrap_or("?")
        .to_string();
    let mut ctx = Ctx::new(name, &mut report);
    let ok = convert_card(&card, &mut ctx).is_ok();
    let saw_gap = ctx.finish();
    ok && !saw_gap
}

fn name_of(v: &serde_json::Value) -> &str {
    v.get("Name").and_then(|n| n.as_str()).unwrap_or("?")
}

fn raw_text(v: &serde_json::Value) -> String {
    v.to_string()
}

fn report_matches(
    label: &str,
    corpus: &[serde_json::Value],
    pred: impl Fn(&str) -> bool,
    want: usize,
) {
    let mut clean: Vec<&str> = Vec::new();
    for v in corpus {
        let t = raw_text(v);
        if !pred(&t) {
            continue;
        }
        if try_convert(v) {
            clean.push(name_of(v));
            if clean.len() >= want {
                break;
            }
        }
    }
    println!("\n=== {label} ({} clean) ===", clean.len());
    for n in clean {
        println!("  {n}");
    }
}

#[test]
#[ignore]
fn discover() {
    let corpus = load_corpus();
    eprintln!("loaded {} cards", corpus.len());

    // ETB trigger that draws a card.
    report_matches(
        "vanilla_etb_trigger (ETB + DrawCards)",
        &corpus,
        |t| {
            t.contains("WhenAPermanentEntersTheBattlefield")
                && t.contains("ThisPermanent")
                && t.contains("\"_Action\":\"DrawCards\"")
        },
        15,
    );

    report_matches(
        "etb_tapped (look for EntersTapped variants)",
        &corpus,
        |t| t.contains("EntersTapped") || t.contains("AsPermanentEnters") && t.contains("Tapped"),
        15,
    );

    report_matches(
        "etb_with_counters",
        &corpus,
        |t| t.contains("EntersWithNumberCounters"),
        15,
    );

    report_matches(
        "modal_choose_one",
        &corpus,
        |t| t.contains("ChooseOne") || t.contains("\"Modal\"") || t.contains("ChooseModes"),
        15,
    );

    report_matches(
        "equipment_with_simple_pump",
        &corpus,
        |t| t.contains("\"Equip\"") || t.contains("EquipCost"),
        15,
    );

    report_matches(
        "saga_chapters",
        &corpus,
        |t| t.contains("\"_Rule\":\"Saga\"") || t.contains("\"Saga\"") && t.contains("Chapter"),
        15,
    );

    report_matches(
        "cost_reduction_static",
        &corpus,
        |t| t.contains("ReduceCost") || t.contains("CostsLess") || t.contains("CostLessToCast"),
        15,
    );

    report_matches(
        "alternative_cost_spell (Flashback/Madness)",
        &corpus,
        |t| {
            t.contains("Flashback")
                || t.contains("Madness")
                || t.contains("AlternativeCost")
                || t.contains("AlternateCastingCost")
        },
        15,
    );

    report_matches(
        "replacement_damage",
        &corpus,
        |t| {
            t.contains("PreventDamage")
                || t.contains("PreventAll")
                || t.contains("ReplaceDamage")
                || t.contains("DamageModification")
        },
        15,
    );

    report_matches(
        "multi_rule_trigger_and_activated",
        &corpus,
        |t| t.contains("\"_Rule\":\"TriggerA\"") && t.contains("\"_Rule\":\"ActivatedAbility\""),
        15,
    );

    // Additional archetypes — fallbacks for the planned ones that don't convert.

    report_matches(
        "vanilla_multi_keyword (creature with 2+ vanilla keywords)",
        &corpus,
        |t| {
            t.matches("\"_Rule\":\"Flying\"").count() >= 1
                && (t.contains("\"_Rule\":\"FirstStrike\"")
                    || t.contains("\"_Rule\":\"Lifelink\"")
                    || t.contains("\"_Rule\":\"Vigilance\"")
                    || t.contains("\"_Rule\":\"Trample\""))
        },
        15,
    );

    report_matches(
        "activated_pump (ActivatedAbility on a creature)",
        &corpus,
        |t| t.contains("\"_Rule\":\"ActivatedAbility\""),
        20,
    );

    report_matches(
        "static_anthem (PermanentLayerEffect granting on others)",
        &corpus,
        |t| t.contains("\"_Rule\":\"PermanentLayerEffect\"") && t.contains("AdjustPT"),
        20,
    );

    report_matches(
        "mana_ability (TapForMana / AddMana)",
        &corpus,
        |t| t.contains("TapForMana") || t.contains("AddMana"),
        20,
    );

    report_matches(
        "etb_trigger_scry_or_other (any ETB trigger that converts)",
        &corpus,
        |t| t.contains("WhenAPermanentEntersTheBattlefield") && t.contains("ThisPermanent"),
        30,
    );
}
