use std::collections::HashSet;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, GainLifePlayer, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;

/// Signals that a replacement effect requires player choice before the event can proceed.
/// Callers receiving this must yield control (the engine will re-enter after the choice).
#[derive(Debug)]
pub struct ReplacementDeferred;

/// CR 119.1: Gain life — increase the player's life total.
pub fn resolve_gain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, player_kind) = match &ability.effect {
        Effect::GainLife { amount, player } => (amount, player),
        _ => return Err(EffectError::MissingParam("GainLife amount".to_string())),
    };

    // Resolve the target object (if any) for TargetedController.
    let target_obj = ability.targets.iter().find_map(|t| {
        if let TargetRef::Object(id) = t {
            state.objects.get(id)
        } else {
            None
        }
    });

    let player_id: PlayerId = match player_kind {
        GainLifePlayer::TargetedController => target_obj
            .map(|o| o.controller)
            .unwrap_or(ability.controller),
        GainLifePlayer::Controller => ability.controller,
        // CR 115.2 + CR 601.2c: "Target player gains N life" — the
        // chosen player is bound on `ability.targets` via the spell-
        // announcement target slot. `target_player()` extracts the
        // first `TargetRef::Player` and falls back to controller.
        GainLifePlayer::TargetPlayer => ability.target_player(),
    };

    // CR 119.7: "If an effect says that a player can't gain life ... a replacement
    // effect that would replace a life gain event affecting that player won't do
    // anything." Short-circuit BEFORE the replacement pipeline.
    if crate::game::static_abilities::player_has_cant_gain_life(state, player_id) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let final_amount = resolve_quantity_with_targets(state, amount, ability);

    if final_amount <= 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let proposed = ProposedEvent::LifeGain {
        player_id,
        amount: final_amount as u32,
        applied: HashSet::new(),
    };

    // CR 614.1a: Route life gain through replacement pipeline.
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::LifeGain {
                player_id,
                amount: gain_amount,
                ..
            } = event
            {
                let player = state
                    .players
                    .iter_mut()
                    .find(|p| p.id == player_id)
                    .ok_or(EffectError::PlayerNotFound)?;
                player.life += gain_amount as i32;
                // CR 119.9: Track life gained this turn for triggered ability matching.
                player.life_gained_this_turn += gain_amount;
                state.layers_dirty = true;

                events.push(GameEvent::LifeChanged {
                    player_id,
                    amount: gain_amount as i32,
                });
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            // TODO(CR 614.7): When multiple replacement effects apply to life gain, controller should choose which applies first. Currently falls through unconditionally.
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Apply life gain, running through the replacement pipeline.
/// Returns the actual amount of life gained (may differ due to replacements like Leyline of Hope).
/// Returns `Err(ReplacementDeferred)` when multiple replacement effects compete and
/// the player must choose which applies first (CR 614.7).
pub fn apply_life_gain(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<u32, ReplacementDeferred> {
    if amount == 0 {
        return Ok(0);
    }
    // CR 119.7: Short-circuit BEFORE the replacement pipeline — "can't gain life"
    // suppresses the life gain event entirely (and any replacements that would
    // otherwise modify it).
    if crate::game::static_abilities::player_has_cant_gain_life(state, player_id) {
        return Ok(0);
    }
    let proposed = ProposedEvent::LifeGain {
        player_id,
        amount,
        applied: HashSet::new(),
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            Ok(apply_life_gain_after_replacement(state, event, events))
        }
        ReplacementResult::Prevented => Ok(0),
        ReplacementResult::NeedsChoice(player) => {
            // CR 614.7: Multiple competing replacements — player must choose.
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            Err(ReplacementDeferred)
        }
    }
}

/// CR 119.1: Apply a post-replacement `ProposedEvent::LifeGain` to the game state.
///
/// Extracted from `apply_life_gain`'s Execute arm so the same mutation can be
/// invoked by `handle_replacement_choice` when a player accepts a life-gain
/// replacement choice. Caller is responsible for emitting `EffectResolved`.
pub fn apply_life_gain_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> u32 {
    let ProposedEvent::LifeGain {
        player_id: pid,
        amount: gain_amount,
        ..
    } = event
    else {
        debug_assert!(
            false,
            "apply_life_gain_after_replacement called with non-LifeGain ProposedEvent"
        );
        return 0;
    };
    if let Some(player) = state.players.iter_mut().find(|p| p.id == pid) {
        player.life += gain_amount as i32;
        player.life_gained_this_turn += gain_amount;
    }
    state.layers_dirty = true;
    events.push(GameEvent::LifeChanged {
        player_id: pid,
        amount: gain_amount as i32,
    });
    gain_amount
}

/// CR 120.3: Damage to a player causes that much life loss.
/// Returns the actual amount of life lost (may differ due to replacements like doubling).
/// Returns `Err(ReplacementDeferred)` when multiple replacement effects compete and
/// the player must choose which applies first (CR 614.7).
pub fn apply_damage_life_loss(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<u32, ReplacementDeferred> {
    if amount == 0 {
        return Ok(0);
    }
    // CR 119.8 + CR 120.3: When a player "can't lose life," damage-to-life-loss
    // conversion is suppressed. Short-circuit BEFORE the replacement pipeline.
    if crate::game::static_abilities::player_has_cant_lose_life(state, player_id) {
        return Ok(0);
    }
    let proposed = ProposedEvent::LifeLoss {
        player_id,
        amount,
        applied: HashSet::new(),
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            Ok(apply_life_loss_after_replacement(state, event, events))
        }
        ReplacementResult::Prevented => Ok(0),
        ReplacementResult::NeedsChoice(player) => {
            // CR 614.7: Multiple competing replacements — player must choose.
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            Err(ReplacementDeferred)
        }
    }
}

/// CR 120.3: Apply a post-replacement `ProposedEvent::LifeLoss` to the game state.
///
/// Extracted from `apply_damage_life_loss`'s Execute arm so the same mutation can
/// be invoked by `handle_replacement_choice` when a player accepts a life-loss
/// replacement choice. Caller is responsible for emitting `EffectResolved`.
pub fn apply_life_loss_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> u32 {
    let ProposedEvent::LifeLoss {
        player_id: pid,
        amount: loss_amount,
        ..
    } = event
    else {
        debug_assert!(
            false,
            "apply_life_loss_after_replacement called with non-LifeLoss ProposedEvent"
        );
        return 0;
    };
    if let Some(player) = state.players.iter_mut().find(|p| p.id == pid) {
        player.life -= loss_amount as i32;
        player.life_lost_this_turn += loss_amount;
    }
    state.layers_dirty = true;
    events.push(GameEvent::LifeChanged {
        player_id: pid,
        amount: -(loss_amount as i32),
    });
    loss_amount
}

/// CR 119.3: If an effect causes a player to lose life, adjust their life total.
pub fn resolve_lose(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, target_filter): (i32, Option<&TargetFilter>) = match &ability.effect {
        Effect::LoseLife { amount, target } => (
            crate::game::quantity::resolve_quantity_with_targets(state, amount, ability),
            target.as_ref(),
        ),
        _ => return Err(EffectError::MissingParam("LoseLife amount".to_string())),
    };

    let target_player_id = resolve_life_loss_target(state, ability, target_filter);

    // CR 119.8: "If an effect says that a player can't lose life ... in that case,
    // the exchange won't happen." Short-circuit BEFORE the replacement pipeline.
    if crate::game::static_abilities::player_has_cant_lose_life(state, target_player_id) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let proposed = ProposedEvent::LifeLoss {
        player_id: target_player_id,
        amount: amount as u32,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::LifeLoss {
                player_id,
                amount: loss_amount,
                ..
            } = event
            {
                let player = state
                    .players
                    .iter_mut()
                    .find(|p| p.id == player_id)
                    .ok_or(EffectError::PlayerNotFound)?;
                player.life -= loss_amount as i32;
                player.life_lost_this_turn += loss_amount;
                state.layers_dirty = true;

                events.push(GameEvent::LifeChanged {
                    player_id,
                    amount: -(loss_amount as i32),
                });
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

fn resolve_life_loss_target(
    state: &GameState,
    ability: &ResolvedAbility,
    target_filter: Option<&TargetFilter>,
) -> PlayerId {
    // CR 115.1: When the filter is a context-ref (Controller, etc.) the acting
    // player MUST come from state slots — not `ability.targets`, which inherits
    // the parent's chosen Player target via chain target propagation. Mirrors
    // the Draw/Mill/Discard guard in `resolve_player_for_context_ref`.
    if let Some(filter) = target_filter {
        if filter.is_context_ref() {
            return super::resolve_player_for_context_ref(state, ability, filter);
        }
    }

    // Non-context-ref filters (e.g., explicit Player target on "target opponent
    // loses 2 life"): the chosen player is in `ability.targets`.
    if let Some(player) = ability.targets.iter().find_map(|target| match target {
        TargetRef::Player(player) => Some(*player),
        _ => None,
    }) {
        return player;
    }

    // No filter and no Player target: defensive fallback to controller (matches
    // historical behavior for `LoseLife { target: None }`).
    ability.controller
}

/// CR 119.5: Set a player's life total to a specific number.
///
/// Per CR 119.5: "If an effect sets a player's life total to a specific number,
/// the player gains or loses the necessary amount of life to end up with the
/// new total." The delta is therefore dispatched as either a `LifeGain` or
/// `LifeLoss` event through [`apply_life_gain`] / [`apply_damage_life_loss`] so
/// the full replacement pipeline fires and the CantGainLife / CantLoseLife
/// short-circuits are consistent with every other life-change event.
pub fn resolve_set_life_total(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let amount = match &ability.effect {
        Effect::SetLifeTotal { amount, .. } => {
            crate::game::quantity::resolve_quantity_with_targets(state, amount, ability)
        }
        _ => return Err(EffectError::MissingParam("SetLifeTotal amount".to_string())),
    };

    let target_player_id = ability
        .targets
        .iter()
        .find_map(|t| {
            if let TargetRef::Player(pid) = t {
                Some(*pid)
            } else {
                None
            }
        })
        .unwrap_or(ability.controller);

    let current_life = state
        .players
        .iter()
        .find(|p| p.id == target_player_id)
        .ok_or(EffectError::PlayerNotFound)?
        .life;
    let diff = amount - current_life;

    // CR 119.5: Decompose into the matching gain/loss event. A diff of 0 is a
    // no-op. apply_life_gain / apply_damage_life_loss each handle their own
    // CR 119.7 / CR 119.8 short-circuits and replacement pipeline routing.
    let deferred = match diff.signum() {
        1 => apply_life_gain(state, target_player_id, diff as u32, events).err(),
        -1 => apply_damage_life_loss(state, target_player_id, (-diff) as u32, events).err(),
        _ => None,
    };
    if deferred.is_some() {
        // CR 614.7: A competing replacement required a player choice; the
        // helper already installed the WaitingFor state. Return without
        // emitting EffectResolved — the resume path will complete resolution.
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, QuantityExpr, StaticDefinition, TargetFilter, TargetRef, TypedFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    /// Helper: add a permanent with the given life-lock `StaticMode` affecting
    /// players matching `ControllerRef`. Mirrors `win_lose::tests::add_cant_win_permanent`.
    fn add_life_lock_permanent(
        state: &mut GameState,
        owner: PlayerId,
        mode: StaticMode,
        affected_controller: ControllerRef,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(900),
            owner,
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(mode).affected(TargetFilter::Typed(
                TypedFilter::default().controller(affected_controller),
            )),
        );
        id
    }

    #[test]
    fn gain_life_increases_controller_life() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 25);
    }

    #[test]
    fn lose_life_decreases_target_life() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn lose_life_parent_target_controller_uses_attack_event_source() {
        let mut state = GameState::new_two_player(42);
        let decree = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Marchesa's Decree".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: PlayerId(0),
            attacks: vec![(
                attacker,
                crate::game::combat::AttackTarget::Player(PlayerId(0)),
            )],
        });
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::ParentTargetController),
            },
            vec![],
            decree,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 19);
    }

    #[test]
    fn gain_life_emits_positive_life_changed() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount == 4)));
    }

    #[test]
    fn lose_life_emits_negative_life_changed() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount == -2)));
    }

    /// CR 119.7: "can't gain life" suppresses life gain, life total unchanged.
    #[test]
    fn gain_life_blocked_by_cant_gain_life() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );

        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 20, "life total must not change");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { .. })),
            "no LifeChanged event should be emitted"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::GainLife,
                ..
            }
        )));
    }

    /// CR 119.7: `apply_life_gain` must short-circuit before the replacement
    /// pipeline — replacements "won't do anything" per CR 119.7.
    #[test]
    fn apply_life_gain_short_circuits_on_cant_gain() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );

        let mut events = Vec::new();
        let gained = apply_life_gain(&mut state, PlayerId(0), 3, &mut events).unwrap();

        assert_eq!(gained, 0);
        assert_eq!(state.players[0].life, 20);
    }

    /// CR 119.8: `apply_damage_life_loss` short-circuits for a CantLoseLife player.
    #[test]
    fn apply_damage_life_loss_short_circuits_on_cant_lose() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantLoseLife,
            ControllerRef::You,
        );

        let mut events = Vec::new();
        let lost = apply_damage_life_loss(&mut state, PlayerId(0), 4, &mut events).unwrap();

        assert_eq!(lost, 0);
        assert_eq!(state.players[0].life, 20);
    }

    /// CR 119.8: `resolve_lose` suppresses life loss for CantLoseLife player.
    #[test]
    fn lose_life_blocked_by_cant_lose_life() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(1),
            StaticMode::CantLoseLife,
            ControllerRef::You,
        );

        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 20);
    }

    /// CR 119.5 + CR 119.7 + CR 119.8: Set-life-total is gated by both locks.
    /// With both active (Teferi's Protection case), no life change occurs even
    /// from a set-life-to-N effect.
    #[test]
    fn set_life_total_blocked_by_both_locks() {
        let mut state = GameState::new_two_player(42);
        let id = add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );
        // Add the CantLoseLife static to the same permanent.
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::CantLoseLife).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );

        // Try to set PlayerId(0)'s life to 10 (would lose 10).
        let ability_loss = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 10 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability_loss, &mut events).unwrap();
        assert_eq!(state.players[0].life, 20, "life loss must be suppressed");

        // Try to set life to 40 (would gain 20).
        let ability_gain = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 40 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability_gain, &mut events).unwrap();
        assert_eq!(state.players[0].life, 20, "life gain must be suppressed");
    }

    /// CR 119.5 + CR 119.8: Setting life to a lower total under CantLoseLife
    /// suppresses only the loss direction — the life total stays the same.
    #[test]
    fn set_life_total_downward_blocked_by_cant_lose_only() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantLoseLife,
            ControllerRef::You,
        );

        // Setting life to 5 would lose 15.
        let ability = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state.players[0].life, 20,
            "loss direction must be suppressed"
        );
    }

    /// CR 119.5 + CR 119.7: Setting life to a higher total under CantGainLife
    /// suppresses only the gain direction — the life total stays the same.
    #[test]
    fn set_life_total_upward_blocked_by_cant_gain_only() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );

        // Setting life to 30 would gain 10.
        let ability = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 30 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state.players[0].life, 20,
            "gain direction must be suppressed"
        );
    }

    /// CR 119.5: With no locks, set-life-total routes through the gain/loss
    /// helpers and updates the life total both directions.
    #[test]
    fn set_life_total_both_directions_without_locks() {
        // Upward.
        let mut state = GameState::new_two_player(42);
        let ability_gain = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 30 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability_gain, &mut events).unwrap();
        assert_eq!(state.players[0].life, 30, "set-life-up must take effect");

        // Downward.
        let ability_loss = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability_loss, &mut events).unwrap();
        assert_eq!(state.players[0].life, 5, "set-life-down must take effect");
    }

    /// CR 119.7: The lock only affects players matching the static's filter.
    /// An opponent with no lock continues to gain life normally.
    #[test]
    fn cant_gain_life_only_affects_matching_player() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );

        // PlayerId(1) is not affected by this permanent's "You" scope.
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 25, "opponent still gains life");
    }

    #[test]
    fn lose_life_controller_filter_does_not_inherit_parent_player_target() {
        // CR 115.1 regression: a chained `LoseLife { target: Some(Controller) }`
        // must hit the spell controller, not the parent's inherited Player
        // target. Mirrors the Discard / Shuffle / Mill / Draw guard.
        let mut state = GameState::new_two_player(42);
        let p0_life_before = state.players[0].life;
        let p1_life_before = state.players[1].life;

        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::Controller),
            },
            vec![TargetRef::Player(PlayerId(1))], // inherited parent target
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].life,
            p0_life_before - 2,
            "P0 (controller) should lose life — Controller filter resolves to caster"
        );
        assert_eq!(
            state.players[1].life, p1_life_before,
            "P1 must not lose life despite being in ability.targets — Controller filter must not consult inherited targets"
        );
    }
}
