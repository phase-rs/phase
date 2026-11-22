use crate::game::casting_costs::eligible_behold_choices;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

// #5051 (CR 400.7): behold-class. This fixed-quality `Effect::Behold` writes no
// ChosenAttribute, so the stale-chosen-type bug cannot manifest here. If
// `Effect::Behold` ever gains a `type_choice` axis (mirroring
// `AbilityCost::Behold` at types/ability.rs `type_choice`), it joins the #5051
// blast radius — clear chosen attributes on non-permanent zone exit then.

/// CR 701.4a: Behold a [quality] — "Reveal a [quality] card from your hand or
/// choose a [quality] permanent you control on the battlefield." The candidate
/// set is the shared authority `eligible_behold_choices` (battlefield-you-control
/// ∪ your matching hand cards). Three outcomes:
///
/// - **No candidate** (whiff): the player cannot behold. Set
///   `cost_payment_failed_flag` (the codebase-canonical "gated action didn't
///   happen" signal, mirrors `choose.rs`) and stash NO continuation, so an
///   "if you do, [rider]" gate reads `performed && !cost_payment_failed_flag` =
///   false and the rider does not fire.
/// - **One candidate** (forced, no agency): auto-select it; a hand card emits
///   `CardsRevealed` (CR 701.4a, card stays in hand), a battlefield permanent
///   reveals nothing. The chain then resolves any rider inline.
/// - **Two or more candidates** (real choice, CR 608.2d): park
///   `WaitingFor::BeholdChoice` for the controller. `resolve_ability_chain`
///   auto-stashes the rider as `pending_continuation`; the choice handler
///   (`engine_resolution_choices.rs`) resolves the reveal and drains it.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let filter = match &ability.effect {
        Effect::Behold { filter } => filter.clone(),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected Behold effect".to_string(),
            ))
        }
    };

    match begin_behold(
        state,
        ability.controller,
        ability.source_id,
        &filter,
        events,
    ) {
        BeholdStart::Unavailable => {
            // CR 701.4a + CR 608.2c: nothing to behold — the rider is suppressed
            // via `cost_payment_failed_flag`; no continuation is stashed so the
            // optional-accept clobber (`resolve_optional_effect_decision`) is a
            // no-op and the Token/rider is evaluated inline as false.
            state.cost_payment_failed_flag = true;
        }
        // The sole candidate was auto-selected; the chain resolves any rider
        // inline. The parked choice drains the auto-stashed rider after submit.
        BeholdStart::Selected | BeholdStart::Parked => {}
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// How a behold began: the single authority shared by the `Effect::Behold`
/// resolver and the resolution-time `AbilityCost::Behold` payment arm.
pub(crate) enum BeholdStart {
    /// No candidate: the player cannot behold.
    Unavailable,
    /// Exactly one candidate (no genuine choice): auto-selected, and revealed
    /// when it is a hand card.
    Selected,
    /// Two or more candidates: `WaitingFor::BeholdChoice` is parked.
    Parked,
}

/// CR 701.4a + CR 608.2d: Begin a behold — zero candidates are unavailable, a
/// sole candidate is auto-selected (revealing it if it is a hand card), and
/// two or more park `WaitingFor::BeholdChoice` for the player.
pub(crate) fn begin_behold(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    filter: &TargetFilter,
    events: &mut Vec<GameEvent>,
) -> BeholdStart {
    let choices = eligible_behold_choices(state, player, source_id, filter);
    match choices.as_slice() {
        [] => BeholdStart::Unavailable,
        [only] => {
            reveal_if_from_hand(state, player, *only, events);
            BeholdStart::Selected
        }
        _ => {
            state.waiting_for = WaitingFor::BeholdChoice { player, choices };
            BeholdStart::Parked
        }
    }
}

/// CR 701.4a: The reveal leg of behold. A chosen HAND card is revealed to all
/// players via `GameEvent::CardsRevealed` WITHOUT moving it (the card stays in
/// hand); a chosen battlefield permanent is already public, so no reveal is
/// emitted. Shared by the auto-select (single-candidate) resolver path and the
/// interactive `WaitingFor::BeholdChoice` submit handler so both behave
/// identically.
pub(crate) fn reveal_if_from_hand(
    state: &mut GameState,
    player: PlayerId,
    chosen_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    let from_hand = state
        .players
        .get(player.0 as usize)
        .is_some_and(|p| p.hand.contains(&chosen_id));
    if !from_hand {
        return;
    }
    let name = state
        .objects
        .get(&chosen_id)
        .map(|obj| obj.name.clone())
        .unwrap_or_default();
    events.push(GameEvent::CardsRevealed {
        player,
        occurrences: state.disclosed_occurrences(&[chosen_id]),
        card_ids: vec![chosen_id],
        card_names: vec![name],
    });
}
