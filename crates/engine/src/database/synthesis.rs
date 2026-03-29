use std::str::FromStr;

use crate::database::mtgjson::{parse_mtgjson_mana_cost, AtomicCard};
use crate::game::printed_cards::derive_colors_from_mana_cost;
use crate::parser::oracle::parse_oracle_text;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, ContinuousModification,
    ControllerRef, Duration, Effect, ManaProduction, NinjutsuVariant, PtValue, QuantityExpr,
    RuntimeHandler, StaticDefinition, TargetFilter, TriggerCondition, TriggerDefinition,
    TypedFilter,
};
use crate::types::card::{CardFace, CardLayout};
use crate::types::card_type::{CardType, CoreType, Supertype};
use crate::types::keywords::{Keyword, PartnerType};
use crate::types::mana::{ManaColor, ManaCost};
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
                Keyword::CommanderNinjutsu(c) => (NinjutsuVariant::CommanderNinjutsu, c),
                Keyword::Sneak(c) => (NinjutsuVariant::Sneak, c),
                Keyword::WebSlinging(c) => (NinjutsuVariant::WebSlinging, c),
                _ => return None,
            };
            Some(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::RuntimeHandled {
                        handler: RuntimeHandler::NinjutsuFamily,
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
                owner: TargetFilter::Controller,
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

/// Synthesize Gift optional cost and delivery effect.
/// Gift is a promise (zero-cost optional additional cost) that sets `additional_cost_paid`
/// when the player promises the gift. Conditional branches ("if the gift was promised" /
/// "wasn't promised") are handled by the parser via `strip_additional_cost_conditional`.
///
/// Gift delivery (opponent receives the gift) is injected as a `GiftDelivery` effect
/// wrapping the first spell ability. The delivery checks `additional_cost_paid` at
/// resolution time — if the gift wasn't promised, it's a no-op and the spell resolves
/// normally. If promised, the opponent receives the gift before the spell's other effects.
pub fn synthesize_gift(face: &mut CardFace) {
    if face.additional_cost.is_some() {
        return;
    }
    // Use rfind (last match) because the MTGJSON bare "Gift" keyword defaults to
    // Gift(Card), while the Oracle-parsed keyword (e.g., Gift(TappedFish)) comes later
    // and is always the correct, specific kind.
    let gift_kind = face.keywords.iter().rev().find_map(|k| match k {
        Keyword::Gift(kind) => Some(kind.clone()),
        _ => None,
    });
    let Some(gift_kind) = gift_kind else {
        return;
    };

    // Gift uses a zero-cost optional additional cost — the "cost" is just a decision.
    face.additional_cost = Some(AdditionalCost::Optional(AbilityCost::Mana {
        cost: ManaCost::zero(),
    }));

    // Inject GiftDelivery as a wrapper around the first spell ability.
    // The delivery effect is a no-op when the gift wasn't promised, so the
    // chain always flows through to the spell's normal effects.
    if let Some(first_ability) = face.abilities.first_mut() {
        let original = std::mem::replace(
            first_ability,
            AbilityDefinition::new(AbilityKind::Spell, Effect::GiftDelivery { kind: gift_kind }),
        );
        first_ability.sub_ability = Some(Box::new(original));
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

/// CR 702.87a: Synthesize level up activated ability — "Pay {cost}: Put a level counter
/// on this permanent. Activate only as a sorcery."
pub fn synthesize_level_up(face: &mut CardFace) {
    use crate::types::ability::ActivationRestriction;

    let level_up_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            if let Keyword::LevelUp(cost) = kw {
                // CR 702.87a: Level up is an activated ability, sorcery-speed only.
                Some(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::PutCounter {
                            counter_type: "level".to_string(),
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::SelfRef,
                        },
                    )
                    .cost(AbilityCost::Mana { cost: cost.clone() })
                    .activation_restrictions(vec![ActivationRestriction::AsSorcery]),
                )
            } else {
                None
            }
        })
        .collect();

    face.abilities.extend(level_up_abilities);
}

/// Brawl variant of CR 903.3: determine if a card can be a Brawl commander.
/// Uses the union of MTGJSON's `leadershipSkills.brawl` (which catches Vehicles/Spacecraft)
/// and our own type-line check (legendary creature or legendary planeswalker, or
/// "can be your commander" in Oracle text).
pub fn compute_brawl_commander(mtgjson: &super::mtgjson::AtomicCard, face: &CardFace) -> bool {
    // Source 1: MTGJSON leadership skills (catches Legendary Vehicles etc.)
    let mtgjson_says = mtgjson
        .leadership_skills
        .as_ref()
        .is_some_and(|ls| ls.brawl);

    // Source 2: type-line analysis
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
    let is_planeswalker = face.card_type.core_types.contains(&CoreType::Planeswalker);
    let explicitly_allowed = face
        .oracle_text
        .as_ref()
        .is_some_and(|text| text.to_ascii_lowercase().contains("can be your commander"));
    let type_line_says = (is_legendary && (is_creature || is_planeswalker)) || explicitly_allowed;

    mtgjson_says || type_line_says
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
    synthesize_gift(face);
    synthesize_case_solve(face);
    // Warp: no synthesis needed — runtime handled by Keyword::Warp directly
    synthesize_mobilize(face);
    synthesize_level_up(face);
}

/// Build a `CardFace` from MTGJSON data, running the Oracle text parser and all synthesis.
/// Both `oracle_loader.rs` and `oracle_gen.rs` call this to ensure identical processing.
pub fn build_oracle_face(mtgjson: &AtomicCard, oracle_id: Option<String>) -> CardFace {
    build_oracle_face_inner(mtgjson, oracle_id, false)
}

/// Build an Oracle face for a multi-face card, skipping MTGJSON keywords
/// to prevent cross-face keyword leakage (B8: Saga back-face keyword contamination).
pub fn build_oracle_face_multi(mtgjson: &AtomicCard, oracle_id: Option<String>) -> CardFace {
    build_oracle_face_inner(mtgjson, oracle_id, true)
}

fn build_oracle_face_inner(
    mtgjson: &AtomicCard,
    oracle_id: Option<String>,
    skip_mtgjson_keywords: bool,
) -> CardFace {
    let card_type = build_card_type(mtgjson);
    // Raw MTGJSON keyword names (lowercased) for keyword-only line detection.
    // Still needed for keyword line detection even when skipping MTGJSON keywords.
    let mtgjson_keyword_names: Vec<String> = mtgjson
        .keywords
        .as_ref()
        .map(|kws| kws.iter().map(|s| s.to_ascii_lowercase()).collect())
        .unwrap_or_default();
    let parser_keyword_names: Vec<String> = if skip_mtgjson_keywords {
        vec!["__force_keyword_extract__".to_string()]
    } else {
        mtgjson_keyword_names.clone()
    };

    // B8: For multi-face cards, skip MTGJSON-provided keywords entirely.
    // MTGJSON duplicates keywords across both faces of Transform/DFC cards,
    // causing the front face to incorrectly gain back-face keywords.
    // Parser-extracted keywords from `extract_keyword_line` are face-specific.
    let mut keywords: Vec<Keyword> = if skip_mtgjson_keywords {
        Vec::new()
    } else {
        mtgjson
            .keywords
            .as_ref()
            .map(|kws| {
                kws.iter()
                    .map(|s| s.parse::<Keyword>().unwrap())
                    .filter(|k| !matches!(k, Keyword::Unknown(_)))
                    .collect()
            })
            .unwrap_or_default()
    };

    let oracle_text = mtgjson.text.as_deref().unwrap_or("");
    let face_name = mtgjson.face_name.as_deref().unwrap_or(&mtgjson.name);

    let types: Vec<String> = mtgjson.types.clone();
    let subtypes: Vec<String> = mtgjson.subtypes.clone();

    let parsed = parse_oracle_text(
        oracle_text,
        face_name,
        &parser_keyword_names,
        &types,
        &subtypes,
    );

    // Merge keywords extracted from Oracle text with MTGJSON keywords
    keywords.extend(parsed.extracted_keywords);

    // CR 702.124c: "Partner with [Name]" — upgrade Generic → With(name).
    // MTGJSON sends both "Partner" and "Partner with" keywords; the former produces
    // Partner(Generic) via FromStr. Scan Oracle text for the actual partner name.
    if mtgjson_keyword_names.contains(&"partner with".to_string()) {
        let lower_oracle = oracle_text.to_lowercase();
        if let Some(line) = lower_oracle
            .lines()
            .find(|l| l.starts_with("partner with "))
        {
            let rest = &line["partner with ".len()..];
            // Name ends at first '(' (reminder text) or end of line
            let name = rest.find('(').map(|i| &rest[..i]).unwrap_or(rest).trim();
            if !name.is_empty() {
                // Extract original-case name from the raw oracle text
                let original_name = mtgjson
                    .text
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("partner with "))
                    .map(|l| {
                        let r = &l["Partner with ".len()..];
                        r.find('(').map(|i| &r[..i]).unwrap_or(r).trim().to_string()
                    })
                    .unwrap_or_else(|| name.to_string());

                // Upgrade any Generic partner to With(name)
                for kw in &mut keywords {
                    if matches!(kw, Keyword::Partner(PartnerType::Generic)) {
                        *kw = Keyword::Partner(PartnerType::With(original_name.clone()));
                        break;
                    }
                }
            }
        }
    }

    // CR 702.124: Deduplicate — if any non-Generic partner variant exists,
    // remove stale Partner(Generic) entries (e.g., MTGJSON "Partner" keyword
    // producing Generic when Oracle text has "Partner—Friends forever").
    let has_specific_partner = keywords
        .iter()
        .any(|kw| matches!(kw, Keyword::Partner(pt) if !matches!(pt, PartnerType::Generic)));
    if has_specific_partner {
        keywords.retain(|kw| !matches!(kw, Keyword::Partner(PartnerType::Generic)));
    }

    // CR 702.11c: Deduplicate — if any HexproofFrom variant exists, remove
    // bare Hexproof (MTGJSON sends both "Hexproof" and "Hexproof from [quality]").
    let has_hexproof_from = keywords
        .iter()
        .any(|kw| matches!(kw, Keyword::HexproofFrom(_)));
    if has_hexproof_from {
        keywords.retain(|kw| !matches!(kw, Keyword::Hexproof));
    }

    let mana_cost = mtgjson
        .mana_cost
        .as_deref()
        .map(parse_mtgjson_mana_cost)
        .unwrap_or_default();

    let mana_derived_colors = derive_colors_from_mana_cost(&mana_cost);
    let mtgjson_colors: Vec<ManaColor> = mtgjson
        .colors
        .iter()
        .filter_map(|c| map_mtgjson_color(c))
        .collect();
    let color_override = if mtgjson_colors != mana_derived_colors {
        Some(mtgjson_colors)
    } else {
        None
    };

    let mut face = CardFace {
        name: face_name.to_string(),
        mana_cost,
        card_type,
        power: mtgjson.power.as_ref().map(|s| parse_pt_value(s)),
        toughness: mtgjson.toughness.as_ref().map(|s| parse_pt_value(s)),
        loyalty: mtgjson.loyalty.clone(),
        defense: mtgjson.defense.clone(),
        oracle_text: mtgjson.text.clone(),
        non_ability_text: None,
        flavor_name: None,
        keywords,
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        color_override,
        scryfall_oracle_id: oracle_id,
        modal: parsed.modal,
        additional_cost: parsed.additional_cost,
        strive_cost: parsed.strive_cost,
        casting_restrictions: parsed.casting_restrictions,
        casting_options: parsed.casting_options,
        solve_condition: parsed.solve_condition,
        brawl_commander: false,
    };

    face.brawl_commander = compute_brawl_commander(mtgjson, &face);
    synthesize_all(&mut face);
    face
}
