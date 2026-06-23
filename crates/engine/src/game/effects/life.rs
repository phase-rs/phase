use std::collections::HashSet;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
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
    let (amount, player_filter) = match &ability.effect {
        Effect::GainLife { amount, player } => (amount, player),
        _ => return Err(EffectError::MissingParam("GainLife amount".to_string())),
    };

    // CR 119.3: Who gains the life. `resolve_life_loss_target` is the single
    // authority for player resolution from a TargetFilter — context-refs
    // (Controller, ParentTargetController) resolve via state slots; explicit
    // Player targets come from `ability.targets`.
    let player_id: PlayerId = resolve_life_loss_target(state, ability, Some(player_filter));

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
                crate::game::layers::mark_layers_full(state);

                events.push(GameEvent::LifeChanged {
                    player_id,
                    amount: gain_amount as i32,
                });
            }
        }
        ReplacementResult::Prevented => {
            // CR 614.1a + CR 614.12a — Issue #317: Cross-event-type
            // substitution ("If you would gain life, draw that many cards
            // instead" — Lich). The applier returned `Prevented` and stashed
            // the substitute effect (e.g., Draw) as a post-replacement
            // continuation. Drain it now so the replacement actually
            // runs in the same resolution step.
            drain_substitution_continuation(state, events);
        }
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
            let gained = apply_life_gain_after_replacement(state, event, events);
            // CR 614.6 + CR 704.3: Drain any mandatory-post-effect continuation
            // (cross-event substitutes such as Lich; same-event work beyond the
            // applier's amount substitution) inside this resolution step so
            // SBAs and priority never fall between the (possibly substituted)
            // life gain and its substitute. Mirrors the drain pattern in
            // `effects/draw.rs::draw_through_replacement`. The `Prevented` arm
            // below already drains via `drain_substitution_continuation`.
            drain_substitution_continuation(state, events);
            Ok(gained)
        }
        ReplacementResult::Prevented => {
            // CR 614.1a + CR 614.12a — Issue #317: Drain substitute effect
            // stashed by cross-event-type replacement (Lich-class).
            drain_substitution_continuation(state, events);
            Ok(0)
        }
        ReplacementResult::NeedsChoice(player) => {
            // CR 614.7: Multiple competing replacements — player must choose.
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            Err(ReplacementDeferred)
        }
    }
}

/// CR 614.1a + CR 614.12a — Issue #317: Drain a `post_replacement_continuation`
/// stashed by cross-event-type substitution in a life-gain or life-loss
/// replacement. Mirrors the drain points in `engine.rs` (land plays) and
/// `stack.rs` (stack resolution); life-change events have no natural drain
/// site otherwise, so the substitute effect ("draw that many cards instead",
/// Lich) would silently never run.
///
/// `EventContextAmount` in the substitute reads `state.last_effect_count`
/// (CR 615.5 fallback path), which the applier stamps with the prevented
/// amount before returning.
fn drain_substitution_continuation(state: &mut GameState, events: &mut Vec<GameEvent>) {
    if state.post_replacement_continuation.is_some() {
        let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
            state, None, None, None, events,
        );
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
    crate::game::layers::mark_layers_full(state);
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
        ReplacementResult::Prevented => {
            // CR 614.1a + CR 614.12a — Issue #317: Drain substitute effect
            // stashed by cross-event-type replacement.
            drain_substitution_continuation(state, events);
            Ok(0)
        }
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
    crate::game::layers::mark_layers_full(state);
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
                crate::game::layers::mark_layers_full(state);

                events.push(GameEvent::LifeChanged {
                    player_id,
                    amount: -(loss_amount as i32),
                });
            }
        }
        ReplacementResult::Prevented => {
            // CR 614.1a + CR 614.12a — Issue #317: Drain substitute effect
            // stashed by cross-event-type replacement.
            drain_substitution_continuation(state, events);
        }
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
    let (amount_expr, target) = match &ability.effect {
        Effect::SetLifeTotal { amount, target } => (amount, target),
        _ => return Err(EffectError::MissingParam("SetLifeTotal amount".to_string())),
    };
    // CR 119.5 + CR 608.2f: Resolve which players' life totals are set. The
    // common single-player forms ("your" → Controller, "target player's" →
    // the chosen target) preserve the original single-player behavior. The
    // non-targeted all-players form ("each player's life total becomes N" —
    // Worldfire, issue #2882) expands to every player in APNAP order.
    let target_player_ids: Vec<PlayerId> = if matches!(target, TargetFilter::AllPlayers) {
        crate::game::players::apnap_order(state)
    } else {
        vec![ability
            .targets
            .iter()
            .find_map(|t| {
                if let TargetRef::Player(pid) = t {
                    Some(*pid)
                } else {
                    None
                }
            })
            .unwrap_or(ability.controller)]
    };

    // CR 119.5: Set each player's life total one at a time, decomposing into the
    // matching gain/loss event. apply_life_gain / apply_damage_life_loss each
    // handle their own CR 119.7 / CR 119.8 short-circuits and replacement
    // pipeline routing.
    for target_player_id in target_player_ids {
        // CR 119.5 + CR 109.5: Resolve the new life total per player so a
        // third-person "the number of [X] THEY control" count (Biorhythm,
        // Shaman of Forgotten Ways) binds to each recipient. `scoped_player` is
        // rebound to the player whose life total is being set; the count's
        // `ScopedPlayer` controller reads it, while `original_controller` (and
        // hence any `You`-scoped count) and the ability's targets / chosen_x
        // stay fixed to the caster. Single-player and caster-scoped forms
        // ("becomes 10", "your life total", Repay in Kind's cross-player
        // extremum) are unaffected — they carry no `ScopedPlayer` ref to vary.
        let mut scoped_ability = ability.clone();
        scoped_ability.set_scoped_player_recursive(target_player_id);
        let amount = crate::game::quantity::resolve_quantity_with_targets(
            state,
            amount_expr,
            &scoped_ability,
        );

        // CR 810.9a: "If a cost or effect needs to know the value of an
        // individual player's life total, that cost or effect uses the
        // team's life total instead" — degenerates to `Player::life` outside
        // team-based formats. CR 810.9c: the diff is still applied to only
        // `target_player_id`'s own life, so the team total moves by exactly
        // the gained/lost amount.
        if !state.players.iter().any(|p| p.id == target_player_id) {
            return Err(EffectError::PlayerNotFound);
        }
        let current_life = crate::game::players::team_life_total(state, target_player_id);
        let diff = amount - current_life;

        let deferred = match diff.signum() {
            1 => apply_life_gain(state, target_player_id, diff as u32, events).err(),
            -1 => apply_damage_life_loss(state, target_player_id, (-diff) as u32, events).err(),
            _ => None,
        };
        if deferred.is_some() {
            // CR 614.7: A competing replacement required a player choice; the
            // helper already installed the WaitingFor state. Return without
            // emitting EffectResolved — the resume path completes resolution.
            // (A multi-player set that hits a replacement mid-list defers from
            // that player onward, mirroring the original single-player path.)
            return Ok(());
        }
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
    use crate::game::game_object::AttachTarget;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AggregateFunction, ControllerRef, QuantityExpr, QuantityRef, SharedQuality,
        StaticDefinition, TargetFilter, TargetRef, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
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
                player: TargetFilter::Controller,
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
    fn set_life_total_all_players_sets_every_player_no_target() {
        // CR 119.5 + issue #2882: Worldfire's "Each player's life total becomes 1"
        // must set EVERY player to 1 with no targeting prompt — not just the
        // controller and not a single chosen player.
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        state.players[1].life = 15;
        let ability = ResolvedAbility::new(
            Effect::SetLifeTotal {
                target: TargetFilter::AllPlayers,
                amount: QuantityExpr::Fixed { value: 1 },
            },
            vec![], // no Player targets — AllPlayers is a non-targeted scope
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_set_life_total(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].life, 1,
            "controller's life should become 1"
        );
        assert_eq!(state.players[1].life, 1, "opponent's life should become 1");
    }

    #[test]
    fn skemfar_shadowsage_gain_life_mode_uses_shared_creature_type_count() {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec![
            "Elf".to_string(),
            "Warrior".to_string(),
            "Druid".to_string(),
            "Human".to_string(),
        ];
        let source = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Skemfar Shadowsage".to_string(),
            Zone::Battlefield,
        );
        for (name, subtypes) in [
            ("Elf Warrior", vec!["Elf", "Warrior"]),
            ("Elf Druid", vec!["Elf", "Druid"]),
            ("Human Warrior", vec!["Human", "Warrior"]),
        ] {
            let id = create_object(
                &mut state,
                CardId(902),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.card_types.subtypes = subtypes
                .into_iter()
                .map(|subtype| subtype.to_string())
                .collect();
        }
        let changeling = create_object(
            &mut state,
            CardId(903),
            PlayerId(0),
            "Masked Vandal".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&changeling).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.card_types.subtypes = vec!["Shapeshifter".to_string()];
        obj.keywords.push(Keyword::Changeling);

        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCountBySharedQuality {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: Some(ControllerRef::You),
                            properties: Vec::new(),
                        }),
                        quality: SharedQuality::CreatureType,
                        aggregate: AggregateFunction::Max,
                    },
                },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 23);
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

    /// CR 115.1 + CR 119.3: Astarion, the Decadent (Feed mode) — "Target
    /// opponent loses life equal to the amount of life they lost this turn."
    /// The third-person "they" anaphor is the effect's player target, so the
    /// amount resolves through `PlayerScope::Target` (the target's *own*
    /// `life_lost_this_turn`), NOT the controller's. The controller's count is
    /// seeded high as a trap: a `Controller`-scoped resolution would drain the
    /// target by 99 instead of 3.
    #[test]
    fn lose_life_amount_target_relative_life_lost_this_turn() {
        use crate::types::ability::PlayerScope;
        let mut state = GameState::new_two_player(42);
        state.players[0].life_lost_this_turn = 99; // controller — must be ignored
        state.players[1].life_lost_this_turn = 3; // target opponent
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::Target,
                    },
                },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        // Target opponent (20 starting) loses 3 — their own life lost this turn.
        assert_eq!(state.players[1].life, 17);
        // Controller is untouched (it is the source, not a target/recipient).
        assert_eq!(state.players[0].life, 20);
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
    fn lose_life_attached_to_resolves_player_host() {
        let mut state = GameState::new_two_player(42);
        let curse = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Curse".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&curse).unwrap().attached_to =
            Some(AttachTarget::Player(PlayerId(1)));

        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::AttachedTo),
            },
            vec![],
            curse,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 18);
    }

    #[test]
    fn gain_life_emits_positive_life_changed() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Controller,
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
                player: TargetFilter::Controller,
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
                player: TargetFilter::Controller,
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

    /// Issue #310 audit: "Each opponent loses N life." (Bloodletting,
    /// Bloodtithe Harvester face, etc.) parses as `Effect::LoseLife
    /// { target: None }` with `player_scope: Opponent` on the surrounding
    /// ability. The player_scope iteration loop must rebind `controller`
    /// to each opponent per CR 608.2 + CR 109.5 so the inner LoseLife
    /// resolver picks the iterated player as the life loser.
    #[test]
    fn player_scope_opponent_lose_life_targets_each_opponent() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::PlayerFilter;

        let mut state = GameState::new_two_player(42);
        let p0_life_before = state.players[0].life;
        let p1_life_before = state.players[1].life;

        let mut ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].life, p0_life_before,
            "caster must not lose life from Each opponent loses N life"
        );
        assert_eq!(
            state.players[1].life,
            p1_life_before - 2,
            "opponent must lose 2 life"
        );
    }

    /// CR 119.3 + CR 109.5: Genesis of the Daleks chapter IV — "...and each of
    /// your opponents loses life equal to ...". End-to-end resolution guard for
    /// the parser fix that lifts the "each of your opponents" subject onto the
    /// sub_ability's `player_scope` (leaving `LoseLife.target: None`) instead of
    /// stamping a non-resolvable `Typed(controller=Opponent)` target.
    ///
    /// This drives the actual parsed encoding through `resolve_ability_chain`:
    /// the controller (p0) must NOT lose life and the opponent (p1) MUST. The
    /// AST-only parser test (`genesis_villainous_branch_splits_destroy_and_lose_life`)
    /// asserts the shape; this test asserts the runtime routing, closing the
    /// path-divergence gap where a green AST test masked the inverse drain.
    #[test]
    fn genesis_each_opponent_loses_life_drains_opponent() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::PlayerFilter;

        let mut state = GameState::new_two_player(42);
        let p0_life_before = state.players[0].life;
        let p1_life_before = state.players[1].life;

        // Genesis branch-1 LoseLife encoding: undirected `target: None` with the
        // each-opponent scope lifted onto the ability (the post-fix shape). The
        // amount is fixed here — the bug under test is target routing, not amount
        // resolution (the dynamic `ZoneChangeAggregateThisTurn` amount is covered
        // by the AST parser test).
        let mut ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 5 },
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].life, p0_life_before,
            "Genesis's controller (p0) must NOT lose life — the each-opponent \
             scope routes the loss to opponents, not the source controller"
        );
        assert_eq!(
            state.players[1].life,
            p1_life_before - 5,
            "each opponent (p1) must lose 5 life"
        );
    }

    /// CR 115.10a + CR 119.3 + CR 608.2c (Wound Reflection / Archfiend of Despair /
    /// Warlock Class L3): "each opponent loses life equal to the life they lost
    /// this turn" resolves with a `LifeLostThisTurn { ScopedPlayer }` amount under
    /// `player_scope: Opponent`. Each iterated opponent must lose its OWN life lost
    /// this turn — NOT the source controller's. The controller's count is seeded
    /// high (99) as a trap: the prior `Controller`-scoped encoding drained every
    /// opponent by 99. This is the canonical runtime regression guard for the
    /// reported bug; the parser fix is covered in oracle_effect/mod.rs.
    #[test]
    fn each_opponent_loses_own_life_lost_uses_scoped_player() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::{PlayerFilter, PlayerScope};

        // 3-player game so two distinct opponents prove per-iteration scoping.
        let mut state = GameState::new(crate::types::FormatConfig::standard(), 3, 42);
        state.players[0].life_lost_this_turn = 99; // controller — trap, must be ignored
        state.players[1].life_lost_this_turn = 3; // opponent A
        state.players[2].life_lost_this_turn = 5; // opponent B
        let p0_life_before = state.players[0].life;
        let p1_life_before = state.players[1].life;
        let p2_life_before = state.players[2].life;

        let mut ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::ScopedPlayer,
                    },
                },
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Controller untouched — it is the source, not an affected opponent.
        assert_eq!(
            state.players[0].life, p0_life_before,
            "controller must not lose life — the trap value (99) must be ignored"
        );
        // Each opponent loses ITS OWN life lost this turn (3 and 5), not 99.
        assert_eq!(
            state.players[1].life,
            p1_life_before - 3,
            "opponent A must lose its own life lost (3), not the controller's 99"
        );
        assert_eq!(
            state.players[2].life,
            p2_life_before - 5,
            "opponent B must lose its own life lost (5), not the controller's 99"
        );
    }

    /// Issue #317 (Lich): "If you would gain life, draw that many cards
    /// instead." The replacement substitutes a *different* event type
    /// (`Effect::Draw`) for the original `LifeGain` event. CR 614.1a +
    /// CR 614.12a: the original event is suppressed, the substitute effect
    /// runs as a post-replacement continuation. `EventContextAmount` in
    /// "draw that many cards" must resolve against the original prevented
    /// gain quantity (via `state.last_effect_count` per the CR 615.5
    /// fallback path).
    #[test]
    fn lich_gain_life_substituted_by_draw_cards_instead() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        // Lich source — its GainLife replacement substitutes Draw.
        let lich = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Lich".to_string(),
            Zone::Battlefield,
        );
        let replacement =
            ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        state
            .objects
            .get_mut(&lich)
            .unwrap()
            .replacement_definitions = vec![replacement].into();

        // Stock the controller's library so Draw has cards to pull.
        for i in 0..10 {
            create_object(
                &mut state,
                CardId(600 + i),
                PlayerId(0),
                format!("Library {i}"),
                Zone::Library,
            );
        }

        let p0_life_before = state.players[0].life;
        let p0_hand_before = state.players[0].hand.len();

        // Resolve a "you gain 4 life" effect on Lich's controller.
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();

        // Life must NOT change — the original LifeGain event is suppressed.
        assert_eq!(
            state.players[0].life, p0_life_before,
            "Lich's controller must not gain life — replacement substitutes Draw"
        );
        // Hand must contain 4 additional cards — "draw that many cards instead".
        assert_eq!(
            state.players[0].hand.len(),
            p0_hand_before + 4,
            "Lich's controller must draw 4 cards (matching the prevented gain amount)"
        );
        // The post-replacement continuation must be drained.
        assert!(
            state.post_replacement_continuation.is_none(),
            "post_replacement_continuation must be drained after life-gain replacement"
        );
    }

    /// Issue #317: A scaling-shape replacement (`Effect::GainLife { amount:
    /// Multiply { factor: 2, inner: EventContextAmount } }` — Boon Reflection
    /// shape) must still flow through Branch 2 of `gain_life_applier` and
    /// modify the amount — not be misclassified as substitution. This pins
    /// the boundary between "scaling" (same event type, modified amount) and
    /// "substitution" (different event type).
    #[test]
    fn gain_life_scaling_shape_modifies_amount_does_not_substitute() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let doubler = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Boon Reflection".to_string(),
            Zone::Battlefield,
        );
        let replacement =
            ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                    },
                    player: TargetFilter::Controller,
                },
            ));
        state
            .objects
            .get_mut(&doubler)
            .unwrap()
            .replacement_definitions = vec![replacement].into();

        let p0_life_before = state.players[0].life;
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].life,
            p0_life_before + 6,
            "Boon Reflection doubles the gain to 6 — scaling, not substitution"
        );
        // CR 614.6: the applier substituted the amount; the `post_effect`
        // filter must suppress stashing the same GainLife execute as a
        // continuation, and the Execute-arm drain must clear any residual
        // template. A leaked Template here would drain on the next zone
        // change as a phantom GainLife — same defect class as Jace
        // empty-library win.
        assert!(
            state.post_replacement_continuation.is_none(),
            "GainLife→GainLife amount-substitution must not leak a post-replacement \
             continuation; found {:?}",
            state.post_replacement_continuation
        );
    }

    #[test]
    fn gain_life_target_player_uses_declared_target() {
        // CR 115.1 + CR 119.3: "target player gains N life" — the gaining
        // player is the declared target in ability.targets, not the controller.
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.players[1].life, 24, "target player (p1) gains 4 life");
        assert_eq!(state.players[0].life, 20, "controller (p0) is unaffected");
    }
}
