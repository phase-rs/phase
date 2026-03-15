use crate::database::legality::LegalityFormat;
use crate::database::CardDatabase;
use crate::game::game_object::GameObject;
use crate::game::static_abilities::{build_static_registry, StaticAbilityHandler};
use crate::game::triggers::build_trigger_registry;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AdditionalCost, Effect, ReplacementDefinition, ReplacementMode,
    StaticDefinition, TriggerDefinition,
};
use crate::types::card::CardFace;
use crate::types::keywords::Keyword;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

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
    #[serde(default)]
    pub coverage_by_format: BTreeMap<String, FormatCoverageSummary>,
    pub cards: Vec<CardCoverageResult>,
    pub missing_handler_frequency: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FormatCoverageSummary {
    pub total_cards: usize,
    pub supported_cards: usize,
    pub coverage_pct: f64,
}

/// Check whether a game object has any mechanics the engine cannot handle.
///
/// Checks keywords (Unknown variant = unrecognized), abilities (api_type
/// not in effect registry), triggers (mode not in trigger registry), and
/// static abilities (mode not in static registry).
pub fn unimplemented_mechanics(obj: &GameObject) -> Vec<String> {
    let mut missing = Vec::new();

    // 1. Any Unknown keyword means the parser didn't recognize it
    for kw in &obj.keywords {
        if let Keyword::Unknown(s) = kw {
            missing.push(format!("Keyword: {s}"));
        }
    }

    // 2. Check abilities against known effect types
    for def in &obj.abilities {
        if let Effect::Unimplemented { name, .. } = &def.effect {
            missing.push(format!("Effect: {name}"));
        }
    }

    // 3. Check trigger modes against trigger registry
    let trigger_registry = build_trigger_registry();
    for trig in &obj.trigger_definitions {
        if matches!(&trig.mode, TriggerMode::Unknown(_))
            || !trigger_registry.contains_key(&trig.mode)
        {
            missing.push(format!("Trigger: {}", trig.mode));
        }
    }

    // 4. Check static ability modes against static registry
    let static_registry = build_static_registry();
    for stat in &obj.static_definitions {
        if !static_registry.contains_key(&stat.mode) {
            missing.push(format!("Static: {}", stat.mode));
        }
    }

    missing
}

/// Analyze card coverage by checking which cards have all their abilities,
/// triggers, keywords, and static abilities supported by the engine's registries.
pub fn analyze_coverage(card_db: &CardDatabase) -> CoverageSummary {
    let trigger_registry = build_trigger_registry();
    let static_registry = build_static_registry();

    let mut cards = Vec::new();
    let mut freq: HashMap<String, usize> = HashMap::new();
    let mut coverage_by_format_accumulators: BTreeMap<String, (usize, usize)> = LegalityFormat::ALL
        .into_iter()
        .map(|format| (format.as_key().to_string(), (0, 0)))
        .collect();

    for (key, face) in card_db.face_iter() {
        let mut missing = Vec::new();

        // Check abilities
        check_abilities(&face.abilities, &mut missing);

        // Check additional cost
        check_additional_cost(&face.additional_cost, &mut missing);

        // Check triggers
        check_triggers(&face.triggers, &trigger_registry, &mut missing);

        // Check keywords
        check_keywords(&face.keywords, &mut missing);

        // Check static abilities
        check_statics(&face.static_abilities, &static_registry, &mut missing);

        // Check replacements
        check_replacements(&face.replacements, &mut missing);

        let supported = missing.is_empty();

        for m in &missing {
            *freq.entry(m.clone()).or_default() += 1;
        }

        for format in LegalityFormat::ALL {
            if card_db
                .legality_status(key, format)
                .is_some_and(|status| status.is_legal())
            {
                let entry = coverage_by_format_accumulators
                    .get_mut(format.as_key())
                    .expect("all legality formats must be pre-seeded");
                entry.0 += 1;
                if supported {
                    entry.1 += 1;
                }
            }
        }

        cards.push(CardCoverageResult {
            card_name: face.name.clone(),
            set_code: String::new(),
            supported,
            missing_handlers: missing,
        });
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

    let coverage_by_format = coverage_by_format_accumulators
        .into_iter()
        .map(|(format, (total_cards, supported_cards))| {
            let coverage_pct = if total_cards > 0 {
                (supported_cards as f64 / total_cards as f64) * 100.0
            } else {
                0.0
            };
            (
                format,
                FormatCoverageSummary {
                    total_cards,
                    supported_cards,
                    coverage_pct,
                },
            )
        })
        .collect();

    CoverageSummary {
        total_cards,
        supported_cards,
        coverage_pct,
        coverage_by_format,
        cards,
        missing_handler_frequency,
    }
}

pub fn card_face_has_unimplemented_parts(face: &CardFace) -> bool {
    ability_definitions_have_unimplemented_parts(&face.abilities)
        || face
            .additional_cost
            .as_ref()
            .is_some_and(additional_cost_has_unimplemented_parts)
        || face.triggers.iter().any(trigger_has_unimplemented_parts)
        || face
            .replacements
            .iter()
            .any(replacement_has_unimplemented_parts)
}

fn check_abilities(abilities: &[AbilityDefinition], missing: &mut Vec<String>) {
    for def in abilities {
        collect_ability_missing_parts(def, missing);
    }
}

fn check_triggers(
    triggers: &[TriggerDefinition],
    trigger_registry: &HashMap<TriggerMode, crate::game::triggers::TriggerMatcher>,
    missing: &mut Vec<String>,
) {
    for def in triggers {
        if let Some(execute) = &def.execute {
            collect_ability_missing_parts(execute, missing);
        }
        if matches!(&def.mode, TriggerMode::Unknown(_)) || !trigger_registry.contains_key(&def.mode)
        {
            let label = format!("Trigger:{}", def.mode);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_keywords(keywords: &[Keyword], missing: &mut Vec<String>) {
    for kw in keywords {
        if let Keyword::Unknown(s) = kw {
            let label = format!("Keyword:{}", s);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_additional_cost(additional_cost: &Option<AdditionalCost>, missing: &mut Vec<String>) {
    if let Some(additional_cost) = additional_cost {
        collect_additional_cost_missing_parts(additional_cost, missing);
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

fn check_replacements(replacements: &[ReplacementDefinition], missing: &mut Vec<String>) {
    for def in replacements {
        if let Some(execute) = &def.execute {
            collect_ability_missing_parts(execute, missing);
        }

        if let ReplacementMode::Optional {
            decline: Some(decline),
        } = &def.mode
        {
            collect_ability_missing_parts(decline, missing);
        }
    }
}

fn ability_definitions_have_unimplemented_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities
        .iter()
        .any(ability_definition_has_unimplemented_parts)
}

fn trigger_has_unimplemented_parts(trigger: &TriggerDefinition) -> bool {
    trigger
        .execute
        .as_ref()
        .is_some_and(|execute| ability_definition_has_unimplemented_parts(execute))
}

fn replacement_has_unimplemented_parts(replacement: &ReplacementDefinition) -> bool {
    replacement
        .execute
        .as_ref()
        .is_some_and(|execute| ability_definition_has_unimplemented_parts(execute))
        || matches!(
            &replacement.mode,
            ReplacementMode::Optional {
                decline: Some(decline),
            } if ability_definition_has_unimplemented_parts(decline)
        )
}

fn ability_definition_has_unimplemented_parts(def: &AbilityDefinition) -> bool {
    matches!(def.effect, Effect::Unimplemented { .. })
        || def
            .cost
            .as_ref()
            .is_some_and(ability_cost_has_unimplemented_parts)
        || def
            .sub_ability
            .as_ref()
            .is_some_and(|sub| ability_definition_has_unimplemented_parts(sub))
        || def
            .mode_abilities
            .iter()
            .any(ability_definition_has_unimplemented_parts)
}

fn additional_cost_has_unimplemented_parts(additional_cost: &AdditionalCost) -> bool {
    match additional_cost {
        AdditionalCost::Optional(cost) => ability_cost_has_unimplemented_parts(cost),
        AdditionalCost::Choice(first, second) => {
            ability_cost_has_unimplemented_parts(first)
                || ability_cost_has_unimplemented_parts(second)
        }
    }
}

fn ability_cost_has_unimplemented_parts(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Composite { costs } => costs.iter().any(ability_cost_has_unimplemented_parts),
        AbilityCost::Unimplemented { .. } => true,
        _ => false,
    }
}

fn collect_ability_missing_parts(def: &AbilityDefinition, missing: &mut Vec<String>) {
    if let Effect::Unimplemented { name, .. } = &def.effect {
        let label = format!("Effect:{name}");
        if !missing.contains(&label) {
            missing.push(label);
        }
    }

    if let Some(cost) = &def.cost {
        collect_ability_cost_missing_parts(cost, missing);
    }

    if let Some(sub_ability) = &def.sub_ability {
        collect_ability_missing_parts(sub_ability, missing);
    }

    for mode_ability in &def.mode_abilities {
        collect_ability_missing_parts(mode_ability, missing);
    }
}

fn collect_additional_cost_missing_parts(
    additional_cost: &AdditionalCost,
    missing: &mut Vec<String>,
) {
    match additional_cost {
        AdditionalCost::Optional(cost) => collect_ability_cost_missing_parts(cost, missing),
        AdditionalCost::Choice(first, second) => {
            collect_ability_cost_missing_parts(first, missing);
            collect_ability_cost_missing_parts(second, missing);
        }
    }
}

fn collect_ability_cost_missing_parts(cost: &AbilityCost, missing: &mut Vec<String>) {
    match cost {
        AbilityCost::Composite { costs } => {
            for nested_cost in costs {
                collect_ability_cost_missing_parts(nested_cost, missing);
            }
        }
        AbilityCost::Unimplemented { description } => {
            let label = format!("Cost:{description}");
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
        _ => {}
    }
}

/// Returns `true` if the coverage summary shows 100% support (CI pass).
/// Returns `false` if there are any unsupported cards (CI fail).
pub fn is_fully_covered(summary: &CoverageSummary) -> bool {
    summary.total_cards > 0 && summary.supported_cards == summary.total_cards
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::legality::{legalities_to_export_map, LegalityStatus};
    use crate::types::ability::{AbilityKind, DamageAmount, Effect, TargetFilter};
    use crate::types::card_type::CardType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

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
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_known_keyword_has_no_unimplemented() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        obj.keywords.push(Keyword::Haste);
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_unknown_keyword_has_unimplemented() {
        let mut obj = make_obj();
        obj.keywords
            .push(Keyword::Unknown("FutureKeyword".to_string()));
        assert!(!unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_registered_ability_has_no_unimplemented() {
        let mut obj = make_obj();
        obj.abilities
            .push(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: DamageAmount::Fixed(3),
                    target: TargetFilter::Any,
                },
            ));
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_unregistered_ability_has_unimplemented() {
        let mut obj = make_obj();
        obj.abilities
            .push(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Fateseal".to_string(),
                    description: None,
                },
            ));
        assert!(!unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn has_unimplemented_via_game_object_method() {
        let mut obj = make_obj();
        assert!(!obj.has_unimplemented_mechanics());
        obj.keywords.push(Keyword::Unknown("Bogus".to_string()));
        assert!(obj.has_unimplemented_mechanics());
    }

    fn make_face() -> CardFace {
        CardFace {
            name: "Test Card".to_string(),
            mana_cost: Default::default(),
            card_type: CardType::default(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![],
            abilities: vec![],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
        }
    }

    #[test]
    fn card_face_with_nested_mode_unimplemented_is_detected() {
        let mut face = make_face();
        face.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "modal".to_string(),
                    description: None,
                },
            )
            .with_modal(
                crate::types::ability::ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 1,
                    mode_descriptions: vec!["Mode".to_string()],
                    allow_repeat_modes: false,
                    constraints: vec![],
                },
                vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Unimplemented {
                        name: "nested".to_string(),
                        description: None,
                    },
                )],
            ),
        );

        assert!(card_face_has_unimplemented_parts(&face));
    }

    #[test]
    fn card_face_with_unimplemented_additional_cost_is_detected() {
        let mut face = make_face();
        face.additional_cost = Some(AdditionalCost::Optional(AbilityCost::Unimplemented {
            description: "mystery cost".to_string(),
        }));

        assert!(card_face_has_unimplemented_parts(&face));
    }

    #[test]
    fn card_face_with_replacement_decline_unimplemented_is_detected() {
        let mut face = make_face();
        face.replacements
            .push(ReplacementDefinition::new(ReplacementEvent::Draw).mode(
                ReplacementMode::Optional {
                    decline: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Unimplemented {
                            name: "decline".to_string(),
                            description: None,
                        },
                    ))),
                },
            ));

        assert!(card_face_has_unimplemented_parts(&face));
    }

    #[test]
    fn analyze_coverage_reports_legality_based_format_totals() {
        let supported = serde_json::json!({
            "alpha": {
                "name": "Alpha",
                "mana_cost": "NoCost",
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": legalities_to_export_map(&HashMap::from([
                    (LegalityFormat::Standard, LegalityStatus::Legal),
                    (LegalityFormat::Modern, LegalityStatus::Legal),
                ])),
            },
            "beta": {
                "name": "Beta",
                "mana_cost": "NoCost",
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [{
                    "kind": "Spell",
                    "effect": { "type": "Unimplemented", "name": "beta_gap", "description": null },
                    "cost": null,
                    "sub_ability": null,
                    "duration": null,
                    "description": null,
                    "target_prompt": null,
                    "sorcery_speed": false,
                    "condition": null,
                    "optional_targeting": false
                }],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": legalities_to_export_map(&HashMap::from([
                    (LegalityFormat::Standard, LegalityStatus::Legal),
                    (LegalityFormat::Commander, LegalityStatus::Legal),
                ])),
            }
        })
        .to_string();

        let db = CardDatabase::from_json_str(&supported).expect("test export should deserialize");
        let summary = analyze_coverage(&db);

        assert_eq!(summary.total_cards, 2);
        assert_eq!(summary.supported_cards, 1);
        assert_eq!(
            summary.coverage_by_format.get("standard"),
            Some(&FormatCoverageSummary {
                total_cards: 2,
                supported_cards: 1,
                coverage_pct: 50.0,
            })
        );
        assert_eq!(
            summary.coverage_by_format.get("modern"),
            Some(&FormatCoverageSummary {
                total_cards: 1,
                supported_cards: 1,
                coverage_pct: 100.0,
            })
        );
        assert_eq!(
            summary.coverage_by_format.get("commander"),
            Some(&FormatCoverageSummary {
                total_cards: 1,
                supported_cards: 0,
                coverage_pct: 0.0,
            })
        );
    }
}
