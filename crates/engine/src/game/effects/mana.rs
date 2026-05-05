use crate::game::quantity::{resolve_quantity, resolve_quantity_with_targets};
use crate::game::{mana_payment, mana_sources};
#[cfg(test)]
use crate::types::ability::ManaContribution;
use crate::types::ability::{
    ChoiceValue, Effect, EffectError, EffectKind, LinkedExileScope, ManaProduction,
    ManaSpendRestriction, ResolvedAbility,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, ManaChoice, ManaChoiceContext, ManaChoicePrompt, WaitingFor,
};
use crate::types::mana::{ManaColor, ManaRestriction, ManaType};

/// Mana effect: adds mana to the controller's mana pool (CR 106.4).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (produced, restrictions, grants, expiry) = match &ability.effect {
        Effect::Mana {
            produced,
            restrictions,
            grants,
            expiry,
            // CR 115.1 + CR 115.7: `target` is the spell-level player target
            // (e.g. Jeska's Will mode 1). The actual count quantity inside
            // `produced` references it via `TargetZoneCardCount`; we don't
            // need it again here since `resolve_quantity_with_targets` reads
            // `ability.targets` directly.
            target: _,
        } => (produced, restrictions, grants, *expiry),
        _ => return Err(EffectError::MissingParam("Produced".to_string())),
    };
    let is_triggered_mana_inline = crate::game::mana_abilities::is_triggered_mana_ability(
        ability,
        state.current_trigger_event.as_ref(),
    );
    let mana_choice = (!is_triggered_mana_inline)
        .then(|| {
            crate::game::mana_abilities::mana_choice_prompt(
                &ability.effect,
                state,
                ability.source_id,
                Some(ability),
            )
        })
        .flatten();
    if let Some(choice) = mana_choice {
        state.waiting_for = WaitingFor::ChooseManaColor {
            player: ability.controller,
            choice,
            context: ManaChoiceContext::ResolvingEffect(Box::new(ability.clone())),
        };
        return Ok(());
    }

    // CR 106.3: Mana is produced by the effects of mana abilities, spells, and
    // abilities that aren't mana abilities. The source of produced mana is the
    // source of the ability or spell.
    // CR 107.1b: When X is part of a mana production quantity (rare — e.g., an
    // effect on the stack that resolved via `ResolvedAbility` and produces X mana),
    // `resolve_quantity_with_targets` threads `ability.chosen_x` through to the
    // `Variable { name: "X" }` branch of `resolve_ref`. Non-X mana production
    // (Fixed, ObjectCount, etc.) is unaffected.
    let mana_types = resolve_mana_types_with_ability(produced, &*state, ability);

    // Resolve restriction templates into concrete restrictions
    let concrete_restrictions = resolve_restrictions(restrictions, state, ability.source_id);

    let recipient = match produced {
        ManaProduction::TriggerEventManaType => state
            .current_trigger_event
            .as_ref()
            .and_then(|event| match event {
                GameEvent::ManaAdded { player_id, .. } => Some(*player_id),
                _ => None,
            })
            .unwrap_or(ability.controller),
        _ => ability.controller,
    };

    // CR 106.4: When an effect instructs a player to add mana, that mana goes
    // into that player's mana pool.
    for mana_type in mana_types {
        mana_payment::produce_mana_with_attributes(
            state,
            ability.source_id,
            mana_type,
            recipient,
            false,
            &concrete_restrictions,
            grants,
            expiry,
            events,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 106.3 + CR 608.2d: Complete a mana-choice prompt created while a spell
/// or non-mana ability effect is resolving.
pub fn handle_choose_mana_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    prompt: &ManaChoicePrompt,
    chosen: ManaChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, crate::game::engine::EngineError> {
    let Effect::Mana {
        produced,
        restrictions,
        grants,
        expiry,
        target: _,
    } = &ability.effect
    else {
        return Err(crate::game::engine::EngineError::InvalidAction(
            "Pending mana choice is not a mana effect".to_string(),
        ));
    };

    let mana_types = chosen_mana_types_for_prompt(state, ability, produced, prompt, chosen)?;
    let concrete_restrictions = resolve_restrictions(restrictions, state, ability.source_id);
    let recipient = ability.controller;
    for mana_type in mana_types {
        mana_payment::produce_mana_with_attributes(
            state,
            ability.source_id,
            mana_type,
            recipient,
            false,
            &concrete_restrictions,
            grants,
            *expiry,
            events,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    state.waiting_for = WaitingFor::Priority { player: recipient };
    state.priority_player = recipient;
    super::drain_pending_continuation(state, events);
    Ok(state.waiting_for.clone())
}

fn chosen_mana_types_for_prompt(
    state: &GameState,
    ability: &ResolvedAbility,
    produced: &ManaProduction,
    prompt: &ManaChoicePrompt,
    chosen: ManaChoice,
) -> Result<Vec<ManaType>, crate::game::engine::EngineError> {
    match (prompt, chosen) {
        (ManaChoicePrompt::SingleColor { options }, ManaChoice::SingleColor(color)) => {
            if !options.contains(&color) {
                return Err(crate::game::engine::EngineError::InvalidAction(
                    "Chosen color is not among the legal options".to_string(),
                ));
            }
            let count = resolve_mana_types_for_ability(produced, state, ability).len();
            Ok(vec![color; count])
        }
        (ManaChoicePrompt::Combination { options }, ManaChoice::Combination(combo)) => {
            if !options.iter().any(|option| option == &combo) {
                return Err(crate::game::engine::EngineError::InvalidAction(
                    "Chosen combination is not among the legal options".to_string(),
                ));
            }
            Ok(combo)
        }
        (ManaChoicePrompt::AnyCombination { count, options }, ManaChoice::Combination(combo)) => {
            if combo.len() != *count || combo.iter().any(|color| !options.contains(color)) {
                return Err(crate::game::engine::EngineError::InvalidAction(
                    "Chosen mana combination is not legal for this prompt".to_string(),
                ));
            }
            Ok(combo)
        }
        _ => Err(crate::game::engine::EngineError::InvalidAction(
            "Mana choice shape does not match the active prompt".to_string(),
        )),
    }
}

/// Resolve parse-time restriction templates into concrete `ManaRestriction` values.
/// CR 106.6: Some spells or abilities that produce mana restrict how that mana can be spent.
pub(crate) fn resolve_restrictions(
    templates: &[ManaSpendRestriction],
    state: &GameState,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaRestriction> {
    templates
        .iter()
        .filter_map(|template| match template {
            ManaSpendRestriction::SpellType(t) => {
                Some(ManaRestriction::OnlyForSpellType(t.clone()))
            }
            ManaSpendRestriction::ChosenCreatureType => state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.chosen_creature_type())
                .map(|ct| ManaRestriction::OnlyForCreatureType(ct.to_string())),
            // CR 106.12: Combined spell type + ability activation restriction.
            ManaSpendRestriction::SpellTypeOrAbilityActivation(t) => {
                Some(ManaRestriction::OnlyForTypeSpellsOrAbilities(t.clone()))
            }
            ManaSpendRestriction::ActivateOnly => Some(ManaRestriction::OnlyForActivation),
            ManaSpendRestriction::XCostOnly => Some(ManaRestriction::OnlyForXCosts),
            ManaSpendRestriction::SpellWithKeywordKind(kind) => {
                Some(ManaRestriction::OnlyForSpellWithKeywordKind(*kind))
            }
            ManaSpendRestriction::SpellWithKeywordKindFromZone { kind, zone } => Some(
                ManaRestriction::OnlyForSpellWithKeywordKindFromZone(*kind, *zone),
            ),
        })
        .collect()
}

/// Resolve a typed mana production descriptor into concrete mana units.
///
/// CR 605.3a: Mana abilities don't use the stack, so they have no `ResolvedAbility`
/// and thus no `chosen_x` — this entry point used to be the legacy path for
/// `mana_abilities::resolve_mana_ability`. The inline mana-ability resolver now
/// always routes through `resolve_mana_types_for_ability` so the cost-paid
/// object snapshot (Food Chain class) and `chosen_x` are visible. Kept as a
/// minimal building block for callers that have neither.
#[allow(dead_code)]
pub(crate) fn resolve_mana_types(
    produced: &ManaProduction,
    state: &GameState,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaType> {
    resolve_mana_types_impl(produced, state, None, controller, source_id)
}

/// Variant of `resolve_mana_types` that threads the resolving ability's context
/// (including `chosen_x`) into quantity resolution. Use this from stack-resolving
/// effect handlers (`effects::mana::resolve`).
fn resolve_mana_types_with_ability(
    produced: &ManaProduction,
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<ManaType> {
    resolve_mana_types_impl(
        produced,
        state,
        Some(ability),
        ability.controller,
        ability.source_id,
    )
}

/// CR 117.1 + CR 202.3: Public-crate wrapper for `resolve_mana_types_with_ability`.
/// Used by the inline mana-ability resolver in `mana_abilities.rs` to thread a
/// `ResolvedAbility` carrying `cost_paid_object` (Food Chain class)
/// and `chosen_x` into the production-count resolution.
pub(crate) fn resolve_mana_types_for_ability(
    produced: &ManaProduction,
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<ManaType> {
    resolve_mana_types_with_ability(produced, state, ability)
}

fn resolve_count(
    count: &crate::types::ability::QuantityExpr,
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> usize {
    let raw = match ability {
        Some(a) => resolve_quantity_with_targets(state, count, a),
        None => resolve_quantity(state, count, controller, source_id),
    };
    raw.max(0) as usize
}

fn resolve_mana_types_impl(
    produced: &ManaProduction,
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaType> {
    match produced {
        // CR 106.1a: Colored mana is produced in the five standard colors.
        ManaProduction::Fixed { colors, .. } => colors.iter().map(mana_color_to_type).collect(),
        // CR 106.1b: Colorless mana is a type of mana distinct from colored mana.
        ManaProduction::Colorless { count } => {
            vec![ManaType::Colorless; resolve_count(count, state, ability, controller, source_id)]
        }
        // CR 106.5: If an ability would produce one or more mana of an undefined type,
        // it produces no mana instead.
        ManaProduction::AnyOneColor {
            count,
            color_options,
            ..
        } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let Some(mana_type) = color_options.first().map(mana_color_to_type) else {
                return Vec::new();
            };
            vec![mana_type; amount]
        }
        ManaProduction::AnyCombination {
            count,
            color_options,
        } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            if color_options.is_empty() {
                return Vec::new();
            }
            (0..amount)
                .map(|index| mana_color_to_type(&color_options[index % color_options.len()]))
                .collect()
        }
        ManaProduction::ChosenColor { count, .. } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            chosen_color_for_mana(state, source_id)
                .map(|color| vec![mana_color_to_type(&color); amount])
                .unwrap_or_default()
        }
        // CR 106.7: Produce mana of any color that a land an opponent controls could produce.
        // Delegates to mana_sources::opponent_land_color_options for the shared computation.
        ManaProduction::OpponentLandColors { count } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let color_options = mana_sources::opponent_land_color_options(state, controller);
            // CR 106.5: If no color can be defined, produce no mana.
            let Some(first) = color_options.first().copied() else {
                return Vec::new();
            };
            vec![first; amount]
        }
        // CR 106.7 + CR 106.1b: Reflecting Pool class — produce N mana of any
        // type (W/U/B/R/G/C) that a land matching `land_filter` could produce.
        // Without an explicit choice override (auto-tap during cost payment, or
        // direct activation without prompt), the first listed type is produced
        // mirroring the `OpponentLandColors` / `AnyOneColor` precedent. The
        // per-type choice prompt is surfaced by `mana_choice_prompt` when the
        // option set has more than one type. CR 106.5: an empty option set
        // (no matching lands, or only mutually-recursive producers) produces
        // no mana.
        ManaProduction::AnyTypeProduceableBy { count, land_filter } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let type_options = mana_sources::produceable_mana_types_by_filter(
                state,
                land_filter,
                controller,
                source_id,
            );
            let Some(first) = type_options.first().copied() else {
                return Vec::new();
            };
            vec![first; amount]
        }
        // CR 605.1a + CR 406.1 + CR 610.3: One mana of any of the colors among the
        // cards exiled-with this source (Pit of Offerings). Reads `state.exile_links`
        // for the relation; the per-color choice is selected by the caller via
        // `color_override` (auto-tap during cost payment, or AI/UI on direct activation),
        // exactly like `AnyOneColor`. Without an override the first listed color is
        // produced. CR 106.5: undefined mana type → produce no mana.
        ManaProduction::ChoiceAmongExiledColors { source } => {
            let color_options = exiled_color_options(state, *source, source_id);
            let Some(first) = color_options.first().copied() else {
                return Vec::new();
            };
            vec![first]
        }
        // CR 605.3b + CR 106.1a: Filter-land combinations. When no override is
        // supplied (stack-resolving paths or direct activation without choice),
        // fall back to the first listed combination — mirrors the
        // `ChoiceAmongExiledColors` precedent. `produce_mana_from_ability`
        // selects the combination via `ProductionOverride::Combination`, so
        // this branch is only hit on the "no override at all" path.
        ManaProduction::ChoiceAmongCombinations { options } => options
            .first()
            .map(|combo| combo.iter().map(mana_color_to_type).collect())
            .unwrap_or_default(),
        // CR 106.1: Mixed colorless + colored production (e.g. {C}{W}, {C}{C}{R}).
        ManaProduction::Mixed {
            colorless_count,
            colors,
        } => {
            let mut mana = vec![ManaType::Colorless; *colorless_count as usize];
            mana.extend(colors.iter().map(mana_color_to_type));
            mana
        }
        // CR 903.4 + CR 903.4f + CR 106.5: Produce mana of one color from the
        // activator's commander color identity. Without a color_override
        // (auto-tap, or no choice needed) this picks the first listed color,
        // mirroring the `ChoiceAmongExiledColors` / `AnyOneColor` precedent.
        // The color-choice prompt is driven by `mana_choice_prompt` when
        // identity.len() > 1. If the identity is empty — no commander or an
        // undefined identity per CR 903.4f — the ability produces no mana.
        ManaProduction::AnyInCommandersColorIdentity { count, .. } => {
            let amount = resolve_count(count, state, ability, controller, source_id);
            let identity = super::super::commander::commander_color_identity(state, controller);
            let Some(first) = identity.first() else {
                return Vec::new();
            };
            vec![mana_color_to_type(first); amount]
        }
        // CR 106.1 + CR 109.1: Produce one mana of each distinct color (W/U/B/R/G)
        // found among permanents matching `filter`. Used by Faeburrow Elder.
        // Returns empty when no colored permanent matches (CR 106.5).
        ManaProduction::DistinctColorsAmongPermanents { filter } => {
            distinct_colors_among_permanents(state, ability, controller, source_id, filter)
                .into_iter()
                .map(|c| mana_color_to_type(&c))
                .collect()
        }
        // CR 603.7c + CR 106.3 + CR 106.5: Vorinclex / Dictate of Karametra —
        // "add one mana of any type that land produced." The mana type is read
        // from the triggering `ManaAdded` event carried in
        // `state.current_trigger_event` at resolution time. If the current
        // event is absent (off-stack resolution) or not a `ManaAdded` event,
        // this produces no mana (CR 106.5 — undefined mana type).
        //
        // Per-event single-type model: a `ManaAdded` event carries exactly one
        // `ManaType`, so this branch produces exactly one mana matching that
        // type. The engine fires one trigger per `ManaAdded` event, so a land
        // tapping for {C}{C} produces two triggers and two singletons here —
        // not one trigger producing `vec![Colorless, Colorless]`. This matches
        // Vorinclex / Mana Reflex / Dictate of Karametra's "any type that land
        // produced" semantics where there is no choice to make.
        //
        // If a future card requires the player to *choose* among multiple
        // produced types in a single resolution ("any one type that land
        // produced"), the resolver must be extended to emit a player choice
        // rather than reading the singular event type. Add a separate
        // `ManaProduction::TriggerEventManaTypeChoice` variant before reusing
        // this branch — silently expanding the vec here would skip the choice.
        ManaProduction::TriggerEventManaType => {
            use crate::types::events::GameEvent;
            match &state.current_trigger_event {
                Some(GameEvent::ManaAdded { mana_type, .. }) => vec![*mana_type],
                _ => Vec::new(),
            }
        }
    }
}

/// CR 106.1 + CR 109.1: Shared helper returning the distinct colors (W/U/B/R/G)
/// present among permanents matching `filter`. Colorless permanents contribute
/// nothing. Used by both the mana ability resolver and `mana_sources` so that
/// cost-payment and direct activation see the same option set.
pub(crate) fn distinct_colors_among_permanents(
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
    filter: &crate::types::ability::TargetFilter,
) -> Vec<crate::types::mana::ManaColor> {
    use crate::game::filter::{matches_target_filter, FilterContext};
    let _ = controller;
    let filter_ctx = match ability {
        Some(a) => FilterContext::from_ability(a),
        None => FilterContext::from_source(state, source_id),
    };
    let zone = filter
        .extract_in_zone()
        .unwrap_or(crate::types::zones::Zone::Battlefield);
    let mut seen: std::collections::HashSet<crate::types::mana::ManaColor> =
        std::collections::HashSet::new();
    for &id in crate::game::targeting::zone_object_ids(state, zone).iter() {
        if !matches_target_filter(state, id, filter, &filter_ctx) {
            continue;
        }
        if let Some(obj) = state.objects.get(&id) {
            for color in &obj.color {
                seen.insert(*color);
            }
        }
    }
    // Stable order for determinism (WUBRG).
    use crate::types::mana::ManaColor;
    [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
    .into_iter()
    .filter(|c| seen.contains(c))
    .collect()
}

/// CR 605.1a + CR 406.1 + CR 610.3: Resolve the legal `ManaType` set for a
/// `ChoiceAmongExiledColors` mana ability. Reads `state.exile_links` keyed to the
/// scope, collects the printed colors of every still-exiled linked object, and
/// drops colorless cards (CR 106.5). Shared by the resolver here and by
/// `mana_sources::mana_options_from_production` so cost-payment and direct
/// activation see the same legal set.
pub(crate) fn exiled_color_options(
    state: &GameState,
    scope: LinkedExileScope,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaType> {
    let mut options: Vec<ManaType> = Vec::new();
    for link in &state.exile_links {
        let host_id = match scope {
            LinkedExileScope::ThisObject => source_id,
        };
        if link.source_id != host_id {
            continue;
        }
        let Some(exiled) = state.objects.get(&link.exiled_id) else {
            continue;
        };
        // CR 400.7: Only consider linked cards still in exile (links are pruned
        // from `state.exile_links` when the exiled card leaves exile, but guard
        // defensively in case ordering interleaves).
        if exiled.zone != crate::types::zones::Zone::Exile {
            continue;
        }
        for color in &exiled.color {
            let mana_type = mana_color_to_type(color);
            if !options.contains(&mana_type) {
                options.push(mana_type);
            }
        }
    }
    options
}

pub(crate) fn chosen_color_for_mana(
    state: &GameState,
    source_id: crate::types::identifiers::ObjectId,
) -> Option<ManaColor> {
    state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.chosen_color())
        .or_else(|| {
            state
                .last_named_choice
                .as_ref()
                .and_then(|choice| match choice {
                    ChoiceValue::Color(color) => Some(*color),
                    _ => None,
                })
        })
}

/// Convert a ManaColor to the runtime ManaType.
/// CR 106.1a: There are five colors of mana: white, blue, black, red, and green.
/// CR 106.1b: There are six types of mana: white, blue, black, red, green, and colorless.
fn mana_color_to_type(color: &ManaColor) -> ManaType {
    match color {
        ManaColor::White => ManaType::White,
        ManaColor::Blue => ManaType::Blue,
        ManaColor::Black => ManaType::Black,
        ManaColor::Red => ManaType::Red,
        ManaColor::Green => ManaType::Green,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ChoiceValue, DevotionColors, QuantityExpr, QuantityRef, TargetFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_mana_ability(produced: ManaProduction) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn produce_single_red_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn produce_multiple_of_same_color() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green, ManaColor::Green, ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 3);
    }

    #[test]
    fn produce_empty_is_noop() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn produce_multi_color_fixed() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::White, ManaColor::Blue],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn emits_mana_added_per_unit() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red, ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        let mana_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::ManaAdded { .. }))
            .collect();
        assert_eq!(mana_events.len(), 2);
    }

    #[test]
    fn emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Mana,
                ..
            }
        )));
    }

    #[test]
    fn empty_produced_adds_no_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn mana_units_track_source() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.source_id, ObjectId(100));
    }

    #[test]
    fn produce_colorless_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 2 },
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
    }

    #[test]
    fn any_one_color_effect_prompts_when_multiple_options() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 2 },
                color_options: vec![ManaColor::Blue, ManaColor::Red],
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                context: ManaChoiceContext::ResolvingEffect(_),
                ..
            } => assert_eq!(options, &[ManaType::Blue, ManaType::Red]),
            other => panic!("expected SingleColor mana choice, got {other:?}"),
        }
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn any_combination_effect_prompts_and_resumes_sub_ability() {
        let mut state = GameState::new_two_player(42);
        let drawn = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let mut ability = make_mana_ability(ManaProduction::AnyCombination {
            count: QuantityExpr::Fixed { value: 2 },
            color_options: ManaColor::ALL.to_vec(),
        });
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));

        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let (choice, pending_effect) = match state.waiting_for.clone() {
            WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::AnyCombination { count, options },
                context: ManaChoiceContext::ResolvingEffect(pending_effect),
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert_eq!(
                    options,
                    vec![
                        ManaType::White,
                        ManaType::Blue,
                        ManaType::Black,
                        ManaType::Red,
                        ManaType::Green,
                    ]
                );
                (
                    ManaChoicePrompt::AnyCombination { count, options },
                    pending_effect,
                )
            }
            other => panic!("expected AnyCombination mana choice, got {other:?}"),
        };
        assert!(state.pending_continuation.is_some());

        handle_choose_mana_effect(
            &mut state,
            &pending_effect,
            &choice,
            ManaChoice::Combination(vec![ManaType::Red, ManaType::Green]),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
        assert!(state.players[0].hand.contains(&drawn));
        assert!(state.pending_continuation.is_none());
    }

    #[test]
    fn any_combination_effect_rejects_wrong_choice_count() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = make_mana_ability(ManaProduction::AnyCombination {
            count: QuantityExpr::Fixed { value: 3 },
            color_options: vec![ManaColor::Black, ManaColor::Green],
        });

        resolve(&mut state, &ability, &mut events).unwrap();
        let (choice, pending_effect) = match state.waiting_for.clone() {
            WaitingFor::ChooseManaColor {
                choice,
                context: ManaChoiceContext::ResolvingEffect(pending_effect),
                ..
            } => (choice, pending_effect),
            other => panic!("expected ChooseManaColor, got {other:?}"),
        };

        let result = handle_choose_mana_effect(
            &mut state,
            &pending_effect,
            &choice,
            ManaChoice::Combination(vec![ManaType::Black, ManaType::Green]),
            &mut events,
        );
        assert!(result.is_err());
    }

    #[test]
    fn chosen_color_resolves_from_object_attribute() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let obj_id = ObjectId(100);
        let mut obj = crate::game::game_object::GameObject::new(
            obj_id,
            CardId(1),
            PlayerId(0),
            "Captivating Crossroads".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Green));
        state.objects.insert(obj_id, obj);

        let mut events = Vec::new();
        let ability = make_mana_ability(ManaProduction::ChosenColor {
            count: QuantityExpr::Fixed { value: 1 },
            contribution: ManaContribution::Base,
        });
        // Override source_id to match our object
        let ability = ResolvedAbility {
            source_id: obj_id,
            ..ability
        };

        resolve(&mut state, &ability, &mut events).unwrap();

        let player = state.players.iter().find(|p| p.id == PlayerId(0)).unwrap();
        assert_eq!(player.mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn chosen_color_dynamic_count_reads_current_named_choice() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::mana::{ManaCost, ManaCostShard};
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Nyx Lotus".to_string(),
            Zone::Battlefield,
        );
        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Green Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&permanent).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 1,
        };
        state.last_named_choice = Some(ChoiceValue::Color(ManaColor::Green));

        let mut events = Vec::new();
        let ability = ResolvedAbility {
            source_id,
            ..make_mana_ability(ManaProduction::ChosenColor {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Devotion {
                        colors: DevotionColors::ChosenColor,
                    },
                },
                contribution: ManaContribution::Base,
            })
        };

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 2);
    }

    #[test]
    fn chosen_color_unresolved_is_noop() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::ChosenColor {
                count: QuantityExpr::Fixed { value: 1 },
                contribution: ManaContribution::Base,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn opponent_land_colors_produces_from_opponent_lands() {
        // CR 106.7: Mana of any color that a land an opponent controls could produce.
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Opponent (PlayerId(1)) has a Mountain on the battlefield with a red mana ability.
        let mountain = create_object(
            &mut state,
            CardId(201),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&mountain).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Mountain".to_string());
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Red],
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

        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();

        // Should produce red mana (from opponent's Mountain).
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn opponent_land_colors_no_opponent_lands_produces_nothing() {
        // CR 106.5 + CR 106.7: If no color can be defined, produce no mana.
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn opponent_land_colors_mirror_exotic_orchard_no_recursion() {
        // CR 106.7: Two opposing Exotic Orchards with no other lands —
        // neither can define a color, so both produce no mana (no infinite recursion).
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Opponent (PlayerId(1)) has an Exotic Orchard (OpponentLandColors ability).
        let opp_orchard = create_object(
            &mut state,
            CardId(301),
            PlayerId(1),
            "Exotic Orchard".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&opp_orchard).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::OpponentLandColors {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        // Player 0 activates their own OpponentLandColors ability.
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();

        // No recursion; opponent's Exotic Orchard is skipped, so no colors available.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn restriction_spell_type_attaches_to_produced_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::SpellType("Creature".to_string())],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.restrictions.len(), 1);
        assert_eq!(
            unit.restrictions[0],
            ManaRestriction::OnlyForSpellType("Creature".to_string())
        );
    }

    #[test]
    fn restriction_chosen_creature_type_resolves_from_source() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let obj_id = ObjectId(200);
        let mut obj = crate::game::game_object::GameObject::new(
            obj_id,
            CardId(2),
            PlayerId(0),
            "Cavern of Souls".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::CreatureType("Elf".to_string()));
        state.objects.insert(obj_id, obj);

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::ChosenCreatureType],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.restrictions.len(), 1);
        assert_eq!(
            unit.restrictions[0],
            ManaRestriction::OnlyForCreatureType("Elf".to_string())
        );
    }

    #[test]
    fn restriction_chosen_creature_type_drops_when_no_choice() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::ChosenCreatureType],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // No source object → restriction can't resolve → mana is unrestricted
        let unit = &state.players[0].mana_pool.mana[0];
        assert!(unit.restrictions.is_empty());
    }

    #[test]
    fn grants_flow_through_to_mana_unit() {
        use crate::types::mana::ManaSpellGrant;

        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![ManaSpellGrant::CantBeCountered],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.grants, vec![ManaSpellGrant::CantBeCountered]);
    }

    /// CR 106.7 + CR 106.1b: Reflecting Pool — produces one mana of any type
    /// that a land you control could produce. With a Plains and a Swamp on the
    /// battlefield, the type union is {W, B}; the resolver picks the first
    /// listed type when no choice override is supplied (mirrors `AnyOneColor`).
    #[test]
    fn any_type_produceable_by_you_control_unions_types() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Player 0 controls a Plains and a Swamp.
        for (card_id, name, color, subtype) in [
            (CardId(401), "Plains", ManaColor::White, "Plains"),
            (CardId(402), "Swamp", ManaColor::Black, "Swamp"),
        ] {
            let id = create_object(
                &mut state,
                card_id,
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push(subtype.to_string());
            Arc::make_mut(&mut obj.abilities).push(
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

        let land_filter = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter,
            }),
            &mut events,
        )
        .unwrap();

        // CR 106.7: Per-unit `first()` selection out of the type union — the
        // union is order-dependent on object iteration, so we assert that the
        // produced mana is one of the two valid contributing types (W or B)
        // rather than pinning to a single iteration order.
        assert_eq!(state.players[0].mana_pool.total(), 1);
        let white = state.players[0].mana_pool.count_color(ManaType::White);
        let black = state.players[0].mana_pool.count_color(ManaType::Black);
        assert_eq!(
            white + black,
            1,
            "produced mana must come from the {{W,B}} type union (got W={white}, B={black})"
        );

        // The full type union (helper-level) must include both colors.
        let options = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)),
            PlayerId(0),
            ObjectId(100),
        );
        assert!(options.contains(&ManaType::White), "union must include W");
        assert!(options.contains(&ManaType::Black), "union must include B");
    }

    /// CR 106.5 + CR 106.7: When no land matches the filter, the type union is
    /// empty, so the ability produces no mana.
    #[test]
    fn any_type_produceable_by_empty_union_produces_nothing() {
        use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let land_filter = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter,
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    /// CR 106.7: Two Reflecting Pools facing each other (no other lands) — the
    /// recursive `AnyTypeProduceableBy` skip prevents infinite recursion and
    /// the union collapses to empty (CR 106.5 — no mana).
    #[test]
    fn any_type_produceable_by_recursive_yields_empty() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let recursive_filter =
            TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));

        // Player 0 has a Reflecting Pool already on the battlefield.
        let pool = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Reflecting Pool".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pool).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyTypeProduceableBy {
                        count: QuantityExpr::Fixed { value: 1 },
                        land_filter: recursive_filter.clone(),
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyTypeProduceableBy {
                count: QuantityExpr::Fixed { value: 1 },
                land_filter: recursive_filter,
            }),
            &mut events,
        )
        .unwrap();

        // Both producers are recursive; no other lands → empty union → no mana.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    /// CR 106.1b: Reflecting Pool reads "any **type**" — a Wastes you control
    /// (which produces colorless) must contribute `Colorless` to the union.
    #[test]
    fn any_type_produceable_by_includes_colorless() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // Player 0 controls a Wastes (produces {C}).
        let wastes = create_object(
            &mut state,
            CardId(601),
            PlayerId(0),
            "Wastes".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&wastes).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let land_filter = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
        let options = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &land_filter,
            PlayerId(0),
            ObjectId(9999),
        );
        assert!(
            options.contains(&ManaType::Colorless),
            "type union must include Colorless when a Wastes is controlled (CR 106.1b)"
        );
    }

    /// CR 106.7 + CR 106.5: P0 controls Exotic Orchard (`OpponentLandColors`),
    /// P1 controls Reflecting Pool (`AnyTypeProduceableBy`), and neither
    /// player controls any other land. The mutual recursion guard must be
    /// symmetric — `opponent_land_color_options` skips both recursive
    /// producers, so the survey terminates with the empty set rather than
    /// re-anchoring `ControllerRef::You` to the wrong player or looping.
    /// Activating either side produces no mana per CR 106.5.
    #[test]
    fn exotic_orchard_with_opponent_reflecting_pool_no_panic() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);

        // P0 controls Exotic Orchard.
        let orchard = create_object(
            &mut state,
            CardId(701),
            PlayerId(0),
            "Exotic Orchard".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&orchard).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::OpponentLandColors {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        // P1 controls Reflecting Pool.
        let pool = create_object(
            &mut state,
            CardId(702),
            PlayerId(1),
            "Reflecting Pool".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&pool).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyTypeProduceableBy {
                        count: QuantityExpr::Fixed { value: 1 },
                        land_filter: TargetFilter::Typed(
                            TypedFilter::land().controller(ControllerRef::You),
                        ),
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        // P0's Exotic Orchard surveys P1's lands → only finds Reflecting Pool
        // (recursive — skipped) → empty set.
        let orchard_opts =
            crate::game::mana_sources::opponent_land_color_options(&state, PlayerId(0));
        assert!(
            orchard_opts.is_empty(),
            "Exotic Orchard facing only an opponent's Reflecting Pool must yield empty (CR 106.5); got {orchard_opts:?}"
        );

        // P1's Reflecting Pool surveys P1's lands → only itself (recursive,
        // skipped) → empty set. (Cross-controller cycle terminates cleanly.)
        let pool_opts = crate::game::mana_sources::produceable_mana_types_by_filter(
            &state,
            &TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)),
            PlayerId(1),
            pool,
        );
        assert!(
            pool_opts.is_empty(),
            "Reflecting Pool with no other own lands must yield empty (CR 106.5); got {pool_opts:?}"
        );

        // Both should activate without panic and produce zero mana.
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            }),
            &mut events,
        )
        .unwrap();
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }
}
