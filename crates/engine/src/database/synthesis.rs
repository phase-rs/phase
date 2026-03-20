use std::str::FromStr;

use crate::database::mtgjson::AtomicCard;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, ContinuousModification,
    ControllerRef, Duration, Effect, ManaProduction, NinjutsuVariant, PtValue, StaticDefinition,
    TargetFilter, TriggerCondition, TriggerDefinition, TypedFilter,
};
use crate::types::card::{CardFace, CardLayout};
use crate::types::card_type::{CardType, CoreType, Supertype};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::phase::Phase;
use crate::types::triggers::TriggerMode;

// ---------------------------------------------------------------------------
// Shared helpers for building card faces from MTGJSON data
// ---------------------------------------------------------------------------

/// Internal layout classification from MTGJSON layout strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Single,
    Split,
    Flip,
    Transform,
    Meld,
    Adventure,
    Modal,
}

pub fn map_layout(layout_str: &str) -> LayoutKind {
    match layout_str {
        "normal" | "saga" | "class" | "case" | "leveler" => LayoutKind::Single,
        "split" => LayoutKind::Split,
        "flip" => LayoutKind::Flip,
        "transform" => LayoutKind::Transform,
        "meld" => LayoutKind::Meld,
        "adventure" => LayoutKind::Adventure,
        "modal_dfc" => LayoutKind::Modal,
        _ => LayoutKind::Single,
    }
}

pub fn build_card_type(mtgjson: &AtomicCard) -> CardType {
    let supertypes = mtgjson
        .supertypes
        .iter()
        .filter_map(|s| Supertype::from_str(s).ok())
        .collect();
    let core_types = mtgjson
        .types
        .iter()
        .filter_map(|s| CoreType::from_str(s).ok())
        .collect();
    let subtypes = mtgjson.subtypes.clone();
    CardType {
        supertypes,
        core_types,
        subtypes,
    }
}

pub fn map_mtgjson_color(code: &str) -> Option<ManaColor> {
    match code {
        "W" => Some(ManaColor::White),
        "U" => Some(ManaColor::Blue),
        "B" => Some(ManaColor::Black),
        "R" => Some(ManaColor::Red),
        "G" => Some(ManaColor::Green),
        _ => None,
    }
}

pub fn parse_pt_value(s: &str) -> PtValue {
    match s.parse::<i32>() {
        Ok(n) => PtValue::Fixed(n),
        Err(_) => PtValue::Variable(s.to_string()),
    }
}

pub fn layout_faces(layout: &CardLayout) -> Vec<&CardFace> {
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

// ---------------------------------------------------------------------------
// Synthesize functions — keyword → ability/trigger expansion
// ---------------------------------------------------------------------------

pub fn synthesize_basic_land_mana(face: &mut CardFace) {
    let land_mana: Vec<(&str, ManaColor)> = vec![
        ("Plains", ManaColor::White),
        ("Island", ManaColor::Blue),
        ("Swamp", ManaColor::Black),
        ("Mountain", ManaColor::Red),
        ("Forest", ManaColor::Green),
    ];

    for (subtype, color) in land_mana {
        if face.card_type.subtypes.iter().any(|s| s == subtype) {
            face.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![color],
                        },
                        restrictions: vec![],
                        expiry: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }
    }
}

pub fn synthesize_equip(face: &mut CardFace) {
    let equip_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            if let Keyword::Equip(cost) = kw {
                Some(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::Attach {
                            target: TargetFilter::Typed(
                                TypedFilter::creature().controller(ControllerRef::You),
                            ),
                        },
                    )
                    .cost(AbilityCost::Mana { cost: cost.clone() }),
                )
            } else {
                None
            }
        })
        .collect();

    face.abilities.extend(equip_abilities);
}

/// CR 702.49: Synthesize marker activated abilities for the Ninjutsu family
/// (Ninjutsu, Sneak, WebSlinging). The actual activation is handled by the
/// GameAction::ActivateNinjutsu path, not by normal activated ability resolution.
pub fn synthesize_ninjutsu_family(face: &mut CardFace) {
    let abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            let (variant, cost) = match kw {
                Keyword::Ninjutsu(c) => (NinjutsuVariant::Ninjutsu, c),
                Keyword::Sneak(c) => (NinjutsuVariant::Sneak, c),
                Keyword::WebSlinging(c) => (NinjutsuVariant::WebSlinging, c),
                _ => return None,
            };
            Some(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Unimplemented {
                        name: format!("{variant:?}"),
                        description: None,
                    },
                )
                .cost(AbilityCost::NinjutsuFamily {
                    variant,
                    mana_cost: cost.clone(),
                }),
            )
        })
        .collect();
    face.abilities.extend(abilities);
}

// Warp is handled at runtime via Keyword::Warp(ManaCost):
// - `prepare_spell_cast` overrides the mana cost when cast from hand
// - `stack.rs::resolve_top` creates a delayed exile trigger on resolution

/// Synthesize Mobilize N trigger: when this creature attacks, create N 1/1 red
/// Warrior creature tokens tapped and attacking. Sacrifice them at end of combat.
pub fn synthesize_mobilize(face: &mut CardFace) {
    use crate::types::ability::PtValue;
    use crate::types::triggers::TriggerMode;

    for kw in &face.keywords {
        if let Keyword::Mobilize(qty) = kw {
            let token_effect = Effect::Token {
                name: "Warrior".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Warrior".to_string()],
                colors: vec![ManaColor::Red],
                keywords: vec![],
                tapped: true,
                count: qty.clone(),
                attach_to: None,
                enters_attacking: true,
            };

            face.triggers.push(
                TriggerDefinition::new(TriggerMode::Attacks)
                    .execute(
                        AbilityDefinition::new(AbilityKind::Spell, token_effect)
                            .duration(Duration::UntilEndOfCombat),
                    )
                    .description(
                        "Mobilize — create Warrior tokens tapped and attacking".to_string(),
                    ),
            );
        }
    }
}

/// If the card has Changeling as a printed keyword, emit a characteristic-defining
/// static ability that grants all creature types (expanded at runtime via
/// `GameState::all_creature_types`).
pub fn synthesize_changeling_cda(face: &mut CardFace) {
    if face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Changeling))
    {
        face.static_abilities.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddAllCreatureTypes])
                .cda(),
        );
    }
}

/// Synthesize `additional_cost` from `Keyword::Kicker(ManaCost)`.
///
/// If the card has Kicker and no additional_cost was already parsed from Oracle text
/// (blight takes precedence since it's parsed from the "as an additional cost" line),
/// set `additional_cost = Some(AdditionalCost::Optional(AbilityCost::Mana { cost }))`.
pub fn synthesize_kicker(face: &mut CardFace) {
    if face.additional_cost.is_some() {
        return;
    }
    if let Some(cost) = face.keywords.iter().find_map(|k| match k {
        Keyword::Kicker(cost) => Some(cost.clone()),
        _ => None,
    }) {
        face.additional_cost = Some(AdditionalCost::Optional(AbilityCost::Mana { cost }));
    }
}

/// CR 719.2: Synthesize the intrinsic Case auto-solve trigger.
/// Every Case with a solve condition has: "At the beginning of your end step,
/// if this Case is not solved and its requirement is met, it becomes solved."
pub fn synthesize_case_solve(face: &mut CardFace) {
    if !face.card_type.subtypes.iter().any(|s| s == "Case") {
        return;
    }
    if face.solve_condition.is_none() {
        return;
    }

    face.triggers.push(
        TriggerDefinition::new(TriggerMode::Phase)
            .phase(Phase::End)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SolveCase,
            ))
            .condition(TriggerCondition::SolveConditionMet)
            .description("CR 719.2: Case auto-solve at end step".to_string()),
    );
}

/// Run all synthesis functions in canonical order on a card face.
/// Both `oracle_loader.rs` and `oracle_gen.rs` call this to ensure the same
/// complete set of synthesizers is applied.
pub fn synthesize_all(face: &mut CardFace) {
    synthesize_basic_land_mana(face);
    synthesize_equip(face);
    synthesize_ninjutsu_family(face);
    synthesize_changeling_cda(face);
    synthesize_kicker(face);
    synthesize_case_solve(face);
    // Warp: no synthesis needed — runtime handled by Keyword::Warp directly
    synthesize_mobilize(face);
}
