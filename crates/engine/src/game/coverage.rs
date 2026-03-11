use crate::database::CardDatabase;
use crate::game::effects::is_known_effect;
use crate::game::game_object::GameObject;
use crate::game::static_abilities::{build_static_registry, StaticAbilityHandler};
use crate::game::triggers::build_trigger_registry;
use crate::types::ability::{
    effect_variant_name, AbilityDefinition, StaticDefinition, TriggerDefinition,
};
use crate::types::keywords::Keyword;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
        let api = effect_variant_name(&def.effect);
        if !api.is_empty() && !is_known_effect(api) {
            missing.push(format!("Effect: {api}"));
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
    use crate::types::ability::{AbilityKind, DamageAmount, Effect, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
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
        obj.abilities.push(crate::types::ability::AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetFilter::Any,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        });
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_unregistered_ability_has_unimplemented() {
        let mut obj = make_obj();
        obj.abilities.push(crate::types::ability::AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::Unimplemented {
                name: "Fateseal".to_string(),
                description: None,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        });
        assert!(!unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn has_unimplemented_via_game_object_method() {
        let mut obj = make_obj();
        assert!(!obj.has_unimplemented_mechanics());
        obj.keywords.push(Keyword::Unknown("Bogus".to_string()));
        assert!(obj.has_unimplemented_mechanics());
    }
}
