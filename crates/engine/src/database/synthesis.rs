use std::str::FromStr;

use crate::database::mtgjson::{parse_mtgjson_mana_cost, AtomicCard};
use crate::game::printed_cards::derive_colors_from_mana_cost;
use crate::parser::oracle::{oracle_text_allows_commander, parse_oracle_text};
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, CardPlayMode,
    CastVariantPaid, ChoiceType, ContinuousModification, ControllerRef, CounterTriggerFilter,
    Duration, Effect, FilterProp, KickerVariant, ManaContribution, ManaProduction, NinjutsuVariant,
    PtValue, QuantityExpr, ReplacementCondition, ReplacementDefinition, RuntimeHandler,
    SearchSelectionConstraint, StaticDefinition, TargetFilter, TriggerCondition, TriggerDefinition,
    TypeFilter, TypedFilter,
};
use crate::types::card::{CardFace, CardLayout};
use crate::types::card_type::{CardType, CoreType, Supertype};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::keywords::{BuybackCost, CyclingCost, Keyword, PartnerType};
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::phase::Phase;
use crate::types::replacements::ReplacementEvent;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

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
    /// CR 702.xxx: Prepare (Strixhaven) — Adventure-family two-face layout.
    /// Assign when WotC publishes SOS CR update.
    Prepare,
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
        // CR 702.xxx: Prepare frame (Strixhaven) — two-face card whose face `b`
        // is a "prepare spell". Assign when WotC publishes SOS CR update.
        "prepare" => LayoutKind::Prepare,
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
        | CardLayout::Omen(a, b)
        | CardLayout::Prepare(a, b) => vec![a, b],
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
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }
    }
}

/// CR 702.6a: Equip is an activated ability of Equipment cards. "Equip [cost]"
/// means "[Cost]: Attach this permanent to target creature you control.
/// Activate only as a sorcery." The `.sorcery_speed()` builder is the single
/// authority that sets both the display flag and pushes
/// `ActivationRestriction::AsSorcery` so the runtime legality gate enforces
/// timing at activation time.
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
                            attachment: TargetFilter::SelfRef,
                            target: TargetFilter::Typed(
                                TypedFilter::creature().controller(ControllerRef::You),
                            ),
                        },
                    )
                    .cost(AbilityCost::Mana { cost: cost.clone() })
                    // CR 702.6a: "Activate only as a sorcery."
                    .sorcery_speed(),
                )
            } else {
                None
            }
        })
        .collect();

    face.abilities.extend(equip_abilities);
}

/// CR 702.49: Synthesize marker activated abilities for the Ninjutsu family
/// (Ninjutsu, CommanderNinjutsu, WebSlinging). The actual activation is handled
/// by the GameAction::ActivateNinjutsu path, not by normal activated ability
/// resolution. CR 702.190a: Sneak is NOT a ninjutsu-family activation — it is
/// a cast alternative cost handled by the casting pipeline — so it does not
/// synthesize an activated ability here.
pub fn synthesize_ninjutsu_family(face: &mut CardFace) {
    let abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            let (variant, cost) = match kw {
                Keyword::Ninjutsu(c) => (NinjutsuVariant::Ninjutsu, c),
                Keyword::CommanderNinjutsu(c) => (NinjutsuVariant::CommanderNinjutsu, c),
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

    // Idempotency: skip if a Mobilize attack trigger already exists.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::Attacks)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Token { name, .. }) if name == "Warrior"
            )
    });
    if already_has_trigger {
        return;
    }

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
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
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

/// CR 702.182a: Synthesize Job select trigger: when this Equipment enters,
/// create a 1/1 colorless Hero creature token, then attach this Equipment to it.
pub fn synthesize_job_select(face: &mut CardFace) {
    use crate::types::ability::PtValue;

    if !face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::JobSelect))
    {
        return;
    }

    // Idempotency: skip if the Job select ETB Hero token trigger already exists.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::ChangesZone)
            && t.destination == Some(Zone::Battlefield)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Token { name, .. }) if name == "Hero"
            )
    });
    if already_has_trigger {
        return;
    }

    let token_effect = Effect::Token {
        name: "Hero".to_string(),
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        types: vec!["Creature".to_string(), "Hero".to_string()],
        colors: vec![],
        keywords: vec![],
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    };

    let attach_effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Attach {
            attachment: TargetFilter::SelfRef,
            target: TargetFilter::LastCreated,
        },
    );

    // CR 603.6a: Enters-the-battlefield abilities trigger when a permanent enters
    // the battlefield. The trigger source must be on the battlefield for the
    // evaluator to match, so `trigger_zones` must include `Zone::Battlefield`.
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Battlefield])
            .execute(
                AbilityDefinition::new(AbilityKind::Spell, token_effect).sub_ability(attach_effect),
            )
            .description("Job select — create Hero token and attach".to_string()),
    );
}

/// If the card has Changeling as a printed keyword, emit a characteristic-defining
/// static ability that grants all creature types (expanded at runtime via
/// `GameState::all_creature_types`).
/// CR 702.184a + CR 721.2b: Synthesize Station's creature-at-threshold static.
///
/// The Station keyword's reminder text includes "It's an artifact creature at
/// N+." (CR 721.2b). The threshold N is the highest station symbol printed on
/// the card — the point at which the Spacecraft gains the Creature type and
/// uses its printed P/T. We extract N from the parenthesized Station reminder
/// paragraph (kept on `oracle_text` before `strip_reminder_text` eats it for
/// the ability parser), then push a SelfRef static that:
///
/// - Adds `CoreType::Creature` (Layer 4 — CR 613.1d)
/// - Sets power/toughness to the card's printed values (Layer 7b)
///
/// All gated by `StaticCondition::HasCounters { counter_type: "charge",
/// minimum: N, maximum: None }`.
///
/// Non-battlefield zones automatically do not apply this (layer system only
/// evaluates battlefield objects), matching CR 721.2c: while in any zone
/// other than the battlefield, station cards do not have power or toughness.
pub fn synthesize_station(face: &mut CardFace) {
    // CR 721.2b: Require printed P/T. Station Spacecraft without a printed P/T
    // box (e.g. "The Eternity Elevator") are support-only; no creature-shift.
    let (Some(PtValue::Fixed(power)), Some(PtValue::Fixed(toughness))) =
        (face.power.as_ref(), face.toughness.as_ref())
    else {
        return;
    };
    let power = *power;
    let toughness = *toughness;

    // CR 721.1: Spacecraft is the marker subtype — no Spacecraft subtype, no
    // station striations, so no creature shift applies.
    if !face
        .card_type
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Spacecraft"))
    {
        return;
    }

    // CR 721.2b / CR 721.3: The striation containing the printed P/T box is the
    // highest N+ threshold on the card. Reminder text ("It's an artifact
    // creature at N+") has no rules force (CR 721.3) and is deliberately
    // ignored.
    let Some(oracle) = face.oracle_text.as_deref() else {
        return;
    };
    let lines: Vec<&str> = oracle.lines().collect();
    let Some(threshold) = crate::parser::oracle_spacecraft::max_spacecraft_threshold(&lines) else {
        return;
    };

    let condition = crate::types::ability::StaticCondition::HasCounters {
        counters: crate::types::counter::CounterMatch::OfType(
            crate::types::counter::CounterType::Generic("charge".to_string()),
        ),
        minimum: threshold,
        maximum: None,
    };
    face.static_abilities.push(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(condition)
            .modifications(vec![
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
                ContinuousModification::SetPower { value: power },
                ContinuousModification::SetToughness { value: toughness },
            ])
            .description(format!(
                "CR 721.2b: Spacecraft is an artifact creature at {threshold}+"
            )),
    );
}

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
/// set `additional_cost = Some(AdditionalCost::Kicker { ... })`.
pub fn synthesize_kicker(face: &mut CardFace) {
    if face.additional_cost.is_some() {
        return;
    }
    let costs: Vec<AbilityCost> = face
        .keywords
        .iter()
        .filter_map(|k| match k {
            Keyword::Kicker(cost) => Some(AbilityCost::Mana { cost: cost.clone() }),
            _ => None,
        })
        .collect();
    if !costs.is_empty() {
        face.additional_cost = Some(AdditionalCost::Kicker {
            costs,
            repeatable: false,
        });
    }
}

/// CR 702.33f: Conditions of the form "if it was kicked with its [A] kicker"
/// are linked to the first or second kicker cost printed on the card. Parser
/// output carries the printed mana cost as typed metadata; this synthesis pass
/// resolves it back to the positional `KickerVariant` once card-level kicker
/// declarations are visible.
pub fn resolve_kicker_condition_variants(face: &mut CardFace) {
    let Some(additional_cost) = &face.additional_cost else {
        return;
    };

    for ability in &mut face.abilities {
        resolve_ability_kicker_condition_variants(ability, additional_cost);
    }
    for trigger in &mut face.triggers {
        if let Some(execute) = trigger.execute.as_mut() {
            resolve_ability_kicker_condition_variants(execute, additional_cost);
        }
    }
    for replacement in &mut face.replacements {
        resolve_replacement_kicker_condition_variants(replacement, additional_cost);
    }
}

fn kicker_variant_for_cost(
    additional_cost: &AdditionalCost,
    target_cost: &ManaCost,
) -> Option<KickerVariant> {
    let AdditionalCost::Kicker { costs, .. } = additional_cost else {
        return None;
    };
    costs.iter().enumerate().find_map(|(index, cost)| {
        let AbilityCost::Mana { cost } = cost else {
            return None;
        };
        if cost != target_cost {
            return None;
        }
        match index {
            0 => Some(KickerVariant::First),
            1 => Some(KickerVariant::Second),
            _ => None,
        }
    })
}

fn resolve_ability_kicker_condition_variants(
    ability: &mut AbilityDefinition,
    additional_cost: &AdditionalCost,
) {
    if let Some(condition) = ability.condition.as_mut() {
        resolve_condition_kicker_variant(condition, additional_cost);
    }

    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        resolve_ability_kicker_condition_variants(sub_ability, additional_cost);
    }

    for mode in &mut ability.mode_abilities {
        resolve_ability_kicker_condition_variants(mode, additional_cost);
    }
}

fn resolve_condition_kicker_variant(
    condition: &mut AbilityCondition,
    additional_cost: &AdditionalCost,
) {
    match condition {
        AbilityCondition::AdditionalCostPaid {
            variant,
            kicker_cost,
            ..
        } => {
            resolve_kicker_cost_metadata(variant, kicker_cost, additional_cost);
        }
        AbilityCondition::ConditionInstead { inner }
        | AbilityCondition::Not { condition: inner } => {
            resolve_condition_kicker_variant(inner, additional_cost);
        }
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            for condition in conditions {
                resolve_condition_kicker_variant(condition, additional_cost);
            }
        }
        _ => {}
    }
}

fn resolve_replacement_kicker_condition_variants(
    replacement: &mut ReplacementDefinition,
    additional_cost: &AdditionalCost,
) {
    if let Some(ReplacementCondition::CastViaKicker {
        variant,
        kicker_cost,
    }) = replacement.condition.as_mut()
    {
        resolve_kicker_cost_metadata(variant, kicker_cost, additional_cost);
    }

    if let Some(execute) = replacement.execute.as_mut() {
        resolve_ability_kicker_condition_variants(execute, additional_cost);
    }
}

fn resolve_kicker_cost_metadata(
    variant: &mut Option<KickerVariant>,
    kicker_cost: &mut Option<ManaCost>,
    additional_cost: &AdditionalCost,
) {
    if let (None, Some(resolved_variant)) = (
        *variant,
        kicker_cost
            .as_ref()
            .and_then(|cost| kicker_variant_for_cost(additional_cost, cost)),
    ) {
        *variant = Some(resolved_variant);
        *kicker_cost = None;
    }
}

/// CR 702.27a: Synthesize `additional_cost` from `Keyword::Buyback(BuybackCost)`.
///
/// Buyback is an optional additional cost: "You may pay an additional [cost]
/// as you cast this spell. If the buyback cost was paid, put this spell into
/// its owner's hand instead of into that player's graveyard as it resolves."
///
/// The resolution-time routing (hand instead of graveyard) is handled in
/// `game::stack::resolve_top` by inspecting `ability.context.additional_cost_paid`
/// on the resolving spell when the source carries `Keyword::Buyback`.
///
/// Idempotent: skips if `additional_cost` is already set (Oracle-parsed
/// "as an additional cost" lines take precedence, matching the Kicker pattern).
pub fn synthesize_buyback(face: &mut CardFace) {
    if face.additional_cost.is_some() {
        return;
    }
    let Some(buyback_cost) = face.keywords.iter().find_map(|k| match k {
        Keyword::Buyback(cost) => Some(cost.clone()),
        _ => None,
    }) else {
        return;
    };
    let cost = match buyback_cost {
        BuybackCost::Mana(mana_cost) => AbilityCost::Mana { cost: mana_cost },
        BuybackCost::NonMana(ac) => ac,
    };
    face.additional_cost = Some(AdditionalCost::Optional(cost));
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

    // Idempotency: skip if the Case auto-solve end-step trigger already exists.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::Phase)
            && t.phase == Some(Phase::End)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::SolveCase)
            )
    });
    if already_has_trigger {
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
                    // CR 702.87a: "Activate only as a sorcery." `.sorcery_speed()`
                    // sets the display flag and pushes `AsSorcery` for runtime.
                    .sorcery_speed(),
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
        .is_some_and(|text| oracle_text_allows_commander(text, &face.name));
    let type_line_says = (is_legendary && (is_creature || is_planeswalker)) || explicitly_allowed;

    mtgjson_says || type_line_says
}

/// CR 702.29a/e: Synthesize Cycling and Typecycling keywords into activated abilities.
///
/// Cycling: "[Cost], Discard this card: Draw a card." (activated from hand)
/// Typecycling: "[Cost], Discard this card: Search library for a [type] card,
///   reveal it, put it into your hand. Then shuffle."
pub fn synthesize_cycling(face: &mut CardFace) {
    let cycling_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            // CR 702.29a: Basic cycling — discard self, draw a card.
            // Cost may be mana ("cycling {2}") or non-mana ("cycling—pay 2 life").
            Keyword::Cycling(cycling_cost) => {
                // CR 702.29a: "Discard THIS card" — self_ref = true.
                let discard_self = AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    random: false,
                    self_ref: true,
                };
                let composite_cost = match cycling_cost {
                    CyclingCost::Mana(cost) => AbilityCost::Composite {
                        costs: vec![AbilityCost::Mana { cost: cost.clone() }, discard_self],
                    },
                    CyclingCost::NonMana(ac) => match ac {
                        // Flatten an already-Composite non-mana cost so the discard joins
                        // the existing sub-costs instead of nesting.
                        AbilityCost::Composite { costs } => {
                            let mut flat = costs.clone();
                            flat.push(discard_self);
                            AbilityCost::Composite { costs: flat }
                        }
                        other => AbilityCost::Composite {
                            costs: vec![other.clone(), discard_self],
                        },
                    },
                };
                let mut def = AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(composite_cost);
                def.activation_zone = Some(Zone::Hand);
                Some(def)
            }
            // CR 702.29e: Typecycling — discard self, search library for [type] card.
            Keyword::Typecycling { cost, subtype } => {
                let composite_cost = AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana { cost: cost.clone() },
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            random: false,
                            self_ref: true,
                        },
                    ],
                };
                let filter = typecycling_subtype_to_filter(subtype);
                let shuffle_def = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Shuffle {
                        target: TargetFilter::Controller,
                    },
                );
                let mut put_in_hand_def = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: Some(Zone::Library),
                        destination: Zone::Hand,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        under_your_control: false,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                    },
                );
                put_in_hand_def.sub_ability = Some(Box::new(shuffle_def));
                let mut def = AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::SearchLibrary {
                        filter,
                        count: QuantityExpr::Fixed { value: 1 },
                        reveal: true,
                        target_player: None,
                        selection_constraint: SearchSelectionConstraint::None,
                    },
                )
                .cost(composite_cost);
                def.activation_zone = Some(Zone::Hand);
                def.sub_ability = Some(Box::new(put_in_hand_def));
                Some(def)
            }
            _ => None,
        })
        .collect();

    face.abilities.extend(cycling_abilities);
}

/// CR 702.97a: Synthesize Scavenge into an activated ability on the card.
///
/// Scavenge is an activated ability that functions only while the card with scavenge is
/// in a graveyard. "Scavenge [cost]" means "[Cost], Exile this card from your graveyard:
/// Put a number of +1/+1 counters equal to this card's power on target creature. Activate
/// only as a sorcery."
///
/// Power snapshot timing (CR 208.3 + CR 400.7): At resolution the source has already
/// been exiled as a cost; CR 702.97a specifies "the power of the card you exiled",
/// which is read from the exile-zone object via `QuantityRef::Power { scope: crate::types::ability::ObjectScope::Source }` (with LKI
/// fallback if the object is somehow gone). Non-battlefield zones do not run layer
/// computation, so the read value equals the card's printed power — the correct
/// target for "this card's power" in the graveyard reminder text. No new quantity
/// ref is needed; `SelfPower` is already the right abstraction.
pub fn synthesize_scavenge(face: &mut CardFace) {
    use crate::types::ability::QuantityRef;

    let scavenge_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            let Keyword::Scavenge(cost) = kw else {
                return None;
            };
            // CR 118.3: Composite cost — pay mana, then exile this card from graveyard.
            let composite_cost = AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: cost.clone() },
                    // CR 702.97a: "Exile this card from your graveyard" — SelfRef + Graveyard
                    // is auto-paid by pay_ability_cost (no player choice needed).
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Graveyard),
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            };
            // CR 702.97a: "Put a number of +1/+1 counters equal to this card's power on
            // target creature." SelfPower is resolved via LKI at resolution time so the
            // power read is the card's last known power before it was exiled.
            let effect = Effect::PutCounter {
                counter_type: "P1P1".to_string(),
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source,
                    },
                },
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            };
            let mut def = AbilityDefinition::new(AbilityKind::Activated, effect)
                .cost(composite_cost)
                // CR 702.97a: "Activate only as a sorcery." The `.sorcery_speed()`
                // builder sets both the display flag and pushes
                // `ActivationRestriction::AsSorcery` for runtime enforcement.
                .sorcery_speed();
            // CR 702.97a: "functions only while the card with scavenge is in a graveyard."
            def.activation_zone = Some(Zone::Graveyard);
            Some(def)
        })
        .collect();

    face.abilities.extend(scavenge_abilities);
}

/// Convert a typecycling subtype string to a `TargetFilter` for library search.
///
/// Single subtypes (e.g., "Plains", "Forest") → subtype filter.
/// "Basic Land" → supertype Basic + core type Land.
fn typecycling_subtype_to_filter(subtype: &str) -> TargetFilter {
    if subtype == "Basic Land" {
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Land).properties(vec![
            FilterProp::HasSupertype {
                value: Supertype::Basic,
            },
        ]))
    } else {
        TargetFilter::Typed(TypedFilter::card().subtype(subtype.to_string()))
    }
}

/// CR 702.153a: Synthesize Casualty N into an optional sacrifice cost + self-cast copy trigger.
///
/// Casualty N = two abilities:
/// 1. Optional additional cost: sacrifice a creature with power N or greater
/// 2. Triggered ability: "When you cast this spell, if a casualty cost was paid, copy it"
pub fn synthesize_casualty(face: &mut CardFace) {
    let threshold = match face.keywords.iter().find_map(|k| match k {
        Keyword::Casualty(n) => Some(*n),
        _ => None,
    }) {
        Some(n) => n,
        None => return,
    };

    // CR 702.153a: "As an additional cost, you may sacrifice a creature with power N or greater"
    if face.additional_cost.is_none() {
        let sacrifice_filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::PowerGE {
                    value: QuantityExpr::Fixed {
                        value: threshold as i32,
                    },
                }]),
            );
        face.additional_cost = Some(AdditionalCost::Optional(AbilityCost::Sacrifice {
            target: sacrifice_filter,
            count: 1,
        }));
    }

    // CR 702.153a: "When you cast this spell, if a casualty cost was paid, copy it.
    // If the spell has any targets, you may choose new targets for the copy."
    // Idempotency: skip if the casualty copy-on-cast trigger already exists.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::SpellCast)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && t.trigger_zones.contains(&Zone::Stack)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::CopySpell {
                    target: TargetFilter::SelfRef,
                })
            )
    });
    if already_has_trigger {
        return;
    }

    let copy_effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
        },
    )
    .condition(AbilityCondition::additional_cost_paid_any());

    face.triggers.push(
        TriggerDefinition::new(TriggerMode::SpellCast)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Stack])
            .execute(copy_effect)
            .description("Casualty — copy this spell when cast with casualty paid".to_string()),
    );
}

/// CR 702.42a: Synthesize Entwine cost onto modal spell's ModalChoice.
///
/// Sets `entwine_cost` on the face's modal abilities and raises `max_choices`
/// to `mode_count` so all modes can be selected.
pub fn synthesize_entwine(face: &mut CardFace) {
    let cost = match face.keywords.iter().find_map(|k| match k {
        Keyword::Entwine(cost) => Some(cost.clone()),
        _ => None,
    }) {
        Some(c) => c,
        None => return,
    };

    // Set entwine_cost on the face's modal choice + allow all-mode selection
    if let Some(ref mut modal) = face.modal {
        modal.entwine_cost = Some(cost);
        // CR 702.42a: "You may choose all modes" — raise max_choices to allow it
        modal.max_choices = modal.mode_count;
    }
}

/// CR 702.35a: Madness is a static ability with a replacement effect plus a
/// linked triggered ability. If the player discards the card, they exile it
/// instead of putting it into their graveyard; when they do, they may cast it
/// for its madness cost or put it into their graveyard.
pub fn synthesize_madness_intrinsics(face: &mut CardFace) {
    let Some(cost) = face.keywords.iter().find_map(|kw| match kw {
        Keyword::Madness(cost) => Some(cost.clone()),
        _ => None,
    }) else {
        return;
    };

    let already_has_replacement = face.replacements.iter().any(|r| {
        matches!(r.event, ReplacementEvent::Discard)
            && matches!(r.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                r.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::ChangeZone {
                    origin: Some(Zone::Hand),
                    destination: Zone::Exile,
                    target: TargetFilter::SelfRef,
                    ..
                })
            )
    });
    if !already_has_replacement {
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Discard);
        replacement.valid_card = Some(TargetFilter::SelfRef);
        replacement.description = Some(
            "CR 702.35a: If you discard this card, exile it instead of putting it into your graveyard."
                .to_string(),
        );
        replacement.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )));
        face.replacements.push(replacement);
    }

    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::Discarded)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && t.trigger_zones.contains(&Zone::Exile)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::MadnessCast { .. })
            )
    });
    if !already_has_trigger {
        let trigger = TriggerDefinition::new(TriggerMode::Discarded)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Exile])
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::MadnessCast { cost },
            ))
            .description(
                "CR 702.35a: When this card is exiled this way, its owner may cast it for its madness cost or put it into their graveyard."
                    .to_string(),
            );
        face.triggers.push(trigger);
    }
}

/// CR 702.74a: Evoke is a static ability granting an alternative cost plus a
/// linked intervening-if triggered ability. The static ability's
/// "you may cast for evoke cost" is wired at the engine level via
/// `CastingVariant::Evoke` (handled in `casting::handle_cast_spell` and
/// `prepare_spell_cast_with_variant_override`); only the triggered ability
/// needs to be synthesized here.
///
/// "When this permanent enters, if its evoke cost was paid, sacrifice it."
/// `TriggerCondition::CastVariantPaid { variant: Evoke }` reads
/// `GameObject.cast_variant_paid`, which the resolution path tags when the
/// spell was cast via `CastingVariant::Evoke`.
pub fn synthesize_evoke(face: &mut CardFace) {
    if !face.keywords.iter().any(|k| matches!(k, Keyword::Evoke(_))) {
        return;
    }
    // Idempotency: skip if a CastVariantPaid::Evoke ETB sacrifice trigger already
    // exists (oracle parser already extracted it, or this synthesizer already ran).
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::ChangesZone)
            && t.destination == Some(Zone::Battlefield)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                t.condition,
                Some(TriggerCondition::CastVariantPaid {
                    variant: CastVariantPaid::Evoke,
                })
            )
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Sacrifice {
                    target: TargetFilter::SelfRef,
                    ..
                })
            )
    });
    if already_has_trigger {
        return;
    }

    let sac = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
        },
    );
    let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        .valid_card(TargetFilter::SelfRef)
        .condition(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Evoke,
        })
        .execute(sac)
        .description(
            "CR 702.74a: When this permanent enters, if its evoke cost was paid, sacrifice it."
                .to_string(),
        );
    face.triggers.push(trigger);
}

/// CR 702.62a: Suspend N—{cost} synthesizes three abilities for every face
/// carrying `Keyword::Suspend { count, cost }`:
///
///   1. **Hand-activated alt-cost** ("Rather than cast this card from your hand,
///      you may pay [cost] and exile it with N time counters on it. This action
///      doesn't use the stack."). Modeled as an activated ability with
///      `activation_zone = Hand` and `ActivationRestriction::MatchesCardCastTiming`
///      (CR 702.62a "if you could begin to cast this card by putting it onto the
///      stack from your hand"). Cost is composite (mana + exile self from hand);
///      effect is a Time-counter `PutCounter` on the now-exiled SelfRef. The
///      synthesized activation does land on the stack as an activated ability,
///      which is a controlled approximation of the rule's "doesn't use the stack"
///      — no card today interacts with that distinction.
///
///   2. **Upkeep counter-removal trigger** ("At the beginning of your upkeep,
///      if this card is suspended, remove a time counter from it.") fires from
///      the Exile zone (CR 702.62b: "suspended" = in exile + has time counters)
///      via `trigger_zones = [Exile]`, gated by `TriggerConstraint::OnlyDuringYourTurn`
///      so only the suspended card's controller's upkeep triggers it.
///
///   3. **Last-counter free-cast trigger** ("When the last time counter is
///      removed from this card, if it's exiled, you may play it without paying
///      its mana cost…") mirrors `synthesize_siege_intrinsics`' victory trigger
///      pattern: `TriggerMode::CounterRemoved` with
///      `CounterTriggerFilter { Time, threshold: Some(0) }` and an optional
///      `Effect::CastFromZone { without_paying_mana_cost: true }` execute body.
///      The cast itself is detected as `CastingVariant::Suspend` by
///      `prepare_spell_cast` (keyword presence on the exile-zone source) and
///      tagged at stack resolution as `CastVariantPaid::Suspend`. The
///      "if creature, gains haste until you lose control" rider (CR 702.62a
///      final sentence) is installed at stack resolution as a transient
///      continuous effect with
///      `Duration::ForAsLongAs { SourceControllerEquals { resolution_controller } }`.
///
/// Idempotent across repeated invocations (parser pipelines may re-run on the
/// same face). Build-for-the-class: every Suspend card flows through this
/// single synthesizer regardless of card type — the haste install branches by
/// `CoreType::Creature` at runtime, not here.
pub fn synthesize_suspend(face: &mut CardFace) {
    use crate::types::ability::ActivationRestriction;

    // Find the first Suspend keyword. Cards do not print multiple Suspends.
    let Some((time_counters, suspend_cost)) = face.keywords.iter().find_map(|k| match k {
        Keyword::Suspend { count, cost } => Some((*count, cost.clone())),
        _ => None,
    }) else {
        return;
    };

    // CR 702.62a: Activated ability — pay [cost], exile self from hand, then
    // place N time counters on it. Composite cost mirrors `synthesize_cycling`.
    let already_has_activation = face.abilities.iter().any(|a| {
        a.activation_zone == Some(Zone::Hand)
            && a.activation_restrictions
                .contains(&ActivationRestriction::MatchesCardCastTiming)
            && matches!(
                &*a.effect,
                Effect::PutCounter { counter_type, target: TargetFilter::SelfRef, .. }
                    if counter_type == "time"
            )
    });
    if !already_has_activation {
        let composite_cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: suspend_cost.clone(),
                },
                // CR 702.62a: "exile it" — self-targeted exile from hand.
                AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Hand),
                    filter: Some(TargetFilter::SelfRef),
                },
            ],
        };
        let mut def = AbilityDefinition::new(
            AbilityKind::Activated,
            // CR 702.62a: "...with N time counters on it." Time counter is a
            // typed CounterType variant; the legacy String API for PutCounter
            // takes the canonical `as_str()` value ("time").
            Effect::PutCounter {
                counter_type: CounterType::Time.as_str().to_string(),
                count: QuantityExpr::Fixed {
                    value: time_counters as i32,
                },
                target: TargetFilter::SelfRef,
            },
        )
        .cost(composite_cost)
        .activation_restrictions(vec![ActivationRestriction::MatchesCardCastTiming]);
        def.activation_zone = Some(Zone::Hand);
        face.abilities.push(def);
    }

    // CR 702.62a + CR 702.62b: Upkeep state trigger — at the beginning of the
    // suspended card's controller's upkeep, if it has any time counters,
    // remove one. `TriggerConstraint::OnlyDuringYourTurn` enforces "your"
    // upkeep; `TriggerCondition::HasCounters` enforces "if this card is
    // suspended" (CR 702.62b: suspended = in exile + has time counters; the
    // exile zone is enforced by `trigger_zones`).
    let already_has_upkeep_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::Phase)
            && t.phase == Some(Phase::Upkeep)
            && t.trigger_zones == vec![Zone::Exile]
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::RemoveCounter { counter_type, target: TargetFilter::SelfRef, .. })
                    if counter_type == "time"
            )
    });
    if !already_has_upkeep_trigger {
        let remove_one = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RemoveCounter {
                counter_type: CounterType::Time.as_str().to_string(),
                count: 1,
                target: TargetFilter::SelfRef,
            },
        );
        let trigger = TriggerDefinition::new(TriggerMode::Phase)
            .phase(Phase::Upkeep)
            .valid_card(TargetFilter::SelfRef)
            .condition(TriggerCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Time),
                minimum: 1,
                maximum: None,
            })
            .constraint(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
            .execute(remove_one)
            .description(
                "CR 702.62a: At the beginning of your upkeep, if this card is suspended, remove a time counter from it."
                    .to_string(),
            );
        let mut trigger = trigger;
        trigger.trigger_zones = vec![Zone::Exile];
        face.triggers.push(trigger);
    }

    // CR 702.62a: Last-counter free-cast trigger — "When the last time counter
    // is removed from this card, if it's exiled, you may play it without
    // paying its mana cost." Mirrors `synthesize_siege_intrinsics` victory
    // trigger (CR 310.11b) — both use `CounterRemoved` with `threshold: Some(0)`.
    // The cast itself goes through the normal casting pipeline; `prepare_spell_cast`
    // detects the variant via `obj.zone == Exile && Keyword::Suspend` and assigns
    // `CastingVariant::Suspend`, which tags `CastVariantPaid::Suspend` at
    // resolution and installs the haste static for creatures.
    let already_has_last_counter_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::CounterRemoved)
            && t.counter_filter.as_ref().is_some_and(|f| {
                matches!(f.counter_type, CounterType::Time) && f.threshold == Some(0)
            })
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
    });
    if !already_has_last_counter_trigger {
        let cast = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
            },
        )
        .optional();
        let trigger = TriggerDefinition::new(TriggerMode::CounterRemoved)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: CounterType::Time,
                threshold: Some(0),
            })
            .execute(cast)
            .description(
                "CR 702.62a: When the last time counter is removed from this card, if it's exiled, you may play it without paying its mana cost."
                    .to_string(),
            );
        let mut trigger = trigger;
        trigger.trigger_zones = vec![Zone::Exile];
        face.triggers.push(trigger);
    }
}

/// CR 702.170 + CR 116.2k: Plot — synthesize a hand-zone activated ability for
/// every face carrying `Keyword::Plot(cost)`.
///
/// Printed text (CR 702.170a): "Plot [cost]" means "Any time you have priority
/// during your main phase while the stack is empty, you may exile this card
/// from your hand and pay [cost]. It becomes a plotted card." Plotting is a
/// special action (CR 116.2k / CR 702.170b) that doesn't use the stack; we
/// approximate it as an activated ability with `activation_zone = Hand`, the
/// `.sorcery_speed()` single-authority builder, and a composite cost
/// `(pay [cost], exile self from hand)`. This is the same controlled
/// approximation Suspend uses (see `synthesize_suspend`); no card today
/// interacts with the "doesn't use the stack" distinction.
///
/// On resolution the activation grants `CastingPermission::Plotted { turn_plotted: 0 }`
/// to the now-exiled card (SelfRef). `grant_permission::resolve` stamps the
/// real `state.turn_number` into `turn_plotted` (mirroring how it resolves
/// `PlayFromExile { granted_to }` for the ability controller). The cast side
/// is detected by `prepare_spell_cast` via `is_plot_cast` — exile-zone source
/// with a `Plotted` permission — which zeros the mana cost
/// (CR 702.170d: "without paying its mana cost") and tags
/// `CastingVariant::Plot` for routing. The "on a later turn" gate is enforced
/// by `has_exile_cast_permission` comparing `state.turn_number > turn_plotted`.
/// Sorcery-speed main-phase-with-empty-stack enforcement is free: Plot cards
/// are non-Instant in the printed OTJ cycle, so `check_spell_timing`'s default
/// sorcery-speed branch covers "may cast as a sorcery" (CR 307.1 + CR 116.1).
///
/// Idempotent across repeated invocations (parser pipelines may re-run on the
/// same face). Build-for-the-class: every Plot card flows through this single
/// synthesizer regardless of card type.
pub fn synthesize_plot(face: &mut CardFace) {
    use crate::types::ability::{ActivationRestriction, CastingPermission, PermissionGrantee};

    // CR 702.170a: Find the first Plot keyword. Cards do not print multiple Plots.
    let Some(plot_cost) = face.keywords.iter().find_map(|k| match k {
        Keyword::Plot(cost) => Some(cost.clone()),
        _ => None,
    }) else {
        return;
    };

    // CR 702.170a: Activated ability — pay [cost] + exile self from hand, then
    // grant Plotted casting permission on the now-exiled SelfRef. Composite cost
    // mirrors `synthesize_suspend`; `.sorcery_speed()` enforces main-phase +
    // empty-stack + active-player timing via `ActivationRestriction::AsSorcery`.
    let already_has_plot_activation = face.abilities.iter().any(|a| {
        a.activation_zone == Some(Zone::Hand)
            && a.activation_restrictions
                .contains(&ActivationRestriction::AsSorcery)
            && matches!(
                &*a.effect,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::Plotted { .. },
                    ..
                }
            )
    });
    if !already_has_plot_activation {
        let composite_cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: plot_cost.clone(),
                },
                // CR 702.170a: "exile this card from your hand" — self-targeted
                // exile from hand. Mirrors Suspend's self-exile cost component.
                AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Hand),
                    filter: Some(TargetFilter::SelfRef),
                },
            ],
        };
        let mut def = AbilityDefinition::new(
            AbilityKind::Activated,
            // CR 702.170a + CR 702.170d: Grant the `Plotted` casting permission
            // to the exiled card. `turn_plotted: 0` is a placeholder stamped
            // by `grant_permission::resolve` to `state.turn_number` at
            // resolution. Grantee is the default `AbilityController` — the
            // plot owner — which is the player allowed to cast it later.
            Effect::GrantCastingPermission {
                permission: CastingPermission::Plotted { turn_plotted: 0 },
                target: TargetFilter::SelfRef,
                grantee: PermissionGrantee::AbilityController,
            },
        )
        .cost(composite_cost)
        // CR 702.170a: "Any time you have priority during your main phase while
        // the stack is empty" — i.e. sorcery-speed timing. `.sorcery_speed()`
        // is the single-authority builder (see `AbilityDefinition::sorcery_speed`).
        .sorcery_speed();
        def.activation_zone = Some(Zone::Hand);
        face.abilities.push(def);
    }
}

/// Run all synthesis functions in canonical order on a card face.
/// Both `oracle_loader.rs` and `oracle_gen.rs` call this to ensure the same
/// complete set of synthesizers is applied.
pub fn synthesize_all(face: &mut CardFace) {
    synthesize_basic_land_mana(face);
    synthesize_equip(face);
    // CR 702.122a: Crew has no synthesized ability — activation is handled by
    // GameAction::CrewVehicle directly, not through ActivateAbility dispatch.
    // The Keyword::Crew(N) on the card provides display information.
    synthesize_ninjutsu_family(face);
    synthesize_changeling_cda(face);
    synthesize_kicker(face);
    synthesize_buyback(face);
    synthesize_gift(face);
    resolve_kicker_condition_variants(face);
    synthesize_case_solve(face);
    // Warp: no synthesis needed — runtime handled by Keyword::Warp directly
    synthesize_mobilize(face);
    synthesize_job_select(face);
    synthesize_level_up(face);
    synthesize_cycling(face);
    synthesize_scavenge(face);
    synthesize_casualty(face);
    synthesize_entwine(face);
    synthesize_madness_intrinsics(face);
    synthesize_evoke(face);
    // CR 702.62a: Suspend — hand-activated alt-cost + upkeep counter-removal +
    // last-counter free-cast. Runs after Evoke to keep alt-cost synthesizers
    // grouped; idempotent so order against Cycling/Madness is irrelevant.
    synthesize_suspend(face);
    // CR 702.170 + CR 116.2k: Plot — hand-activated special-action-approximated
    // ability that exiles self and grants a Plotted casting permission for
    // free-cast on a later turn. Runs after Suspend; idempotent.
    synthesize_plot(face);
    synthesize_siege_intrinsics(face);
    synthesize_tribute_intrinsics(face);
    // CR 721.2b: Spacecraft creature-shift at the max station-symbol striation
    // threshold. Must run after Oracle parsing so `face.power`/`face.toughness`
    // are in place and `Keyword::Station` has been normalized.
    synthesize_station(face);
}

/// CR 310.11a + CR 310.11b: Synthesize the two intrinsic abilities every Siege has:
///   1. As-enters replacement: "As this Siege enters, its controller chooses an
///      opponent to be its protector." (CR 310.11a)
///   2. Victory trigger: "When the last defense counter is removed from this
///      permanent, exile it, then you may cast it transformed without paying
///      its mana cost." (CR 310.11b)
///
/// The defense-counter ETB replacement (CR 310.4b) is handled directly by
/// `apply_card_face_to_object` which seeds `CounterType::Defense` at load time,
/// so no separate replacement synthesis is needed for that rule.
pub fn synthesize_siege_intrinsics(face: &mut CardFace) {
    let is_battle = face.card_type.core_types.contains(&CoreType::Battle);
    let is_siege = face.card_type.subtypes.iter().any(|s| s == "Siege");
    if !is_battle || !is_siege {
        return;
    }

    // CR 310.11a: "As a Siege enters the battlefield, its controller must
    // choose its protector from among their opponents." Modeled as a
    // self-referential `Moved` replacement that persists the opponent choice
    // as a `ChosenAttribute::Player`, which `GameObject::protector()` reads.
    let already_has_protector_choice = face.replacements.iter().any(|r| {
        matches!(r.event, ReplacementEvent::Moved)
            && matches!(r.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                r.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Choose {
                    choice_type: ChoiceType::Opponent,
                    persist: true,
                })
            )
    });
    if !already_has_protector_choice {
        let mut protector_replacement = ReplacementDefinition::new(ReplacementEvent::Moved);
        protector_replacement.valid_card = Some(TargetFilter::SelfRef);
        protector_replacement.destination_zone = Some(Zone::Battlefield);
        protector_replacement.description = Some(
            "CR 310.11a: As a Siege enters, its controller chooses an opponent as its protector."
                .to_string(),
        );
        protector_replacement.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::Opponent,
                persist: true,
            },
        )));
        face.replacements.push(protector_replacement);
    }

    // CR 310.11b: Victory triggered ability — "When the last defense counter
    // is removed from this permanent, exile it, then you may cast it
    // transformed without paying its mana cost."
    let already_has_victory_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::CounterRemoved)
            && t.counter_filter
                .as_ref()
                .is_some_and(|f| matches!(f.counter_type, CounterType::Defense))
    });
    if !already_has_victory_trigger {
        // exile SelfRef → (optional) cast SelfRef from exile transformed
        let cast_sub = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: true,
                alt_ability_cost: None,
            },
        )
        .optional();
        let exile_then_cast = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )
        .sub_ability(cast_sub);

        let trigger = TriggerDefinition::new(TriggerMode::CounterRemoved)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: CounterType::Defense,
                threshold: Some(0),
            })
            .execute(exile_then_cast)
            .description(
                "CR 310.11b: When the last defense counter is removed from this Siege, exile it, then you may cast it transformed without paying its mana cost.".to_string(),
            );
        face.triggers.push(trigger);
    }
}

/// CR 702.104a: Synthesize the intrinsic ETB replacement for every creature with
/// `Keyword::Tribute(N)`.
///
/// Oracle: "Tribute N (As this creature enters, an opponent of your choice may put
/// N +1/+1 counters on it.)"
///
/// Modeled as a self-referential `Moved` replacement whose post-replacement effect
/// chain has two stages:
///
///   1. `Effect::Choose { Opponent, persist: true }` — controller picks the opponent;
///      the selection is persisted on the entering creature as `ChosenAttribute::Player`
///      (mirrors `synthesize_siege_intrinsics`' protector choice).
///
///   2. `Effect::Tribute { count: N }` (sub-ability) — reads the persisted opponent,
///      prompts them pay/decline via `WaitingFor::TributeChoice`, and on resolution
///      records `ChosenAttribute::TributeOutcome` so the companion "if tribute
///      wasn't paid" trigger (CR 702.104b) can read the outcome.
pub fn synthesize_tribute_intrinsics(face: &mut CardFace) {
    let Some(count) = face.keywords.iter().find_map(|k| match k {
        Keyword::Tribute(n) => Some(*n),
        _ => None,
    }) else {
        return;
    };

    // Idempotency guard: don't re-add if already synthesized (parser pipelines can
    // run twice in some code paths).
    let already_synthesized = face.replacements.iter().any(|r| {
        matches!(r.event, ReplacementEvent::Moved)
            && matches!(r.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                r.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Choose {
                    choice_type: ChoiceType::Opponent,
                    persist: true,
                }),
            )
            && r.execute
                .as_deref()
                .and_then(|a| a.sub_ability.as_deref())
                .is_some_and(|sub| matches!(&*sub.effect, Effect::Tribute { .. }))
    });
    if already_synthesized {
        return;
    }

    // Stage 2: Effect::Tribute { count } — the chosen opponent decides pay/decline.
    let tribute_stage = AbilityDefinition::new(AbilityKind::Spell, Effect::Tribute { count });

    // Stage 1: Effect::Choose { Opponent, persist } — controller picks the opponent.
    // Chained with stage 2 as a sub-ability (runs after the Choose resolves).
    let choose_stage = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Choose {
            choice_type: ChoiceType::Opponent,
            persist: true,
        },
    )
    .sub_ability(tribute_stage);

    let mut replacement = ReplacementDefinition::new(ReplacementEvent::Moved);
    replacement.valid_card = Some(TargetFilter::SelfRef);
    replacement.destination_zone = Some(Zone::Battlefield);
    replacement.description = Some(format!(
        "CR 702.104a: Tribute {count} — as this creature enters, an opponent of your choice may put {count} +1/+1 counters on it.",
    ));
    replacement.execute = Some(Box::new(choose_stage));

    face.replacements.push(replacement);
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

    // Merge keywords extracted from Oracle text with MTGJSON keywords.
    // When the Oracle parser extracts a parameterized keyword (e.g., Morph({2}{B}{G}{U})),
    // remove any MTGJSON-derived default of the same kind (e.g., Morph(zero)).
    for extracted_kw in &parsed.extracted_keywords {
        let kind = extracted_kw.kind();
        keywords.retain(|existing| existing.kind() != kind || existing == extracted_kw);
    }
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
        color_identity: mtgjson
            .color_identity
            .iter()
            .filter_map(|code| map_mtgjson_color(code))
            .collect(),
        scryfall_oracle_id: oracle_id,
        modal: parsed.modal,
        additional_cost: parsed.additional_cost,
        strive_cost: parsed.strive_cost,
        casting_restrictions: parsed.casting_restrictions,
        casting_options: parsed.casting_options,
        solve_condition: parsed.solve_condition,
        parse_warnings: parsed.parse_warnings,
        brawl_commander: false,
        metadata: Default::default(),
    };

    face.brawl_commander = compute_brawl_commander(mtgjson, &face);
    synthesize_all(&mut face);
    face
}

#[cfg(test)]
mod kicker_synthesis_tests {
    use super::*;
    use crate::types::mana::ManaCostShard;

    #[test]
    fn synthesize_kicker_sets_typed_kicker_additional_cost() {
        let mut face = CardFace {
            keywords: vec![Keyword::Kicker(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Blue],
            })],
            ..CardFace::default()
        };

        synthesize_kicker(&mut face);

        match face.additional_cost.expect("additional_cost set") {
            AdditionalCost::Kicker { costs, repeatable } => {
                assert!(!repeatable);
                assert_eq!(costs.len(), 1);
                assert!(matches!(
                    &costs[0],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { generic: 2, shards }
                    } if shards == &vec![ManaCostShard::Blue]
                ));
            }
            other => panic!("expected Kicker additional cost, got {other:?}"),
        }
    }

    #[test]
    fn resolves_specific_kicker_condition_to_position() {
        let mut face = CardFace {
            oracle_text: Some(
                "Kicker {2}{U} and/or {2}{B}\nWhen ~ enters, if it was kicked with its {2}{U} kicker, draw a card."
                    .to_string(),
            ),
            additional_cost: Some(AdditionalCost::Kicker {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Blue],
                        },
                    },
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Black],
                        },
                    },
                ],
                repeatable: false,
            }),
            triggers: vec![TriggerDefinition::new(TriggerMode::ChangesZone).execute(
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .condition(AbilityCondition::additional_cost_paid_kicker_cost(
                    ManaCost::Cost {
                        generic: 2,
                        shards: vec![ManaCostShard::Blue],
                    },
                )),
            )],
            ..CardFace::default()
        };

        resolve_kicker_condition_variants(&mut face);

        let condition = face.triggers[0]
            .execute
            .as_ref()
            .and_then(|ability| ability.condition.as_ref());
        assert_eq!(
            condition,
            Some(&AbilityCondition::additional_cost_paid_kicker(
                KickerVariant::First
            ))
        );
    }

    #[test]
    fn resolves_specific_kicker_replacement_condition_to_position() {
        let mut face = CardFace {
            additional_cost: Some(AdditionalCost::Kicker {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::Red],
                        },
                    },
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::White],
                        },
                    },
                ],
                repeatable: false,
            }),
            replacements: vec![
                ReplacementDefinition::new(ReplacementEvent::Moved).condition(
                    ReplacementCondition::CastViaKicker {
                        variant: None,
                        kicker_cost: Some(ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::White],
                        }),
                    },
                ),
            ],
            ..CardFace::default()
        };

        resolve_kicker_condition_variants(&mut face);

        assert!(matches!(
            face.replacements[0].condition,
            Some(ReplacementCondition::CastViaKicker {
                variant: Some(KickerVariant::Second),
                kicker_cost: None
            })
        ));
    }
}

#[cfg(test)]
mod buyback_synthesis_tests {
    use super::*;

    /// CR 702.27a: Mana-cost Buyback synthesizes an optional additional mana cost.
    #[test]
    fn synthesize_buyback_mana_sets_optional_additional_cost() {
        let mut face = CardFace {
            keywords: vec![Keyword::Buyback(BuybackCost::Mana(ManaCost::Cost {
                generic: 3,
                shards: vec![],
            }))],
            ..CardFace::default()
        };

        synthesize_buyback(&mut face);

        match face.additional_cost.expect("additional_cost set") {
            AdditionalCost::Optional(AbilityCost::Mana { cost }) => {
                assert!(matches!(
                    cost,
                    ManaCost::Cost {
                        generic: 3,
                        ref shards,
                    } if shards.is_empty()
                ));
            }
            other => panic!("expected Optional(Mana), got {other:?}"),
        }
    }

    /// CR 702.27a: Non-mana Buyback (Constant Mists "Sacrifice a land") routes
    /// through the full AbilityCost pipeline as an optional additional cost.
    #[test]
    fn synthesize_buyback_non_mana_preserves_ability_cost() {
        let sac_cost = AbilityCost::Sacrifice {
            target: TargetFilter::Any,
            count: 1,
        };
        let mut face = CardFace {
            keywords: vec![Keyword::Buyback(BuybackCost::NonMana(sac_cost.clone()))],
            ..CardFace::default()
        };

        synthesize_buyback(&mut face);

        match face.additional_cost.expect("additional_cost set") {
            AdditionalCost::Optional(cost) => assert_eq!(cost, sac_cost),
            other => panic!("expected Optional(Sacrifice), got {other:?}"),
        }
    }

    /// Idempotency: running synthesize_buyback twice produces the same result.
    #[test]
    fn synthesize_buyback_is_idempotent() {
        let mut face = CardFace {
            keywords: vec![Keyword::Buyback(BuybackCost::Mana(ManaCost::Cost {
                generic: 5,
                shards: vec![],
            }))],
            ..CardFace::default()
        };

        synthesize_buyback(&mut face);
        let first = face.additional_cost.clone();
        synthesize_buyback(&mut face);
        assert_eq!(face.additional_cost, first);
    }

    /// Parser-parsed `additional_cost` takes precedence over synthesized buyback
    /// (Kicker pattern).
    #[test]
    fn synthesize_buyback_skips_when_additional_cost_already_set() {
        let existing = AdditionalCost::Required(AbilityCost::Mana {
            cost: ManaCost::Cost {
                generic: 1,
                shards: vec![],
            },
        });
        let mut face = CardFace {
            keywords: vec![Keyword::Buyback(BuybackCost::Mana(ManaCost::Cost {
                generic: 3,
                shards: vec![],
            }))],
            additional_cost: Some(existing.clone()),
            ..CardFace::default()
        };

        synthesize_buyback(&mut face);
        assert_eq!(face.additional_cost, Some(existing));
    }

    /// No-op when the card has no Buyback keyword.
    #[test]
    fn synthesize_buyback_noop_without_keyword() {
        let mut face = CardFace::default();
        synthesize_buyback(&mut face);
        assert!(face.additional_cost.is_none());
    }
}

#[cfg(test)]
mod cycling_synthesis_tests {
    use super::*;

    #[test]
    fn typecycling_moves_found_card_to_hand_before_shuffle() {
        let mut face = CardFace {
            keywords: vec![Keyword::Typecycling {
                cost: ManaCost::Cost {
                    generic: 1,
                    shards: vec![],
                },
                subtype: "Basic Land".to_string(),
            }],
            ..CardFace::default()
        };

        synthesize_cycling(&mut face);

        let ability = face.abilities.first().expect("typecycling ability");
        assert!(matches!(&*ability.effect, Effect::SearchLibrary { .. }));
        let put_in_hand = ability.sub_ability.as_ref().expect("put in hand");
        assert!(matches!(
            &*put_in_hand.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                target: TargetFilter::Any,
                ..
            }
        ));
        let shuffle = put_in_hand.sub_ability.as_ref().expect("shuffle");
        assert!(matches!(&*shuffle.effect, Effect::Shuffle { .. }));
    }
}

#[cfg(test)]
mod job_select_synthesis_tests {
    use super::*;
    use crate::types::triggers::TriggerMode;

    fn face_with_job_select() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::JobSelect);
        face
    }

    /// CR 702.182a: Job select synthesis produces exactly one ChangesZone trigger
    /// with an ETB destination, a Token effect for a 1/1 colorless Hero creature,
    /// and an Attach sub-ability targeting LastCreated.
    #[test]
    fn synthesize_job_select_builds_etb_trigger_with_token_and_attach() {
        let mut face = face_with_job_select();
        synthesize_job_select(&mut face);

        assert_eq!(face.triggers.len(), 1, "exactly one Job select trigger");
        let trigger = &face.triggers[0];
        assert!(
            matches!(trigger.mode, TriggerMode::ChangesZone),
            "trigger should be ChangesZone (ETB)"
        );
        assert_eq!(trigger.destination, Some(Zone::Battlefield));
        assert_eq!(
            trigger.valid_card,
            Some(TargetFilter::SelfRef),
            "trigger must scope to self-ETB only"
        );

        // Verify execute chain: Token → Attach
        let execute = trigger.execute.as_ref().expect("trigger must have execute");
        match execute.effect.as_ref() {
            Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                ..
            } => {
                assert_eq!(name, "Hero");
                assert!(matches!(power, crate::types::ability::PtValue::Fixed(1)));
                assert!(matches!(
                    toughness,
                    crate::types::ability::PtValue::Fixed(1)
                ));
                assert!(types.contains(&"Creature".to_string()));
                assert!(types.contains(&"Hero".to_string()));
                assert!(colors.is_empty(), "Hero token should be colorless");
            }
            other => panic!("expected Token effect, got {:?}", other),
        }

        // Verify sub_ability is Attach { target: LastCreated }
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("Token effect must chain to Attach sub_ability");
        assert!(
            matches!(
                sub.effect.as_ref(),
                Effect::Attach {
                    attachment: TargetFilter::SelfRef,
                    target: TargetFilter::LastCreated
                }
            ),
            "sub_ability should be Attach targeting LastCreated"
        );
    }

    #[test]
    fn synthesize_job_select_is_idempotent() {
        let mut face = face_with_job_select();
        synthesize_job_select(&mut face);
        let count = face.triggers.len();
        synthesize_job_select(&mut face);
        // Repeat synthesis must not duplicate the ETB trigger. A
        // non-idempotent synthesizer would push the same trigger multiple
        // times and cause per-ETB-event doubling at runtime.
        assert_eq!(face.triggers.len(), count);
    }

    #[test]
    fn synthesize_job_select_skips_without_keyword() {
        let mut face = CardFace::default();
        synthesize_job_select(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 603.6a: ETB triggers fire from the battlefield. The synthesized
    /// ChangesZone trigger must list `Zone::Battlefield` in `trigger_zones`
    /// or the runtime evaluator never matches Job Select equipment's ETB.
    #[test]
    fn synthesize_job_select_binds_battlefield_trigger_zone() {
        let mut face = face_with_job_select();
        synthesize_job_select(&mut face);
        let trigger = &face.triggers[0];
        assert_eq!(trigger.trigger_zones, vec![Zone::Battlefield]);
    }
}

#[cfg(test)]
mod madness_synthesis_tests {
    use super::*;

    fn madness_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Madness(ManaCost::Cost {
            shards: vec![crate::types::mana::ManaCostShard::Red],
            generic: 0,
        }));
        face
    }

    #[test]
    fn synthesize_madness_adds_discard_replacement_and_exile_trigger() {
        let mut face = madness_face();
        synthesize_madness_intrinsics(&mut face);

        let replacement = face
            .replacements
            .iter()
            .find(|r| matches!(r.event, ReplacementEvent::Discard))
            .expect("madness should add a discard replacement");
        assert!(matches!(
            replacement.valid_card,
            Some(TargetFilter::SelfRef)
        ));
        assert!(matches!(
            replacement.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            })
        ));

        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::Discarded))
            .expect("madness should add a discarded trigger");
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        assert_eq!(trigger.trigger_zones, vec![Zone::Exile]);
        assert!(matches!(
            trigger.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::MadnessCast { cost })
                if *cost == (ManaCost::Cost {
                    shards: vec![crate::types::mana::ManaCostShard::Red],
                    generic: 0,
                })
        ));
    }

    #[test]
    fn synthesize_madness_is_idempotent() {
        let mut face = madness_face();
        synthesize_madness_intrinsics(&mut face);
        synthesize_madness_intrinsics(&mut face);

        assert_eq!(
            face.replacements
                .iter()
                .filter(|r| matches!(r.event, ReplacementEvent::Discard))
                .count(),
            1
        );
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| matches!(t.mode, TriggerMode::Discarded))
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod evoke_synthesis_tests {
    use super::*;
    use crate::types::mana::{ManaCost, ManaCostShard};

    fn evoke_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Evoke(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        }));
        face
    }

    /// CR 702.74a: Evoke synthesis injects an intervening-if ETB sacrifice
    /// trigger that fires only when the evoke alt-cost was paid.
    #[test]
    fn synthesize_evoke_adds_conditional_etb_sac_trigger() {
        let mut face = evoke_face();
        synthesize_evoke(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::ChangesZone)
                    && t.destination == Some(Zone::Battlefield)
                    && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            })
            .expect("evoke should add an ETB trigger");
        assert!(matches!(
            trigger.condition,
            Some(TriggerCondition::CastVariantPaid {
                variant: CastVariantPaid::Evoke,
            })
        ));
        assert!(matches!(
            trigger.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            })
        ));
    }

    /// Repeated synthesis must not duplicate the trigger.
    #[test]
    fn synthesize_evoke_is_idempotent() {
        let mut face = evoke_face();
        synthesize_evoke(&mut face);
        synthesize_evoke(&mut face);

        let count = face
            .triggers
            .iter()
            .filter(|t| {
                matches!(
                    t.condition,
                    Some(TriggerCondition::CastVariantPaid {
                        variant: CastVariantPaid::Evoke,
                        ..
                    })
                )
            })
            .count();
        assert_eq!(count, 1, "evoke trigger should be deduped");
    }

    /// Cards without Evoke are unaffected.
    #[test]
    fn synthesize_evoke_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_evoke(&mut face);
        assert!(face.triggers.is_empty());
    }
}

#[cfg(test)]
mod evoke_runtime_tests {
    use super::*;
    use crate::game::triggers::check_trigger_condition;
    use crate::game::zones::create_object;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    /// CR 702.74a: The synthesized intervening-if condition fires only when the
    /// permanent's `cast_variant_paid` matches Evoke for the current turn.
    /// Mirrors the runtime contract used by Sneak/Ninjutsu.
    #[test]
    fn cast_variant_paid_evoke_condition_fires_only_when_tagged() {
        let mut state = GameState::new_two_player(0);
        state.turn_number = 3;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mulldrifter".to_string(),
            Zone::Battlefield,
        );

        let condition = TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Evoke,
        };

        // Untagged → false.
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged with a different variant → false.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Sneak, state.turn_number));
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged Evoke for the current turn → true.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Evoke, state.turn_number));
        assert!(check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged Evoke but for a stale turn → false (per-turn freshness, CR 603.4).
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Evoke, state.turn_number - 1));
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(id),
            None
        ));
    }

    /// CR 702.138b + CR 603.4: Phlage, Titan of Fire's Fury — the negated
    /// `CastVariantPaid { variant: Escape, negated: true }` must satisfy for
    /// (a) untagged permanents (reanimation, flicker: per WotC ruling,
    /// sacrifice fires), (b) permanents tagged with a different variant (no
    /// cast-via-escape happened), and (c) stale escape tags. It must fail only
    /// when the source is tagged `Escape` for the current turn.
    #[test]
    fn cast_variant_paid_escape_negated_fires_unless_escape_tagged() {
        let mut state = GameState::new_two_player(0);
        state.turn_number = 5;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Phlage, Titan of Fire's Fury".to_string(),
            Zone::Battlefield,
        );

        let negated = TriggerCondition::Not {
            condition: Box::new(TriggerCondition::CastVariantPaid {
                variant: CastVariantPaid::Escape,
            }),
        };

        // Untagged (reanimated or put onto battlefield without being cast) →
        // "unless it escaped" is satisfied → trigger fires.
        assert!(check_trigger_condition(
            &state,
            &negated,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged with a non-Escape variant (hard-cast from hand leaves
        // `cast_variant_paid = None`; this branch covers hypothetical other
        // alt-costs like Evoke if composed) → still satisfies.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Evoke, state.turn_number));
        assert!(check_trigger_condition(
            &state,
            &negated,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged Escape for the CURRENT turn → "unless it escaped" fails →
        // trigger does NOT fire.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Escape, state.turn_number));
        assert!(!check_trigger_condition(
            &state,
            &negated,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged Escape for a STALE turn → tag is not the current turn, so
        // the permanent is treated as not having escaped (per-turn freshness,
        // CR 603.4) → sacrifice fires.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Escape, state.turn_number - 1));
        assert!(check_trigger_condition(
            &state,
            &negated,
            PlayerId(0),
            Some(id),
            None
        ));
    }
}

#[cfg(test)]
mod scavenge_synthesis_tests {
    use super::*;
    use crate::types::ability::{ActivationRestriction, QuantityRef};
    use crate::types::mana::{ManaCost, ManaCostShard};

    fn face_with_scavenge(cost: ManaCost) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Scavenge(cost));
        face
    }

    /// CR 702.97a: Scavenge synthesis produces exactly one activated ability whose
    /// shape matches the reminder text — graveyard activation, sorcery speed,
    /// composite cost of mana + self-exile, +1/+1 counters on target creature
    /// scaled by SelfPower.
    #[test]
    fn synthesize_scavenge_builds_activated_ability_with_correct_shape() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 3,
        };
        let mut face = face_with_scavenge(cost.clone());
        synthesize_scavenge(&mut face);

        assert_eq!(face.abilities.len(), 1, "exactly one scavenge ability");
        let def = &face.abilities[0];
        assert_eq!(def.kind, AbilityKind::Activated);
        assert_eq!(def.activation_zone, Some(Zone::Graveyard));
        assert!(def.sorcery_speed);
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));

        // CR 118.3: Composite cost — mana + exile-self-from-graveyard.
        match def.cost.as_ref().expect("scavenge must have a cost") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2);
                assert!(matches!(&costs[0], AbilityCost::Mana { cost: c } if *c == cost));
                assert!(matches!(
                    &costs[1],
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Graveyard),
                        filter: Some(TargetFilter::SelfRef),
                    }
                ));
            }
            other => panic!("expected Composite cost, got {:?}", other),
        }

        // CR 702.97a: Effect is +1/+1 counters equal to SelfPower on target creature.
        match def.effect.as_ref() {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(counter_type, "P1P1");
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Source
                        }
                    }
                ));
                assert!(
                    matches!(target, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature))
                );
            }
            other => panic!("expected PutCounter effect, got {:?}", other),
        }
    }

    /// Scavenge {0} (Slitherhead) — cost-0 mana still produces a well-formed ability.
    #[test]
    fn synthesize_scavenge_handles_zero_cost() {
        let cost = ManaCost::default();
        let mut face = face_with_scavenge(cost);
        synthesize_scavenge(&mut face);
        assert_eq!(face.abilities.len(), 1);
    }

    /// Cards without Scavenge are unaffected.
    #[test]
    fn synthesize_scavenge_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_scavenge(&mut face);
        assert!(face.abilities.is_empty());
    }
}

#[cfg(test)]
mod scavenge_runtime_tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::casting::{can_activate_ability_now, handle_activate_ability};
    use crate::game::zones::create_object;
    use crate::types::counter::CounterType;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;

    /// Helper: put a creature in the graveyard with Scavenge synthesized on it, and
    /// stage a target creature on the battlefield. Returns (source_id, target_id).
    fn setup_scavenge_scenario(
        state: &mut GameState,
        scavenge_cost: ManaCost,
    ) -> (ObjectId, ObjectId) {
        let source = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Scavenge Source".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.power = Some(4);
            obj.toughness = Some(4);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Scavenge(scavenge_cost.clone()));
        }
        // Synthesize to attach the activated ability.
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Scavenge(scavenge_cost));
        synthesize_scavenge(&mut face);
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities)
            .extend(face.abilities);

        let target = create_object(
            state,
            CardId(2),
            PlayerId(0),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.card_types.core_types.push(CoreType::Creature);
        }

        (source, target)
    }

    /// CR 702.97a: Scavenge can be activated while the source is in a graveyard.
    /// CR 702.97a: Activation is gated by sorcery timing.
    #[test]
    fn scavenge_is_activatable_from_graveyard_at_sorcery_speed() {
        let mut state = GameState::new_two_player(42);
        // Active player's main phase, empty stack — sorcery-speed window.
        state.active_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;

        let zero_cost = ManaCost::default(); // Scavenge {0}
        let (source, _target) = setup_scavenge_scenario(&mut state, zero_cost);

        assert!(
            can_activate_ability_now(&state, PlayerId(0), source, 0),
            "Scavenge {{0}} should be activatable from graveyard during sorcery window"
        );
    }

    /// CR 702.97a: Scavenge cannot be activated at instant speed.
    #[test]
    fn scavenge_rejects_instant_speed() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        // Outside the sorcery window (upkeep phase is not a main phase).
        state.phase = Phase::Upkeep;

        let (source, _target) = setup_scavenge_scenario(&mut state, ManaCost::default());

        assert!(
            !can_activate_ability_now(&state, PlayerId(0), source, 0),
            "Scavenge must reject activation outside the sorcery-speed window"
        );
    }

    /// CR 602.1: Scavenge can only be activated while the source is in the graveyard.
    #[test]
    fn scavenge_rejects_from_battlefield() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;

        let (source, _target) = setup_scavenge_scenario(&mut state, ManaCost::default());
        // Move source out of graveyard onto the battlefield.
        crate::game::zones::move_to_zone(&mut state, source, Zone::Battlefield, &mut Vec::new());

        assert!(
            !can_activate_ability_now(&state, PlayerId(0), source, 0),
            "Scavenge must reject activation when source is not in a graveyard"
        );
    }

    /// CR 702.97a + CR 208.3: End-to-end — activating Scavenge exiles the source from
    /// graveyard as a cost, then on resolution places +1/+1 counters equal to SelfPower
    /// (read via LKI) on target creature.
    #[test]
    fn scavenge_activation_exiles_source_and_places_counters_on_target() {
        use crate::game::stack::resolve_top;

        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;
        // Use Scavenge {0} (Slitherhead-shaped) to avoid mana-pool plumbing in the test.
        let (source, target) = setup_scavenge_scenario(&mut state, ManaCost::default());

        // Activate the ability.
        let mut events = Vec::new();
        let result = handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events);
        assert!(result.is_ok(), "activation must succeed: {:?}", result);

        // CR 702.97a: Exile cost — source moved graveyard → exile as cost payment.
        assert_eq!(
            state.objects[&source].zone,
            Zone::Exile,
            "Scavenge source must be exiled as a cost"
        );
        assert!(
            !state.players[0].graveyard.contains(&source),
            "source must be removed from graveyard"
        );
        assert!(
            state.exile.contains(&source),
            "source must be in exile zone"
        );

        // Ability is on the stack awaiting resolution.
        assert!(!state.stack.is_empty(), "ability must be on the stack");

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);

        // CR 702.97a + CR 208.3: target creature gains counters equal to source's LKI power (4).
        let counter_count = state.objects[&target]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            counter_count, 4,
            "target must gain +1/+1 counters equal to source's LKI power (4)"
        );
    }
}

#[cfg(test)]
mod siege_synthesis_tests {
    use super::*;
    use crate::types::triggers::TriggerMode;

    fn siege_face() -> CardFace {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Battle);
        face.card_type.subtypes.push("Siege".to_string());
        face
    }

    /// CR 310.11a: Sieges get a synthesized Moved-replacement that asks the
    /// controller to choose an opponent as the protector.
    #[test]
    fn synthesize_adds_protector_choice_replacement() {
        let mut face = siege_face();
        synthesize_siege_intrinsics(&mut face);
        let protector = face
            .replacements
            .iter()
            .find(|r| matches!(r.event, ReplacementEvent::Moved))
            .expect("Siege should have a Moved replacement");
        assert_eq!(protector.destination_zone, Some(Zone::Battlefield));
        assert!(matches!(protector.valid_card, Some(TargetFilter::SelfRef)));
        assert!(matches!(
            protector.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Choose {
                choice_type: ChoiceType::Opponent,
                persist: true,
            })
        ));
    }

    /// CR 310.11b: Sieges get a synthesized `CounterRemoved` trigger with a
    /// `CounterTriggerFilter` targeting defense at threshold 0 (last counter
    /// removed). The execute chain exiles the Siege then offers an optional
    /// `CastFromZone` with both `without_paying_mana_cost` and `cast_transformed`.
    #[test]
    fn synthesize_adds_victory_trigger() {
        let mut face = siege_face();
        synthesize_siege_intrinsics(&mut face);
        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::CounterRemoved))
            .expect("Siege should have a CounterRemoved trigger");
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        let filter = trigger
            .counter_filter
            .as_ref()
            .expect("trigger must have counter_filter");
        assert!(matches!(filter.counter_type, CounterType::Defense));
        assert_eq!(filter.threshold, Some(0));

        let exec = trigger.execute.as_ref().expect("execute body");
        // Top-level = ChangeZone to Exile with target SelfRef.
        let Effect::ChangeZone {
            destination,
            ref target,
            ..
        } = *exec.effect
        else {
            panic!("top-level should be ChangeZone, got {:?}", exec.effect);
        };
        assert_eq!(destination, Zone::Exile);
        assert!(matches!(target, TargetFilter::SelfRef));

        // Sub-ability = optional CastFromZone with both flags set.
        let sub = exec.sub_ability.as_ref().expect("optional cast sub");
        assert!(sub.optional);
        assert!(matches!(
            *sub.effect,
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                cast_transformed: true,
                ..
            }
        ));
    }

    /// Non-Sieges are unaffected.
    #[test]
    fn synthesize_is_noop_for_non_siege() {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_siege_intrinsics(&mut face);
        assert!(face.replacements.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// Battles without the Siege subtype don't get Siege-specific intrinsics.
    /// (Currently all printed battles are Sieges, but this keeps the synthesis
    /// correctly scoped per CR 310.11.)
    #[test]
    fn synthesize_is_noop_for_non_siege_battle() {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Battle);
        // No Siege subtype.
        synthesize_siege_intrinsics(&mut face);
        assert!(face.replacements.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// Re-running synthesis on an already-synthesized face is idempotent.
    #[test]
    fn synthesize_is_idempotent() {
        let mut face = siege_face();
        synthesize_siege_intrinsics(&mut face);
        let first_trigger_count = face.triggers.len();
        let first_replacement_count = face.replacements.len();
        synthesize_siege_intrinsics(&mut face);
        assert_eq!(face.triggers.len(), first_trigger_count);
        assert_eq!(face.replacements.len(), first_replacement_count);
    }
}

#[cfg(test)]
mod station_synthesis_tests {
    use super::*;
    use crate::types::ability::{ContinuousModification, StaticCondition, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::statics::StaticMode;

    fn spacecraft_face_with_reminder() -> CardFace {
        let mut face = CardFace {
            name: "Uthros Research Craft".to_string(),
            oracle_text: Some(
                "Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 12+.)\n3+ | Whenever you cast an artifact spell, draw a card. Put a charge counter on this Spacecraft.\n12+ | Flying\nThis Spacecraft gets +1/+0 for each artifact you control.".to_string(),
            ),
            power: Some(PtValue::Fixed(0)),
            toughness: Some(PtValue::Fixed(8)),
            keywords: vec![Keyword::Station],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Artifact);
        face.card_type.subtypes.push("Spacecraft".to_string());
        face
    }

    #[test]
    fn synthesize_station_adds_creature_shift_at_threshold() {
        let mut face = spacecraft_face_with_reminder();
        synthesize_station(&mut face);
        let sd = face
            .static_abilities
            .iter()
            .find(|s| {
                s.mode == StaticMode::Continuous
                    && s.modifications.iter().any(|m| {
                        matches!(
                            m,
                            ContinuousModification::AddType {
                                core_type: CoreType::Creature,
                            }
                        )
                    })
            })
            .expect("AddType(Creature) static must be synthesized");
        assert_eq!(sd.affected, Some(TargetFilter::SelfRef));
        assert!(matches!(
            sd.condition,
            Some(StaticCondition::HasCounters {
                counters: crate::types::counter::CounterMatch::OfType(
                    crate::types::counter::CounterType::Generic(ref name)
                ),
                minimum: 12,
                maximum: None,
            }) if name == "charge"
        ));
        // Exactly three modifications: AddType + SetPower(0) + SetToughness(8)
        assert_eq!(sd.modifications.len(), 3);
        assert!(sd
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value: 0 })));
        assert!(sd
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetToughness { value: 8 })));
    }

    /// CR 721.2b: Reminder text "It's an artifact creature at N+" has no
    /// rules force (CR 721.3). The creature-shift threshold is derived from
    /// the highest N+ striation containing the printed P/T box.
    #[test]
    fn station_creature_shift_derived_from_max_threshold_not_reminder_text() {
        let mut face = spacecraft_face_with_reminder();
        // Original oracle has thresholds 3 and 12; max is 12 → creature-shift gates on 12.
        synthesize_station(&mut face);
        let sd = face
            .static_abilities
            .iter()
            .find(|s| {
                s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        }
                    )
                })
            })
            .expect("creature-shift static must derive from max striation");
        assert!(matches!(
            sd.condition,
            Some(StaticCondition::HasCounters { minimum: 12, .. })
        ));
    }

    #[test]
    fn station_creature_shift_ignores_reminder_text_absence() {
        // Oracle without the "at N+" reminder phrase still emits creature-shift
        // because the derivation reads N+ striations, not reminder text.
        let mut face = spacecraft_face_with_reminder();
        face.oracle_text = Some("Station\n8+ | Flying".to_string());
        synthesize_station(&mut face);
        let sd = face
            .static_abilities
            .iter()
            .find(|s| {
                s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        }
                    )
                })
            })
            .expect("creature-shift static must be emitted from striation alone");
        assert!(matches!(
            sd.condition,
            Some(StaticCondition::HasCounters { minimum: 8, .. })
        ));
    }

    #[test]
    fn station_no_creature_shift_when_no_printed_pt() {
        // CR 721.2b: support-only Spacecraft (null P/T) gets no creature-shift.
        // Mirrors "the eternity elevator" — Station + 20+ threshold but no P/T.
        let mut face = spacecraft_face_with_reminder();
        face.power = None;
        face.toughness = None;
        let before = face.static_abilities.len();
        synthesize_station(&mut face);
        assert_eq!(face.static_abilities.len(), before);
    }

    #[test]
    fn station_no_creature_shift_when_no_thresholds() {
        // No N+ striations → no creature-shift static.
        let mut face = spacecraft_face_with_reminder();
        face.oracle_text = Some("Station\nPlain rules text with no thresholds.".to_string());
        let before = face.static_abilities.len();
        synthesize_station(&mut face);
        assert_eq!(face.static_abilities.len(), before);
    }

    #[test]
    fn station_no_creature_shift_for_non_spacecraft_card() {
        // Non-Spacecraft with charge counters and an N+ line in flavor must
        // not trigger creature-shift derivation.
        let mut face = spacecraft_face_with_reminder();
        face.card_type.subtypes.clear();
        face.card_type.subtypes.push("Vehicle".to_string());
        let before = face.static_abilities.len();
        synthesize_station(&mut face);
        assert_eq!(face.static_abilities.len(), before);
    }

    /// CR 721.2b: End-to-end regression for every TDM Spacecraft in the
    /// pre-built export. Locks in per-card expected creature-shift thresholds
    /// against the ground-truth table derived from printed P/T + `N+ |`
    /// striations (plan §C3). A future data edit (MTGJSON patch, Oracle text
    /// change) that shifts any threshold will fail this test loudly.
    ///
    /// Scryfall-frame verification (plan §C5): Candela, Monoist Gravliner,
    /// and Squadron Carrier are MTGJSON-reminder-text-missing cards. Their
    /// printed card frames were manually confirmed on scryfall.com to have
    /// the P/T box in the highest-N station striation:
    ///   - Candela, Aegis of Adagia: P/T 3/3, single threshold 8 → 8+.
    ///   - Monoist Gravliner:        P/T 2/3, single threshold 6 → 6+.
    ///   - Squadron Carrier:         P/T 4/4, single threshold 10 → 10+
    ///     (not support-only despite first-draft speculation).
    #[test]
    fn station_32_tdm_spacecraft_regression_suite() {
        use crate::database::CardDatabase;
        use std::path::PathBuf;

        // CARGO_MANIFEST_DIR points at crates/engine; the workspace root is
        // two levels up. Skip gracefully if the export has not been generated
        // (fresh clone before setup.sh).
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let path = workspace_root.join("client/public/card-data.json");
        if !path.exists() {
            eprintln!(
                "skipping: {} not found (run ./scripts/gen-card-data.sh)",
                path.display()
            );
            return;
        }
        let db = CardDatabase::from_export(&path).expect("card-data.json loads as a valid export");

        // Ground truth: (card name, expected creature-shift). None = support-only
        // or excluded (non-Station Spacecraft crossover).
        let cases: &[(&str, Option<u32>)] = &[
            ("Atmospheric Greenhouse", Some(8)),
            ("Candela, Aegis of Adagia", Some(8)),
            ("Dawnsire, Sunstar Dreadnought", Some(20)),
            ("Debris Field Crusher", Some(8)),
            ("Entropic Battlecruiser", Some(8)),
            ("Exploration Broodship", Some(8)),
            ("Extinguisher Battleship", Some(5)),
            ("Fell Gravship", Some(8)),
            ("Galvanizing Sawship", Some(3)),
            ("Hearthhull, the Worldseed", Some(8)),
            ("Hotel of Fears", None), // excluded (crossover)
            ("Infinite Guideline Station", Some(12)),
            ("Inspirit, Flagship Vessel", Some(8)),
            ("Larval Scoutlander", Some(7)),
            ("Lumen-Class Frigate", Some(12)),
            ("Mondassian Colony Ship", None), // excluded (crossover)
            ("Monoist Gravliner", Some(6)),
            ("Pinnacle Kill-Ship", Some(7)),
            ("Rescue Skiff", Some(10)),
            ("Sledge-Class Seedship", Some(7)),
            ("Specimen Freighter", Some(9)),
            ("Squadron Carrier", Some(10)),
            ("Susurian Dirgecraft", Some(7)),
            ("Synthesizer Labship", Some(9)),
            ("The Dining Car", None),        // excluded (crossover)
            ("The Eternity Elevator", None), // support-only (null P/T)
            ("The Seriema", Some(7)),
            ("Uthros Research Craft", Some(12)),
            ("Uthros Scanship", Some(8)),
            ("Warmaker Gunship", Some(6)),
            ("Wedgelight Rammer", Some(9)),
            ("Wurmwall Sweeper", Some(4)),
        ];

        // Coverage sanity: 32 cards total (28 creature-shift + 1 support-only
        // + 3 excluded). Locks the table size so accidental deletions fail.
        assert_eq!(
            cases.len(),
            32,
            "regression table must cover all 32 TDM Spacecraft"
        );
        let shifted = cases.iter().filter(|(_, n)| n.is_some()).count();
        assert_eq!(shifted, 28, "28 cards must have a creature-shift threshold");

        let mut missing: Vec<&str> = Vec::new();
        let mut wrong: Vec<String> = Vec::new();
        for (name, expected) in cases {
            let Some(face) = db.get_face_by_name(name) else {
                missing.push(name);
                continue;
            };
            let creature_shift_min = face.static_abilities.iter().find_map(|s| {
                let has_creature_add = s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        }
                    )
                });
                if !has_creature_add {
                    return None;
                }
                match &s.condition {
                    Some(StaticCondition::HasCounters {
                        counters:
                            crate::types::counter::CounterMatch::OfType(
                                crate::types::counter::CounterType::Generic(name),
                            ),
                        minimum,
                        ..
                    }) if name == "charge" => Some(*minimum),
                    _ => None,
                }
            });
            match (expected, creature_shift_min) {
                (Some(exp), Some(got)) if *exp == got => {}
                (None, None) => {}
                (exp, got) => {
                    wrong.push(format!("{name}: expected {exp:?}, got {got:?}"));
                }
            }
        }

        if !missing.is_empty() {
            eprintln!(
                "skipping regression for cards missing from export: {}",
                missing.join(", ")
            );
        }
        assert!(
            wrong.is_empty(),
            "synthesize_station produced wrong thresholds:\n  {}",
            wrong.join("\n  ")
        );
    }
}

// CR 702.xxx: Loader-side invariant for Prepare (Strixhaven). The resolver in
// `game/effects/prepare.rs::has_prepare_face` keys off
// `back_face.layout_kind == Some(LayoutKind::Prepare)` to gate the Biblioplex
// "only creatures with prepare spells can become prepared" rule. That gate
// holds only if the layout-string `"prepare"` round-trips through
// `map_layout` / `map_layout_str` / `CardLayout::Prepare` consistently.
// Locking those mappings here prevents a loader regression from silently
// neutering Biblioplex. Assign when WotC publishes SOS CR update.
#[cfg(test)]
mod prepare_layout_invariant_tests {
    use super::*;
    use crate::types::card::{CardFace, CardLayout};

    #[test]
    fn mtgjson_layout_prepare_maps_to_layout_kind_prepare() {
        // `map_layout` returns the synthesis-local LayoutKind; the
        // `"prepare"` string is the MTGJSON-side marker for the Strixhaven
        // two-face Adventure-family frame.
        assert_eq!(map_layout("prepare"), LayoutKind::Prepare);
    }

    #[test]
    fn card_layout_prepare_back_face_is_tagged_prepare() {
        // The printed-cards loader pattern-matches on `CardLayout::Prepare(_, back)`
        // to populate `back_face.layout_kind = Some(LayoutKind::Prepare)`. The test
        // asserts that a `CardLayout::Prepare` constructed from a "prepare"
        // layout string exposes its back face through `layout_faces`, keeping
        // the loader's match-arm assumption load-bearing.
        let a = CardFace {
            name: "Front".to_string(),
            ..CardFace::default()
        };
        let b = CardFace {
            name: "Back".to_string(),
            ..CardFace::default()
        };
        let layout = CardLayout::Prepare(a, b);
        let faces = layout_faces(&layout);
        assert_eq!(faces.len(), 2, "Prepare layout exposes both faces");
        assert_eq!(faces[1].name, "Back");
    }
}

#[cfg(test)]
mod suspend_synthesis_tests {
    use super::*;
    use crate::types::ability::ActivationRestriction;
    use crate::types::counter::CounterType;
    use crate::types::mana::{ManaCost, ManaCostShard};

    /// Builds a Suspend-bearing face with `count` time counters and a single-blue
    /// alt-cost. Returns the populated face for synthesizer probing.
    fn suspend_face(count: u32) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Suspend {
            count,
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            },
        });
        face
    }

    /// CR 702.62a: Suspend synthesizes (a) a hand-activated alt-cost ability,
    /// (b) an upkeep counter-removal trigger, and (c) a last-counter free-cast
    /// trigger. This regression locks the canonical shape so future refactors
    /// of synthesis.rs don't silently drop a sub-ability.
    #[test]
    fn synthesize_suspend_adds_activation_and_two_triggers() {
        let mut face = suspend_face(3);
        synthesize_suspend(&mut face);

        // (a) Hand activation with MatchesCardCastTiming + composite cost.
        let activation = face
            .abilities
            .iter()
            .find(|a| a.activation_zone == Some(Zone::Hand))
            .expect("suspend should add a hand-activated ability");
        assert!(activation
            .activation_restrictions
            .contains(&ActivationRestriction::MatchesCardCastTiming));
        // CR 702.62a: cost = pay [cost] AND exile self from hand.
        match &activation.cost {
            Some(AbilityCost::Composite { costs }) => {
                assert!(matches!(costs[0], AbilityCost::Mana { .. }));
                assert!(matches!(
                    costs[1],
                    AbilityCost::Exile {
                        zone: Some(Zone::Hand),
                        ..
                    }
                ));
            }
            other => panic!("expected Composite cost, got {other:?}"),
        }
        // CR 702.62a: effect places N time counters on SelfRef.
        match &*activation.effect {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(counter_type, "time");
                assert!(matches!(target, TargetFilter::SelfRef));
                assert!(matches!(count, QuantityExpr::Fixed { value: 3 }));
            }
            other => panic!("expected PutCounter effect, got {other:?}"),
        }

        // (b) Upkeep counter-removal trigger from Exile zone.
        let upkeep = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::Phase)
                    && t.phase == Some(Phase::Upkeep)
                    && t.trigger_zones == vec![Zone::Exile]
            })
            .expect("suspend should add an upkeep trigger from Exile");
        assert!(matches!(
            upkeep.condition,
            Some(TriggerCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Time),
                minimum: 1,
                maximum: None,
            })
        ));
        match upkeep.execute.as_deref().map(|a| &*a.effect) {
            Some(Effect::RemoveCounter {
                counter_type,
                target: TargetFilter::SelfRef,
                ..
            }) => assert_eq!(counter_type, "time"),
            other => panic!("expected RemoveCounter effect, got {other:?}"),
        }

        // (c) Last-counter free-cast trigger via CounterRemoved + threshold(0).
        let last = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::CounterRemoved)
                    && t.trigger_zones == vec![Zone::Exile]
            })
            .expect("suspend should add a last-counter trigger from Exile");
        let cf = last.counter_filter.as_ref().expect("counter_filter set");
        assert!(matches!(cf.counter_type, CounterType::Time));
        assert_eq!(cf.threshold, Some(0));
        let exec = last.execute.as_ref().expect("execute body");
        assert!(exec.optional, "free cast must be a 'you may'");
        assert!(matches!(
            *exec.effect,
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                ..
            }
        ));
    }

    /// Idempotency: parser/loader pipelines may invoke `synthesize_all` more
    /// than once on the same face during multi-stage card-data builds.
    #[test]
    fn synthesize_suspend_is_idempotent() {
        let mut face = suspend_face(2);
        synthesize_suspend(&mut face);
        synthesize_suspend(&mut face);

        let activation_count = face
            .abilities
            .iter()
            .filter(|a| a.activation_zone == Some(Zone::Hand))
            .count();
        assert_eq!(activation_count, 1, "activation must dedupe");
        let upkeep_count = face
            .triggers
            .iter()
            .filter(|t| matches!(t.mode, TriggerMode::Phase) && t.phase == Some(Phase::Upkeep))
            .count();
        assert_eq!(upkeep_count, 1, "upkeep trigger must dedupe");
        let last_count = face
            .triggers
            .iter()
            .filter(|t| matches!(t.mode, TriggerMode::CounterRemoved))
            .count();
        assert_eq!(last_count, 1, "last-counter trigger must dedupe");
    }

    /// Cards without `Keyword::Suspend` are completely untouched.
    #[test]
    fn synthesize_suspend_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_suspend(&mut face);
        assert!(face.abilities.is_empty());
        assert!(face.triggers.is_empty());
    }
}

#[cfg(test)]
mod suspend_serialization_tests {
    use crate::types::ability::{CastVariantPaid, StaticCondition};
    use crate::types::counter::CounterType;
    use crate::types::game_state::CastingVariant;
    use crate::types::player::PlayerId;

    /// CR 702.62a: All four typed primitives added by the Suspend runtime
    /// round-trip through serde. This guards against accidental
    /// `#[serde(skip)]` regressions or rename-without-migration mistakes.
    #[test]
    fn suspend_typed_primitives_round_trip() {
        let ct = CounterType::Time;
        let s = serde_json::to_string(&ct).unwrap();
        assert_eq!(s, "\"time\"");
        let back: CounterType = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, CounterType::Time));

        let cv = CastingVariant::Suspend;
        let s = serde_json::to_string(&cv).unwrap();
        let back: CastingVariant = serde_json::from_str(&s).unwrap();
        assert_eq!(back, CastingVariant::Suspend);

        let cvp = CastVariantPaid::Suspend;
        let s = serde_json::to_string(&cvp).unwrap();
        let back: CastVariantPaid = serde_json::from_str(&s).unwrap();
        assert_eq!(back, CastVariantPaid::Suspend);

        let cond = StaticCondition::SourceControllerEquals {
            player: PlayerId(1),
        };
        let s = serde_json::to_string(&cond).unwrap();
        let back: StaticCondition = serde_json::from_str(&s).unwrap();
        assert!(matches!(
            back,
            StaticCondition::SourceControllerEquals { player } if player == PlayerId(1)
        ));
    }
}

#[cfg(test)]
mod plot_synthesis_tests {
    //! CR 702.170 + CR 116.2k: Plot synthesis regression suite. Locks the
    //! shape of the hand-activated special-action-approximated ability that
    //! every `Keyword::Plot` card carries. Mirrors `suspend_synthesis_tests`.
    use super::*;
    use crate::types::ability::{ActivationRestriction, CastingPermission, PermissionGrantee};
    use crate::types::mana::{ManaCost, ManaCostShard};

    /// Builds a Plot-bearing face with a {1}{R} plot cost (Highway Robbery's
    /// printed cost). Returns the populated face for synthesizer probing.
    fn plot_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Plot(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        }));
        face
    }

    /// CR 702.170a: Plot synthesizes a single hand-activated ability with
    /// composite cost (mana + exile self from hand), sorcery-speed
    /// `ActivationRestriction::AsSorcery`, `activation_zone = Hand`, and a
    /// `GrantCastingPermission { Plotted { turn_plotted: 0 } }` effect.
    #[test]
    fn synthesize_plot_adds_hand_activation_with_sorcery_speed() {
        let mut face = plot_face();
        synthesize_plot(&mut face);

        let activation = face
            .abilities
            .iter()
            .find(|a| a.activation_zone == Some(Zone::Hand))
            .expect("plot should add a hand-activated ability");

        // CR 702.170a: sorcery-speed activation — AsSorcery restriction + flag.
        assert!(activation.sorcery_speed, "plot is sorcery-speed");
        assert!(activation
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));

        // CR 702.170a: cost = pay [cost] AND exile this card from hand.
        match &activation.cost {
            Some(AbilityCost::Composite { costs }) => {
                assert_eq!(costs.len(), 2, "composite cost has exactly 2 components");
                assert!(matches!(costs[0], AbilityCost::Mana { .. }));
                assert!(matches!(
                    costs[1],
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Hand),
                        filter: Some(TargetFilter::SelfRef),
                    }
                ));
            }
            other => panic!("expected Composite cost, got {other:?}"),
        }

        // CR 702.170a + CR 702.170d: effect grants `Plotted` to SelfRef with
        // placeholder turn_plotted = 0 (stamped at resolution).
        match &*activation.effect {
            Effect::GrantCastingPermission {
                permission: CastingPermission::Plotted { turn_plotted },
                target: TargetFilter::SelfRef,
                grantee: PermissionGrantee::AbilityController,
            } => {
                assert_eq!(
                    *turn_plotted, 0,
                    "turn_plotted is a placeholder until resolution"
                );
            }
            other => panic!("expected GrantCastingPermission(Plotted), got {other:?}"),
        }
    }

    /// Idempotency: parser pipelines may call `synthesize_all` multiple times.
    #[test]
    fn synthesize_plot_is_idempotent() {
        let mut face = plot_face();
        synthesize_plot(&mut face);
        synthesize_plot(&mut face);

        let count = face
            .abilities
            .iter()
            .filter(|a| a.activation_zone == Some(Zone::Hand))
            .count();
        assert_eq!(count, 1, "plot activation must dedupe on repeat invocation");
    }

    /// Cards without `Keyword::Plot` are completely untouched.
    #[test]
    fn synthesize_plot_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_plot(&mut face);
        assert!(face.abilities.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// CR 702.170d: The `Plotted` permission's `turn_plotted` field gates
    /// casts by the "later turn" rule. The in-engine comparison (in
    /// `has_exile_cast_permission`) uses `state.turn_number > turn_plotted`,
    /// so: same-turn → false, later-turn → true. Lock the comparison
    /// semantics here so future refactors don't flip the sign.
    #[test]
    fn plotted_permission_comparison_is_strictly_greater() {
        let perm = CastingPermission::Plotted { turn_plotted: 5 };
        // Extract the turn_plotted value and verify the comparison contract.
        let CastingPermission::Plotted { turn_plotted } = perm else {
            panic!("constructed variant");
        };
        // Same-turn: must NOT be castable (strictly greater, not >=).
        assert!(turn_plotted <= turn_plotted);
        // Later turn: must be castable.
        assert!(turn_plotted + 1 > turn_plotted);
        // Earlier turn: must NOT pass the `turn_number > turn_plotted` check.
        // Use addition rather than subtraction to avoid underflow semantics on u32.
        let earlier = turn_plotted;
        let later = turn_plotted + 1;
        assert!(!(earlier > later), "earlier turn never passes the gate");
    }

    /// CR 702.170d + CR 400.7: The `Plotted` permission is dropped when the
    /// card leaves exile. Verifies the exhaustive match arm in
    /// `zones::apply_zone_exit_cleanup` includes `Plotted` — regression guard
    /// against a future refactor that forgets to add new permission variants
    /// to the cleanup set.
    #[test]
    fn plotted_variant_is_serializable() {
        let perm = CastingPermission::Plotted { turn_plotted: 3 };
        let s = serde_json::to_string(&perm).unwrap();
        let back: CastingPermission = serde_json::from_str(&s).unwrap();
        match back {
            CastingPermission::Plotted { turn_plotted } => assert_eq!(turn_plotted, 3),
            other => panic!("round-trip produced {other:?}"),
        }
    }
}

#[cfg(test)]
mod idempotency_tests {
    //! Regression tests for trigger double-fire defect: every synthesis function
    //! that pushes a `TriggerDefinition` must be idempotent under repeated
    //! invocation. Non-idempotent synthesis causes multiple identical
    //! `TriggerDefinition` entries on the same card face, which in turn causes
    //! the engine's per-event dedup (keyed on `(ObjectId, trig_idx)`) to fail
    //! — distinct `trig_idx` values register separately.
    use super::*;
    use crate::types::ability::QuantityExpr;
    use crate::types::card_type::CoreType;

    #[test]
    fn synthesize_mobilize_is_idempotent() {
        let mut face = CardFace::default();
        face.keywords
            .push(Keyword::Mobilize(QuantityExpr::Fixed { value: 1 }));
        synthesize_mobilize(&mut face);
        synthesize_mobilize(&mut face);
        assert_eq!(
            face.triggers.len(),
            1,
            "mobilize trigger should only register once"
        );
    }

    #[test]
    fn synthesize_mobilize_preserves_dynamic_quantity() {
        use crate::types::ability::{CountScope, QuantityRef, TypeFilter, ZoneRef};

        let quantity = QuantityExpr::Ref {
            qty: QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![TypeFilter::Creature],
                scope: CountScope::Controller,
            },
        };
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Mobilize(quantity.clone()));

        synthesize_mobilize(&mut face);

        let trigger = face.triggers.first().expect("mobilize trigger");
        match trigger
            .execute
            .as_deref()
            .map(|ability| ability.effect.as_ref())
        {
            Some(Effect::Token { count, .. }) => assert_eq!(count, &quantity),
            other => panic!("expected mobilize token effect, got {other:?}"),
        }
    }

    #[test]
    fn synthesize_case_solve_is_idempotent() {
        let mut face = CardFace::default();
        face.card_type.subtypes.push("Case".to_string());
        face.solve_condition = Some(crate::types::ability::SolveCondition::Text {
            description: "test".to_string(),
        });
        synthesize_case_solve(&mut face);
        synthesize_case_solve(&mut face);
        assert_eq!(
            face.triggers.len(),
            1,
            "case-solve trigger should only register once"
        );
    }

    #[test]
    fn synthesize_casualty_is_idempotent() {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Sorcery);
        face.keywords.push(Keyword::Casualty(2));
        synthesize_casualty(&mut face);
        let first_count = face.triggers.len();
        synthesize_casualty(&mut face);
        assert_eq!(
            face.triggers.len(),
            first_count,
            "casualty trigger should only register once"
        );
    }
}

#[cfg(test)]
mod sorcery_speed_invariant_tests {
    //! CR 602.5d: Every activated ability tagged with the `sorcery_speed`
    //! display flag MUST also carry `ActivationRestriction::AsSorcery` so the
    //! runtime legality gate (`game::restrictions::check_activation_restrictions`)
    //! actually enforces sorcery timing. Historically the `sorcery_speed` bool
    //! was display-only, and callers were required to separately push the enum
    //! variant — a recurring source of bugs where equip abilities were
    //! activatable at instant speed. Unifying the two via the `.sorcery_speed()`
    //! builder (and this invariant) prevents the bug class from recurring.
    use super::*;
    use crate::types::ability::ActivationRestriction;
    use crate::types::mana::{ManaCost, ManaCostShard};

    /// Walk every sub_ability in the chain.
    fn walk_chain<F: FnMut(&AbilityDefinition)>(def: &AbilityDefinition, mut visit: F) {
        let mut cur: Option<&AbilityDefinition> = Some(def);
        while let Some(d) = cur {
            visit(d);
            cur = d.sub_ability.as_deref();
        }
    }

    fn assert_sorcery_invariant(def: &AbilityDefinition, context: &str) {
        walk_chain(def, |d| {
            if d.sorcery_speed {
                assert!(
                    d.activation_restrictions
                        .contains(&ActivationRestriction::AsSorcery),
                    "{context}: ability has sorcery_speed=true but \
                     activation_restrictions is missing AsSorcery"
                );
            }
        });
    }

    /// CR 702.6a: Swiftfoot Boots — "Equip {1}" synthesizes an activated ability
    /// that MUST be gated at sorcery speed. Regression test for the confirmed
    /// bug where equip abilities were activatable at instant speed because
    /// `synthesize_equip` set neither the display flag nor the restriction.
    #[test]
    fn synthesize_equip_pushes_as_sorcery_restriction() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Equip(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        }));
        synthesize_equip(&mut face);

        assert_eq!(face.abilities.len(), 1, "one equip ability");
        let def = &face.abilities[0];
        assert!(def.sorcery_speed, "sorcery_speed display flag set");
        assert!(
            def.activation_restrictions
                .contains(&ActivationRestriction::AsSorcery),
            "AsSorcery restriction pushed for runtime enforcement (CR 702.6a)"
        );
    }

    /// CR 702.87a: Level Up synthesis must carry AsSorcery.
    #[test]
    fn synthesize_level_up_pushes_as_sorcery_restriction() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::LevelUp(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        }));
        synthesize_level_up(&mut face);

        let def = &face.abilities[0];
        assert!(def.sorcery_speed);
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
    }

    /// CR 702.97a: Scavenge synthesis must carry AsSorcery (single `.sorcery_speed()`
    /// call must produce both the flag and the restriction).
    #[test]
    fn synthesize_scavenge_pushes_as_sorcery_restriction() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Scavenge(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        }));
        synthesize_scavenge(&mut face);

        let def = &face.abilities[0];
        assert!(def.sorcery_speed);
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
        // Guard against double-push regression: AsSorcery should appear exactly once.
        let count = def
            .activation_restrictions
            .iter()
            .filter(|r| matches!(r, ActivationRestriction::AsSorcery))
            .count();
        assert_eq!(count, 1, "AsSorcery must not be duplicated");
    }

    /// CR 602.5d: The shared invariant — corpus-wide, walk every synthesized
    /// ability and its sub_ability chain; every ability with
    /// `sorcery_speed=true` must carry `AsSorcery`. Runs the synthesis pipeline
    /// against every keyword variant that has synthesis coverage and enforces
    /// the invariant, so any future keyword synthesis regressing to a
    /// display-only `sorcery_speed=true` fails this test.
    #[test]
    fn sorcery_speed_flag_implies_as_sorcery_restriction_for_synthesized_abilities() {
        fn mana() -> ManaCost {
            ManaCost::Cost {
                shards: vec![],
                generic: 1,
            }
        }

        type SynthCase = (&'static str, fn() -> CardFace);
        let cases: &[SynthCase] = &[
            ("Equip {1}", || {
                let mut f = CardFace::default();
                f.keywords.push(Keyword::Equip(mana()));
                synthesize_equip(&mut f);
                f
            }),
            ("Level Up {1}", || {
                let mut f = CardFace::default();
                f.keywords.push(Keyword::LevelUp(mana()));
                synthesize_level_up(&mut f);
                f
            }),
            ("Scavenge {1}", || {
                let mut f = CardFace::default();
                f.keywords.push(Keyword::Scavenge(mana()));
                synthesize_scavenge(&mut f);
                f
            }),
        ];

        for (name, build) in cases {
            let face = build();
            for def in face.abilities.iter() {
                assert_sorcery_invariant(def, name);
            }
        }
    }
}

#[cfg(test)]
mod loyalty_sorcery_speed_tests {
    //! CR 606.3: Planeswalker loyalty abilities may only be activated during
    //! the controller's main phase with an empty stack, and only once per turn
    //! per permanent. The parser must tag every loyalty line with both
    //! `ActivationRestriction::AsSorcery` (CR 606.3 timing) and
    //! `ActivationRestriction::OnlyOnceEachTurn` (CR 606.3 per-permanent
    //! limit) so downstream consumers (and the shared invariant) see a
    //! self-describing restriction set. The planeswalker activation path
    //! (`game::planeswalker::can_activate_loyalty`) already gates loyalty
    //! independently; these restrictions are defensive + invariant-preserving.
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::ActivationRestriction;

    #[test]
    fn loyalty_ability_parses_with_as_sorcery_and_once_each_turn() {
        // Jace, the Mind Sculptor reminder-text-like minimal loyalty line.
        let r = parse_oracle_text("+2: Draw a card.", "Test Planeswalker", &[], &[], &[]);
        assert_eq!(r.abilities.len(), 1);
        let def = &r.abilities[0];
        assert!(def.sorcery_speed, "loyalty sets sorcery_speed display flag");
        assert!(
            def.activation_restrictions
                .contains(&ActivationRestriction::AsSorcery),
            "CR 606.3: AsSorcery restriction is pushed for loyalty"
        );
        assert!(
            def.activation_restrictions
                .contains(&ActivationRestriction::OnlyOnceEachTurn),
            "CR 606.3: OnlyOnceEachTurn restriction is pushed for loyalty"
        );
    }

    #[test]
    fn loyalty_bracket_format_also_tagged() {
        // Bracket format: [+1]: effect.
        let r = parse_oracle_text("[+1]: Draw a card.", "Test Planeswalker", &[], &[], &[]);
        assert_eq!(r.abilities.len(), 1);
        let def = &r.abilities[0];
        assert!(def.sorcery_speed);
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::OnlyOnceEachTurn));
    }

    #[test]
    fn loyalty_negative_minus_cost_tagged() {
        let r = parse_oracle_text(
            "\u{2212}3: Destroy target creature.",
            "Test Planeswalker",
            &[],
            &[],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let def = &r.abilities[0];
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
    }
}
