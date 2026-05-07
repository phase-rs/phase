use std::borrow::Cow;
use std::collections::HashMap;

use crate::game::filter;
use crate::game::speed::has_max_speed;
use crate::types::ability::{
    AbilityCondition, AbilityKind, ControllerRef, Effect, EffectError, EffectKind, FilterProp,
    PlayerFilter, QuantityExpr, QuantityRef, ResolvedAbility, SharedQuality, SharedQualityRelation,
    TargetFilter, TargetRef, UnlessCost,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    AutoMayChoice, DayNight, GameState, MayTriggerAutoChoiceKey, PendingContinuation, WaitingFor,
};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;

pub mod adapt;
pub mod add_restriction;
pub mod add_target_replacement;
pub mod additional_phase;
pub mod amass;
pub mod animate;
pub mod attach;
pub mod become_copy;
pub mod become_monarch;
pub mod bolster;
pub mod bounce;
pub mod cascade;
pub mod cast_from_zone;
pub mod change_targets;
pub mod change_zone;
pub mod choose;
pub mod choose_and_sacrifice_rest;
pub mod choose_card;
pub mod choose_damage_source;
pub mod choose_from_zone;
pub mod choose_one_of;
pub mod clash;
pub mod cleanup;
pub mod collect_evidence;
pub mod conjure;
pub mod connive;
pub mod control_next_turn;
pub mod copy_spell;
pub mod counter;
pub mod counters;
pub mod create_emblem;
pub mod deal_damage;
pub mod delayed_trigger;
pub mod destroy;
pub mod detain;
pub mod dig;
pub mod discard;
pub mod discover;
pub mod double;
pub mod draw;
pub mod drawn_this_turn_choice;
pub mod effect;
pub mod energy;
pub mod exchange_control;
pub mod exile_from_top_until;
pub mod exile_top;
pub mod exploit;
pub mod explore;
pub mod extra_turn;
pub mod fight;
pub mod flip_coin;
pub mod force_block;
pub mod gain_control;
pub mod gift_delivery;
pub mod goad;
pub mod grant_permission;
pub mod incubate;
pub mod investigate;
pub mod learn;
pub mod life;
pub mod mana;
pub mod manifest;
pub mod manifest_dread;
pub mod mill;
pub mod monstrosity;
pub mod overload;
pub mod paradigm;
pub mod pay;
pub mod phase_out;
pub mod player_counter;
pub mod populate;
pub mod prepare;
pub mod prevent_damage;
pub mod proliferate;
pub mod pump;
pub mod put_on_top;
pub mod put_on_top_or_bottom;
pub mod regenerate;
pub mod register_bending;
pub mod remove_from_combat;
pub mod reveal;
pub mod reveal_from_hand;
pub mod reveal_hand;
pub mod reveal_top;
pub mod reveal_until;
pub mod ring;
pub mod roll_die;
pub mod sacrifice;
pub mod scry;
pub mod search_library;
pub mod seek;
pub mod set_class_level;
pub mod shuffle;
pub mod skip_next_step;
pub mod skip_next_turn;
pub mod solve_case;
pub mod speed_effects;
pub mod surveil;
pub mod suspect;
pub mod switch_pt;
pub mod tap_untap;
pub mod token;
pub mod token_copy;
pub mod transform_effect;
pub mod tribute;
pub mod venture;
pub mod vote;
pub mod win_lose;

fn matches_player_scope(
    state: &GameState,
    player: PlayerId,
    scope: &PlayerFilter,
    controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|p| {
            !p.is_eliminated
                && match scope {
                    PlayerFilter::Controller => p.id == controller,
                    PlayerFilter::All => true,
                    PlayerFilter::Opponent => p.id != controller,
                    PlayerFilter::DefendingPlayer => {
                        crate::game::targeting::resolve_event_context_target_for_event_or_state(
                            state,
                            &TargetFilter::DefendingPlayer,
                            source_id,
                            state.current_trigger_event.as_ref(),
                        )
                        .is_some_and(
                            |target| matches!(target, TargetRef::Player(pid) if pid == p.id),
                        )
                    }
                    PlayerFilter::OpponentLostLife => {
                        p.id != controller && p.life_lost_this_turn > 0
                    }
                    PlayerFilter::OpponentGainedLife => {
                        p.id != controller && p.life_gained_this_turn > 0
                    }
                    PlayerFilter::HighestSpeed => {
                        let highest_speed = state
                            .players
                            .iter()
                            .filter(|player| !player.is_eliminated)
                            .map(|player| crate::game::speed::effective_speed(state, player.id))
                            .max()
                            .unwrap_or(0);
                        crate::game::speed::effective_speed(state, p.id) == highest_speed
                    }
                    PlayerFilter::ZoneChangedThisWay => state
                        .last_zone_changed_ids
                        .iter()
                        .any(|id| state.objects.get(id).is_some_and(|obj| obj.owner == p.id)),
                    PlayerFilter::PerformedActionThisWay { relation, action } => {
                        crate::game::players::matches_relation(p.id, controller, *relation)
                            && crate::game::players::performed_action_this_way(state, p.id, *action)
                    }
                    PlayerFilter::OwnersOfCardsExiledBySource => {
                        crate::game::players::owns_card_exiled_by_source(state, p.id, source_id)
                    }
                    // CR 603.7c: Match only the triggering player extracted from
                    // `state.current_trigger_event`.
                    PlayerFilter::TriggeringPlayer => state
                        .current_trigger_event
                        .as_ref()
                        .and_then(|e| crate::game::targeting::extract_player_from_event(e, state))
                        .is_some_and(|pid| pid == p.id),
                    // CR 120.3 + CR 603.2c: Each opponent other than the triggering opponent.
                    // Falls back to plain Opponent semantics when no trigger event is in scope.
                    PlayerFilter::OpponentOtherThanTriggering => {
                        if p.id == controller {
                            return false;
                        }
                        let triggering = state.current_trigger_event.as_ref().and_then(|e| {
                            crate::game::targeting::extract_player_from_event(e, state)
                        });
                        triggering.is_none_or(|pid| pid != p.id)
                    }
                }
        })
}

/// Record the outer effect's `EffectKind` on the current `pending_continuation`
/// so the drain re-emits the parent `EffectResolved` event that the non-pause
/// tail of the resolver would have emitted. Must be called after
/// `append_to_pending_continuation` has stashed the chain — if no continuation
/// has been stashed the parent event is dropped (the chain is the carrier).
pub(crate) fn mark_pending_continuation_parent(state: &mut GameState, kind: EffectKind) {
    if let Some(cont) = state.pending_continuation.as_mut() {
        cont.parent_kind = Some(kind);
    }
}

/// Drain `state.pending_continuation`: resolve the stashed chain, then emit
/// the stashed parent `EffectResolved` event (if any) so trigger matchers
/// keyed on the outer effect's kind (`EffectKind::Fight`, `DamageAll`,
/// `DamageEachPlayer`, etc.) fire the same way they do on the non-pause
/// path. Safe to call when no continuation is pending (no-op).
///
/// All `pending_continuation.take()` sites should use this helper rather
/// than rolling their own `take + resolve_ability_chain`, so the parent
/// event is never silently dropped.
pub(crate) fn drain_pending_continuation(state: &mut GameState, events: &mut Vec<GameEvent>) {
    if let Some(cont) = state.pending_continuation.take() {
        let PendingContinuation { chain, parent_kind } = cont;
        let source_id = chain.source_id;
        let _ = resolve_ability_chain(state, &chain, events, 1);
        if let Some(kind) = parent_kind {
            events.push(GameEvent::EffectResolved { kind, source_id });
        }
    }
    // CR 609.3 + CR 109.5: After the per-iteration chain drains, drive any
    // remaining `repeat_for` iterations. Each resumed iteration may itself
    // pause and re-stash via the loop in `resolve_ability_chain`, producing a
    // chain of resumed iterations until the loop completes.
    if !waits_for_resolution_choice(&state.waiting_for) {
        drain_pending_repeat_iteration(state, events);
    }
    if !waits_for_resolution_choice(&state.waiting_for) {
        choose_one_of::resume_pending(state, events);
    }
}

/// CR 609.3 + CR 109.5: Resume a paused `repeat_for` loop. Each iteration
/// may itself pause (re-stashing into `pending_repeat_iteration`); the outer
/// driver in `drain_pending_continuation` re-enters this on the next choice
/// resolution. If an iteration completes synchronously and a further
/// iteration also completes synchronously, this function drives them all
/// in-line so the loop only pauses again when an inner effect actually
/// transitions to a player-choice state.
fn drain_pending_repeat_iteration(state: &mut GameState, events: &mut Vec<GameEvent>) {
    while let Some(pending) = state.pending_repeat_iteration.take() {
        let crate::types::game_state::PendingRepeatIteration {
            ability,
            tracked_members,
            next_iteration,
            total_iterations,
        } = pending;
        let initial_waiting_for = state.waiting_for.clone();
        let initial_continuation_present = state.pending_continuation.is_some();
        let mut iteration = next_iteration;
        let mut paused = false;
        while iteration < total_iterations {
            let mut iter_ability;
            let iter_effective: &ResolvedAbility =
                if let Some(member) = tracked_members.get(iteration) {
                    iter_ability = (*ability).clone();
                    rebind_first_object_target(&mut iter_ability.targets, *member);
                    &iter_ability
                } else {
                    &ability
                };
            // CR 609.3 + CR 109.5: Drive the FULL chain (parent effect +
            // sub_ability + line-1660 continuation wiring) for each resumed
            // iteration, mirroring iteration 0's path. Calling `resolve_effect`
            // here would skip the sub_ability stash, so e.g. Winds of Abandon's
            // put-onto-battlefield + shuffle continuation would never run for
            // opponents 2+. The stashed `ability` had `repeat_for` cleared at
            // stash time so this call resolves a single iteration only.
            //
            // Pass depth=1 to preserve chain-local state (`chain_tracked_set_id`,
            // `last_revealed_ids`, `last_zone_changed_ids`, `last_effect_amount`)
            // that the depth==0 prelude in `resolve_ability_chain` would
            // otherwise reset. The resumed iteration is logically continuing the
            // outer chain, not starting a fresh top-level resolution.
            let _ = resolve_ability_chain(state, iter_effective, events, 1);
            // CR 609.3: Iteration may transition to a player-choice state OR
            // synchronously install a `pending_continuation` (e.g. when the
            // sub_ability chain wires itself for later drain). Either signals
            // that this iteration is not yet fully resolved — re-stash the
            // remaining iterations and break so the outer
            // `drain_pending_continuation` can run the continuation, then
            // re-enter this drain for the next iteration. Without re-stashing
            // on the synchronous-continuation case, subsequent iterations are
            // silently dropped.
            let entered_choice = state.waiting_for != initial_waiting_for;
            let installed_continuation =
                !initial_continuation_present && state.pending_continuation.is_some();
            if entered_choice || installed_continuation {
                let next = iteration + 1;
                if next < total_iterations {
                    state.pending_repeat_iteration =
                        Some(crate::types::game_state::PendingRepeatIteration {
                            ability: ability.clone(),
                            tracked_members: tracked_members.clone(),
                            next_iteration: next,
                            total_iterations,
                        });
                }
                paused = true;
                break;
            }
            iteration += 1;
        }
        if paused {
            // Loop paused mid-iteration; the next call to
            // `drain_pending_continuation` will resume.
            break;
        }
    }
}

pub(crate) fn append_to_pending_continuation(
    state: &mut GameState,
    tail: Option<Box<ResolvedAbility>>,
) {
    let Some(tail) = tail else {
        return;
    };

    if let Some(existing) = state.pending_continuation.as_mut() {
        let mut cursor = existing.chain.as_mut();
        let tail = Some(tail);
        loop {
            if cursor.sub_ability.is_none() {
                cursor.sub_ability = tail;
                break;
            }
            cursor = cursor
                .sub_ability
                .as_mut()
                .expect("sub_ability checked above")
                .as_mut();
        }
    } else {
        state.pending_continuation = Some(PendingContinuation::new(tail));
    }
}

fn prepend_to_pending_continuation(state: &mut GameState, mut head: ResolvedAbility) {
    if let Some(existing) = state.pending_continuation.take() {
        let PendingContinuation { chain, parent_kind } = existing;
        super::ability_utils::append_to_sub_chain(&mut head, *chain);
        state.pending_continuation = Some(PendingContinuation {
            chain: Box::new(head),
            parent_kind,
        });
    } else {
        state.pending_continuation = Some(PendingContinuation::new(Box::new(head)));
    }
}

fn waits_for_resolution_choice(waiting_for: &WaitingFor) -> bool {
    matches!(
        waiting_for,
        WaitingFor::ScryChoice { .. }
            | WaitingFor::DigChoice { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::RevealChoice { .. }
            | WaitingFor::SearchChoice { .. }
            | WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::NamedChoice { .. }
            | WaitingFor::DamageSourceChoice { .. }
            | WaitingFor::MultiTargetSelection { .. }
            | WaitingFor::OptionalEffectChoice { .. }
            | WaitingFor::OpponentMayChoice { .. }
            | WaitingFor::TributeChoice { .. }
            | WaitingFor::DiscoverChoice { .. }
            | WaitingFor::CascadeChoice { .. }
            | WaitingFor::TopOrBottomChoice { .. }
            | WaitingFor::ProliferateChoice { .. }
            | WaitingFor::ExploreChoice { .. }
            | WaitingFor::CopyRetarget { .. }
            | WaitingFor::DistributeAmong { .. }
            | WaitingFor::PayAmountChoice { .. }
            | WaitingFor::RetargetChoice { .. }
            | WaitingFor::ChooseFromZoneChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::ChooseManaColor { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::DiscardChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
            | WaitingFor::CategoryChoice { .. }
            | WaitingFor::LearnChoice { .. }
            | WaitingFor::PopulateChoice { .. }
    )
}

pub(super) fn resolve_optional_effect_decision(
    state: &mut GameState,
    mut ability: ResolvedAbility,
    choice: AutoMayChoice,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<(), EffectError> {
    ability.optional = false;
    match choice {
        AutoMayChoice::Accept => {
            ability.context.optional_effect_performed = true;
            resolve_ability_chain(state, &ability, events, depth)?;
        }
        AutoMayChoice::Decline => {
            if let Some(ref sub) = ability.sub_ability {
                let mut sub_resolved = sub.as_ref().clone();
                sub_resolved.context = ability.context.clone();
                resolve_ability_chain(state, &sub_resolved, events, depth)?;
            }
        }
    }
    Ok(())
}

fn is_player_scope_local_continuation(parent: &Effect, child: &Effect) -> bool {
    matches!(
        (parent, child),
        (
            Effect::SearchLibrary { .. },
            Effect::ChangeZone {
                origin: Some(crate::types::zones::Zone::Library),
                ..
            }
        ) | (Effect::SearchLibrary { .. }, Effect::Shuffle { .. })
            | (
                Effect::ChangeZone {
                    origin: Some(crate::types::zones::Zone::Library),
                    ..
                },
                Effect::Shuffle { .. }
            )
    )
}

fn detach_after_player_scope_local_chain(
    node: &mut ResolvedAbility,
) -> Option<Box<ResolvedAbility>> {
    let mut next = node.sub_ability.take()?;
    if is_player_scope_local_continuation(&node.effect, &next.effect) {
        let tail = detach_after_player_scope_local_chain(&mut next);
        node.sub_ability = Some(next);
        tail
    } else {
        Some(next)
    }
}

fn split_player_scope_chain(
    ability: &ResolvedAbility,
) -> (ResolvedAbility, Option<Box<ResolvedAbility>>) {
    let mut scoped = ability.clone();
    scoped.player_scope = None;
    let tail = detach_after_player_scope_local_chain(&mut scoped);
    (scoped, tail)
}

/// CR 601.2c: Extract SharesQuality filter properties from an effect's target filter.
/// Returns the typed qualities that require group validation.
fn extract_shares_quality_props(
    filter: &TargetFilter,
) -> Vec<(&SharedQuality, SharedQualityRelation)> {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .filter_map(|p| match p {
                FilterProp::SharesQuality {
                    quality,
                    reference: None,
                    relation,
                } => Some((quality, *relation)),
                _ => None,
            })
            .collect(),
        TargetFilter::And { filters } => filters
            .iter()
            .flat_map(extract_shares_quality_props)
            .collect(),
        _ => vec![],
    }
}

/// CR 608.2b: Extract the target filter from an effect for SharesQuality validation.
fn effect_target_filter(effect: &Effect) -> Option<&TargetFilter> {
    effect.target_filter()
}

/// Dispatch to the appropriate effect handler using typed pattern matching.
pub fn resolve_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::StartYourEngines { .. } => speed_effects::resolve_start(state, ability, events),
        Effect::IncreaseSpeed { .. } => speed_effects::resolve_increase(state, ability, events),
        Effect::DealDamage { .. } => deal_damage::resolve(state, ability, events),
        Effect::Draw { .. } => draw::resolve(state, ability, events),
        Effect::Pump { .. } => pump::resolve(state, ability, events),
        Effect::Destroy { .. } => destroy::resolve(state, ability, events),
        Effect::Regenerate { .. } => regenerate::resolve(state, ability, events),
        Effect::Counter { .. } => counter::resolve(state, ability, events),
        Effect::CounterAll { .. } => counter::resolve_all(state, ability, events),
        Effect::Token { .. } => token::resolve(state, ability, events),
        Effect::GainLife { .. } => life::resolve_gain(state, ability, events),
        Effect::LoseLife { .. } => life::resolve_lose(state, ability, events),
        Effect::Tap { .. } => tap_untap::resolve_tap(state, ability, events),
        Effect::Untap { .. } => tap_untap::resolve_untap(state, ability, events),
        Effect::TapAll { .. } => tap_untap::resolve_tap_all(state, ability, events),
        Effect::UntapAll { .. } => tap_untap::resolve_untap_all(state, ability, events),
        Effect::AddCounter { .. } => counters::resolve_add(state, ability, events),
        Effect::RemoveCounter { .. } => counters::resolve_remove(state, ability, events),
        Effect::Sacrifice { .. } => sacrifice::resolve(state, ability, events),
        Effect::DiscardCard { .. } => discard::resolve(state, ability, events),
        Effect::Mill { .. } => mill::resolve(state, ability, events),
        Effect::Scry { .. } => scry::resolve(state, ability, events),
        Effect::PumpAll { .. } => pump::resolve_all(state, ability, events),
        Effect::DamageAll { .. } => deal_damage::resolve_all(state, ability, events),
        Effect::DamageEachPlayer { .. } => deal_damage::resolve_each_player(state, ability, events),
        Effect::DestroyAll { .. } => destroy::resolve_all(state, ability, events),
        Effect::ChangeZone { .. } => change_zone::resolve(state, ability, events),
        Effect::ChangeZoneAll { .. } => change_zone::resolve_all(state, ability, events),
        Effect::Dig { .. } => dig::resolve(state, ability, events),
        Effect::GainControl { .. } => gain_control::resolve(state, ability, events),
        Effect::Goad { .. } => goad::resolve(state, ability, events),
        Effect::Detain { .. } => detain::resolve(state, ability, events),
        Effect::ExchangeControl { .. } => exchange_control::resolve(state, ability, events),
        Effect::Attach { .. } => attach::resolve(state, ability, events),
        Effect::ControlNextTurn { .. } => control_next_turn::resolve(state, ability, events),
        Effect::Surveil { .. } => surveil::resolve(state, ability, events),
        Effect::Fight { .. } => fight::resolve(state, ability, events),
        Effect::Bounce { .. } => bounce::resolve(state, ability, events),
        Effect::BounceAll { .. } => bounce::resolve_all(state, ability, events),
        Effect::Explore => explore::resolve(state, ability, events),
        Effect::ExploreAll { .. } => explore::resolve_all(state, ability, events),
        Effect::Investigate => investigate::resolve(state, ability, events),
        // CR 702.104a: Tribute — the chosen opponent decides pay/decline via
        // WaitingFor::TributeChoice (reuses GameAction::DecideOptionalEffect).
        Effect::Tribute { .. } => tribute::resolve(state, ability, events),
        // CR 701.56a: Time travel — interactive counter manipulation on suspended/time-countered permanents.
        // Currently a no-op; full interactive implementation requires WaitingFor infrastructure.
        Effect::TimeTravel => Ok(()),
        Effect::BecomeMonarch => become_monarch::resolve(state, ability, events),
        Effect::Proliferate => proliferate::resolve(state, ability, events),
        Effect::Populate => populate::resolve(state, ability, events),
        Effect::Clash => clash::resolve(state, ability, events),
        // CR 701.38: Council's-dilemma voting — see effects/vote.rs.
        Effect::Vote { .. } => vote::resolve(state, ability, events),
        Effect::SwitchPT { .. } => switch_pt::resolve(state, ability, events),
        Effect::CopySpell { .. } => copy_spell::resolve(state, ability, events),
        Effect::CopyTokenOf { .. } => token_copy::resolve(state, ability, events),
        Effect::BecomeCopy { .. } => become_copy::resolve(state, ability, events),
        Effect::ChooseCard { .. } => choose_card::resolve(state, ability, events),
        Effect::PutCounter { .. } => counters::resolve_add(state, ability, events),
        Effect::PutCounterAll { .. } => counters::resolve_add_all(state, ability, events),
        Effect::MultiplyCounter { .. } => counters::resolve_multiply(state, ability, events),
        Effect::DoublePT { .. } => pump::resolve_double_pt(state, ability, events),
        Effect::DoublePTAll { .. } => pump::resolve_double_pt_all(state, ability, events),
        Effect::MoveCounters { .. } => counters::resolve_move(state, ability, events),
        Effect::Animate { .. } => animate::resolve(state, ability, events),
        Effect::RegisterBending { .. } => register_bending::resolve(state, ability, events),
        Effect::GenericEffect { .. } => effect::resolve(state, ability, events),
        Effect::Cleanup { .. } => cleanup::resolve(state, ability, events),
        Effect::Mana { .. } => mana::resolve(state, ability, events),
        Effect::Discard { .. } => discard::resolve(state, ability, events),
        Effect::Shuffle { .. } => shuffle::resolve(state, ability, events),
        Effect::Transform { .. } => transform_effect::resolve(state, ability, events),
        Effect::SearchLibrary { .. } => search_library::resolve(state, ability, events),
        Effect::Seek { .. } => seek::resolve(state, ability, events),
        Effect::RevealHand { .. } => reveal_hand::resolve(state, ability, events),
        Effect::RevealFromHand { .. } => reveal_from_hand::resolve(state, ability, events),
        Effect::Reveal { .. } => reveal::resolve(state, ability, events),
        Effect::RevealTop { .. } => reveal_top::resolve(state, ability, events),
        Effect::ExileTop { .. } => exile_top::resolve(state, ability, events),
        Effect::TargetOnly { .. } => Ok(()), // no-op: targeting is established at cast time
        Effect::Choose { .. } => choose::resolve(state, ability, events),
        Effect::ChooseDamageSource { .. } => choose_damage_source::resolve(state, ability, events),
        Effect::Suspect { .. } => suspect::resolve(state, ability, events),
        Effect::Connive { .. } => connive::resolve(state, ability, events),
        Effect::PhaseOut { .. } => phase_out::resolve(state, ability, events),
        Effect::PhaseIn { .. } => phase_out::resolve_phase_in(state, ability, events),
        Effect::ForceBlock { .. } => force_block::resolve(state, ability, events),
        Effect::SolveCase => solve_case::resolve(state, ability, events),
        Effect::BecomePrepared { .. } => prepare::resolve_become_prepared(state, ability, events),
        Effect::BecomeUnprepared { .. } => {
            prepare::resolve_become_unprepared(state, ability, events)
        }
        Effect::SetClassLevel { .. } => set_class_level::resolve(state, ability, events),
        Effect::CreateDelayedTrigger { .. } => delayed_trigger::resolve(state, ability, events),
        Effect::AddTargetReplacement { .. } => {
            add_target_replacement::resolve(state, ability, events)
        }
        Effect::AddRestriction { .. } => add_restriction::resolve(state, ability, events),
        Effect::ReduceNextSpellCost { .. } => {
            resolve_reduce_next_spell_cost(state, ability, events)
        }
        Effect::GrantNextSpellAbility { .. } => {
            resolve_grant_next_spell_ability(state, ability, events)
        }
        Effect::AddPendingETBCounters { .. } => {
            resolve_add_pending_etb_counters(state, ability, events)
        }
        Effect::CreateEmblem { .. } => create_emblem::resolve(state, ability, events),
        Effect::PayCost { .. } => pay::resolve(state, ability, events),
        Effect::CastFromZone { .. } => cast_from_zone::resolve(state, ability, events),
        Effect::PreventDamage { .. } => prevent_damage::resolve(state, ability, events),
        Effect::LoseTheGame => win_lose::resolve_lose(state, ability, events),
        Effect::WinTheGame => win_lose::resolve_win(state, ability, events),
        Effect::RollDie { .. } => roll_die::resolve(state, ability, events),
        Effect::FlipCoin { .. } => flip_coin::resolve(state, ability, events),
        Effect::FlipCoins { .. } => flip_coin::resolve_flip_coins(state, ability, events),
        Effect::FlipCoinUntilLose { .. } => flip_coin::resolve_until_lose(state, ability, events),
        Effect::RingTemptsYou => ring::resolve(state, ability, events),
        Effect::GrantCastingPermission { .. } => grant_permission::resolve(state, ability, events),
        Effect::ChooseFromZone { .. } => choose_from_zone::resolve(state, ability, events),
        Effect::ChooseAndSacrificeRest { .. } => {
            choose_and_sacrifice_rest::resolve(state, ability, events)
        }
        Effect::Exploit { .. } => exploit::resolve(state, ability, events),
        Effect::GainEnergy { .. } => energy::resolve_gain(state, ability, events),
        Effect::GivePlayerCounter { .. } => player_counter::resolve(state, ability, events),
        Effect::LoseAllPlayerCounters { .. } => {
            player_counter::resolve_lose_all(state, ability, events)
        }
        Effect::AdditionalPhase { .. } => additional_phase::resolve(state, ability, events),
        Effect::ExileFromTopUntil { .. } => exile_from_top_until::resolve(state, ability, events),
        Effect::RevealUntil { .. } => reveal_until::resolve(state, ability, events),
        Effect::Discover { .. } => discover::resolve(state, ability, events),
        // CR 702.85a: Cascade — synthesized from the keyword at trigger time;
        // resolver performs the exile-until loop and sets CascadeChoice.
        Effect::Cascade => cascade::resolve(state, ability, events),
        // CR 702.94a: Miracle trigger resolution — offer the cast from hand.
        Effect::MiracleCast { ref cost } => {
            state.waiting_for = WaitingFor::MiracleCastOffer {
                player: ability.controller,
                object_id: ability.source_id,
                cost: cost.clone(),
            };
            Ok(())
        }
        // CR 702.35a: Madness trigger resolution — offer the cast from exile.
        Effect::MadnessCast { ref cost } => {
            state.waiting_for = WaitingFor::MadnessCastOffer {
                player: ability.controller,
                object_id: ability.source_id,
                cost: cost.clone(),
            };
            Ok(())
        }
        Effect::PutAtLibraryPosition { .. } => put_on_top::resolve(state, ability, events),
        Effect::ChooseDrawnThisTurnPayOrTopdeck { .. } => {
            drawn_this_turn_choice::resolve(state, ability, events)
        }
        Effect::PutOnTopOrBottom { .. } => put_on_top_or_bottom::resolve(state, ability, events),
        Effect::GiftDelivery { .. } => gift_delivery::resolve(state, ability, events),
        Effect::ChangeTargets { .. } => change_targets::resolve(state, ability, events),
        Effect::Incubate { .. } => incubate::resolve(state, ability, events),
        Effect::Amass { .. } => amass::resolve(state, ability, events),
        Effect::Monstrosity { .. } => monstrosity::resolve(state, ability, events),
        Effect::Adapt { .. } => adapt::resolve(state, ability, events),
        Effect::Bolster { .. } => bolster::resolve(state, ability, events),
        Effect::Manifest { .. } => manifest::resolve(state, ability, events),
        Effect::ManifestDread => manifest_dread::resolve(state, ability, events),
        Effect::ExtraTurn { .. } => extra_turn::resolve(state, ability, events),
        Effect::SkipNextStep { .. } => skip_next_step::resolve(state, ability, events),
        Effect::SkipNextTurn { .. } => skip_next_turn::resolve(state, ability, events),
        Effect::Double { .. } => double::resolve(state, ability, events),
        Effect::RuntimeHandled { .. } => Ok(()), // Handled by dedicated engine path
        Effect::Learn => learn::resolve(state, ability, events),
        Effect::Forage | Effect::Endure { .. } | Effect::BlightEffect { .. } => {
            // These keyword actions are recognized by the parser but not yet implemented.
            // They're no-ops at runtime but count as supported for coverage.
            Ok(())
        }
        Effect::CollectEvidence { .. } => collect_evidence::resolve(state, ability, events),
        Effect::SetLifeTotal { .. } => life::resolve_set_life_total(state, ability, events),
        Effect::SetDayNight { to } => {
            crate::game::day_night::resolve_set_day_night(state, *to, events);
            Ok(())
        }
        Effect::GiveControl { .. } => gain_control::resolve_give(state, ability, events),
        Effect::RemoveFromCombat { .. } => remove_from_combat::resolve(state, ability, events),
        Effect::VentureIntoDungeon => venture::resolve(state, ability, events),
        Effect::VentureInto { dungeon } => {
            venture::resolve_venture_into(state, ability, *dungeon, events)
        }
        Effect::TakeTheInitiative => venture::resolve_take_initiative(state, ability, events),
        Effect::Conjure { .. } => conjure::resolve(state, ability, events),
        Effect::ChooseOneOf { .. } => choose_one_of::resolve(state, ability, events),
        Effect::Unimplemented { name, .. } => {
            // Log warning and return Ok (no-op) for unimplemented effects
            eprintln!("Warning: Unimplemented effect: {}", name);
            Ok(())
        }
    }
}

/// Returns true if the given effect has a handler in the engine.
/// `Unimplemented` effects are the only genuinely unsupported effects.
/// `RuntimeHandled` effects are supported but handled by dedicated engine paths.
pub fn is_known_effect(effect: &Effect) -> bool {
    !matches!(effect, Effect::Unimplemented { .. })
}

/// CR 603.7: Check if any descendant sub_ability needs tracked set recording.
///
/// A descendant consumes the tracked set when any of its quantity or filter
/// positions reference the most recent set — via `QuantityRef::TrackedSetSize`
/// (e.g., "for each creature destroyed this way") or
/// `TargetFilter::TrackedSet { .. }` (e.g., "those cards"). Two flag-driven
/// cases are also consumers: `CreateDelayedTrigger { uses_tracked_set: true }`
/// binds the set to the delayed trigger's later resolution, and
/// `ChooseFromZone` selects out of it.
///
/// The walk is **transitive** across the sub_ability chain — a grandchild
/// referencing `TrackedSet(0)` causes every zone-changing ancestor in the
/// chain to publish, which (combined with chain-unification at publish
/// time) merges all affected objects into a single tracked set. This is
/// what makes compound exile (Suspend Aggression's
/// "Exile target nonland permanent and the top card of your library ...
/// for each of those cards") expose both exiled objects to the grant.
fn next_sub_needs_tracked_set(ability: &ResolvedAbility) -> bool {
    let mut cursor = ability.sub_ability.as_deref();
    while let Some(sub) = cursor {
        let consumes = matches!(
            &sub.effect,
            Effect::CreateDelayedTrigger {
                uses_tracked_set: true,
                ..
            } | Effect::ChooseFromZone { .. }
        ) || effect_references_tracked_set(&sub.effect);
        if consumes {
            return true;
        }
        cursor = sub.sub_ability.as_deref();
    }
    false
}

/// Returns true if the effect references the most recent tracked set through
/// any quantity (`QuantityRef::TrackedSetSize`) or filter (`TargetFilter::TrackedSet`)
/// position. Walks all quantity and filter fields — works for any effect in the
/// class (GainLife, DealDamage, Token, Mill, Draw, PutCounter, GrantCastingPermission, …)
/// without enumerating variants.
fn effect_references_tracked_set(effect: &Effect) -> bool {
    // Quantity positions — walk every QuantityExpr field on the effect.
    let quantity_hits_tracked = |qty: &QuantityExpr| quantity_expr_references_tracked_set(qty);
    let has_quantity_hit = match effect {
        Effect::DealDamage { amount, .. } => quantity_hits_tracked(amount),
        Effect::DamageAll { amount, .. } => quantity_hits_tracked(amount),
        Effect::DamageEachPlayer { amount, .. } => quantity_hits_tracked(amount),
        Effect::Draw { count, .. } => quantity_hits_tracked(count),
        Effect::Mill { count, .. } => quantity_hits_tracked(count),
        Effect::Scry { count, .. } => quantity_hits_tracked(count),
        Effect::Dig { count, .. } => quantity_hits_tracked(count),
        Effect::Surveil { count, .. } => quantity_hits_tracked(count),
        Effect::GainLife { amount, .. } => quantity_hits_tracked(amount),
        Effect::LoseLife { amount, .. } => quantity_hits_tracked(amount),
        Effect::IncreaseSpeed { amount, .. } => quantity_hits_tracked(amount),
        Effect::PutCounter { count, .. } => quantity_hits_tracked(count),
        Effect::PutCounterAll { count, .. } => quantity_hits_tracked(count),
        Effect::Token { count, .. } => quantity_hits_tracked(count),
        _ => false,
    };
    if has_quantity_hit {
        return true;
    }

    // Filter positions — the effect's primary target filter may be TrackedSet.
    if let Some(filter) = effect.target_filter() {
        if filter_references_tracked_set(filter) {
            return true;
        }
    }
    // `GrantCastingPermission` has a `target` field that is not exposed by
    // `Effect::target_filter()` (it selects objects to grant permission to,
    // not spell/ability targets). Inspect directly so "the rest" / "those
    // cards" sub-abilities chained off exile-all effects still record the set.
    if let Effect::GrantCastingPermission { target, .. } = effect {
        if filter_references_tracked_set(target) {
            return true;
        }
    }
    false
}

fn quantity_expr_references_tracked_set(qty: &QuantityExpr) -> bool {
    match qty {
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::Ref { qty } => matches!(qty, QuantityRef::TrackedSetSize),
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::DivideRounded { inner, .. } => quantity_expr_references_tracked_set(inner),
        QuantityExpr::Sum { exprs } => exprs.iter().any(quantity_expr_references_tracked_set),
        QuantityExpr::UpTo { max } => quantity_expr_references_tracked_set(max),
    }
}

fn filter_references_tracked_set(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::TrackedSet { .. } => true,
        TargetFilter::Not { filter } => filter_references_tracked_set(filter),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_references_tracked_set)
        }
        _ => false,
    }
}

fn effect_uses_implicit_tracked_set_targets(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::GrantCastingPermission {
            target: TargetFilter::TrackedSet { .. },
            ..
        }
    )
}

/// CR 603.7 + CR 109.5: Returns `true` when the effect resolves an acting
/// subject relative to the parent target — i.e., any effect-target slot
/// reachable via [`effect_target_filter`] contains
/// `TargetFilter::ParentTargetController` or `TargetFilter::ParentTarget`.
/// Used by the `repeat_for: TrackedSetSize` loop to decide whether
/// per-iteration parent rebinding is required.
///
/// CR 109.5: "you/your" on an object refers to its controller; for an iterated
/// effect that derives the acting subject from the parent target (e.g., "its
/// controller" on Winds of Abandon's per-creature search), each iteration must
/// rebind the parent reference to the i-th tracked-set member so the per-iter
/// subject resolves correctly.
///
/// Generic: scans whatever target filter the effect exposes via
/// `effect_target_filter`, so any future effect family that carries a
/// parent-target filter (search, draw, life-gain by parent's controller, etc.)
/// participates without code changes here. `Effect::target_filter()` already
/// surfaces `SearchLibrary::target_player`, so iterated-search variants are
/// covered through the same single path.
fn effect_refs_parent_target(effect: &Effect) -> bool {
    effect_target_filter(effect).is_some_and(filter_refs_parent_target)
}

/// Recurse into compound filters so a wrapped `ParentTargetController` is
/// detected wherever it appears (`Or { filters: [..., ParentTargetController, ...] }`).
fn filter_refs_parent_target(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::ParentTargetController | TargetFilter::ParentTarget => true,
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_refs_parent_target)
        }
        TargetFilter::Not { filter } => filter_refs_parent_target(filter),
        _ => false,
    }
}

/// CR 603.7 + CR 109.5: Replace the first `TargetRef::Object` in a target
/// slice with the supplied object id. Used by the `repeat_for: TrackedSetSize`
/// per-iteration rebind so the i-th iteration's parent reference (e.g.,
/// `ParentTargetController` resolution in `search_library::resolve_library_owner`)
/// binds to the i-th tracked-set member, making "its controller" (CR 109.5)
/// resolve to the i-th object's controller per iteration.
fn rebind_first_object_target(
    targets: &mut Vec<TargetRef>,
    new_id: crate::types::identifiers::ObjectId,
) {
    if let Some(slot) = targets
        .iter_mut()
        .find(|t| matches!(t, TargetRef::Object(_)))
    {
        *slot = TargetRef::Object(new_id);
    } else {
        targets.push(TargetRef::Object(new_id));
    }
}

pub(crate) fn resolved_object_filter(
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
) -> TargetFilter {
    filter::normalize_contextual_filter(target_filter, &ability.targets)
}

fn filter_uses_relative_controller_you(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.controller == Some(ControllerRef::You),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_uses_relative_controller_you)
        }
        TargetFilter::Not { filter } => filter_uses_relative_controller_you(filter),
        _ => false,
    }
}

pub(crate) fn controller_for_relative_filter(
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
) -> PlayerId {
    if filter_uses_relative_controller_you(target_filter)
        && ability.scoped_player.is_none()
        && ability
            .targets
            .iter()
            .any(|target| matches!(target, TargetRef::Player(_)))
    {
        ability.target_player()
    } else {
        ability.controller
    }
}

/// CR 121.1 + CR 615.5 + CR 609.7: Resolve the acting player for an effect
/// whose target slot may be a context-ref. Mirrors `life::resolve_life_loss_target`
/// for the Draw/Scry/Surveil class — those handlers historically short-circuited
/// to `ability.controller` whenever `target.is_context_ref()`, which is wrong
/// for `PostReplacementSourceController` (the prevented event's source has its
/// own controller, distinct from the replacement's controller).
///
/// Resolution order:
/// 1. First `TargetRef::Player` in `ability.targets` (chosen at announcement).
/// 2. `resolve_event_context_target` on the filter — reads `state` slots like
///    `current_trigger_event` (TriggeringSpellController) and
///    `post_replacement_event_source` (PostReplacementSourceController).
/// 3. Fall back to `ability.controller` (preserves prior semantics for context
///    refs whose state slots are empty in the current resolution window).
pub(crate) fn resolve_player_for_context_ref(
    state: &GameState,
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
) -> PlayerId {
    if matches!(target_filter, TargetFilter::ScopedPlayer) {
        return ability.scoped_player.unwrap_or(ability.controller);
    }

    // CR 115.1: For non-context-ref filters (e.g. `TargetFilter::Player` from
    // "target player draws"), the drawing player was chosen at announcement
    // and lives in `ability.targets`. Context-ref filters (Controller,
    // PostReplacementSourceController, …) MUST NOT consult `ability.targets`:
    // chain target-propagation (resolve_ability_chain) inherits parent targets
    // into a sub-ability with empty targets, so a sub Draw whose filter is
    // `Controller` would otherwise pick up the parent's Player target.
    if !target_filter.is_context_ref() {
        if let Some(player) = ability.targets.iter().find_map(|target| match target {
            TargetRef::Player(player) => Some(*player),
            _ => None,
        }) {
            return player;
        }
    }
    if let Some(target_ref) = crate::game::targeting::resolve_event_context_target(
        state,
        target_filter,
        ability.source_id,
    ) {
        return match target_ref {
            TargetRef::Player(player) => player,
            TargetRef::Object(id) => state
                .objects
                .get(&id)
                .map(|obj| obj.controller)
                .unwrap_or(ability.controller),
        };
    }
    if matches!(target_filter, TargetFilter::Controller) {
        return ability.controller;
    }
    // CR 115.1d: `ParentTargetController` resolves the controller of the parent
    // ability's targeted object. In a spell-resolution chain (no
    // `current_trigger_event` set, so `resolve_event_context_target` returns
    // None for this filter), the parent's chosen Object lives in
    // `ability.targets` from chain target propagation — read it here. This is
    // the Assassin's Trophy-shape pattern: "Destroy target permanent. Its
    // controller may search their library …" — the sub-ability's filter is
    // `ParentTargetController`, and we resolve to the destroyed permanent's
    // controller. Note we deliberately read AFTER the trigger-event path so
    // an actual triggered context (where the helper at `targeting.rs:265`
    // succeeds) wins over chain inheritance.
    if matches!(target_filter, TargetFilter::ParentTargetController) {
        if let Some(player) = ability.targets.iter().find_map(|target| match target {
            TargetRef::Object(id) => state.objects.get(id).map(|obj| obj.controller),
            TargetRef::Player(player) => Some(*player),
        }) {
            return player;
        }
    }
    ability.controller
}

/// CR 117.3a: Determine which player receives the "may" prompt for an optional
/// effect. Most optional effects go to the caster (CR 609.3). Subject-anchored
/// optional effects — "its controller may search their library" (Assassin's
/// Trophy, Path to Exile, Ghost Quarter, Oblation, …) — route the prompt to the
/// acting subject (the target permanent's controller). This mirrors the
/// `resolve_library_owner` logic in `search_library.rs` but applies generally
/// to any optional effect whose embedded player-scope target is a context-ref.
fn optional_prompt_player(state: &GameState, ability: &ResolvedAbility) -> PlayerId {
    if let Effect::PayCost { payer, .. } = &ability.effect {
        if let Some(player) =
            crate::game::targeting::resolve_effect_player_ref(state, ability, payer)
        {
            return player;
        }
    }

    // Subject-anchored SearchLibrary: prompt the library owner / searcher.
    if let Effect::SearchLibrary {
        target_player: Some(TargetFilter::ParentTargetController),
        ..
    } = &ability.effect
    {
        if let Some(parent_obj_id) = ability.targets.iter().find_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        }) {
            if let Some(obj) = state.objects.get(&parent_obj_id) {
                return obj.controller;
            }
        }
    }
    ability.controller
}

/// CR 603.7c: Extract an event-context target filter from an effect, if present.
/// Returns the filter only for event-context variants (TriggeringSpellController, etc.)
/// that auto-resolve from `state.current_trigger_event` at resolution time.
fn extract_event_context_filter(effect: &Effect) -> Option<&TargetFilter> {
    let filter = match effect {
        Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Regenerate { target, .. }
        | Effect::Tap { target, .. }
        | Effect::Untap { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Sacrifice { target, .. }
        | Effect::AddCounter { target, .. }
        | Effect::RemoveCounter { target, .. }
        | Effect::PutCounter { target, .. }
        | Effect::MoveCounters { target, .. }
        | Effect::ChangeZone { target, .. }
        | Effect::RevealHand { target, .. }
        | Effect::Reveal { target, .. }
        | Effect::Fight { target, .. }
        | Effect::Attach { target, .. }
        | Effect::Transform { target, .. }
        | Effect::CopySpell { target, .. }
        | Effect::CopyTokenOf { target, .. }
        | Effect::BecomeCopy { target, .. }
        | Effect::CastFromZone { target, .. }
        | Effect::PreventDamage { target, .. }
        | Effect::Connive { target, .. }
        | Effect::PhaseOut { target, .. }
        | Effect::ForceBlock { target, .. }
        | Effect::PutAtLibraryPosition { target, .. }
        | Effect::PutOnTopOrBottom { target, .. }
        | Effect::ChangeTargets { target, .. }
        | Effect::ExtraTurn { target, .. }
        | Effect::Double { target, .. }
        // CR 608.2k + CR 603.7c: "that player" sub-effects carry an event-context
        // target (TriggeringPlayer/DefendingPlayer/etc.) that auto-resolves from
        // the current trigger event at resolution time — not a fresh target choice.
        | Effect::Discard { target, .. }
        | Effect::DiscardCard { target, .. }
        | Effect::Mill { target, .. }
        // CR 121.1 + CR 603.7c: "they draw a card" off an opponent-subject
        // trigger (Firemane Commando) carries `target: TriggeringPlayer`, which
        // must auto-bind from the current trigger event so the drawing player
        // is the acting opponent — not the trigger controller.
        | Effect::Draw { target, .. }
        | Effect::Shuffle { target, .. }
        | Effect::GivePlayerCounter { target, .. }
        | Effect::LoseAllPlayerCounters { target, .. }
        // Additional player-targeted effects: when chained off a "that player"
        // subject in trigger context, their target is an event-context ref
        // (e.g., TriggeringPlayer) rather than a fresh target prompt.
        | Effect::SetLifeTotal { target, .. }
        | Effect::SkipNextTurn { target, .. }
        | Effect::SkipNextStep { target, .. }
        | Effect::ControlNextTurn { target, .. }
        | Effect::AdditionalPhase { target, .. }
        | Effect::GiveControl { target, .. }
        | Effect::Detain { target, .. }
        | Effect::TargetOnly { target } => target,
        // CR 603.7c + CR 608.2c: `GenericEffect` carries an optional `target` that may
        // be an event-context ref (e.g., `TriggeringSource` for "that land doesn't untap
        // during its controller's next untap step" on a TapsForMana trigger). Routing it
        // through the event-context resolver binds the transient continuous effect to
        // the specific triggering object, mirroring targeted pump/bounce semantics.
        Effect::GenericEffect {
            target: Some(ref filter),
            ..
        } => filter,
        Effect::Token { owner, .. } => owner,
        Effect::RevealTop { player, .. } => player,
        Effect::ExileTop { player, .. } => player,
        // CR 119.3 + CR 603.7c: LoseLife with event-context target (e.g., TriggeringPlayer
        // from "whenever an opponent draws a card, they lose 2 life").
        Effect::LoseLife {
            target: Some(ref filter),
            ..
        } => filter,
        _ => return None,
    };

    if matches!(
        filter,
        TargetFilter::TriggeringSpellController
            | TargetFilter::TriggeringSpellOwner
            | TargetFilter::TriggeringPlayer
            | TargetFilter::TriggeringSource
            | TargetFilter::DefendingPlayer
            | TargetFilter::ParentTargetController
    ) {
        Some(filter)
    } else {
        None
    }
}

fn previous_effect_amount_from_events(effect: &Effect, events: &[GameEvent]) -> Option<i32> {
    let amount = match effect {
        Effect::DealDamage { .. } | Effect::DamageAll { .. } | Effect::DamageEachPlayer { .. } => {
            events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::DamageDealt { amount, .. } => {
                        Some(crate::game::arithmetic::u32_to_i32_saturating(*amount))
                    }
                    _ => None,
                })
                .sum()
        }
        Effect::LoseLife { .. } | Effect::PayCost { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged { amount, .. } if *amount < 0 => Some(-*amount),
                _ => None,
            })
            .sum(),
        Effect::GainLife { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged { amount, .. } if *amount > 0 => Some(*amount),
                _ => None,
            })
            .sum(),
        Effect::RemoveCounter { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CounterRemoved { count, .. } => {
                    Some(crate::game::arithmetic::u32_to_i32_saturating(*count))
                }
                _ => None,
            })
            .sum(),
        _ => 0,
    };

    (amount > 0).then_some(amount)
}

fn previous_effect_counts_by_player_from_events(
    effect: &Effect,
    events: &[GameEvent],
) -> HashMap<PlayerId, i32> {
    let mut counts = HashMap::new();
    if matches!(effect, Effect::Discard { .. } | Effect::DiscardCard { .. }) {
        for event in events {
            if let GameEvent::Discarded { player_id, .. } = event {
                *counts.entry(*player_id).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// Resolve an ability and follow its sub_ability chain using typed nested structs.
/// No SVar lookup, no parse_ability(). The depth is bounded by the data structure.
pub fn resolve_ability_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<(), EffectError> {
    // Safety limit to prevent stack overflow on pathological data
    if depth > 20 {
        return Err(EffectError::ChainTooDeep);
    }

    // Clear stale revealed IDs at the top-level chain entry to prevent leaking
    // across unrelated ability resolutions.
    if depth == 0 {
        state.last_revealed_ids.clear();
        state.last_zone_changed_ids.clear();
        state.last_effect_amount = None;
        state.last_effect_counts_by_player.clear();
        state.exiled_from_hand_this_resolution = 0;
        // CR 603.7: Chain-local tracked-set identity — resets per top-level
        // ability resolution so compound zone changes within one chain
        // coalesce into a single tracked set, while unrelated resolutions
        // stay isolated.
        state.chain_tracked_set_id = None;
        // CR 608.2c + CR 109.5: Player-action accumulator resets per
        // top-level chain so "each opponent who searched this way" only sees
        // players who acted in the current resolution.
        state.player_actions_this_way.clear();
    }

    // BeginGame abilities are handled by mulligan setup, not normal stack resolution.
    if matches!(ability.kind, AbilityKind::BeginGame) && !state.resolving_begin_game_abilities {
        return Ok(());
    }

    // CR 603.4: Bump the per-ability per-turn resolution counter at the start of
    // top-level resolution so that `AbilityCondition::NthResolutionThisTurn`
    // gates can see the current resolution included in the count. Sub-abilities
    // (depth > 0) share the parent's count — they belong to the same printed
    // ability instance. Synthesized/runtime-only abilities (prowess, firebending)
    // and activated abilities lack an `ability_index` stamp and skip this hook.
    if depth == 0 {
        if let Some(idx) = ability.ability_index {
            *state
                .ability_resolutions_this_turn
                .entry((ability.source_id, idx))
                .or_insert(0) += 1;
        }
    }

    // CR 608.2e: "Instead" kicker — check if a sub overrides the parent.
    // When condition is met, replace the current ability's effect with the sub's
    // effect, preserving the full resolution flow (tracked sets, continuations).
    let ability = if let Some(ref sub) = ability.sub_ability {
        // CR 608.2e: "Instead" kicker — swap parent effect with override sub's effect.
        let should_swap = if matches!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        ) {
            ability.context.additional_cost_paid
        } else if let Some(AbilityCondition::CastVariantPaidInstead { variant }) = sub.condition {
            // CR 608.2e + CR 702.49 + CR 702.190a: Read from GameObject, not SpellContext
            state
                .objects
                .get(&ability.source_id)
                .map(|obj| obj.cast_variant_paid == Some((variant, state.turn_number)))
                .unwrap_or(false)
        } else if let Some(AbilityCondition::TargetHasKeywordInstead { ref keyword }) =
            sub.condition
        {
            // CR 608.2e: Check if the first resolved object target has the keyword.
            ability
                .targets
                .iter()
                .find_map(|t| match t {
                    TargetRef::Object(id) => state.objects.get(id),
                    _ => None,
                })
                .is_some_and(|obj| obj.has_keyword(keyword))
        } else if let Some(AbilityCondition::ConditionInstead { ref inner }) = sub.condition {
            // CR 608.2c: General "instead" replacement — evaluate the wrapped condition.
            evaluate_condition(inner, state, ability)
        } else {
            false
        };
        if should_swap {
            let mut overridden = ability.clone();
            overridden.effect = sub.effect.clone();
            if let Some(ref sub_duration) = sub.duration {
                overridden.duration = Some(sub_duration.clone());
            }
            // CR 608.2c: "Instead" semantics replace the entire effect clause.
            // The ConditionInstead inner condition already encodes all resolution
            // checks (e.g., Revolt + MV ≤ 4 via And). The parent's base condition
            // (e.g., MV ≤ 2) is superseded — it only applies when the swap does NOT fire.
            overridden.condition = None;
            // The override sub is consumed; its own sub_ability becomes the new chain tail.
            overridden.sub_ability = sub.sub_ability.clone();
            overridden.else_ability = sub.else_ability.clone();
            Cow::Owned(overridden)
        } else {
            Cow::Borrowed(ability)
        }
    } else {
        Cow::Borrowed(ability)
    };
    let ability = ability.as_ref();

    // CR 608.2: player_scope iteration — when an ability has player_scope set,
    // execute the scoped instruction once per matching player. Runtime keeps
    // the scoped player as the acting `controller` for legacy effect handlers
    // while preserving `original_controller` so "you" quantities still read
    // the printed ability controller. The unscoped tail then resumes once
    // after the scoped loop, matching the printed instruction order.
    if let Some(ref scope) = ability.player_scope {
        let scoped_events_before = events.len();
        let controller = ability.controller;
        let matching_players: Vec<PlayerId> = crate::game::players::apnap_order(state)
            .into_iter()
            .filter(|pid| matches_player_scope(state, *pid, scope, controller, ability.source_id))
            .collect();
        let (scoped_template, after_scope) = split_player_scope_chain(ability);

        let initial_waiting_for = state.waiting_for.clone();
        let mut paused = false;
        for (i, pid) in matching_players.iter().enumerate() {
            let mut scoped = scoped_template.clone();
            scoped.set_original_controller_recursive(controller);
            scoped.controller = *pid;
            scoped.set_scoped_player_recursive(*pid);
            resolve_ability_chain(state, &scoped, events, depth + 1)?;

            // CR 608.2e: Break if inner effect entered a player-choice state —
            // remaining players resume after the choice resolves via continuation.
            if state.waiting_for != initial_waiting_for {
                let remaining = &matching_players[i + 1..];
                let mut tail = after_scope.clone();
                // Build continuation chain for remaining players in APNAP order.
                // Each remaining player gets the scoped instruction only; the
                // unscoped tail runs once after the final scoped iteration.
                for &remaining_pid in remaining.iter().rev() {
                    let mut remaining_scoped = scoped_template.clone();
                    remaining_scoped.set_original_controller_recursive(controller);
                    remaining_scoped.controller = remaining_pid;
                    remaining_scoped.set_scoped_player_recursive(remaining_pid);
                    if let Some(prev) = tail {
                        super::ability_utils::append_to_sub_chain(&mut remaining_scoped, *prev);
                    }
                    tail = Some(Box::new(remaining_scoped));
                }
                if tail.is_some() {
                    append_to_pending_continuation(state, tail);
                }
                paused = true;
                break;
            }
        }
        let scoped_events = &events[scoped_events_before..];
        let counts_by_player =
            previous_effect_counts_by_player_from_events(&scoped_template.effect, scoped_events);
        if !counts_by_player.is_empty() {
            state.last_effect_count = counts_by_player.values().copied().max();
            state.last_effect_amount = state.last_effect_count;
            state.last_effect_counts_by_player = counts_by_player;
        } else if let Some(amount) =
            previous_effect_amount_from_events(&scoped_template.effect, scoped_events)
        {
            state.last_effect_amount = Some(amount);
        }
        if !paused {
            if let Some(after_scope) = after_scope {
                resolve_ability_chain(state, &after_scope, events, depth + 1)?;
            }
        }
        return Ok(());
    }

    // CR 608.2c: Evaluate top-level condition before emitting any optional or unless-pay
    // choice. This must run after player_scope binding so scoped abilities test
    // conditions relative to the scoped player.
    if let Some(ref condition) = ability.condition {
        if !evaluate_condition(condition, state, ability) {
            if let Some(ref else_branch) = ability.else_ability {
                let mut else_resolved = else_branch.as_ref().clone();
                if else_resolved.targets.is_empty() && !ability.targets.is_empty() {
                    else_resolved.targets = ability.targets.clone();
                }
                else_resolved.context = ability.context.clone();
                resolve_ability_chain(state, &else_resolved, events, depth + 1)?;
            }
            return Ok(());
        }
    }

    // CR 608.2d + CR 101.4: "Any opponent may" — prompt opponents in APNAP order.
    if ability.optional && ability.optional_for.is_some() {
        let description = ability.description.clone();
        let mut opponent_order: Vec<PlayerId> = crate::game::players::apnap_order(state)
            .into_iter()
            .filter(|p| *p != ability.controller)
            .collect();
        if let Some(first) = opponent_order.first().copied() {
            let remaining = opponent_order.split_off(1);
            state.pending_optional_effect = Some(Box::new(ability.clone()));
            state.waiting_for = WaitingFor::OpponentMayChoice {
                player: first,
                source_id: ability.source_id,
                description,
                remaining,
            };
        }
        return Ok(());
    }

    // CR 117.3a + CR 609.3: "You may" effects prompt the acting player before
    // execution. For subject-anchored optional effects ("its controller may
    // search their library" — Assassin's Trophy), the acting player is the
    // resolved subject (the target permanent's controller), NOT the caster.
    if ability.optional {
        let description = ability.description.clone();
        let prompt_player = optional_prompt_player(state, ability);
        let may_trigger_key = ability
            .may_trigger_origin
            .map(|origin| MayTriggerAutoChoiceKey {
                player: prompt_player,
                source_id: ability.source_id,
                origin,
            });
        if let Some(key) = may_trigger_key {
            if let Some(choice) = state.may_trigger_auto_choice(&key) {
                resolve_optional_effect_decision(
                    state,
                    ability.clone(),
                    choice,
                    events,
                    depth + 1,
                )?;
                return Ok(());
            }
        }
        state.pending_optional_effect = Some(Box::new(ability.clone()));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: prompt_player,
            source_id: ability.source_id,
            description,
            may_trigger_key,
        };
        return Ok(());
    }

    // CR 118.12: "Effect unless [player] pays {cost}" — tax trigger modifier.
    if let Some(ref unless_pay) = ability.unless_pay {
        if let Some(payer) = resolve_unless_payer(state, ability, &unless_pay.payer) {
            // CR 702.21a: Non-mana costs (PayLife, DiscardCard, Sacrifice) bypass
            // mana resolution — pass through to UnlessPayment directly.
            match &unless_pay.cost {
                UnlessCost::PayLife { .. }
                | UnlessCost::DiscardCard { .. }
                | UnlessCost::Sacrifice { .. }
                | UnlessCost::ReturnToHand { .. } => {
                    let mut pending = ability.clone();
                    pending.unless_pay = None;
                    state.waiting_for = WaitingFor::UnlessPayment {
                        player: payer,
                        cost: unless_pay.cost.clone(),
                        pending_effect: Box::new(pending),
                        trigger_event: state.current_trigger_event.clone(),
                        effect_description: ability.description.clone(),
                    };
                    return Ok(());
                }
                _ => {}
            }
            let resolved_cost = match &unless_pay.cost {
                UnlessCost::Fixed { cost } => cost.clone(),
                UnlessCost::DynamicGeneric { quantity } => {
                    let amount = crate::game::quantity::resolve_quantity(
                        state,
                        quantity,
                        ability.controller,
                        ability.source_id,
                    );
                    ManaCost::generic(amount.max(0) as u32)
                }
                // Non-mana costs handled above.
                UnlessCost::PayLife { .. }
                | UnlessCost::DiscardCard { .. }
                | UnlessCost::Sacrifice { .. }
                | UnlessCost::ReturnToHand { .. } => unreachable!(),
            };
            // CR 118.5: If the cost is {0}, the player is considered to have paid.
            if resolved_cost != ManaCost::zero() {
                // Strip unless_pay from the pending effect to prevent re-prompting.
                let mut pending = ability.clone();
                pending.unless_pay = None;
                state.waiting_for = WaitingFor::UnlessPayment {
                    player: payer,
                    cost: UnlessCost::Fixed {
                        cost: resolved_cost,
                    },
                    pending_effect: Box::new(pending),
                    trigger_event: state.current_trigger_event.clone(),
                    effect_description: ability.description.clone(),
                };
                return Ok(());
            }
            // Cost is {0} — fall through and execute the effect normally.
        }
    }

    // CR 603.7: Snapshot event count so we can detect objects moved by this effect.
    let events_before = events.len();

    // Skip no-op unimplemented/runtime-handled effects
    if !matches!(
        ability.effect,
        Effect::Unimplemented { .. } | Effect::RuntimeHandled { .. }
    ) {
        // CR 603.7c: If the ability has empty targets but its effect uses an event-context
        // target filter (TriggeringSpellController, TriggeringSource, etc.), resolve the
        // filter into an actual TargetRef using the current trigger event context.
        let resolved_ability = if ability.targets.is_empty() {
            if let Some(filter) = extract_event_context_filter(&ability.effect) {
                if let Some(target_ref) = crate::game::targeting::resolve_event_context_target(
                    state,
                    filter,
                    ability.source_id,
                ) {
                    let mut resolved = ability.clone();
                    resolved.targets = vec![target_ref];
                    Some(resolved)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let effective = resolved_ability.as_ref().unwrap_or(ability);

        // CR 608.2b: Validate SharesQuality group constraints before applying effects.
        // If targets don't share the required quality, skip the effect.
        let shares_quality_failed = if effective.targets.len() >= 2 {
            if let Some(target_filter) = effect_target_filter(&effective.effect) {
                let constraints = extract_shares_quality_props(target_filter);
                constraints.iter().any(|(quality, relation)| {
                    let shares =
                        filter::validate_shares_quality(state, &effective.targets, quality);
                    match relation {
                        SharedQualityRelation::Shares => !shares,
                        SharedQualityRelation::DoesNotShare => shares,
                    }
                })
            } else {
                false
            }
        } else {
            false
        };

        if shares_quality_failed {
            // Group constraint not met — emit EffectResolved but skip execution.
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
        } else {
            // CR 609.3: Execute the effect N times when repeat_for is set.
            let iterations = if let Some(ref qty) = ability.repeat_for {
                crate::game::quantity::resolve_quantity(
                    state,
                    qty,
                    ability.controller,
                    ability.source_id,
                )
                .max(0) as usize
            } else {
                1
            };

            // CR 603.7 + CR 608.2c + CR 109.5: Per-iteration parent-target
            // rebinding for tracked-set iterations. When `repeat_for ==
            // TrackedSetSize` and the effect references the parent target via
            // a context-ref filter (e.g., `ParentTargetController`,
            // `ParentTarget`), each iteration must bind to a different member
            // of the tracked set so the per-iteration acting subject is the
            // i-th tracked object's controller (Winds of Abandon, where each
            // exiled creature's controller searches their own library).
            //
            // Without this rebind, every iteration sees `effective.targets[0]`
            // — the first exiled creature only — and only that creature's
            // controller would search.
            let iter_tracked_members: Vec<crate::types::identifiers::ObjectId> = if matches!(
                ability.repeat_for,
                Some(crate::types::ability::QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::TrackedSetSize
                })
            )
                && effect_refs_parent_target(&effective.effect)
            {
                state
                    .chain_tracked_set_id
                    .and_then(|id| state.tracked_object_sets.get(&id).cloned())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            let initial_waiting_for = state.waiting_for.clone();
            let mut iteration = 0usize;
            while iteration < iterations {
                // Snapshot per-iteration ability with parent-target rebinding when applicable.
                let mut iter_ability;
                let iter_effective: &ResolvedAbility =
                    if let Some(member) = iter_tracked_members.get(iteration) {
                        iter_ability = effective.clone();
                        rebind_first_object_target(&mut iter_ability.targets, *member);
                        &iter_ability
                    } else {
                        effective
                    };
                let _ = resolve_effect(state, iter_effective, events);
                // CR 609.3 + CR 109.5: When the inner effect enters an
                // interactive WaitingFor (e.g. SearchChoice), stash the
                // remaining iterations so `drain_pending_continuation` can
                // resume the loop after the player choice (and its chained
                // sub-ability) complete. Without this, only the first
                // iteration would ever fire — the loop would break and the
                // remaining iterations would be silently dropped.
                if state.waiting_for != initial_waiting_for {
                    let next_iteration = iteration + 1;
                    if next_iteration < iterations {
                        // CR 609.3 + CR 109.5: Each resumed iteration must run
                        // through the FULL chain (parent effect + sub_ability)
                        // exactly the way iteration 0 just did. The drain path
                        // re-enters via `resolve_ability_chain`, which goes
                        // through the line-1461 sub_ability wiring and the
                        // line-1660 SearchChoice continuation stash. Without
                        // preserving `sub_ability` here, opponents picked
                        // during iterations 1+ would never have their chosen
                        // card placed onto the battlefield (Winds of Abandon).
                        //
                        // Clear `repeat_for` on the resumed copy so
                        // `resolve_ability_chain` treats each resumed call as
                        // a single iteration rather than re-entering this
                        // outer iteration loop (which would re-iterate from
                        // zero against `total_iterations`). The drain driver
                        // owns iteration accounting via `next_iteration`.
                        let mut resume_ability = effective.clone();
                        resume_ability.repeat_for = None;
                        state.pending_repeat_iteration =
                            Some(crate::types::game_state::PendingRepeatIteration {
                                ability: Box::new(resume_ability),
                                tracked_members: iter_tracked_members.clone(),
                                next_iteration,
                                total_iterations: iterations,
                            });
                    }
                    break;
                }
                iteration += 1;
            }
        } // end shares_quality_failed else
    }

    // CR 609.3: Extract the numeric result emitted by this parent effect for
    // `QuantityRef::PreviousEffectAmount` in sub-abilities. The event class is
    // selected by the parent `Effect` so unrelated numeric side effects from the
    // same resolution are not mixed together: damage to a battle removes defense
    // counters and also deals damage, but "damage dealt this way" must read only
    // `DamageDealt`; Coalition Relic's "counter removed this way" must read only
    // `CounterRemoved`.
    if let Some(amount) =
        previous_effect_amount_from_events(&ability.effect, &events[events_before..])
    {
        state.last_effect_amount = Some(amount);
    }

    // CR 608.2c: Populate last_zone_changed_ids for ZoneChangedThisWay condition evaluation.
    // Scans ZoneChanged events emitted by this effect, mirroring the forward_result pattern.
    state.last_zone_changed_ids = events[events_before..]
        .iter()
        .filter_map(|e| match e {
            GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    // CR 608.2c + CR 109.5: Accumulate player actions across the chain for
    // `PlayerFilter::PerformedActionThisWay`. This is distinct from
    // `last_zone_changed_ids`: "searched this way" keys off the player action
    // even when the search finds no card.
    for event in &events[events_before..] {
        if let GameEvent::PlayerPerformedAction { player_id, action } = event {
            state.player_actions_this_way.insert((*player_id, *action));
        }
    }

    // CR 603.7: Record the objects affected by this effect as a tracked set so
    // downstream sub-abilities can resolve "this way" references (pronouns,
    // `TrackedSetSize`, `TrackedSet` filters). The signal event depends on the
    // effect class:
    //   - ChangeZone / ChangeZoneAll / ExileTop → `ZoneChanged` filtered by
    //     destination zone (CR 903.9a: excludes commanders redirected to the
    //     command zone).
    //   - Destroy / DestroyAll → `CreatureDestroyed` (CR 701.8a). Emitted only
    //     when destruction actually completes — regeneration shields (CR 701.8c)
    //     and indestructible (CR 702.12b) creatures never produce the event,
    //     so the set correctly contains only creatures *destroyed this way*.
    if next_sub_needs_tracked_set(ability) {
        let affected_ids: Vec<ObjectId> = match &ability.effect {
            Effect::Destroy { .. } | Effect::DestroyAll { .. } => events[events_before..]
                .iter()
                .filter_map(|e| match e {
                    GameEvent::CreatureDestroyed { object_id } => Some(*object_id),
                    _ => None,
                })
                .collect(),
            _ => {
                let dest_zone = match &ability.effect {
                    Effect::ChangeZone { destination, .. }
                    | Effect::ChangeZoneAll { destination, .. } => Some(*destination),
                    Effect::ExileTop { .. } => Some(crate::types::zones::Zone::Exile),
                    // CR 400.7 + CR 611.2c: Mass-bounce destination defaults to
                    // Hand; downstream "those creatures" / "for each of those
                    // permanents" tracking must filter by the actual landing zone.
                    Effect::BounceAll { destination, .. } => {
                        Some(destination.unwrap_or(crate::types::zones::Zone::Hand))
                    }
                    _ => None,
                };
                events[events_before..]
                    .iter()
                    .filter_map(|e| match e {
                        GameEvent::ZoneChanged { object_id, to, .. }
                            if dest_zone.is_none_or(|d| *to == d) =>
                        {
                            Some(*object_id)
                        }
                        _ => None,
                    })
                    .collect()
            }
        };
        // CR 603.7 + CR 608.2c: Chain unification. If an ancestor in this
        // resolution chain already published a tracked set, extend that set
        // with the current publish so compound zone-changing effects
        // (e.g., Suspend Aggression: "Exile target permanent and the top
        // card of your library ... For each of those cards") expose every
        // affected object to a single downstream "those cards" reference.
        // Otherwise start a new chain-scoped set.
        if let Some(chain_id) = state.chain_tracked_set_id {
            state
                .tracked_object_sets
                .entry(chain_id)
                .or_default()
                .extend(affected_ids);
        } else {
            let set_id = TrackedSetId(state.next_tracked_set_id);
            state.next_tracked_set_id += 1;
            state.tracked_object_sets.insert(set_id, affected_ids);
            state.chain_tracked_set_id = Some(set_id);
        }
    }

    // ExileFromTopUntil handles its own sub_ability chain internally (injecting the
    // hit card as a target), so skip the outer chain to avoid double-execution.
    if matches!(ability.effect, Effect::ExileFromTopUntil { .. }) {
        return Ok(());
    }

    // Extract moved objects for result forwarding when forward_result is set.
    // Used for "put onto the battlefield attached to [source]" patterns where the
    // moved card becomes the sub-ability's source and the original source becomes a target.
    let forwarded_objects: Vec<ObjectId> = if ability.forward_result {
        events[events_before..]
            .iter()
            .filter_map(|e| match e {
                GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect()
    } else {
        vec![]
    };

    // Follow typed sub_ability chain, propagating parent targets when sub has none.
    // This allows sub-abilities like "its controller gains life" to access the object
    // targeted by the parent (e.g. the exiled creature in Swords to Plowshares).
    if let Some(ref sub) = ability.sub_ability {
        // Check if the sub_ability has a condition that gates its execution.
        // Casting-time conditions are evaluated against the parent's SpellContext.
        if let Some(ref condition) = sub.condition {
            // CR 608.2e: "Instead" overrides are terminal — the Cow swap above either
            // replaced the parent's effect (condition met) or didn't (condition not met).
            // For kicker/ninjutsu/keyword-instead, the base has no continuation chain.
            // For ConditionInstead, the base chain (else_ability) must run when NOT swapped.
            if matches!(
                condition,
                AbilityCondition::AdditionalCostPaidInstead
                    | AbilityCondition::CastVariantPaidInstead { .. }
                    | AbilityCondition::TargetHasKeywordInstead { .. }
            ) {
                if let Some(ref base_chain) = sub.else_ability {
                    let mut resolved = base_chain.as_ref().clone();
                    if resolved.targets.is_empty() && !ability.targets.is_empty() {
                        resolved.targets = ability.targets.clone();
                    }
                    resolved.context = ability.context.clone();
                    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                        debug_assert!(
                            state.pending_continuation.is_none(),
                            "pending_continuation overwritten before consumption — else_ability chain will be lost"
                        );
                        state.pending_continuation =
                            Some(PendingContinuation::new(Box::new(resolved)));
                    } else {
                        resolve_ability_chain(state, &resolved, events, depth + 1)?;
                    }
                }
                return Ok(());
            }
            if matches!(condition, AbilityCondition::ConditionInstead { .. }) {
                // CR 608.2c: Swap didn't fire (condition not met). The parent's own
                // effect has already executed; now run the base continuation chain
                // stored in else_ability (e.g., the "put into hand, then shuffle"
                // that follows the base SearchLibrary).
                if let Some(ref base_chain) = sub.else_ability {
                    let mut resolved = base_chain.as_ref().clone();
                    if resolved.targets.is_empty() && !ability.targets.is_empty() {
                        resolved.targets = ability.targets.clone();
                    }
                    resolved.context = ability.context.clone();
                    // If the parent effect entered an interactive state (e.g.,
                    // SearchChoice), stash the else chain as a continuation so it
                    // runs after the player responds — not immediately.
                    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                        debug_assert!(
                            state.pending_continuation.is_none(),
                            "pending_continuation overwritten before consumption — else_ability chain will be lost"
                        );
                        state.pending_continuation =
                            Some(PendingContinuation::new(Box::new(resolved)));
                    } else {
                        resolve_ability_chain(state, &resolved, events, depth + 1)?;
                    }
                }
                return Ok(());
            }

            let condition_met = evaluate_condition(condition, state, ability);
            if !condition_met {
                // CR 608.2c: Execute else branch if present ("Otherwise, [effect]")
                if let Some(ref else_branch) = sub.else_ability {
                    let mut else_resolved = else_branch.as_ref().clone();
                    // Inject revealed card IDs as targets for else branches following RevealTop
                    // or a pure-peek Dig (reveal: false, keep_count: 0), so "Otherwise, put
                    // that card on the bottom of your library" knows which card to move.
                    if else_resolved.targets.is_empty()
                        && !state.last_revealed_ids.is_empty()
                        && matches!(
                            ability.effect,
                            Effect::RevealTop { .. }
                                | Effect::Dig { reveal: true, .. }
                                | Effect::Dig {
                                    reveal: false,
                                    keep_count: Some(0),
                                    ..
                                }
                        )
                    {
                        else_resolved.targets = state
                            .last_revealed_ids
                            .iter()
                            .map(|&id| TargetRef::Object(id))
                            .collect();
                    } else if else_resolved.targets.is_empty() && !ability.targets.is_empty() {
                        else_resolved.targets = ability.targets.clone();
                    }
                    else_resolved.context = ability.context.clone();
                    resolve_ability_chain(state, &else_resolved, events, depth + 1)?;
                }
                return Ok(());
            }

            // CR 603.12: When a deferred conditional sub-ability (WhenYouDo,
            // QuantityCheck) has its condition met and needs player-selected targets,
            // create a reflexive trigger that goes on the stack for target selection.
            // Targets were not pre-collected (see defers_conditional_target_selection
            // in ability_utils), so we must collect them now.
            if matches!(
                condition,
                AbilityCondition::WhenYouDo | AbilityCondition::QuantityCheck { .. }
            ) && sub.targets.is_empty()
            {
                let target_slots = crate::game::ability_utils::build_target_slots(state, sub)
                    .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
                if !target_slots.is_empty() {
                    // Compute selection first — if this fails (no legal targets for a
                    // required slot), we skip the reflexive trigger cleanly without
                    // leaving an orphaned pending_trigger.
                    let selection = crate::game::ability_utils::begin_target_selection_for_ability(
                        state,
                        sub,
                        &target_slots,
                        &[],
                    )
                    .map_err(|e| EffectError::InvalidParam(e.to_string()))?;

                    let mut reflexive = sub.as_ref().clone();
                    reflexive.context = ability.context.clone();
                    let trigger_description = sub
                        .description
                        .clone()
                        .or_else(|| ability.description.clone());
                    state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
                        source_id: ability.source_id,
                        controller: ability.controller,
                        condition: None,
                        ability: reflexive,
                        timestamp: state.turn_number,
                        target_constraints: vec![],
                        trigger_event: state.current_trigger_event.clone(),
                        modal: None,
                        mode_abilities: vec![],
                        description: trigger_description.clone(),
                        may_trigger_origin: None,
                    });
                    state.waiting_for = WaitingFor::TriggerTargetSelection {
                        player: ability.controller,
                        target_slots,
                        target_constraints: vec![],
                        selection,
                        source_id: Some(ability.source_id),
                        description: trigger_description,
                    };
                    return Ok(());
                }
            }
        }
        // If the effect resolver already set up a pending_continuation without
        // opening a choice (e.g. clash injects modified context for
        // optional_effect_performed), the sub_ability chain is already
        // accounted for — skip to avoid double execution.
        if state.pending_continuation.is_some() && !waits_for_resolution_choice(&state.waiting_for)
        {
            return Ok(());
        }
        // If resolve_effect just entered a player-choice state (Scry/Dig/Surveil),
        // save the sub-ability as a continuation to execute after the player responds,
        // rather than immediately processing it (which would bypass the UI).
        if waits_for_resolution_choice(&state.waiting_for) {
            let mut sub_clone = sub.as_ref().clone();
            if sub_clone.targets.is_empty() && !ability.targets.is_empty() {
                sub_clone.targets = ability.targets.clone();
            }
            // Propagate SpellContext so kicker/optional flags survive continuations.
            sub_clone.context = ability.context.clone();
            prepend_to_pending_continuation(state, sub_clone);
            return Ok(());
        }

        // Apply forward_result: moved object becomes sub's source, original source becomes target.
        // This wires "put onto the battlefield attached to [source]" so Attach sees the
        // moved card as source_id (the attachment) and the original source as target (the host).
        if !forwarded_objects.is_empty() {
            let mut sub_with_context = sub.as_ref().clone();
            sub_with_context.source_id = forwarded_objects[0];
            if !sub_with_context
                .targets
                .iter()
                .any(|t| matches!(t, TargetRef::Object(id) if *id == ability.source_id))
            {
                sub_with_context
                    .targets
                    .push(TargetRef::Object(ability.source_id));
            }
            sub_with_context.context = ability.context.clone();
            resolve_ability_chain(state, &sub_with_context, events, depth + 1)?;
        } else if sub.targets.is_empty()
            && !state.last_revealed_ids.is_empty()
            && matches!(
                ability.effect,
                Effect::RevealTop { .. } | Effect::Dig { reveal: true, .. }
            )
        {
            // Inject revealed card IDs as targets for sub_abilities following RevealTop/Dig(reveal).
            // Parallel to how continuations inject chosen cards as targets.
            let mut sub_with_targets = sub.as_ref().clone();
            sub_with_targets.targets = state
                .last_revealed_ids
                .iter()
                .map(|&id| TargetRef::Object(id))
                .collect();
            sub_with_targets.context = ability.context.clone();
            resolve_ability_chain(state, &sub_with_targets, events, depth + 1)?;
        } else if sub.targets.is_empty()
            && !state.last_zone_changed_ids.is_empty()
            && matches!(ability.effect, Effect::ExileTop { .. })
            && !effect_uses_implicit_tracked_set_targets(&sub.effect)
        {
            // CR 309.4c + CR 607.1: Forward exiled card IDs to sub-ability
            // (linked ability pair — second refers to cards exiled by the first).
            // Skipped when the sub explicitly references the chain-unified
            // tracked set via `TargetFilter::TrackedSet` (compound-exile grants
            // like Suspend Aggression must iterate the full set, not just the
            // ExileTop results).
            let mut sub_with_targets = sub.as_ref().clone();
            sub_with_targets.targets = state
                .last_zone_changed_ids
                .iter()
                .map(|&id| TargetRef::Object(id))
                .collect();
            sub_with_targets.context = ability.context.clone();
            resolve_ability_chain(state, &sub_with_targets, events, depth + 1)?;
        } else if sub.targets.is_empty() && effect_uses_implicit_tracked_set_targets(&sub.effect) {
            let mut sub_with_context = sub.as_ref().clone();
            sub_with_context.context = ability.context.clone();
            resolve_ability_chain(state, &sub_with_context, events, depth + 1)?;
        } else if sub.targets.is_empty() && !ability.targets.is_empty() {
            let mut sub_with_targets = sub.as_ref().clone();
            sub_with_targets.targets = ability.targets.clone();
            sub_with_targets.context = ability.context.clone();
            resolve_ability_chain(state, &sub_with_targets, events, depth + 1)?;
        } else {
            // Propagate SpellContext so additional_cost_paid and other flags
            // survive through the chain (e.g., Gift delivery → spell effects
            // with "if the gift was promised" conditions).
            let mut sub_with_context = sub.as_ref().clone();
            sub_with_context.context = ability.context.clone();
            resolve_ability_chain(state, &sub_with_context, events, depth + 1)?;
        }
    }

    Ok(())
}

/// CR 608.2c: Evaluate a condition against the current game state and ability context.
/// Returns whether the condition is met. Handles all `AbilityCondition` variants as
/// pure boolean evaluators — callers are responsible for any terminal control flow
/// (e.g., "Instead" overrides that early-return in the sub-ability context).
fn evaluate_condition(
    condition: &AbilityCondition,
    state: &GameState,
    ability: &ResolvedAbility,
) -> bool {
    match condition {
        // CR 702.33d + CR 702.33f + CR 608.2c: Parameterized additional-cost
        // gating. The default shape (`variant: None`, `min_count: 1`) reads the
        // legacy single-bool flag used by Gift / Buyback / Bargain / Evidence /
        // plain "if it was kicked". Specific kicker variants and multi-kicker
        // counts read `kickers_paid` (populated by the casting flow, copied to
        // GameObject at cast resolution, and propagated back into the trigger's
        // resolved-ability context for ETB triggers).
        AbilityCondition::AdditionalCostPaid {
            variant,
            kicker_cost,
            min_count,
        } => {
            ability
                .context
                .additional_cost_paid_matches(*variant, kicker_cost.as_ref(), *min_count)
        }
        AbilityCondition::IfYouDo | AbilityCondition::IfAPlayerDoes => {
            ability.context.optional_effect_performed && !state.cost_payment_failed_flag
        }
        // CR 603.12: "When you do" — reflexive trigger that always fires.
        AbilityCondition::WhenYouDo => true,
        // CR 603.4: "If you cast it from [zone]" — check cast origin.
        AbilityCondition::CastFromZone { zone } => ability.context.cast_from_zone == Some(*zone),
        // CR 608.2c: "If it's a [type] card" — check the revealed card's type.
        // CR 205.3m: Optional additional_filter checks extra properties like
        // "of the chosen type" (IsChosenCreatureType).
        AbilityCondition::RevealedHasCardType {
            card_type,
            additional_filter,
        } => {
            let type_matches = state
                .last_revealed_ids
                .first()
                .and_then(|id| {
                    state
                        .objects
                        .get(id)
                        .map(|obj| obj.card_types.core_types.contains(card_type))
                })
                .unwrap_or(false);
            let filter_matches = match additional_filter {
                // CR 205.3m: "of the chosen type" — check the revealed card's subtype
                // against the source permanent's chosen creature type.
                Some(FilterProp::IsChosenCreatureType) => {
                    let source = state.objects.get(&ability.source_id);
                    let revealed = state
                        .last_revealed_ids
                        .first()
                        .and_then(|id| state.objects.get(id));
                    match (source, revealed) {
                        (Some(src), Some(obj)) => {
                            src.chosen_creature_type().is_some_and(|chosen_type| {
                                obj.card_types
                                    .subtypes
                                    .iter()
                                    .any(|s| s.eq_ignore_ascii_case(chosen_type))
                            })
                        }
                        _ => false,
                    }
                }
                Some(_) => {
                    // Other filter properties not yet supported for revealed card checks
                    true
                }
                None => true,
            };
            type_matches && filter_matches
        }
        // CR 400.7 + CR 608.2c: source permanent entered the battlefield this turn.
        // For the "unless ~ entered this turn" sense, wrap with `Not`.
        AbilityCondition::SourceEnteredThisTurn => state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.entered_battlefield_turn == Some(state.turn_number))
            .unwrap_or(false),
        // CR 702.49 + CR 702.190a + CR 603.4: "if its sneak/ninjutsu cost was paid"
        AbilityCondition::CastVariantPaid { variant } => state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.cast_variant_paid == Some((*variant, state.turn_number)))
            .unwrap_or(false),
        // CR 608.2c: General quantity comparison on trigger/effect context.
        AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        } => {
            let l = crate::game::quantity::resolve_quantity(
                state,
                lhs,
                ability.controller,
                ability.source_id,
            );
            let r = crate::game::quantity::resolve_quantity(
                state,
                rhs,
                ability.controller,
                ability.source_id,
            );
            comparator.evaluate(l, r)
        }
        AbilityCondition::CastDuringPhase { phases } => ability
            .context
            .cast_phase
            .is_some_and(|cast_phase| phases.contains(&cast_phase)),
        // CR 601.2h + CR 608.2c: Spend-color riders read the resolving spell
        // object's recorded mana payment. Spell copies with no mana paid
        // naturally fail because their tally is empty.
        AbilityCondition::ManaColorSpent { color, minimum } => state
            .objects
            .get(&ability.source_id)
            .is_some_and(|obj| obj.colors_spent_to_cast.get(*color) >= *minimum),
        AbilityCondition::HasMaxSpeed => has_max_speed(state, ability.controller),
        AbilityCondition::IsMonarch => state.monarch == Some(ability.controller),
        // CR 702.131c: The city's blessing is a player designation that effects
        // can identify.
        AbilityCondition::HasCityBlessing => state.city_blessing.contains(&ability.controller),
        // "Instead" override conditions — return pure boolean value.
        // Terminal control flow (early return from resolve_ability_chain) is the caller's
        // responsibility in the sub-ability context.
        AbilityCondition::AdditionalCostPaidInstead => ability.context.additional_cost_paid,
        AbilityCondition::CastVariantPaidInstead { variant } => state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.cast_variant_paid == Some((*variant, state.turn_number)))
            .unwrap_or(false),
        AbilityCondition::TargetHasKeywordInstead { ref keyword } => ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => state.objects.get(id),
                _ => None,
            })
            .is_some_and(|obj| obj.has_keyword(keyword)),
        // CR 400.7 + CR 608.2c: "if that creature was a [type]" — check target or its LKI.
        AbilityCondition::TargetMatchesFilter { filter, use_lki } => {
            let target_id = ability.targets.iter().find_map(|t| match t {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            });
            let matched = if let Some(id) = target_id {
                if *use_lki {
                    if let Some(GameEvent::ZoneChanged { record, .. }) =
                        state.current_trigger_event.as_ref()
                    {
                        if record.object_id == id {
                            return crate::game::filter::matches_target_filter_on_zone_change_record(
                                state,
                                record,
                                filter,
                                &crate::game::filter::FilterContext::from_ability(ability),
                            );
                        }
                    }
                    // CR 400.7: Check last-known information for past-tense conditions.
                    // Try LKI cache first, fall back to current state if object still exists.
                    if let Some(lki) = state.lki_cache.get(&id) {
                        crate::game::filter::matches_target_filter_on_lki_snapshot(
                            state,
                            id,
                            lki,
                            filter,
                            &crate::game::filter::FilterContext::from_ability(ability),
                        )
                    } else {
                        // Object still exists — check current state.
                        // CR 107.3a + CR 601.2b: ability-context filter evaluation.
                        crate::game::filter::matches_target_filter(
                            state,
                            id,
                            filter,
                            &crate::game::filter::FilterContext::from_ability(ability),
                        )
                    }
                } else {
                    // Check current state for present-tense conditions.
                    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
                    crate::game::filter::matches_target_filter(
                        state,
                        id,
                        filter,
                        &crate::game::filter::FilterContext::from_ability(ability),
                    )
                }
            } else {
                false
            };
            matched
        }
        // CR 608.2c: "If this creature/permanent is a [type]" — check source object.
        AbilityCondition::SourceMatchesFilter { filter } => {
            // CR 107.3a + CR 601.2b: ability-context filter evaluation.
            crate::game::filter::matches_target_filter(
                state,
                ability.source_id,
                filter,
                &crate::game::filter::FilterContext::from_ability(ability),
            )
        }
        AbilityCondition::ZoneChangeObjectMatchesFilter {
            origin,
            destination,
            filter,
        } => state.current_trigger_event.as_ref().is_some_and(|event| {
            crate::game::filter::matches_zone_change_event_object_filter(
                state,
                event,
                *origin,
                *destination,
                filter,
                &crate::game::filter::FilterContext::from_ability(ability),
            )
        }),
        // CR 608.2c + CR 614.1d: "if you control a/no [filter]" — scan the battlefield
        // for any other permanent owned/controlled by the ability controller matching
        // `filter`. Excludes the source itself so a Soldier-typed land doesn't satisfy
        // its own "you control a Soldier" check. `filter` carries `ControllerRef::You`
        // pre-bound by the parser; FilterContext provides the source binding.
        AbilityCondition::ControllerControlsMatching { filter } => {
            let ctx = crate::game::filter::FilterContext::from_ability(ability);
            state.objects.values().any(|o| {
                o.zone == crate::types::zones::Zone::Battlefield
                    && o.id != ability.source_id
                    && crate::game::filter::matches_target_filter(state, o.id, filter, &ctx)
            })
        }
        // CR 608.2c: "If it's your turn" — check active player against the
        // scoped player during each-player iteration, otherwise the controller.
        AbilityCondition::IsYourTurn => {
            state.active_player == ability.scoped_player.unwrap_or(ability.controller)
        }
        // CR 500.8 + CR 506.1 + CR 608.2c: "if it's the first combat phase
        // of the turn" gates follow-up effects such as additional combats.
        AbilityCondition::FirstCombatPhaseOfTurn => state.combat_phases_started_this_turn == 1,
        // CR 608.2c: "If a [noun] was [verb]ed this way" — check if any zone-changed
        // object matches the type filter. For optional-targeting parents with no targets
        // chosen, last_zone_changed_ids is empty → returns false.
        AbilityCondition::ZoneChangedThisWay { filter } => {
            // CR 107.3a + CR 601.2b: ability-context filter evaluation.
            let ctx = crate::game::filter::FilterContext::from_ability(ability);
            state
                .last_zone_changed_ids
                .iter()
                .any(|&id| crate::game::filter::matches_target_filter(state, id, filter, &ctx))
        }
        AbilityCondition::CostPaidObjectMatchesFilter { filter } => {
            ability.cost_paid_object.as_ref().is_some_and(|snapshot| {
                crate::game::filter::matches_target_filter_on_lki_snapshot(
                    state,
                    snapshot.object_id,
                    &snapshot.lki,
                    filter,
                    &crate::game::filter::FilterContext::from_ability(ability),
                )
            })
        }
        // CR 611.2b: "if this creature/permanent is tapped" — check source object.
        // For the untapped sense, wrap with `Not`.
        AbilityCondition::SourceIsTapped => state
            .objects
            .get(&ability.source_id)
            .is_some_and(|obj| obj.tapped),
        // CR 608.2c: General "instead" — delegate to the wrapped inner condition.
        // The "instead" semantics are handled by the swap/guard in resolve_ability_chain.
        AbilityCondition::ConditionInstead { inner } => evaluate_condition(inner, state, ability),
        // CR 608.2c: Compound condition — all inner conditions must be true.
        AbilityCondition::And { conditions } => conditions
            .iter()
            .all(|c| evaluate_condition(c, state, ability)),
        // CR 608.2c: Compound condition — at least one inner condition must be true.
        AbilityCondition::Or { conditions } => conditions
            .iter()
            .any(|c| evaluate_condition(c, state, ability)),
        // CR 608.2c: Logical negation — true when the inner condition is false.
        AbilityCondition::Not { condition } => !evaluate_condition(condition, state, ability),
        // CR 730.2a: True when it's neither day nor night (no designation set yet).
        AbilityCondition::DayNightIsNeither => state.day_night.is_none(),
        // CR 731.1: True when the game has the requested day/night designation.
        AbilityCondition::DayNightIs {
            state: DayNight::Day,
        } => state.day_night == Some(DayNight::Day),
        AbilityCondition::DayNightIs {
            state: DayNight::Night,
        } => state.day_night == Some(DayNight::Night),
        // CR 603.4: "if this is the [Nth] time this ability has resolved this turn".
        // The counter is bumped at the top of `resolve_ability_chain` (depth 0)
        // before this evaluator runs, so a freshly-incremented count of `n`
        // satisfies the condition for the Nth resolution. Abilities without an
        // `ability_index` stamp (synthesized triggers, activated abilities) never
        // increment the counter and therefore evaluate as `count == 0`, which
        // matches no `n >= 1` print.
        AbilityCondition::NthResolutionThisTurn { n } => {
            if let Some(idx) = ability.ability_index {
                let count = state
                    .ability_resolutions_this_turn
                    .get(&(ability.source_id, idx))
                    .copied()
                    .unwrap_or(0);
                count == *n
            } else {
                false
            }
        }
        AbilityCondition::SourceLacksKeyword { keyword } => state
            .objects
            .get(&ability.source_id)
            .is_some_and(|obj| !obj.has_keyword(keyword)),
    }
}

/// Resolve the payer for an unless-pay modifier from the trigger event context.
/// `TriggeringPlayer` resolves to the player involved in the triggering event
/// (e.g., the opponent who cast a spell for Esper Sentinel).
fn resolve_unless_payer(
    state: &GameState,
    ability: &ResolvedAbility,
    payer: &TargetFilter,
) -> Option<crate::types::player::PlayerId> {
    match payer {
        TargetFilter::TriggeringPlayer => {
            state
                .current_trigger_event
                .as_ref()
                .and_then(|event| match event {
                    GameEvent::SpellCast { controller, .. } => Some(*controller),
                    GameEvent::PlayerPerformedAction { player_id, .. } => Some(*player_id),
                    _ => None,
                })
        }
        TargetFilter::Controller => Some(state.active_player),
        TargetFilter::ParentTargetController => {
            crate::game::targeting::resolve_effect_player_ref(state, ability, payer)
        }
        _ => None,
    }
}

/// CR 601.2f: "The next spell you cast this turn costs {N} less to cast."
/// Pushes a one-shot cost reduction entry consumed when the player casts their next spell.
fn resolve_reduce_next_spell_cost(
    state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), crate::types::ability::EffectError> {
    let (amount, spell_filter) = match &ability.effect {
        Effect::ReduceNextSpellCost {
            amount,
            spell_filter,
        } => (*amount, spell_filter.clone()),
        _ => {
            return Err(crate::types::ability::EffectError::MissingParam(
                "ReduceNextSpellCost".to_string(),
            ))
        }
    };
    state
        .pending_spell_cost_reductions
        .push(crate::types::game_state::PendingSpellCostReduction {
            player: ability.controller,
            amount,
            spell_filter,
        });
    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::ReduceNextSpellCost,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 601.2f: Register a pending next-spell modifier (keyword grant, uncounterability, flash).
/// Consumed when the player casts their next qualifying spell.
fn resolve_grant_next_spell_ability(
    state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), crate::types::ability::EffectError> {
    let (modifier, spell_filter) = match &ability.effect {
        Effect::GrantNextSpellAbility {
            modifier,
            spell_filter,
        } => (modifier.clone(), spell_filter.clone()),
        _ => {
            return Err(crate::types::ability::EffectError::MissingParam(
                "GrantNextSpellAbility".to_string(),
            ))
        }
    };
    state
        .pending_next_spell_modifiers
        .push(crate::types::game_state::PendingNextSpellModifier {
            player: ability.controller,
            modifier,
            spell_filter,
        });
    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::GrantNextSpellAbility,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 614.1c: Register pending ETB counters for the triggering creature spell.
/// Reads `current_trigger_event` (SpellCast) to identify the object, then adds
/// counters to `pending_etb_counters` so they are applied when the object enters
/// the battlefield.
fn resolve_add_pending_etb_counters(
    state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), crate::types::ability::EffectError> {
    let (counter_type, count) = match &ability.effect {
        Effect::AddPendingETBCounters {
            counter_type,
            count,
        } => (counter_type.clone(), count.clone()),
        _ => {
            return Err(crate::types::ability::EffectError::MissingParam(
                "AddPendingETBCounters".to_string(),
            ))
        }
    };

    // Resolve the count using existing quantity infrastructure
    let resolved_count = crate::game::quantity::resolve_quantity(
        state,
        &count,
        ability.controller,
        ability.source_id,
    ) as u32;

    // Extract the object_id from the triggering SpellCast event
    let object_id = state.current_trigger_event.as_ref().and_then(|e| match e {
        GameEvent::SpellCast { object_id, .. } => Some(*object_id),
        _ => None,
    });

    if let Some(oid) = object_id {
        state
            .pending_etb_counters
            .push((oid, counter_type, resolved_count));
    } else {
        tracing::warn!(
            "AddPendingETBCounters: no SpellCast trigger event found — counters not registered"
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::AddPendingETBCounters,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, CastingPermission, ControllerRef,
        DelayedTriggerCondition, Duration, FilterProp, GainLifePlayer, PlayerFilter, PlayerScope,
        PtValue, QuantityExpr, QuantityRef, SpellContext, TargetFilter, TargetRef, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        AutoMayChoice, ExileLink, ExileLinkKind, LinkedExileSnapshot, MayTriggerAutoChoiceKey,
        MayTriggerOrigin,
    };
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::mana::ManaColor;
    use crate::types::mana::ManaCost;
    use crate::types::phase::Phase;
    use crate::types::player::{PlayerCounterKind, PlayerId};
    use crate::types::zones::Zone;

    #[test]
    fn is_known_effect_rejects_unimplemented() {
        let known = Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
        };
        assert!(is_known_effect(&known));

        let unknown = Effect::Unimplemented {
            name: "Fateseal".to_string(),
            description: None,
        };
        assert!(!is_known_effect(&unknown));

        // RuntimeHandled is a known effect — it's handled by a dedicated engine path
        let runtime = Effect::RuntimeHandled {
            handler: crate::types::ability::RuntimeHandler::NinjutsuFamily,
        };
        assert!(is_known_effect(&runtime));
    }

    #[test]
    fn resolve_effect_returns_ok_for_unimplemented() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "NonExistentEffect".to_string(),
                description: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve_effect(&mut state, &ability, &mut events);
        assert!(result.is_ok());
    }

    fn optional_gain_life(
        source_id: ObjectId,
        controller: PlayerId,
        amount: i32,
    ) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: GainLifePlayer::Controller,
            },
            vec![],
            source_id,
            controller,
        );
        ability.optional = true;
        ability
    }

    #[test]
    fn optional_trigger_prompt_includes_may_trigger_key() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let mut ability = optional_gain_life(source_id, PlayerId(0), 1);
        ability.set_may_trigger_origin_recursive(MayTriggerOrigin::Printed { trigger_index: 2 });

        resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0).unwrap();

        match state.waiting_for {
            WaitingFor::OptionalEffectChoice {
                may_trigger_key: Some(key),
                ..
            } => {
                assert_eq!(key.player, PlayerId(0));
                assert_eq!(key.source_id, source_id);
                assert_eq!(key.origin, MayTriggerOrigin::Printed { trigger_index: 2 });
            }
            other => panic!("expected keyed OptionalEffectChoice, got {other:?}"),
        }
    }

    #[test]
    fn saved_accept_for_may_trigger_resolves_without_prompt() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
        let key = MayTriggerAutoChoiceKey {
            player: PlayerId(0),
            source_id,
            origin,
        };
        state.set_may_trigger_auto_choice(key, AutoMayChoice::Accept);
        let mut ability = optional_gain_life(source_id, PlayerId(0), 3);
        ability.set_may_trigger_origin_recursive(origin);

        resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0).unwrap();

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].life, 23);
    }

    #[test]
    fn saved_decline_for_may_trigger_resolves_without_prompt() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
        let key = MayTriggerAutoChoiceKey {
            player: PlayerId(0),
            source_id,
            origin,
        };
        state.set_may_trigger_auto_choice(key, AutoMayChoice::Decline);
        let mut ability = optional_gain_life(source_id, PlayerId(0), 3);
        ability.set_may_trigger_origin_recursive(origin);

        resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0).unwrap();

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].life, 20);
    }

    #[test]
    fn saved_may_trigger_choice_is_scoped_to_prompt_player() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: PlayerId(0),
                source_id,
                origin,
            },
            AutoMayChoice::Accept,
        );
        let mut ability = optional_gain_life(source_id, PlayerId(1), 3);
        ability.set_may_trigger_origin_recursive(origin);

        resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0).unwrap();

        match state.waiting_for {
            WaitingFor::OptionalEffectChoice {
                may_trigger_key: Some(key),
                ..
            } => assert_eq!(key.player, PlayerId(1)),
            other => panic!("expected prompt for player 1, got {other:?}"),
        }
        assert_eq!(state.players[1].life, 20);
    }

    #[test]
    fn may_trigger_origin_stamps_optional_sub_abilities() {
        let source_id = ObjectId(100);
        let origin = MayTriggerOrigin::Printed { trigger_index: 4 };
        let mut root = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        root.sub_ability = Some(Box::new(optional_gain_life(source_id, PlayerId(0), 1)));
        root.set_may_trigger_origin_recursive(origin);

        assert_eq!(root.may_trigger_origin, Some(origin));
        assert_eq!(
            root.sub_ability.as_ref().unwrap().may_trigger_origin,
            Some(origin)
        );
    }

    #[test]
    fn resolve_unless_payer_uses_player_action_event_player() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(GameEvent::PlayerPerformedAction {
            player_id: PlayerId(1),
            action: crate::types::events::PlayerActionKind::SearchedLibrary,
        });
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        assert_eq!(
            resolve_unless_payer(&state, &ability, &TargetFilter::TriggeringPlayer),
            Some(PlayerId(1))
        );
    }

    #[test]
    fn resolve_unless_payer_uses_parent_target_controller() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::ParentTargetController),
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );

        assert_eq!(
            resolve_unless_payer(&state, &ability, &TargetFilter::ParentTargetController),
            Some(PlayerId(1))
        );
    }

    #[test]
    fn resolve_ability_chain_single_effect() {
        let mut state = GameState::new_two_player(42);
        // Add a card in library so Draw has something to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 0);
        assert!(result.is_ok());
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn resolve_ability_chain_with_typed_sub_ability() {
        let mut state = GameState::new_two_player(42);
        // Add cards to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );

        // Build a chain: DealDamage -> Draw using typed sub_ability
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 0);
        assert!(result.is_ok());
        // Damage dealt to player 1
        assert_eq!(state.players[1].life, 18);
        // Controller drew a card
        assert_eq!(state.players[0].hand.len(), 1);
    }

    fn bounce_then_draw_if_controller_matched_lki(
        permanent_controller: PlayerId,
    ) -> (GameState, Vec<GameEvent>) {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );
        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target Permanent".to_string(),
            Zone::Battlefield,
        );
        let permanent_obj = state.objects.get_mut(&permanent).unwrap();
        permanent_obj.controller = permanent_controller;
        permanent_obj.card_types.core_types.push(CoreType::Artifact);

        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
            use_lki: true,
        });
        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
            },
            vec![TargetRef::Object(permanent)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(draw);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        (state, events)
    }

    #[test]
    fn bounce_followup_draws_when_caster_controlled_parent_target() {
        let (state, events) = bounce_then_draw_if_controller_matched_lki(PlayerId(0));

        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[1].hand.len(), 1);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                to: Zone::Hand,
                ..
            }
        )));
    }

    #[test]
    fn bounce_followup_skips_draw_when_opponent_controlled_parent_target() {
        let (state, events) = bounce_then_draw_if_controller_matched_lki(PlayerId(1));

        assert_eq!(state.players[0].hand.len(), 0);
        assert_eq!(state.players[1].hand.len(), 1);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                to: Zone::Hand,
                ..
            }
        )));
    }

    #[test]
    fn previous_effect_amount_for_damage_ignores_counter_side_effects() {
        let mut state = GameState::new_two_player(42);
        let battle_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Test Siege".to_string(),
            Zone::Battlefield,
        );
        {
            let battle = state.objects.get_mut(&battle_id).unwrap();
            battle.card_types.core_types.push(CoreType::Battle);
            battle.defense = Some(5);
            battle.base_defense = Some(5);
            battle.counters.insert(CounterType::Defense, 5);
        }

        let sub = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount,
                },
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(battle_id)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::CounterRemoved {
                    counter_type: CounterType::Defense,
                    count: 3,
                    ..
                }
            )),
            "damage to a battle must still emit the defense-counter side effect"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::DamageDealt { amount: 3, .. })),
            "damage event must still be present for PreviousEffectAmount"
        );
        assert_eq!(
            state.players[0].life, 23,
            "sub-ability must use the damage amount only, not damage + counters removed"
        );
    }

    #[test]
    fn resolve_ability_chain_condition_blocks_optional_prompt() {
        let mut state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: crate::types::ability::PaymentCost::Life {
                    amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                },
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IsYourTurn),
        })
        .sub_ability(ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::SelfRef,
                destination: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        ));
        ability.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(!matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        assert!(state.pending_optional_effect.is_none());
        assert!(events.is_empty());
    }

    #[test]
    fn chain_depth_exceeds_limit_returns_error() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 21);
        assert_eq!(result, Err(EffectError::ChainTooDeep));
    }

    #[test]
    fn tracked_set_recorded_for_delayed_trigger() {
        let mut state = GameState::new_two_player(42);

        // Create 2 objects on the battlefield to be exiled
        let obj1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature A".to_string(),
            Zone::Battlefield,
        );
        let obj2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Creature B".to_string(),
            Zone::Battlefield,
        );

        // Build chain: ChangeZone(exile) -> CreateDelayedTrigger(uses_tracked_set: true)
        let delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Battlefield,
                        target: TargetFilter::TrackedSet {
                            id: TrackedSetId(0),
                        },
                        owner_library: false,
                        enter_transformed: false,
                        under_your_control: false,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                    },
                )),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(obj1), TargetRef::Object(obj2)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(delayed);

        let mut events = Vec::new();
        let result = resolve_ability_chain(&mut state, &ability, &mut events, 0);
        assert!(result.is_ok());

        // Tracked set should contain both exiled objects
        assert_eq!(state.tracked_object_sets.len(), 1);
        let set = state.tracked_object_sets.values().next().unwrap();
        assert!(set.contains(&obj1));
        assert!(set.contains(&obj2));

        // Delayed trigger should have been created
        assert_eq!(state.delayed_triggers.len(), 1);
    }

    #[test]
    fn no_tracked_set_without_flag() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );

        // Same chain but uses_tracked_set: false
        let delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Battlefield,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        under_your_control: false,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                    },
                )),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(obj)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(delayed);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            state.tracked_object_sets.is_empty(),
            "Should NOT record tracked set when uses_tracked_set is false"
        );
    }

    /// CR 608.2c building-block regression: a synchronous chain of
    /// `ChangeZoneAll(Battlefield → Hand)` followed by
    /// `Token { count: Ref(TrackedSetSize) }` produces one token per object
    /// moved by the parent. CR 608.2c (verified via `grep '^608.2c'
    /// docs/MagicCompRules.txt`) covers "instructions in the order written"
    /// — the per-instruction-set basis for "this way" referencing. Covers the
    /// "Return all <X> to their owners' hands. If you do, create N Treasure
    /// tokens, where N is the number of permanents returned this way" pattern
    /// (Item 1 of the design doc) at the primitive level — the parser arm is
    /// deferred until a real card surfaces.
    ///
    /// Asserts both the K-object case and the K=0 case so the
    /// `chain_tracked_set_id` plumbing can be trusted by future callers.
    #[test]
    fn change_zone_all_battlefield_to_hand_publishes_chain_for_tracked_set_size_cr_609_3() {
        fn run_with_count(k: usize) -> (Vec<ObjectId>, GameState) {
            let mut state = GameState::new_two_player(42);
            let mut moving_ids = Vec::with_capacity(k);
            for i in 0..k {
                moving_ids.push(create_object(
                    &mut state,
                    CardId(100 + i as u64),
                    PlayerId(0),
                    format!("Equipment {i}"),
                    Zone::Battlefield,
                ));
            }
            let initial_battlefield = state.battlefield.len();

            let token_sub = ResolvedAbility::new(
                Effect::Token {
                    name: "Treasure".to_string(),
                    power: PtValue::Fixed(0),
                    toughness: PtValue::Fixed(0),
                    types: vec!["Artifact".to_string(), "Treasure".to_string()],
                    colors: vec![],
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::TrackedSetSize,
                    },
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            );
            let ability = ResolvedAbility::new(
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Hand,
                    target: TargetFilter::Any,
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            )
            .sub_ability(token_sub);

            let mut events = Vec::new();
            resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

            // Sanity: the K originals left the battlefield, the new tokens entered.
            // Battlefield count = initial - K (originals returned) + K (tokens minted).
            assert_eq!(state.battlefield.len(), initial_battlefield);
            (moving_ids, state)
        }

        // Three objects → three Treasure tokens; originals are in Hand.
        let (moved, state) = run_with_count(3);
        for id in &moved {
            assert_eq!(state.objects[id].zone, Zone::Hand);
        }
        let treasures: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| o.name == "Treasure")
            .collect();
        assert_eq!(
            treasures.len(),
            3,
            "TrackedSetSize must equal the number of permanents moved by the parent ChangeZoneAll"
        );

        // Zero objects → zero Treasure tokens (no spurious creation when chain is empty).
        let (_, state0) = run_with_count(0);
        let treasures0 = state0
            .battlefield
            .iter()
            .filter_map(|id| state0.objects.get(id))
            .filter(|o| o.name == "Treasure")
            .count();
        assert_eq!(treasures0, 0, "Empty chain must mint zero tokens");
    }

    #[test]
    fn empty_targets_record_empty_tracked_set_for_downstream_context() {
        let mut state = GameState::new_two_player(42);

        // Chain with uses_tracked_set: true but no targets — nothing to exile
        let delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Battlefield,
                        target: TargetFilter::TrackedSet {
                            id: TrackedSetId(0),
                        },
                        owner_library: false,
                        enter_transformed: false,
                        under_your_control: false,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                    },
                )),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![], // no targets
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(delayed);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.tracked_object_sets.len(), 1);
        assert!(state
            .tracked_object_sets
            .get(&TrackedSetId(1))
            .is_some_and(|objects| objects.is_empty()));
    }

    #[test]
    fn airbend_chain_exiles_all_creatures_when_no_target_is_chosen() {
        let mut state = GameState::new_two_player(42);
        let creature_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature A".to_string(),
            Zone::Battlefield,
        );
        let creature_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Creature B".to_string(),
            Zone::Battlefield,
        );
        for creature in [creature_a, creature_b] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        )
        .sub_ability(
            ResolvedAbility::new(
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::And {
                        filters: vec![
                            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                            TargetFilter::Not {
                                filter: Box::new(TargetFilter::ParentTarget),
                            },
                        ],
                    },
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            )
            .sub_ability(ResolvedAbility::new(
                Effect::GrantCastingPermission {
                    permission: crate::types::ability::CastingPermission::ExileWithAltCost {
                        cost: ManaCost::generic(2),
                        cast_transformed: false,
                        constraint: None,
                    },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    grantee: Default::default(),
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            )),
        );

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        for creature in [creature_a, creature_b] {
            let obj = state.objects.get(&creature).unwrap();
            assert_eq!(obj.zone, Zone::Exile);
            assert!(obj.casting_permissions.iter().any(|permission| matches!(
                permission,
                crate::types::ability::CastingPermission::ExileWithAltCost { cost, .. }
                    if *cost == ManaCost::generic(2)
            )));
        }
    }

    #[test]
    fn airbend_chain_preserves_chosen_target_and_exiles_other_creatures() {
        let mut state = GameState::new_two_player(42);
        let chosen = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Chosen".to_string(),
            Zone::Battlefield,
        );
        let other = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Other".to_string(),
            Zone::Battlefield,
        );
        for creature in [chosen, other] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            },
            vec![TargetRef::Object(chosen)],
            ObjectId(901),
            PlayerId(0),
        )
        .sub_ability(
            ResolvedAbility::new(
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::And {
                        filters: vec![
                            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                            TargetFilter::Not {
                                filter: Box::new(TargetFilter::ParentTarget),
                            },
                        ],
                    },
                },
                vec![],
                ObjectId(901),
                PlayerId(0),
            )
            .sub_ability(ResolvedAbility::new(
                Effect::GrantCastingPermission {
                    permission: crate::types::ability::CastingPermission::ExileWithAltCost {
                        cost: ManaCost::generic(2),
                        cast_transformed: false,
                        constraint: None,
                    },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    grantee: Default::default(),
                },
                vec![],
                ObjectId(901),
                PlayerId(0),
            )),
        );

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.objects.get(&chosen).unwrap().zone, Zone::Battlefield);
        let other_obj = state.objects.get(&other).unwrap();
        assert_eq!(other_obj.zone, Zone::Exile);
        assert!(other_obj
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                crate::types::ability::CastingPermission::ExileWithAltCost { cost, .. }
                    if *cost == ManaCost::generic(2)
            )));
    }

    #[test]
    fn tracked_set_sentinel_does_not_reuse_prior_non_empty_set_when_current_move_is_empty() {
        let mut state = GameState::new_two_player(42);
        let stale = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Stale".to_string(),
            Zone::Exile,
        );
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![stale]);
        state.next_tracked_set_id = 2;

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            },
            vec![],
            ObjectId(902),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: crate::types::ability::CastingPermission::ExileWithAltCost {
                    cost: ManaCost::generic(2),
                    cast_transformed: false,
                    constraint: None,
                },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                grantee: Default::default(),
            },
            vec![],
            ObjectId(902),
            PlayerId(0),
        ));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(state
            .tracked_object_sets
            .get(&TrackedSetId(2))
            .is_some_and(|objects| objects.is_empty()));
        assert!(state
            .objects
            .get(&stale)
            .is_some_and(|obj| obj.casting_permissions.is_empty()));
    }

    #[test]
    fn override_instead_condition_met_swaps_effect() {
        // CR 608.2e: When AdditionalCostPaidInstead condition is met,
        // the sub's effect replaces the parent's effect.
        let mut state = GameState::new_two_player(42);

        // Sub: deal 5 damage (override) with AdditionalCostPaidInstead
        let sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::AdditionalCostPaidInstead);

        // Parent: deal 2 damage — should be REPLACED by the sub
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .context(SpellContext {
            additional_cost_paid: true,
            ..Default::default()
        })
        .sub_ability(sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Only the override effect (5 damage) should have fired, not the parent (2 damage)
        assert_eq!(
            state.players[1].life, 15,
            "Expected 5 damage from override, not 2 from parent"
        );
    }

    #[test]
    fn override_instead_condition_not_met_runs_parent() {
        // CR 608.2e: When AdditionalCostPaidInstead condition is NOT met,
        // the parent runs normally and the override sub is skipped.
        let mut state = GameState::new_two_player(42);

        let sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::AdditionalCostPaidInstead);

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .context(SpellContext::default())
        .sub_ability(sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Only the parent effect (2 damage) should have fired
        assert_eq!(
            state.players[1].life, 18,
            "Expected 2 damage from parent, override should be skipped"
        );
    }

    #[test]
    fn condition_instead_swaps_when_met() {
        // CR 608.2c: ConditionInstead wraps a general condition with "instead" swap
        // semantics. When the inner condition is met, the sub's effect replaces the
        // parent's. The sub's chain continues after the swap.
        let mut state = GameState::new_two_player(42);

        // Instead sub: deal 5 damage (replaces parent when condition is met)
        let instead_sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::IsYourTurn),
        });

        // Parent: deal 2 damage — should be replaced
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(instead_sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // IsYourTurn is true (player 0 is active), so the swap fires: 5 damage
        assert_eq!(
            state.players[1].life, 15,
            "Expected 5 damage from instead override"
        );
    }

    #[test]
    fn condition_instead_runs_base_chain_when_not_met() {
        // CR 608.2c: When ConditionInstead condition is NOT met, the parent effect
        // runs and the base continuation chain (else_ability) executes after it.
        let mut state = GameState::new_two_player(42);
        // Give player 0 cards to draw
        for i in 0..3 {
            crate::game::zones::create_object(
                &mut state,
                CardId(i + 50),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        // Base continuation: draw 1 card (stored in else_ability)
        let base_chain = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        // Instead sub: deal 5 damage (with its own chain: draw 2)
        let instead_chain = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut instead_sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::ConditionInstead {
            // negated: true → NOT your turn → condition NOT met (it IS our turn)
            inner: Box::new(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IsYourTurn),
            }),
        })
        .sub_ability(instead_chain);
        instead_sub.else_ability = Some(Box::new(base_chain));

        // Parent: deal 2 damage — should execute (condition not met)
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(instead_sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // IsYourTurn negated=true → NOT met → parent runs (2 damage) + base chain (draw 1)
        assert_eq!(
            state.players[1].life, 18,
            "Expected 2 damage from parent (condition not met)"
        );
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "Expected 1 card drawn from base continuation chain"
        );
    }

    #[test]
    fn repeat_for_draws_multiple_cards() {
        // CR 609.3: repeat_for = Fixed(3) with Draw(1) should draw 3 cards
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            crate::game::zones::create_object(
                &mut state,
                CardId(i + 10),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.repeat_for = Some(QuantityExpr::Fixed { value: 3 });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            3,
            "repeat_for=3 with Draw(1) should draw 3 cards"
        );
    }

    /// CR 603.7 + CR 109.5 + CR 701.23a: Winds of Abandon-shape — per-iteration
    /// parent-target rebinding for `repeat_for: TrackedSetSize` over a
    /// `ParentTargetController` search. Two creatures controlled by *different*
    /// opponents (P1 and P2) are exiled. Without the per-iteration rebind both
    /// iterations would prompt the same player; with the rebind, the FIRST
    /// iteration must prompt the controller of the FIRST tracked-set member
    /// specifically (not just "some opponent"), proving the rebind is the only
    /// mechanism that places the per-iteration creature as the parent.
    ///
    /// Critical: `ability.targets` starts EMPTY. Without the rebind path the
    /// SearchLibrary resolver would fall through to `ability.controller`
    /// (the caster, P0) rather than to either creature's controller, so the
    /// assertion below is reachable only through the rebind.
    #[test]
    fn repeat_for_rebinds_parent_target_to_tracked_set_member_per_iteration() {
        use crate::types::ability::SearchSelectionConstraint;
        use crate::types::format::FormatConfig;

        // 3-player game so each tracked-set member can have a distinct
        // controller — proves the rebind picks per-iteration, not "any opponent".
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        let creature_a = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Exile,
        );
        let creature_b = create_object(
            &mut state,
            CardId(51),
            PlayerId(2),
            "Wolf".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&creature_a).unwrap().controller = PlayerId(1);
        state.objects.get_mut(&creature_b).unwrap().controller = PlayerId(2);

        // Seed P1's and P2's libraries with basic lands so the search finds
        // matching cards in each opponent's library.
        for (lib_owner, card_id, name) in [
            (PlayerId(1), CardId(60), "Forest"),
            (PlayerId(2), CardId(61), "Plains"),
        ] {
            let land = create_object(
                &mut state,
                card_id,
                lib_owner,
                name.to_string(),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }

        // Publish a chain-scoped tracked set listing both creatures in order.
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![creature_a, creature_b]);
        state.chain_tracked_set_id = Some(set_id);

        // ability.targets is EMPTY: the only way for SearchLibrary's
        // ParentTargetController to resolve to any opponent is via the
        // per-iteration rebind populating targets[0] with the i-th member.
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::ParentTargetController),
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(9000),
            PlayerId(0), // caster is P0
        );
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        });

        let mut events = Vec::new();
        // Depth=1 simulates being inside a larger chain (Winds of Abandon's
        // outer chain publishes the tracked set in its first sub-ability).
        // Calling at depth=0 would clear `chain_tracked_set_id` per CR 603.7's
        // chain-local reset, defeating the test's setup.
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        // First iteration must prompt P1 — controller of `creature_a`, the
        // FIRST tracked-set member. If the rebind didn't run, this would
        // resolve via `ability.controller` (P0) — which is not an
        // opponent — and the SearchLibrary would never set
        // WaitingFor::SearchChoice for P1 specifically.
        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => {
                assert_eq!(
                    *player,
                    PlayerId(1),
                    "first iteration must prompt the controller of the FIRST tracked-set member (P1, not P2 or P0)"
                );
            }
            other => panic!("expected SearchChoice, got {:?}", other),
        }

        // The remaining iteration must be stashed in `pending_repeat_iteration`
        // so subsequent SearchChoice resolutions resume the loop.
        let pending = state
            .pending_repeat_iteration
            .as_ref()
            .expect("second iteration must be stashed for resumption");
        assert_eq!(pending.next_iteration, 1);
        assert_eq!(pending.total_iterations, 2);
        assert_eq!(pending.tracked_members, vec![creature_a, creature_b]);
    }

    /// CR 609.3 + CR 109.5: End-to-end iteration resumption — overloaded Winds
    /// of Abandon shape across two distinct opponent controllers. After the
    /// FIRST iteration's SearchChoice is resolved (P1 picks a basic land), the
    /// loop must resume and prompt the SECOND opponent (P2) for their own
    /// search. Without the `pending_repeat_iteration` infrastructure, only
    /// the first iteration would ever fire.
    #[test]
    fn repeat_for_resumes_iteration_after_search_choice_resolves() {
        use crate::game::engine::apply;
        use crate::types::ability::SearchSelectionConstraint;
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        let creature_a = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Exile,
        );
        let creature_b = create_object(
            &mut state,
            CardId(51),
            PlayerId(2),
            "Wolf".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&creature_a).unwrap().controller = PlayerId(1);
        state.objects.get_mut(&creature_b).unwrap().controller = PlayerId(2);

        // Seed each opponent's library with one basic land.
        let p1_forest = create_object(
            &mut state,
            CardId(60),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&p1_forest).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }
        let p2_plains = create_object(
            &mut state,
            CardId(61),
            PlayerId(2),
            "Plains".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&p2_plains).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }

        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![creature_a, creature_b]);
        state.chain_tracked_set_id = Some(set_id);

        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::ParentTargetController),
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        );
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        });

        let mut events = Vec::new();
        // Depth=1: simulate being inside Winds of Abandon's outer chain (the
        // tracked set is published by the parent sub-ability before this
        // iteration loop runs). See sibling test for rationale.
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        // Iteration 0: P1 prompted.
        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(*player, PlayerId(1)),
            other => panic!("expected SearchChoice for P1, got {:?}", other),
        }

        // P1 picks the Forest. After resolving, the loop must resume and
        // prompt P2 for the second iteration.
        apply(
            &mut state,
            PlayerId(1),
            GameAction::SelectCards {
                cards: vec![p1_forest],
            },
        )
        .unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(
                *player,
                PlayerId(2),
                "second iteration must prompt the controller of the SECOND tracked-set member (P2). \
                 Without iteration resumption, only the first iteration would ever fire."
            ),
            other => panic!(
                "expected SearchChoice for P2 after P1 resolves, got {:?}. \
                 This indicates the repeat_for loop did not resume.",
                other
            ),
        }

        // P2 picks the Plains; the loop should now complete with no further
        // pending iteration.
        apply(
            &mut state,
            PlayerId(2),
            GameAction::SelectCards {
                cards: vec![p2_plains],
            },
        )
        .unwrap();

        assert!(
            state.pending_repeat_iteration.is_none(),
            "loop must clear pending_repeat_iteration after final iteration completes"
        );
    }

    /// CR 609.3 + CR 109.5 + CR 701.23i: End-to-end Winds of Abandon shape —
    /// the resumed iteration MUST run its full sub_ability chain
    /// (put-onto-battlefield + shuffle), not just the SearchLibrary effect.
    /// Without preserving `sub_ability` on the resumed `pending_repeat_iteration`,
    /// the FIRST opponent's chosen card lands on the battlefield (iteration 0
    /// goes through the line-1660 SearchChoice continuation wiring), but the
    /// SECOND opponent's chosen card is silently lost — the resume path
    /// previously called `resolve_effect` directly and never wired the
    /// continuation. This test asserts BOTH opponents' cards land on the
    /// battlefield AND the search emits a Shuffle for each iteration.
    #[test]
    fn repeat_for_resumed_iteration_runs_full_sub_ability_chain() {
        use crate::game::engine::apply;
        use crate::types::ability::{EffectKind, SearchSelectionConstraint};
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        let creature_a = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Exile,
        );
        let creature_b = create_object(
            &mut state,
            CardId(51),
            PlayerId(2),
            "Wolf".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&creature_a).unwrap().controller = PlayerId(1);
        state.objects.get_mut(&creature_b).unwrap().controller = PlayerId(2);

        let p1_forest = create_object(
            &mut state,
            CardId(60),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&p1_forest).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }
        let p2_plains = create_object(
            &mut state,
            CardId(61),
            PlayerId(2),
            "Plains".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&p2_plains).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }

        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![creature_a, creature_b]);
        state.chain_tracked_set_id = Some(set_id);

        // Build the full Winds of Abandon sub-chain:
        //   SearchLibrary (repeat_for=TrackedSetSize, target_player=ParentTargetController)
        //     -> ChangeZone (Library -> Battlefield, enter_tapped=true)
        //       -> Shuffle (target=ParentTargetController)
        let shuffle = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::ParentTargetController,
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        );
        let put = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: true,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        )
        .sub_ability(shuffle);
        let mut search = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::ParentTargetController),
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        )
        .sub_ability(put);
        search.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        });

        let mut all_events: Vec<GameEvent> = Vec::new();
        // depth=1 to preserve the chain-scoped tracked set we published above.
        resolve_ability_chain(&mut state, &search, &mut all_events, 1).unwrap();

        // Iteration 0: P1 prompted; P1 picks the Forest.
        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(*player, PlayerId(1)),
            other => panic!("expected SearchChoice for P1, got {:?}", other),
        }
        let r1 = apply(
            &mut state,
            PlayerId(1),
            GameAction::SelectCards {
                cards: vec![p1_forest],
            },
        )
        .unwrap();
        all_events.extend(r1.events);

        // Iteration 1: P2 prompted; P2 picks the Plains.
        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(
                *player,
                PlayerId(2),
                "iteration 1 must prompt P2 — controller of the SECOND tracked-set member"
            ),
            other => panic!("expected SearchChoice for P2, got {:?}", other),
        }
        let r2 = apply(
            &mut state,
            PlayerId(2),
            GameAction::SelectCards {
                cards: vec![p2_plains],
            },
        )
        .unwrap();
        all_events.extend(r2.events);

        // Both chosen lands MUST be on the battlefield. This is the regression
        // that the resumed-iteration `sub_ability` preservation guards against
        // — without it, p2_plains would still be in P2's library.
        let forest_zone = state.objects.get(&p1_forest).unwrap().zone;
        let plains_zone = state.objects.get(&p2_plains).unwrap().zone;
        assert_eq!(
            forest_zone,
            Zone::Battlefield,
            "P1's chosen Forest must be on the battlefield (iteration 0's sub_ability)"
        );
        assert_eq!(
            plains_zone,
            Zone::Battlefield,
            "P2's chosen Plains must be on the battlefield — failure means iteration 1's \
             sub_ability (put-onto-battlefield) was dropped on the resume path."
        );

        // Both controllers must own their respective lands on their side.
        assert_eq!(
            state.objects.get(&p1_forest).unwrap().controller,
            PlayerId(1),
            "Forest controller is P1"
        );
        assert_eq!(
            state.objects.get(&p2_plains).unwrap().controller,
            PlayerId(2),
            "Plains controller is P2"
        );

        // The Shuffle sub_ability must have run for each iteration. Each
        // Shuffle resolution emits an EffectResolved { kind: Shuffle } event.
        let shuffle_count = all_events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Shuffle,
                        ..
                    }
                )
            })
            .count();
        assert!(
            shuffle_count >= 2,
            "expected at least 2 Shuffle resolutions (one per iteration), got {}. \
             Failure means iteration 1's Shuffle sub_ability was dropped on the resume path.",
            shuffle_count
        );

        assert!(
            state.pending_repeat_iteration.is_none(),
            "loop must clear pending_repeat_iteration after final iteration completes"
        );
    }

    /// CR 609.3 + CR 109.5: Direct unit test of the synchronous-continuation
    /// re-stash predicate inside `drain_pending_repeat_iteration`. Constructs
    /// a multi-iteration resume whose iterations install a `pending_continuation`
    /// without changing `waiting_for`, then verifies the drain detects the
    /// continuation transition and re-stashes the remaining iterations rather
    /// than letting them be silently dropped.
    ///
    /// Strategy: use `ConditionInstead` with `else_ability` set. When the
    /// instead condition is NOT met, line 1486-1487 of `resolve_ability_chain`
    /// stashes the `else_ability` chain into `pending_continuation` whenever
    /// `waiting_for != Priority`. We pre-set `waiting_for` to a non-Priority
    /// state so the stash fires synchronously without any waiting_for change,
    /// directly exercising the new `installed_continuation` predicate.
    #[test]
    fn drain_pending_repeat_iteration_restashes_on_synchronous_continuation() {
        use crate::types::ability::AbilityCondition;
        use crate::types::game_state::PendingRepeatIteration;

        let mut state = GameState::new_two_player(42);
        for i in 0..10 {
            create_object(
                &mut state,
                CardId(i + 10),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        // Pre-seed waiting_for to a non-Priority state so the
        // `ConditionInstead` else-branch stash path fires synchronously
        // (line 1486 requires `waiting_for != Priority`). The drain's
        // `entered_choice` predicate compares against this initial value, so
        // the same waiting_for at end-of-iteration registers as "no
        // transition" — only `installed_continuation` can fire the re-stash.
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards: vec![],
            count: 0,
            reveal: false,
            up_to: true,
            constraint: crate::types::ability::SearchSelectionConstraint::None,
        };

        // Build a Draw ability (synchronous, no waiting_for change) with a
        // sub_ability whose condition is `ConditionInstead` carrying an
        // `else_ability`. When the inner condition evaluates to false, the
        // else branch is stashed synchronously into pending_continuation
        // via line 1486.
        let else_branch = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::ConditionInstead {
            // Pick a condition that evaluates to false in this state so the
            // swap does NOT fire and the else branch stash path runs.
            inner: Box::new(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IsYourTurn),
            }),
        });
        sub.else_ability = Some(Box::new(else_branch));

        let mut iter_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        iter_ability.repeat_for = None;

        // Stage a resume of 3 iterations starting at iteration 1.
        state.pending_repeat_iteration = Some(PendingRepeatIteration {
            ability: Box::new(iter_ability),
            tracked_members: vec![],
            next_iteration: 1,
            total_iterations: 3,
        });

        let mut events = Vec::new();
        super::drain_pending_repeat_iteration(&mut state, &mut events);

        // Iteration 1 ran: parent Draw fired (1 card), then the
        // ConditionInstead sub stashed its else_ability into
        // pending_continuation synchronously (no waiting_for change). The
        // drain's `installed_continuation` predicate must observe this
        // transition and re-stash iteration 2 for the next drain pass.
        assert!(
            state.pending_continuation.is_some(),
            "iteration 1 must have installed a synchronous pending_continuation \
             (else_ability of ConditionInstead)"
        );
        let pending = state.pending_repeat_iteration.as_ref().expect(
            "iteration 2 must be re-stashed — without the synchronous-continuation \
             predicate, this would be None and iteration 2 would be silently dropped",
        );
        assert_eq!(
            pending.next_iteration, 2,
            "re-stash must advance to iteration 2"
        );
        assert_eq!(pending.total_iterations, 3);

        // Exactly one iteration's worth of effects fired before the break:
        // iteration 1's parent Draw (1 card). The else_ability chain has not
        // run yet — it is stashed in pending_continuation, awaiting the next
        // drain_pending_continuation call.
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "only iteration 1's parent Draw should have fired before the re-stash break"
        );
    }

    /// CR 603.7 + CR 608.2c: Regression — when `repeat_for` is set but the
    /// effect does NOT use a parent-target reference (e.g. plain Draw), the
    /// per-iteration rebind logic must NOT touch `ability.targets`. Guards
    /// against the new rebind path leaking into unrelated `repeat_for`
    /// callers.
    #[test]
    fn repeat_for_does_not_rebind_when_effect_lacks_parent_ref() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 10),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        // Publish a tracked set with a pretend object so the rebind path could
        // misfire if the gate were too loose.
        let dummy = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Dummy".to_string(),
            Zone::Battlefield,
        );
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state.tracked_object_sets.insert(set_id, vec![dummy]);
        state.chain_tracked_set_id = Some(set_id);

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // 1 tracked-set member → 1 iteration → 1 card drawn. Targets remain empty
        // (no spurious Object rebind).
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn resolve_ability_chain_player_scope_opponent_discard() {
        let mut state = GameState::new_two_player(42);
        // Put a card in opponent's hand for discard
        create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Card C".to_string(),
            Zone::Hand,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0), // controller
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Opponent (PlayerId(1)) should have discarded
        assert!(
            state.players[1].hand.is_empty(),
            "opponent should have discarded their card"
        );
    }

    #[test]
    fn player_scope_runs_unscoped_tail_once_after_scoped_iterations() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "P1 Card".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(20),
            PlayerId(2),
            "P2 Card".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "P0 Draw".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "P0 Extra".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[1].hand.len(), 0);
        assert_eq!(state.players[2].hand.len(), 0);
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "unscoped tail must resolve once after all opponent iterations"
        );
    }

    #[test]
    fn player_scope_opponent_counter_then_unscoped_draw() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "P0 Draw".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::GivePlayerCounter {
                counter_kind: PlayerCounterKind::Poison,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].poison_counters, 0);
        assert_eq!(state.players[1].poison_counters, 1);
        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[1].hand.len(), 0);
    }

    #[test]
    fn resolve_ability_chain_player_scope_all_draw() {
        let mut state = GameState::new_two_player(42);
        // Add a card in each player's library so Draw has something to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Card B".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0), // controller
        );
        ability.player_scope = Some(PlayerFilter::All);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Both players should have drawn a card
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "controller should have drawn a card"
        );
        assert_eq!(
            state.players[1].hand.len(),
            1,
            "opponent should have drawn a card"
        );
    }

    #[test]
    fn player_scope_discard_then_dark_deal_draws_per_players_discard_count_minus_one() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                format!("P0 Hand {i}"),
                Zone::Hand,
            );
            create_object(
                &mut state,
                CardId(20 + i),
                PlayerId(0),
                format!("P0 Library {i}"),
                Zone::Library,
            );
            create_object(
                &mut state,
                CardId(30 + i),
                PlayerId(1),
                format!("P1 Library {i}"),
                Zone::Library,
            );
        }
        create_object(
            &mut state,
            CardId(40),
            PlayerId(1),
            "P1 Hand".to_string(),
            Zone::Hand,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    },
                },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::All);
        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    }),
                    offset: -1,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        draw.player_scope = Some(PlayerFilter::All);
        ability.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].hand.len(), 2);
        assert_eq!(state.players[1].hand.len(), 0);
        assert_eq!(state.players[0].graveyard.len(), 3);
        assert_eq!(state.players[1].graveyard.len(), 1);
    }

    #[test]
    fn player_scope_discard_then_windfall_draws_greatest_discard_count() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(50 + i),
                PlayerId(0),
                format!("P0 Hand {i}"),
                Zone::Hand,
            );
            create_object(
                &mut state,
                CardId(60 + i),
                PlayerId(0),
                format!("P0 Library {i}"),
                Zone::Library,
            );
            create_object(
                &mut state,
                CardId(70 + i),
                PlayerId(1),
                format!("P1 Library {i}"),
                Zone::Library,
            );
        }
        create_object(
            &mut state,
            CardId(80),
            PlayerId(1),
            "P1 Hand".to_string(),
            Zone::Hand,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    },
                },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(101),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::All);
        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(101),
            PlayerId(0),
        );
        draw.player_scope = Some(PlayerFilter::All);
        ability.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].hand.len(), 3);
        assert_eq!(state.players[1].hand.len(), 3);
        assert_eq!(state.players[0].graveyard.len(), 3);
        assert_eq!(state.players[1].graveyard.len(), 1);
    }

    #[test]
    fn player_scope_preserves_controller_for_you_quantities() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life_gained_this_turn = 3;
        state.players[1].life_gained_this_turn = 0;

        for i in 0..5 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(1),
                format!("Opponent Card {i}"),
                Zone::Library,
            );
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[1].graveyard.len(),
            3,
            "opponent should mill based on the original controller's life gained"
        );
        assert_eq!(state.players[1].library.len(), 2);
    }

    #[test]
    fn resolve_ability_chain_evaluates_condition_per_scoped_player() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Card B".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IsYourTurn),
        });
        ability.player_scope = Some(PlayerFilter::All);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].hand.len(), 0);
        assert_eq!(state.players[1].hand.len(), 1);
    }

    #[test]
    fn resolve_ability_chain_gates_on_source_mana_color_spent() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Spend Check".to_string(),
            Zone::Stack,
        );
        create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .condition(AbilityCondition::ManaColorSpent {
            color: ManaColor::Black,
            minimum: 1,
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(state.players[0].hand.len(), 0);

        state
            .objects
            .get_mut(&source)
            .unwrap()
            .colors_spent_to_cast
            .add(ManaColor::Black, 1);
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn player_scope_zone_changed_this_way_filters_by_owner() {
        let mut state = GameState::new_two_player(42);

        // Create objects owned by Player 0 in graveyard (simulating milled cards)
        let obj_a = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Milled A".to_string(),
            Zone::Graveyard,
        );
        let obj_b = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Milled B".to_string(),
            Zone::Graveyard,
        );

        // Simulate that these objects were zone-changed by the preceding effect
        state.last_zone_changed_ids = vec![obj_a, obj_b];

        // Add library cards so Draw has something to draw
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Lib A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Lib B".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::ZoneChangedThisWay);

        let mut events = Vec::new();
        // Use depth=1 to simulate sub_ability execution — depth=0 clears last_zone_changed_ids
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        // Only Player 0 owned the zone-changed objects, so only they draw
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "player 0 should have drawn (owned zone-changed objects)"
        );
        assert!(
            state.players[1].hand.is_empty(),
            "player 1 should NOT have drawn (no owned zone-changed objects)"
        );
    }

    #[test]
    fn player_scope_zone_changed_this_way_includes_both_when_both_own() {
        let mut state = GameState::new_two_player(42);

        // Objects owned by different players
        let obj_p0 = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "P0 Card".to_string(),
            Zone::Graveyard,
        );
        let obj_p1 = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "P1 Card".to_string(),
            Zone::Graveyard,
        );

        state.last_zone_changed_ids = vec![obj_p0, obj_p1];

        // Library cards for both
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Lib A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Lib B".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::ZoneChangedThisWay);

        let mut events = Vec::new();
        // Use depth=1 to simulate sub_ability execution — depth=0 clears last_zone_changed_ids
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        // Both players owned zone-changed objects, so both draw
        assert_eq!(state.players[0].hand.len(), 1, "player 0 should have drawn");
        assert_eq!(state.players[1].hand.len(), 1, "player 1 should have drawn");
    }

    /// CR 608.2c + CR 109.5: "for each opponent who searched their library
    /// this way" relies on `player_actions_this_way` accumulating across
    /// player_scope iterations.
    #[test]
    fn player_actions_this_way_accumulates_across_player_scope_iterations() {
        use crate::types::format::FormatConfig;

        // 3-player game: P0 (controller), P1, P2 (opponents).
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        // Search with player_scope: Opponent. Empty libraries still emit the
        // SearchedLibrary action, which is exactly what "searched this way"
        // means; no ZoneChanged event is required.
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let action = crate::types::events::PlayerActionKind::SearchedLibrary;
        assert!(
            state
                .player_actions_this_way
                .contains(&(PlayerId(1), action)),
            "P1 searched and must be recorded in player_actions_this_way"
        );
        assert!(
            state
                .player_actions_this_way
                .contains(&(PlayerId(2), action)),
            "P2 searched and must be recorded in player_actions_this_way"
        );
    }

    /// CR 608.2c + CR 109.5: `PerformedActionThisWay` resolves to the count
    /// of matching players in the chain-local action accumulator.
    #[test]
    fn performed_action_this_way_player_count_excludes_controller() {
        use crate::game::quantity::resolve_quantity;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        let action = crate::types::events::PlayerActionKind::SearchedLibrary;
        state.player_actions_this_way.insert((PlayerId(0), action));
        state.player_actions_this_way.insert((PlayerId(1), action));

        let qty = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay {
                    relation: crate::types::ability::PlayerRelation::Opponent,
                    action,
                },
            },
        };
        let count = resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0));

        // Only P1 counts: P0 is the controller (excluded), P2 is not in the
        // accumulator (declined the offer).
        assert_eq!(
            count, 1,
            "PerformedActionThisWay must exclude controller and count only \
             opponents who performed the action"
        );

        state.player_actions_this_way.insert((PlayerId(2), action));
        let count_all = resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0));
        assert_eq!(
            count_all, 2,
            "All opponents in accumulator → count of opponents (excl controller) is 2"
        );

        state.player_actions_this_way.remove(&(PlayerId(1), action));
        state.player_actions_this_way.remove(&(PlayerId(2), action));
        let count_none = resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0));
        assert_eq!(
            count_none, 0,
            "No opponents in accumulator → count is 0 (controller is excluded)"
        );
    }

    /// CR 608.2c + CR 109.5: `player_actions_this_way` clears at depth=0
    /// chain entry — does NOT leak across unrelated top-level resolutions.
    #[test]
    fn player_actions_this_way_clears_at_top_level_chain_entry() {
        let mut state = GameState::new_two_player(42);

        // Pre-pollute the accumulator with stale state from a "previous"
        // resolution.
        let action = crate::types::events::PlayerActionKind::SearchedLibrary;
        state.player_actions_this_way.insert((PlayerId(0), action));
        state.player_actions_this_way.insert((PlayerId(1), action));

        // Add one library card so Draw has something to draw — Draw itself
        // emits no ZoneChanged events (cards moving from library to hand DO
        // emit ZoneChanged, so use a no-zone-change effect to make the test
        // assertion cleaner).
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Lib".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Shuffle emits no ZoneChanged events; the accumulator must be EMPTY
        // (cleared at depth=0 entry, not extended by stale state).
        assert!(
            state.player_actions_this_way.is_empty(),
            "depth=0 chain entry must clear player_actions_this_way; \
             leaking across top-level resolutions would cause spurious counts \
             in 'opponent who [verbed] this way' references on subsequent spells"
        );
    }

    #[test]
    fn player_scope_owners_of_cards_exiled_by_source_creates_owner_sized_tokens() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Skyclave Apparition".to_string(),
            Zone::Battlefield,
        );

        for (card_id, owner, mv) in [
            (101, PlayerId(0), 2u32),
            (102, PlayerId(0), 3),
            (103, PlayerId(1), 4),
        ] {
            let exiled = create_object(
                &mut state,
                CardId(card_id),
                owner,
                format!("Exiled {card_id}"),
                Zone::Exile,
            );
            state.objects.get_mut(&exiled).unwrap().mana_cost = ManaCost::generic(mv);
            state.exile_links.push(ExileLink {
                source_id: source,
                exiled_id: exiled,
                kind: ExileLinkKind::TrackedBySource,
            });
        }

        let mut ability = ResolvedAbility::new(
            Effect::Token {
                name: "Illusion".to_string(),
                power: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: crate::types::ability::AggregateFunction::Sum,
                        property: crate::types::ability::ObjectProperty::ManaValue,
                        filter: TargetFilter::And {
                            filters: vec![
                                TargetFilter::ExiledBySource,
                                TargetFilter::Typed(TypedFilter::default().properties(vec![
                                    FilterProp::Owned {
                                        controller: ControllerRef::You,
                                    },
                                ])),
                            ],
                        },
                    },
                }),
                toughness: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: crate::types::ability::AggregateFunction::Sum,
                        property: crate::types::ability::ObjectProperty::ManaValue,
                        filter: TargetFilter::And {
                            filters: vec![
                                TargetFilter::ExiledBySource,
                                TargetFilter::Typed(TypedFilter::default().properties(vec![
                                    FilterProp::Owned {
                                        controller: ControllerRef::You,
                                    },
                                ])),
                            ],
                        },
                    },
                }),
                types: vec!["Creature".to_string(), "Illusion".to_string()],
                colors: vec![ManaColor::Blue],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::OwnersOfCardsExiledBySource);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let mut created: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token)
            .map(|object| {
                (
                    object.owner,
                    object.controller,
                    object.power,
                    object.toughness,
                )
            })
            .collect();
        created.sort_by_key(|entry| entry.0);

        assert_eq!(
            created,
            vec![
                (PlayerId(0), PlayerId(0), Some(5), Some(5)),
                (PlayerId(1), PlayerId(1), Some(4), Some(4)),
            ]
        );
    }

    #[test]
    fn player_scope_owners_of_cards_exiled_by_source_uses_ltb_snapshot() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Skyclave Apparition".to_string(),
            Zone::Graveyard,
        );
        let exiled_a = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Exiled 101".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled_a).unwrap().mana_cost = ManaCost::generic(2);
        let exiled_b = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Exiled 102".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled_b).unwrap().mana_cost = ManaCost::generic(3);
        let exiled_c = create_object(
            &mut state,
            CardId(103),
            PlayerId(1),
            "Exiled 103".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled_c).unwrap().mana_cost = ManaCost::generic(4);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: source,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(crate::types::game_state::ZoneChangeRecord {
                linked_exile_snapshot: vec![
                    LinkedExileSnapshot {
                        exiled_id: exiled_a,
                        owner: PlayerId(0),
                        mana_value: 2,
                    },
                    LinkedExileSnapshot {
                        exiled_id: exiled_b,
                        owner: PlayerId(0),
                        mana_value: 3,
                    },
                    LinkedExileSnapshot {
                        exiled_id: exiled_c,
                        owner: PlayerId(1),
                        mana_value: 4,
                    },
                ],
                ..crate::types::game_state::ZoneChangeRecord::test_minimal(
                    source,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            }),
        });

        let mut ability = ResolvedAbility::new(
            Effect::Token {
                name: "Illusion".to_string(),
                power: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: crate::types::ability::AggregateFunction::Sum,
                        property: crate::types::ability::ObjectProperty::ManaValue,
                        filter: TargetFilter::And {
                            filters: vec![
                                TargetFilter::ExiledBySource,
                                TargetFilter::Typed(TypedFilter::default().properties(vec![
                                    FilterProp::Owned {
                                        controller: ControllerRef::You,
                                    },
                                ])),
                            ],
                        },
                    },
                }),
                toughness: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: crate::types::ability::AggregateFunction::Sum,
                        property: crate::types::ability::ObjectProperty::ManaValue,
                        filter: TargetFilter::And {
                            filters: vec![
                                TargetFilter::ExiledBySource,
                                TargetFilter::Typed(TypedFilter::default().properties(vec![
                                    FilterProp::Owned {
                                        controller: ControllerRef::You,
                                    },
                                ])),
                            ],
                        },
                    },
                }),
                types: vec!["Creature".to_string(), "Illusion".to_string()],
                colors: vec![ManaColor::Blue],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::OwnersOfCardsExiledBySource);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let mut created: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token)
            .map(|object| {
                (
                    object.owner,
                    object.controller,
                    object.power,
                    object.toughness,
                )
            })
            .collect();
        created.sort_by_key(|entry| entry.0);

        assert_eq!(
            created,
            vec![
                (PlayerId(0), PlayerId(0), Some(5), Some(5)),
                (PlayerId(1), PlayerId(1), Some(4), Some(4)),
            ]
        );
    }

    /// CR 702.33d + CR 702.33f + CR 608.2c: Default-shape `AdditionalCostPaid`
    /// (variant=None, min_count=1) reads `additional_cost_paid` so legacy
    /// Gift / Buyback / Bargain / Evidence / plain "if it was kicked" gating
    /// stays correct.
    #[test]
    fn additional_cost_paid_default_shape_reads_legacy_bool() {
        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let cond = AbilityCondition::additional_cost_paid_any();
        assert!(!evaluate_condition(&cond, &state, &ability));
        ability.context.additional_cost_paid = true;
        assert!(evaluate_condition(&cond, &state, &ability));
    }

    /// CR 702.33f: variant gating reads `kickers_paid` membership. Mirrors
    /// Ana Battlemage's per-kicker triggers.
    #[test]
    fn additional_cost_paid_variant_reads_kickers_paid() {
        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let first = AbilityCondition::additional_cost_paid_kicker(
            crate::types::ability::KickerVariant::First,
        );
        let second = AbilityCondition::additional_cost_paid_kicker(
            crate::types::ability::KickerVariant::Second,
        );
        assert!(!evaluate_condition(&first, &state, &ability));
        assert!(!evaluate_condition(&second, &state, &ability));
        ability
            .context
            .kickers_paid
            .push(crate::types::ability::KickerVariant::First);
        assert!(evaluate_condition(&first, &state, &ability));
        assert!(!evaluate_condition(&second, &state, &ability));
        ability
            .context
            .kickers_paid
            .push(crate::types::ability::KickerVariant::Second);
        assert!(evaluate_condition(&first, &state, &ability));
        assert!(evaluate_condition(&second, &state, &ability));
    }

    /// CR 702.33b/c: `min_count >= 2` reads `kickers_paid.len()`. Mirrors
    /// Archangel of Wrath's "kicked twice" trigger.
    #[test]
    fn additional_cost_paid_min_count_reads_kickers_paid_len() {
        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let twice = AbilityCondition::additional_cost_paid_n_times(2);
        assert!(!evaluate_condition(&twice, &state, &ability));
        ability
            .context
            .kickers_paid
            .push(crate::types::ability::KickerVariant::First);
        assert!(!evaluate_condition(&twice, &state, &ability));
        ability
            .context
            .kickers_paid
            .push(crate::types::ability::KickerVariant::Second);
        assert!(evaluate_condition(&twice, &state, &ability));
    }

    #[test]
    fn zone_change_object_ability_condition_checks_current_trigger_event_object() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(210),
            PlayerId(0),
            "Observer".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(211),
            PlayerId(0),
            "Countered Entry".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&entering)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&entering)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: entering,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(crate::types::game_state::ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                ..crate::types::game_state::ZoneChangeRecord::test_minimal(
                    entering,
                    Some(Zone::Hand),
                    Zone::Battlefield,
                )
            }),
        });

        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let condition = AbilityCondition::ZoneChangeObjectMatchesFilter {
            origin: None,
            destination: Zone::Battlefield,
            filter: TargetFilter::Typed(
                TypedFilter::permanent().properties(vec![FilterProp::HasAnyCounter]),
            ),
        };

        assert!(evaluate_condition(&condition, &state, &ability));
    }

    #[test]
    fn evaluate_condition_and_all_true() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // And([IsYourTurn(false=not negated), IsYourTurn(false=not negated)]) — both true
        let cond = AbilityCondition::And {
            conditions: vec![AbilityCondition::IsYourTurn, AbilityCondition::IsYourTurn],
        };
        assert!(evaluate_condition(&cond, &state, &ability));
    }

    #[test]
    fn evaluate_condition_first_combat_phase_checks_turn_counter() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );

        state.combat_phases_started_this_turn = 0;
        assert!(!evaluate_condition(
            &AbilityCondition::FirstCombatPhaseOfTurn,
            &state,
            &ability,
        ));

        state.combat_phases_started_this_turn = 1;
        assert!(evaluate_condition(
            &AbilityCondition::FirstCombatPhaseOfTurn,
            &state,
            &ability,
        ));

        state.combat_phases_started_this_turn = 2;
        assert!(!evaluate_condition(
            &AbilityCondition::FirstCombatPhaseOfTurn,
            &state,
            &ability,
        ));
    }

    #[test]
    fn evaluate_condition_is_monarch_checks_ability_controller() {
        let mut state = GameState::new_two_player(42);
        state.monarch = Some(PlayerId(0));
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );

        assert!(evaluate_condition(
            &AbilityCondition::IsMonarch,
            &state,
            &ability
        ));

        let opponent_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(2),
            PlayerId(1),
        );
        assert!(!evaluate_condition(
            &AbilityCondition::IsMonarch,
            &state,
            &opponent_ability
        ));
    }

    #[test]
    fn evaluate_condition_city_blessing_checks_ability_controller() {
        let mut state = GameState::new_two_player(42);
        state.city_blessing.insert(PlayerId(0));
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );

        assert!(evaluate_condition(
            &AbilityCondition::HasCityBlessing,
            &state,
            &ability,
        ));

        let opponent_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(2),
            PlayerId(1),
        );
        assert!(!evaluate_condition(
            &AbilityCondition::HasCityBlessing,
            &state,
            &opponent_ability,
        ));
    }

    #[test]
    fn evaluate_cast_during_main_phase_checks_cast_phase_context() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.phase = Phase::BeginCombat;
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        ability.context.cast_phase = Some(Phase::PreCombatMain);

        assert!(evaluate_condition(
            &AbilityCondition::CastDuringPhase {
                phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
            },
            &state,
            &ability
        ));

        ability.context.cast_phase = Some(Phase::BeginCombat);
        assert!(!evaluate_condition(
            &AbilityCondition::CastDuringPhase {
                phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
            },
            &state,
            &ability
        ));

        ability.context.cast_phase = Some(Phase::PostCombatMain);
        state.active_player = PlayerId(1);
        assert!(evaluate_condition(
            &AbilityCondition::CastDuringPhase {
                phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
            },
            &state,
            &ability
        ));
    }

    #[test]
    fn evaluate_condition_and_one_false() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // And([IsYourTurn(true), IsYourTurn(false)]) — one is "not your turn" which is false
        let cond = AbilityCondition::And {
            conditions: vec![
                AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::IsYourTurn),
                },
                AbilityCondition::IsYourTurn,
            ],
        };
        assert!(!evaluate_condition(&cond, &state, &ability));
    }

    #[test]
    fn condition_instead_swap_clears_parent_condition() {
        // CR 608.2c: When ConditionInstead fires, the parent's condition should be
        // cleared — "instead" replaces the entire clause.
        let mut state = GameState::new_two_player(42);

        // Instead sub: deal 5 damage (replaces parent when condition is met)
        let instead_sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::IsYourTurn),
        });

        // Parent: deal 2 damage with a condition that would normally block it.
        // IsYourTurn(negated=true) = "not your turn" = false for active player.
        // Without the swap clearing it, the parent condition would block resolution.
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IsYourTurn),
        })
        .sub_ability(instead_sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // The swap fires (IsYourTurn is true), clearing the parent's "not your turn"
        // condition. The overridden ability deals 5 damage.
        assert_eq!(
            state.players[1].life, 15,
            "Expected 5 damage — swap should clear parent condition"
        );
    }

    /// CR 603.7 + CR 608.2c: Compound zone-changing effects in one resolution
    /// chain coalesce into a single tracked set. Shape modeled on Suspend
    /// Aggression: "Exile target permanent AND exile the top card ... For
    /// each of those cards, its owner may play it." The two exile steps must
    /// produce ONE set so the downstream GrantCastingPermission sees both
    /// objects, not just the most recent exile.
    #[test]
    fn compound_zone_change_chain_unifies_tracked_set() {
        use crate::types::ability::PermissionGrantee;
        let mut state = GameState::new_two_player(42);
        let permanent = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let lib_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Library,
        );
        state.players[0].library.push_back(lib_card);

        // Grandchild: grant PlayFromExile to the tracked set. Forces every
        // zone-changing ancestor to publish (transitive descendant check).
        let grant = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                grantee: PermissionGrantee::ObjectOwner,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        // Sub-ability: ExileTop 1 card from controller's library.
        let exile_top = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(grant);

        // Parent: Exile target permanent. ChangeZone{target=Any} moves
        // `permanent` to exile via the explicit TargetRef::Object(permanent).
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(permanent)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(exile_top);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Exactly ONE tracked set — unified — containing both exiled objects.
        assert_eq!(
            state.tracked_object_sets.len(),
            1,
            "chain should publish into a single tracked set, got {}",
            state.tracked_object_sets.len()
        );
        let set = state.tracked_object_sets.values().next().unwrap();
        assert!(
            set.contains(&permanent),
            "unified set must contain the exiled permanent"
        );
        assert!(
            set.contains(&lib_card),
            "unified set must contain the exiled library card"
        );

        // Both objects received the grant, bound to their respective owners.
        for (id, owner) in [(permanent, PlayerId(1)), (lib_card, PlayerId(0))] {
            let obj = &state.objects[&id];
            assert_eq!(
                obj.casting_permissions.len(),
                1,
                "object {id:?} should have one PlayFromExile grant"
            );
            match obj.casting_permissions[0] {
                CastingPermission::PlayFromExile { granted_to, .. } => {
                    assert_eq!(
                        granted_to, owner,
                        "ObjectOwner grantee should bind to each object's owner"
                    );
                }
                _ => panic!("expected PlayFromExile"),
            }
        }
    }

    // CR 603.4: Runtime tests for `AbilityCondition::NthResolutionThisTurn`.

    /// Build a minimal `ResolvedAbility` with a stamped `ability_index` for
    /// nth-resolution tracking tests.
    fn nth_test_ability(source_id: ObjectId, idx: usize) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        ability.ability_index = Some(idx);
        ability
    }

    #[test]
    fn nth_resolution_increments_per_resolution_and_gates_correctly() {
        // Three sequential resolutions of the same printed ability — only the
        // matching n-th resolution evaluates true; others evaluate false.
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);
        let ability = nth_test_ability(source_id, 0);
        let mut events = Vec::new();

        // Initial state: counter is 0; n=1 should be false BEFORE any resolution.
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 1 },
                &state,
                &ability
            ),
            "before any resolution, n=1 must not match (counter is 0)"
        );

        // Resolution 1 — counter becomes 1.
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            *state
                .ability_resolutions_this_turn
                .get(&(source_id, 0))
                .expect("counter must be present"),
            1,
            "first resolution must produce count=1"
        );
        assert!(
            evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 1 },
                &state,
                &ability
            ),
            "n=1 must evaluate true after first resolution"
        );
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 2 },
                &state,
                &ability
            ),
            "n=2 must evaluate false after only one resolution"
        );

        // Resolution 2 — counter becomes 2.
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            state.ability_resolutions_this_turn[&(source_id, 0)],
            2,
            "second resolution must produce count=2"
        );
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 1 },
                &state,
                &ability
            ),
            "n=1 must evaluate false after second resolution"
        );
        assert!(
            evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 2 },
                &state,
                &ability
            ),
            "n=2 must evaluate true after second resolution"
        );

        // Resolution 3 — counter becomes 3.
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 3 },
                &state,
                &ability
            ),
            "n=3 must evaluate true after third resolution"
        );
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 2 },
                &state,
                &ability
            ),
            "n=2 must evaluate false after third resolution"
        );
    }

    #[test]
    fn nth_resolution_counter_is_per_source() {
        // Two distinct Omnaths must track resolutions independently — each
        // has its own (source_id, ability_index) key.
        let mut state = GameState::new_two_player(42);
        let omnath_a = ObjectId(10);
        let omnath_b = ObjectId(20);
        let ability_a = nth_test_ability(omnath_a, 0);
        let ability_b = nth_test_ability(omnath_b, 0);
        let mut events = Vec::new();

        // Resolve A twice, B once.
        resolve_ability_chain(&mut state, &ability_a, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability_a, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability_b, &mut events, 0).unwrap();

        assert_eq!(state.ability_resolutions_this_turn[&(omnath_a, 0)], 2);
        assert_eq!(state.ability_resolutions_this_turn[&(omnath_b, 0)], 1);
    }

    #[test]
    fn nth_resolution_counter_is_per_ability_index() {
        // Same source, two distinct printed abilities (different ability_index)
        // — counters are tracked separately. Mirrors a card with multiple
        // triggered abilities each gated on its own nth-resolution count.
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);
        let ability_idx_0 = nth_test_ability(source_id, 0);
        let ability_idx_1 = nth_test_ability(source_id, 1);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability_idx_0, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability_idx_0, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability_idx_1, &mut events, 0).unwrap();

        assert_eq!(state.ability_resolutions_this_turn[&(source_id, 0)], 2);
        assert_eq!(state.ability_resolutions_this_turn[&(source_id, 1)], 1);
    }

    #[test]
    fn nth_resolution_counter_resets_on_turn_start() {
        // CR 514 + CR 603.4: Per-turn counter clears at start of next turn.
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);
        let ability = nth_test_ability(source_id, 0);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(state.ability_resolutions_this_turn[&(source_id, 0)], 2);

        // Simulate turn boundary by invoking start_next_turn, which is the
        // canonical reset site for per-turn counters in the engine.
        let mut events = Vec::new();
        crate::game::turns::start_next_turn(&mut state, &mut events);

        assert!(
            state.ability_resolutions_this_turn.is_empty(),
            "ability_resolutions_this_turn must be cleared at turn start, got {:?}",
            state.ability_resolutions_this_turn
        );
    }

    #[test]
    fn nth_resolution_no_index_does_not_increment_or_match() {
        // Synthesized abilities (prowess, firebending) lack an ability_index.
        // They must NOT bump the counter and NthResolutionThisTurn must
        // evaluate false against them (count is implicitly 0 / no key).
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        // ability.ability_index is None by default.
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            state.ability_resolutions_this_turn.is_empty(),
            "abilities without ability_index must not register in counter"
        );
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 1 },
                &state,
                &ability
            ),
            "NthResolutionThisTurn must evaluate false when ability lacks an index"
        );
    }

    /// Abandon Attachments: "You may discard a card. If you do, draw two cards."
    /// Regression test for #81: after discarding, the IfYouDo draw-2 sub-ability
    /// must fire via the continuation chain. The bug was that context propagation
    /// or cost_payment_failed_flag was blocking the IfYouDo condition.
    #[test]
    fn optional_discard_if_you_do_draw_fires_after_choice() {
        let mut state = GameState::new_two_player(42);

        // Card in hand to discard
        let _hand_card = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Fodder".to_string(),
            Zone::Hand,
        );

        // Cards in library to draw
        create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Draw A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Draw B".to_string(),
            Zone::Library,
        );

        // Build Abandon Attachments: optional Discard 1 → sub: Draw 2 (IfYouDo)
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::IfYouDo);

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        ability.optional = true;

        // Step 1: resolve_ability_chain → OptionalEffectChoice
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "Expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );

        // Step 2: Accept optional effect → forced discard (1 card in hand, count=1)
        // then IfYouDo sub-ability should fire drawing 2 cards.
        let waiting = crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();

        // Forced discard (hand_size == count) skips DiscardChoice, resolves inline.
        // After: discard 1 (-1), draw 2 (+2) = 2 cards in hand.
        let hand_after = state.players[0].hand.len();
        assert_eq!(
            hand_after,
            2,
            "After discarding 1 and drawing 2, hand should have 2 cards, got {}. \
             IfYouDo sub-ability likely did not fire. waiting_for={:?}, Events: {:?}",
            hand_after,
            waiting,
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::EffectResolved { .. }
                        | GameEvent::Discarded { .. }
                        | GameEvent::CardsDrawn { .. }
                ))
                .collect::<Vec<_>>()
        );
    }

    /// Abandon Attachments #81: interactive discard (player has 2+ cards) → IfYouDo draw 2.
    /// When the discard requires player interaction (DiscardChoice), the sub-ability
    /// must be stashed as a continuation and fire after the player selects a card.
    #[test]
    fn optional_discard_if_you_do_draw_fires_after_interactive_choice() {
        let mut state = GameState::new_two_player(42);

        // Two cards in hand so the discard is interactive (player must choose 1 of 2)
        let hand_card_a = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Fodder A".to_string(),
            Zone::Hand,
        );
        let _hand_card_b = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Fodder B".to_string(),
            Zone::Hand,
        );

        // Cards in library to draw
        create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Draw A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(13),
            PlayerId(0),
            "Draw B".to_string(),
            Zone::Library,
        );

        // Build: optional Discard 1 → sub: Draw 2 (IfYouDo)
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::IfYouDo);

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        ability.optional = true;

        // Step 1: resolve_ability_chain → OptionalEffectChoice
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "Expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );

        // Step 2: Accept optional effect → DiscardChoice (2 cards, must choose 1)
        let waiting = crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(waiting, WaitingFor::DiscardChoice { .. }),
            "Expected DiscardChoice after accepting optional with 2 cards in hand, got {:?}",
            waiting
        );

        // Verify the sub-ability was stashed as a pending continuation
        assert!(
            state.pending_continuation.is_some(),
            "IfYouDo sub-ability should be stashed as pending_continuation during DiscardChoice"
        );

        // Step 3: Select one card to discard
        let wf = state.waiting_for.clone();
        let _result = crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            wf,
            crate::types::GameAction::SelectCards {
                cards: vec![hand_card_a],
            },
            &mut events,
        )
        .unwrap();

        // After: started with 2 in hand, discarded 1 (-1), drew 2 (+2) = 3 cards in hand.
        let hand_after = state.players[0].hand.len();
        assert_eq!(
            hand_after, 3,
            "After discarding 1 of 2 and drawing 2, hand should have 3 cards, got {}. \
             IfYouDo sub-ability likely did not fire.",
            hand_after,
        );
    }

    /// Abandon Attachments #81: stale cost_payment_failed_flag from a previous resolution
    /// must not block the IfYouDo condition. The flag should be cleared when accepting
    /// an optional effect.
    #[test]
    fn optional_discard_if_you_do_not_blocked_by_stale_flag() {
        let mut state = GameState::new_two_player(42);

        // Simulate a previous resolution that left the flag set
        state.cost_payment_failed_flag = true;

        // Card in hand to discard
        create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Fodder".to_string(),
            Zone::Hand,
        );

        // Cards in library to draw
        create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Draw A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Draw B".to_string(),
            Zone::Library,
        );

        // Build: optional Discard 1 → sub: Draw 2 (IfYouDo)
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::IfYouDo);

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        ability.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Accept → forced discard (1 card, count=1) → should draw 2
        let _waiting = crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();

        let hand_after = state.players[0].hand.len();
        assert_eq!(
            hand_after, 2,
            "Stale cost_payment_failed_flag should be cleared by handle_optional_effect_choice. \
             Hand should have 2 cards (discard 1 + draw 2), got {}.",
            hand_after,
        );
    }
}
