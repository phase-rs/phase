use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

use crate::database::CardDatabase;
use crate::game::effects::is_known_effect;
use crate::game::game_object::GameObject;
use crate::game::static_abilities::{build_static_registry, StaticAbilityHandler};
use crate::game::triggers::build_trigger_registry;
use crate::parser::ability::parse_ability;
use crate::types::ability::{
    effect_variant_name, AbilityDefinition, StaticDefinition, TriggerDefinition,
};
use crate::types::keywords::Keyword;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardCoverageResult {
    pub card_name: String,
    pub set_code: String,
    pub supported: bool,
    pub missing_handlers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub total_cards: usize,
    pub supported_cards: usize,
    pub coverage_pct: f64,
    pub cards: Vec<CardCoverageResult>,
    pub missing_handler_frequency: Vec<(String, usize)>,
}

/// Check whether a game object has any mechanics the engine cannot handle.
///
/// Checks keywords (Unknown variant = unrecognized), abilities (api_type
/// not in effect registry), triggers (mode not in trigger registry), and
/// static abilities (mode not in static registry).
pub fn has_unimplemented_mechanics(obj: &GameObject) -> bool {
    // 1. Any Unknown keyword means the parser didn't recognize it
    if obj
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Unknown(_)))
    {
        return true;
    }

    // 2. Check abilities against known effect types
    for def in &obj.abilities {
        let api = effect_variant_name(&def.effect);
        if !api.is_empty() && !is_known_effect(api) {
            return true;
        }
    }

    // 3. Check trigger modes against trigger registry
    let trigger_registry = build_trigger_registry();
    for trig in &obj.trigger_definitions {
        if matches!(&trig.mode, TriggerMode::Unknown(_))
            || !trigger_registry.contains_key(&trig.mode)
        {
            return true;
        }
    }

    // 4. Check static ability modes against static registry
    let static_registry = build_static_registry();
    for stat in &obj.static_definitions {
        if !static_registry.contains_key(&stat.mode) {
            return true;
        }
    }

    false
}

/// Analyze Standard-legal card coverage by checking which cards have
/// all their abilities, triggers, keywords, and static abilities
/// supported by the engine's registries.
pub fn analyze_standard_coverage(card_db: &CardDatabase) -> CoverageSummary {
    let trigger_registry = build_trigger_registry();
    let static_registry = build_static_registry();

    let mut cards = Vec::new();
    let mut freq: HashMap<String, usize> = HashMap::new();

    for (_key, card_rules) in card_db.iter() {
        let faces = layout_faces(&card_rules.layout);
        for face in &faces {
            // Check if card is in a Standard set via set code in name suffix or metadata.
            // Since our card data doesn't have set codes, we analyze ALL cards
            // and report coverage. The set_code field is left empty for file-based cards.
            let mut missing = Vec::new();

            // Check abilities (SP$, AB$, DB$ lines)
            check_abilities(&face.abilities, &mut missing);

            // Check triggers
            check_triggers(&face.triggers, &trigger_registry, &mut missing);

            // Check keywords
            check_keywords(&face.keywords, &mut missing);

            // Check static abilities
            check_statics(&face.static_abilities, &static_registry, &mut missing);

            // Check SVar-referenced sub-abilities
            for svar_val in face.svars.values() {
                if svar_val.contains("$ ") {
                    if let Ok(def) = parse_ability(svar_val) {
                        let api = effect_variant_name(&def.effect);
                        if !api.is_empty() && !is_known_effect(api) {
                            let label = format!("Effect:{api}");
                            if !missing.contains(&label) {
                                missing.push(label);
                            }
                        }
                    }
                }
            }

            let supported = missing.is_empty();

            for m in &missing {
                *freq.entry(m.clone()).or_default() += 1;
            }

            cards.push(CardCoverageResult {
                card_name: face.name.clone(),
                set_code: String::new(),
                supported,
                missing_handlers: missing,
            });
        }
    }

    let total_cards = cards.len();
    let supported_cards = cards.iter().filter(|c| c.supported).count();
    let coverage_pct = if total_cards > 0 {
        (supported_cards as f64 / total_cards as f64) * 100.0
    } else {
        0.0
    };

    let mut missing_handler_frequency: Vec<(String, usize)> = freq.into_iter().collect();
    missing_handler_frequency.sort_by(|a, b| b.1.cmp(&a.1));

    CoverageSummary {
        total_cards,
        supported_cards,
        coverage_pct,
        cards,
        missing_handler_frequency,
    }
}

fn check_abilities(abilities: &[AbilityDefinition], missing: &mut Vec<String>) {
    for def in abilities {
        let api = effect_variant_name(&def.effect);
        if !api.is_empty() && !is_known_effect(api) {
            let label = format!("Effect:{api}");
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_triggers(
    triggers: &[TriggerDefinition],
    trigger_registry: &HashMap<TriggerMode, crate::game::triggers::TriggerMatcher>,
    missing: &mut Vec<String>,
) {
    for def in triggers {
        if matches!(&def.mode, TriggerMode::Unknown(_)) || !trigger_registry.contains_key(&def.mode)
        {
            let label = format!("Trigger:{}", def.mode);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_keywords(keywords: &[String], missing: &mut Vec<String>) {
    for kw_str in keywords {
        let kw = Keyword::from_str(kw_str).unwrap();
        if matches!(kw, Keyword::Unknown(_)) {
            let label = format!("Keyword:{}", kw_str);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_statics(
    statics: &[StaticDefinition],
    static_registry: &HashMap<StaticMode, StaticAbilityHandler>,
    missing: &mut Vec<String>,
) {
    for def in statics {
        if !static_registry.contains_key(&def.mode) {
            let label = format!("Static:{}", def.mode);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

/// Returns `true` if the coverage summary shows 100% support (CI pass).
/// Returns `false` if there are any unsupported cards (CI fail).
pub fn is_fully_covered(summary: &CoverageSummary) -> bool {
    summary.total_cards > 0 && summary.supported_cards == summary.total_cards
}

fn layout_faces(layout: &crate::types::card::CardLayout) -> Vec<&crate::types::card::CardFace> {
    use crate::types::card::CardLayout;
    match layout {
        CardLayout::Single(face) => vec![face],
        CardLayout::Split(a, b)
        | CardLayout::Flip(a, b)
        | CardLayout::Transform(a, b)
        | CardLayout::Meld(a, b)
        | CardLayout::Adventure(a, b)
        | CardLayout::Modal(a, b)
        | CardLayout::Omen(a, b) => vec![a, b],
        CardLayout::Specialize(base, variants) => {
            let mut faces = vec![base];
            faces.extend(variants);
            faces
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[cfg(feature = "forge-compat")]
    use std::path::Path;

    #[cfg(feature = "forge-compat")]
    fn create_card_file(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(format!("{}.txt", name)), content).unwrap();
    }

    #[test]
    #[cfg(feature = "forge-compat")]
    fn card_with_supported_effect_is_marked_supported() {
        let tmp = tempfile::tempdir().unwrap();
        // Lightning Bolt uses DealDamage which is in the effect registry
        create_card_file(
            tmp.path(),
            "bolt",
            "Name:Lightning Bolt\nManaCost:R\nTypes:Instant\nA:SP$ DealDamage | Cost$ R | NumDmg$ 3\nOracle:Deal 3 damage.",
        );

        let db = CardDatabase::load(tmp.path()).unwrap();
        let summary = analyze_standard_coverage(&db);

        assert_eq!(summary.total_cards, 1);
        assert_eq!(summary.supported_cards, 1);
        assert!(summary.cards[0].supported);
        assert!(summary.cards[0].missing_handlers.is_empty());
    }

    #[test]
    #[cfg(feature = "forge-compat")]
    fn card_with_unknown_effect_is_marked_unsupported() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(
            tmp.path(),
            "custom",
            "Name:Custom Card\nManaCost:U\nTypes:Instant\nA:SP$ Fateseal | Cost$ U | Amount$ 2\nOracle:Fateseal 2.",
        );

        let db = CardDatabase::load(tmp.path()).unwrap();
        let summary = analyze_standard_coverage(&db);

        assert_eq!(summary.total_cards, 1);
        assert_eq!(summary.supported_cards, 0);
        assert!(!summary.cards[0].supported);
        assert!(summary.cards[0]
            .missing_handlers
            .contains(&"Effect:Fateseal".to_string()));
    }

    #[test]
    #[cfg(feature = "forge-compat")]
    fn vanilla_creature_is_supported() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(
            tmp.path(),
            "bear",
            "Name:Grizzly Bears\nManaCost:1 G\nTypes:Creature Bear\nPT:2/2\nOracle:No text.",
        );

        let db = CardDatabase::load(tmp.path()).unwrap();
        let summary = analyze_standard_coverage(&db);

        assert_eq!(summary.supported_cards, 1);
    }

    #[test]
    #[cfg(feature = "forge-compat")]
    fn card_with_unknown_keyword_is_unsupported() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(
            tmp.path(),
            "future",
            "Name:Future Card\nManaCost:W\nTypes:Creature\nK:FutureKeyword\nPT:1/1\nOracle:Has FutureKeyword.",
        );

        let db = CardDatabase::load(tmp.path()).unwrap();
        let summary = analyze_standard_coverage(&db);

        assert_eq!(summary.supported_cards, 0);
        assert!(summary.cards[0]
            .missing_handlers
            .iter()
            .any(|m| m.starts_with("Keyword:")));
    }

    #[test]
    #[cfg(feature = "forge-compat")]
    fn missing_handler_frequency_sorted_descending() {
        let tmp = tempfile::tempdir().unwrap();
        // Two cards both missing the same effect
        create_card_file(
            tmp.path(),
            "card_a",
            "Name:Card A\nManaCost:R\nTypes:Instant\nA:SP$ Fateseal | Cost$ R\nOracle:Fateseal.",
        );
        create_card_file(
            tmp.path(),
            "card_b",
            "Name:Card B\nManaCost:U\nTypes:Instant\nA:SP$ Fateseal | Cost$ U\nOracle:Fateseal again.",
        );
        create_card_file(
            tmp.path(),
            "card_c",
            "Name:Card C\nManaCost:G\nTypes:Instant\nA:SP$ Polymorph | Cost$ G\nOracle:Polymorph.",
        );

        let db = CardDatabase::load(tmp.path()).unwrap();
        let summary = analyze_standard_coverage(&db);

        assert!(!summary.missing_handler_frequency.is_empty());
        // Fateseal appears 2 times, Polymorph 1 time
        assert_eq!(summary.missing_handler_frequency[0].0, "Effect:Fateseal");
        assert_eq!(summary.missing_handler_frequency[0].1, 2);
    }

    #[test]
    #[cfg(feature = "forge-compat")]
    fn coverage_percentage_calculated_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(
            tmp.path(),
            "bolt",
            "Name:Lightning Bolt\nManaCost:R\nTypes:Instant\nA:SP$ DealDamage | Cost$ R | NumDmg$ 3\nOracle:Deal 3.",
        );
        create_card_file(
            tmp.path(),
            "bear",
            "Name:Bear\nManaCost:1 G\nTypes:Creature\nPT:2/2\nOracle:Vanilla.",
        );
        create_card_file(
            tmp.path(),
            "custom",
            "Name:Custom\nManaCost:U\nTypes:Instant\nA:SP$ Fateseal | Cost$ U\nOracle:Fateseal.",
        );

        let db = CardDatabase::load(tmp.path()).unwrap();
        let summary = analyze_standard_coverage(&db);

        assert_eq!(summary.total_cards, 3);
        assert_eq!(summary.supported_cards, 2);
        // 2/3 * 100 = 66.67%
        assert!((summary.coverage_pct - 66.66).abs() < 1.0);
    }

    fn make_obj() -> GameObject {
        GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Test Card".to_string(),
            Zone::Battlefield,
        )
    }

    #[test]
    fn vanilla_object_has_no_unimplemented_mechanics() {
        let obj = make_obj();
        assert!(!has_unimplemented_mechanics(&obj));
    }

    #[test]
    fn object_with_known_keyword_has_no_unimplemented() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        obj.keywords.push(Keyword::Haste);
        assert!(!has_unimplemented_mechanics(&obj));
    }

    #[test]
    fn object_with_unknown_keyword_has_unimplemented() {
        let mut obj = make_obj();
        obj.keywords
            .push(Keyword::Unknown("FutureKeyword".to_string()));
        assert!(has_unimplemented_mechanics(&obj));
    }

    fn parse_test_ability(raw: &str) -> crate::types::ability::AbilityDefinition {
        crate::parser::ability::parse_ability(raw).expect("test ability should parse")
    }

    #[test]
    fn object_with_registered_ability_has_no_unimplemented() {
        let mut obj = make_obj();
        obj.abilities
            .push(parse_test_ability("SP$ DealDamage | Cost$ R | NumDmg$ 3"));
        assert!(!has_unimplemented_mechanics(&obj));
    }

    #[test]
    fn object_with_unregistered_ability_has_unimplemented() {
        let mut obj = make_obj();
        obj.abilities
            .push(parse_test_ability("SP$ Fateseal | Cost$ U | Amount$ 2"));
        assert!(has_unimplemented_mechanics(&obj));
    }

    #[test]
    fn has_unimplemented_via_game_object_method() {
        let mut obj = make_obj();
        assert!(!obj.has_unimplemented_mechanics());
        obj.keywords.push(Keyword::Unknown("Bogus".to_string()));
        assert!(obj.has_unimplemented_mechanics());
    }

    #[test]
    #[cfg(feature = "forge-compat")]
    fn ci_passes_when_all_cards_supported() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(
            tmp.path(),
            "bolt",
            "Name:Lightning Bolt\nManaCost:R\nTypes:Instant\nA:SP$ DealDamage | Cost$ R | NumDmg$ 3\nOracle:Deal 3.",
        );
        create_card_file(
            tmp.path(),
            "bear",
            "Name:Bear\nManaCost:1 G\nTypes:Creature\nPT:2/2\nOracle:Vanilla.",
        );

        let db = CardDatabase::load(tmp.path()).unwrap();
        let summary = analyze_standard_coverage(&db);
        assert!(is_fully_covered(&summary));
    }

    #[test]
    #[cfg(feature = "forge-compat")]
    fn ci_fails_when_any_card_unsupported() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(
            tmp.path(),
            "bolt",
            "Name:Lightning Bolt\nManaCost:R\nTypes:Instant\nA:SP$ DealDamage | Cost$ R | NumDmg$ 3\nOracle:Deal 3.",
        );
        create_card_file(
            tmp.path(),
            "custom",
            "Name:Custom\nManaCost:U\nTypes:Instant\nA:SP$ Fateseal | Cost$ U\nOracle:Fateseal.",
        );

        let db = CardDatabase::load(tmp.path()).unwrap();
        let summary = analyze_standard_coverage(&db);
        assert!(!is_fully_covered(&summary));
    }
}
