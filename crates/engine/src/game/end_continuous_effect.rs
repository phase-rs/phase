//! CR 116.2c: the "pay a cost to end a continuous effect" special action.
//!
//! > 116.2c Some effects allow a player to take an action at a later time,
//! > usually to end a continuous effect [...]. Doing so is a special action. A
//! > player can take such an action any time they have priority, unless that
//! > effect specifies another timing restriction, for as long as the effect
//! > allows it.
//!
//! The shipped class is the Licid cycle ("You may pay {W} to end this effect").
//! Nothing in this module knows what a Licid or an Aura is: the permission rides
//! on the [`TransientContinuousEffect`] the creating resolution installed
//! (`end_permission`), so any future card that grants a mana-paid termination
//! permission is handled without a line of new code here.
//!
//! Mirrors `game::companion`'s seven-function shape (CR 116.2g), with ONE
//! deliberate divergence: `companion.rs` hard-gates its action to a sorcery-speed
//! window because CR 116.2g states that restriction. CR 116.2c states none, and
//! neither do the shipped cards, so this action has **no** phase gate, **no**
//! empty-stack gate, and **no** active-player gate. Calming Licid's printed
//! ruling (2013-04-15) and Flanking Licid's (2024-11-08) both say the payment is
//! a special action taken at priority that can't be responded to (CR 116.1 —
//! special actions don't use the stack).

use crate::types::events::GameEvent;
use crate::types::game_state::{EndEffectGroupId, GameState, ManaAbilityResume, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::SpecialAction;
use crate::types::player::PlayerId;

use super::casting::{self, SpecialActionManaPayment};
use super::engine::EngineError;
use super::layers;

/// Engine-authored presentation payload for one legal CR 116.2c action.
///
/// The frontend displays these fields verbatim and echoes the enclosing
/// `GameAction` back. Dispatch still revalidates `group` against live state and
/// never trusts the echoed name or cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndContinuousEffectCandidate {
    pub group: EndEffectGroupId,
    pub source_name: String,
    pub cost: crate::types::mana::ManaCost,
}

/// CR 116.2c: every live pay-to-end permission `player` controls, one entry per
/// [`EndEffectGroupId`].
///
/// De-duplicated by group because the permission names the whole continuous
/// effect one resolution created: a two-`StaticDefinition` clause installs two
/// `TransientContinuousEffect`s sharing one group, and offering the same
/// termination twice would be a duplicate action, not two choices.
///
/// CR 109.5 + CR 113.8: `tce.controller` records the activated ability's
/// controller, and it — not the source object's current controller — is the
/// meaning of "you" in the permission.
/// `transient_effect_is_live` gates out an effect that has lapsed but has not
/// yet been swept by a layer pass.
fn permissions_for(state: &GameState, player: PlayerId) -> Vec<EndContinuousEffectCandidate> {
    let mut out: Vec<EndContinuousEffectCandidate> = Vec::new();
    for tce in state.transient_continuous_effects.iter() {
        let Some(permission) = tce.end_permission.as_ref() else {
            continue;
        };
        if tce.controller != player || !layers::transient_effect_is_live(state, tce) {
            continue;
        }
        if out.iter().any(|seen| seen.group == permission.group) {
            continue;
        }
        out.push(EndContinuousEffectCandidate {
            group: permission.group,
            source_name: tce.source_name.clone(),
            cost: permission.cost.clone(),
        });
    }
    out
}

/// CR 116.2c: the object whose resolution installed `group`, for the
/// `ContinuousEffectEnded` event. Read before removal, since removing the group
/// destroys the only record of it.
pub(crate) fn source_of_group(state: &GameState, group: EndEffectGroupId) -> Option<ObjectId> {
    state
        .transient_continuous_effects
        .iter()
        .find(|tce| {
            tce.end_permission
                .as_ref()
                .is_some_and(|permission| permission.group == group)
        })
        .map(|tce| tce.source_id)
}

/// CR 116.2c: the non-payment preconditions for ending `group`. Kept separate
/// from payment so a forged direct action fails before auto-tapping any mana
/// source.
///
/// The permission itself is the whole precondition: CR 116.2c grants the action
/// "any time they have priority, unless that effect specifies another timing
/// restriction", and no shipped card in this class states one. Deliberately does
/// NOT consult `restrictions::is_sorcery_speed_window` — see the module doc.
fn validate_end_continuous_effect(
    state: &GameState,
    player: PlayerId,
    group: EndEffectGroupId,
) -> Result<EndContinuousEffectCandidate, EngineError> {
    permissions_for(state, player)
        .into_iter()
        .find(|candidate| candidate.group == group)
        .ok_or_else(|| {
            EngineError::InvalidAction(
                "No live continuous effect you control offers that termination cost".to_string(),
            )
        })
}

/// CR 116.2c: the groups `player` may end right now, including affordability
/// after automatic mana-source activation.
///
/// SINGLE AUTHORITY for the legal-action offering (`ai_support::candidates`).
pub fn end_continuous_effect_candidates(
    state: &GameState,
    player: PlayerId,
) -> Vec<EndContinuousEffectCandidate> {
    permissions_for(state, player)
        .into_iter()
        .filter(|candidate| {
            casting::can_pay_special_action_mana_cost_after_auto_tap(
                state,
                player,
                None,
                &candidate.cost,
                SpecialAction::EndContinuousEffect,
            )
        })
        .collect()
}

/// CR 116.2c + CR 118.1: pay the permission's printed cost.
///
/// `PaymentContext::SpecialAction(EndContinuousEffect)` is what makes mana
/// restricted to casting spells or activating abilities ineligible here: this
/// payment is neither (CR 116.1 — a special action doesn't use the stack).
///
/// A paused mana-source replacement leaves the continuous effect INSTALLED and
/// returns that prompt; only a completed payment commits the termination.
fn pay_end_continuous_effect_cost(
    state: &mut GameState,
    player: PlayerId,
    group: EndEffectGroupId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<SpecialActionManaPayment, EngineError> {
    let resume = ManaAbilityResume::EndContinuousEffect {
        player,
        group,
        cost: cost.clone(),
    };
    casting::pay_special_action_mana_cost_with_resume(
        state,
        player,
        None,
        cost,
        SpecialAction::EndContinuousEffect,
        Some(&resume),
        events,
    )
}

/// CR 116.2c: commit the termination only after full payment succeeds.
///
/// This is also the sole point that clears the player's undoable mana-tap record
/// for this non-mana action (mirroring `companion::commit_companion_to_hand`).
/// Doing it in `engine.rs`'s pre-dispatch `lands_tapped_for_mana` group would
/// clear the undo record even when payment fails or pauses.
///
/// CR 613.1: no characteristic is restored here, and none should be. Card types,
/// subtypes, and granted keywords are DERIVED from
/// `state.transient_continuous_effects` by `evaluate_layers`, so
/// `GameState::end_continuous_effect`'s removal + `layers_dirty.mark_full()` IS
/// the restoration. Explicit restore code would be a second, divergent source of
/// truth.
///
/// CR 704.3 + CR 704.5p: returning `WaitingFor::Priority` is what DETACHES an
/// ex-Aura Licid. `apply_action`'s tail runs
/// `engine_priority::run_post_action_pipeline` for any arm whose result is
/// `Priority`, and that pipeline's `while matches!(state.waiting_for, Priority)`
/// loop is the CR 704.3 state-based-action check, which now includes the
/// widened `sba::check_illegal_attachment_unattach`. Do NOT call the pipeline
/// explicitly here: this function runs INSIDE `apply_action`, so an explicit
/// call would run it twice.
fn commit_end_continuous_effect(
    state: &mut GameState,
    player: PlayerId,
    group: EndEffectGroupId,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let source_id = source_of_group(state, group);
    let removed = state.end_continuous_effect(group);
    if let (true, Some(source_id)) = (removed, source_id) {
        events.push(GameEvent::ContinuousEffectEnded {
            group,
            source_id,
            player,
        });
    }
    state.lands_tapped_for_mana.remove(&player);
    WaitingFor::Priority { player }
}

/// CR 116.2c: initiate the pay-to-end special action. Preflight occurs before
/// automatic mana activation, so an unaffordable or forged action cannot mutate
/// mana, waiting state, the effect list, or undo tracking.
pub fn handle_end_continuous_effect(
    state: &mut GameState,
    player: PlayerId,
    group: EndEffectGroupId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let candidate = validate_end_continuous_effect(state, player, group)?;
    if !casting::can_pay_special_action_mana_cost_after_auto_tap(
        state,
        player,
        None,
        &candidate.cost,
        SpecialAction::EndContinuousEffect,
    ) {
        return Err(EngineError::ActionNotAllowed(
            "Cannot pay the cost to end that continuous effect".to_string(),
        ));
    }

    match pay_end_continuous_effect_cost(state, player, group, &candidate.cost, events)? {
        SpecialActionManaPayment::Paid => {
            Ok(commit_end_continuous_effect(state, player, group, events))
        }
        SpecialActionManaPayment::Paused => Ok(state.waiting_for.clone()),
    }
}

/// CR 116.2c + CR 605.3b + CR 616.1: resume a pay-to-end payment after a
/// mana-source cost replacement choice.
///
/// `cost` was latched at initiation and must NOT be re-derived here: the board
/// may have changed while the replacement choice was pending. A second pause
/// retains the same typed root for another resumption.
pub(crate) fn resume_end_continuous_effect_payment(
    state: &mut GameState,
    player: PlayerId,
    group: EndEffectGroupId,
    cost: crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match pay_end_continuous_effect_cost(state, player, group, &cost, events)? {
        SpecialActionManaPayment::Paid => {
            Ok(commit_end_continuous_effect(state, player, group, events))
        }
        SpecialActionManaPayment::Paused => Ok(state.waiting_for.clone()),
    }
}
