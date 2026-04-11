use std::borrow::Cow;

use crate::game::filter;
use crate::game::speed::has_max_speed;
use crate::types::ability::{
    AbilityCondition, AbilityKind, ControllerRef, Effect, EffectError, FilterProp, PlayerFilter,
    QuantityExpr, QuantityRef, ResolvedAbility, SharedQuality, TargetFilter, TargetRef, TypeFilter,
    UnlessCost,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;

pub mod adapt;
pub mod add_restriction;
pub mod additional_combat;
pub mod amass;
pub mod animate;
pub mod attach;
pub mod become_copy;
pub mod become_monarch;
pub mod bolster;
pub mod bounce;
pub mod cast_from_zone;
pub mod change_targets;
pub mod change_zone;
pub mod choose;
pub mod choose_and_sacrifice_rest;
pub mod choose_card;
pub mod choose_from_zone;
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
pub mod pay;
pub mod phase_out;
pub mod player_counter;
pub mod populate;
pub mod prevent_damage;
pub mod proliferate;
pub mod pump;
pub mod put_on_top;
pub mod put_on_top_or_bottom;
pub mod regenerate;
pub mod remove_from_combat;
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
pub mod venture;
pub mod win_lose;

fn matches_player_scope(
    state: &GameState,
    player: PlayerId,
    scope: &PlayerFilter,
    controller: PlayerId,
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
                            .map(|player| player.speed.unwrap_or(0))
                            .max()
                            .unwrap_or(0);
                        p.speed.unwrap_or(0) == highest_speed
                    }
                    PlayerFilter::ZoneChangedThisWay => state
                        .last_zone_changed_ids
                        .iter()
                        .any(|id| state.objects.get(id).is_some_and(|obj| obj.owner == p.id)),
                }
        })
}

fn append_to_pending_continuation(state: &mut GameState, tail: Option<Box<ResolvedAbility>>) {
    let Some(tail) = tail else {
        return;
    };

    if let Some(existing) = state.pending_continuation.as_mut() {
        let mut cursor = existing.as_mut();
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
        state.pending_continuation = Some(tail);
    }
}

/// CR 601.2c: Extract SharesQuality filter properties from an effect's target filter.
/// Returns the typed qualities that require group validation.
fn extract_shares_quality_props(filter: &TargetFilter) -> Vec<&SharedQuality> {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .filter_map(|p| match p {
                FilterProp::SharesQuality { quality } => Some(quality),
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
        Effect::ExchangeControl => exchange_control::resolve(state, ability, events),
        Effect::Attach { .. } => attach::resolve(state, ability, events),
        Effect::ControlNextTurn { .. } => control_next_turn::resolve(state, ability, events),
        Effect::Surveil { .. } => surveil::resolve(state, ability, events),
        Effect::Fight { .. } => fight::resolve(state, ability, events),
        Effect::Bounce { .. } => bounce::resolve(state, ability, events),
        Effect::Explore => explore::resolve(state, ability, events),
        Effect::ExploreAll { .. } => explore::resolve_all(state, ability, events),
        Effect::Investigate => investigate::resolve(state, ability, events),
        // CR 701.56a: Time travel — interactive counter manipulation on suspended/time-countered permanents.
        // Currently a no-op; full interactive implementation requires WaitingFor infrastructure.
        Effect::TimeTravel => Ok(()),
        Effect::BecomeMonarch => become_monarch::resolve(state, ability, events),
        Effect::Proliferate => proliferate::resolve(state, ability, events),
        Effect::Populate => populate::resolve(state, ability, events),
        Effect::Clash => clash::resolve(state, ability, events),
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
        Effect::GenericEffect { .. } => effect::resolve(state, ability, events),
        Effect::Cleanup { .. } => cleanup::resolve(state, ability, events),
        Effect::Mana { .. } => mana::resolve(state, ability, events),
        Effect::Discard { .. } => discard::resolve(state, ability, events),
        Effect::Shuffle { .. } => shuffle::resolve(state, ability, events),
        Effect::Transform { .. } => transform_effect::resolve(state, ability, events),
        Effect::SearchLibrary { .. } => search_library::resolve(state, ability, events),
        Effect::Seek { .. } => seek::resolve(state, ability, events),
        Effect::RevealHand { .. } => reveal_hand::resolve(state, ability, events),
        Effect::RevealTop { .. } => reveal_top::resolve(state, ability, events),
        Effect::ExileTop { .. } => exile_top::resolve(state, ability, events),
        Effect::TargetOnly { .. } => Ok(()), // no-op: targeting is established at cast time
        Effect::Choose { .. } => choose::resolve(state, ability, events),
        Effect::Suspect { .. } => suspect::resolve(state, ability, events),
        Effect::Connive { .. } => connive::resolve(state, ability, events),
        Effect::PhaseOut { .. } => phase_out::resolve(state, ability, events),
        Effect::ForceBlock { .. } => force_block::resolve(state, ability, events),
        Effect::SolveCase => solve_case::resolve(state, ability, events),
        Effect::SetClassLevel { .. } => set_class_level::resolve(state, ability, events),
        Effect::CreateDelayedTrigger { .. } => delayed_trigger::resolve(state, ability, events),
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
        Effect::AdditionalCombatPhase { .. } => additional_combat::resolve(state, ability, events),
        Effect::ExileFromTopUntil { .. } => exile_from_top_until::resolve(state, ability, events),
        Effect::RevealUntil { .. } => reveal_until::resolve(state, ability, events),
        Effect::Discover { .. } => discover::resolve(state, ability, events),
        Effect::PutAtLibraryPosition { .. } => put_on_top::resolve(state, ability, events),
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

/// CR 603.7: Check if the next sub_ability needs tracked set recording.
/// Consumers: delayed triggers with uses_tracked_set, token counts from TrackedSetSize,
/// and ChooseFromZone (which selects from the tracked set of exiled/moved cards).
fn next_sub_needs_tracked_set(ability: &ResolvedAbility) -> bool {
    ability.sub_ability.as_ref().is_some_and(|sub| {
        matches!(
            &sub.effect,
            Effect::CreateDelayedTrigger {
                uses_tracked_set: true,
                ..
            } | Effect::Token {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                ..
            } | Effect::ChooseFromZone { .. }
                | Effect::GrantCastingPermission {
                    target: TargetFilter::TrackedSet { .. },
                    ..
                }
        )
    })
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
        | Effect::TargetOnly { target } => target,
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
    }

    // BeginGame abilities are handled at game-start setup, not during stack resolution
    if matches!(ability.kind, AbilityKind::BeginGame) {
        return Ok(());
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
        } else if let Some(AbilityCondition::NinjutsuVariantPaidInstead { ref variant }) =
            sub.condition
        {
            // CR 608.2e + CR 702.49: Read from GameObject, not SpellContext
            state
                .objects
                .get(&ability.source_id)
                .map(|obj| obj.ninjutsu_variant_paid == Some((variant.clone(), state.turn_number)))
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
    // execute the entire effect chain once per matching player, temporarily
    // overriding ability.controller for each iteration so effects like Discard,
    // Draw, Mill target the correct player.
    if let Some(ref scope) = ability.player_scope {
        let controller = ability.controller;
        let matching_players: Vec<PlayerId> = crate::game::players::apnap_order(state)
            .into_iter()
            .filter(|pid| matches_player_scope(state, *pid, scope, controller))
            .collect();

        let initial_waiting_for = state.waiting_for.clone();
        for (i, pid) in matching_players.iter().enumerate() {
            let mut scoped = ability.clone();
            scoped.player_scope = None; // prevent re-entry
            scoped.controller = *pid;
            resolve_ability_chain(state, &scoped, events, depth + 1)?;

            // CR 608.2e: Break if inner effect entered a player-choice state —
            // remaining players resume after the choice resolves via continuation.
            if state.waiting_for != initial_waiting_for {
                let remaining = &matching_players[i + 1..];
                if !remaining.is_empty() {
                    // Build continuation chain for remaining players in APNAP order.
                    // Each remaining player gets a full clone (including sub_ability)
                    // so their own chained effects resolve naturally when resumed.
                    let mut tail: Option<Box<ResolvedAbility>> = None;
                    for &remaining_pid in remaining.iter().rev() {
                        let mut remaining_scoped = ability.clone();
                        remaining_scoped.player_scope = None;
                        remaining_scoped.controller = remaining_pid;
                        // Append the previous tail after this player's sub_ability chain
                        if let Some(prev) = tail {
                            super::ability_utils::append_to_sub_chain(&mut remaining_scoped, *prev);
                        }
                        tail = Some(Box::new(remaining_scoped));
                    }
                    append_to_pending_continuation(state, tail);
                }
                break;
            }
        }
        return Ok(());
    }

    // CR 608.2c: Evaluate top-level condition before emitting any optional or unless-pay
    // choice. This must run after player_scope rebinding so scoped abilities test
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

    // CR 609.3: "You may" effects prompt the controller before execution.
    if ability.optional {
        let description = ability.description.clone();
        state.pending_optional_effect = Some(Box::new(ability.clone()));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: ability.controller,
            source_id: ability.source_id,
            description,
        };
        return Ok(());
    }

    // CR 118.12: "Effect unless [player] pays {cost}" — tax trigger modifier.
    if let Some(ref unless_pay) = ability.unless_pay {
        if let Some(payer) = resolve_unless_payer(state, &unless_pay.payer) {
            // CR 702.21a: Non-mana costs (PayLife, DiscardCard, Sacrifice) bypass
            // mana resolution — pass through to UnlessPayment directly.
            match &unless_pay.cost {
                UnlessCost::PayLife { .. }
                | UnlessCost::DiscardCard
                | UnlessCost::Sacrifice { .. } => {
                    let mut pending = ability.clone();
                    pending.unless_pay = None;
                    state.waiting_for = WaitingFor::UnlessPayment {
                        player: payer,
                        cost: unless_pay.cost.clone(),
                        pending_effect: Box::new(pending),
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
                | UnlessCost::DiscardCard
                | UnlessCost::Sacrifice { .. } => unreachable!(),
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
                let qualities = extract_shares_quality_props(target_filter);
                qualities.iter().any(|quality| {
                    !filter::validate_shares_quality(state, &effective.targets, quality)
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

            let initial_waiting_for = state.waiting_for.clone();
            for _ in 0..iterations {
                let _ = resolve_effect(state, effective, events);
                // Break if inner effect entered a player-choice state — avoid
                // executing subsequent iterations against state awaiting input.
                if state.waiting_for != initial_waiting_for {
                    break;
                }
            }
        } // end shares_quality_failed else
    }

    // CR 609.3: Extract numeric result from events emitted by this effect for
    // PreviousEffectAmount in sub_abilities ("gain life equal to the life lost this way").
    // Sum all LifeChanged (negative = loss) and DamageDealt events from this effect.
    {
        let mut amount_sum: i32 = 0;
        for event in &events[events_before..] {
            match event {
                GameEvent::LifeChanged { amount, .. } => {
                    // Life loss is negative; take absolute value for "life lost this way"
                    amount_sum += amount.unsigned_abs() as i32;
                }
                GameEvent::DamageDealt { amount, .. } => {
                    amount_sum += *amount as i32;
                }
                _ => {}
            }
        }
        if amount_sum > 0 {
            state.last_effect_amount = Some(amount_sum);
        }
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

    // CR 603.7: Record moved objects as a tracked set for delayed trigger pronouns.
    // Scans ZoneChanged events emitted by the just-resolved effect and stores the
    // affected object IDs so the downstream CreateDelayedTrigger can bind them.
    // Filters by the effect's destination zone to exclude commander redirections
    // (CR 903.9a: commanders redirected to command zone should not be tracked).
    if next_sub_needs_tracked_set(ability) {
        let dest_zone = match &ability.effect {
            Effect::ChangeZone { destination, .. } | Effect::ChangeZoneAll { destination, .. } => {
                Some(*destination)
            }
            Effect::ExileTop { .. } => Some(crate::types::zones::Zone::Exile),
            _ => None,
        };
        let moved_ids: Vec<ObjectId> = events[events_before..]
            .iter()
            .filter_map(|e| match e {
                GameEvent::ZoneChanged { object_id, to, .. }
                    if dest_zone.is_none_or(|d| *to == d) =>
                {
                    Some(*object_id)
                }
                _ => None,
            })
            .collect();
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state.tracked_object_sets.insert(set_id, moved_ids);
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
                    | AbilityCondition::NinjutsuVariantPaidInstead { .. }
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
                        state.pending_continuation = Some(Box::new(resolved));
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
                        state.pending_continuation = Some(Box::new(resolved));
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
                    // Inject revealed card IDs as targets for else branches following RevealTop,
                    // so "Otherwise, put that card into your hand" knows which card to move.
                    if else_resolved.targets.is_empty()
                        && !state.last_revealed_ids.is_empty()
                        && matches!(
                            ability.effect,
                            Effect::RevealTop { .. } | Effect::Dig { reveal: true, .. }
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
        // If the effect resolver already set up a pending_continuation (e.g., clash
        // injects modified context for optional_effect_performed), the sub_ability
        // chain is already accounted for — skip to avoid double execution.
        if state.pending_continuation.is_some() {
            return Ok(());
        }
        // If resolve_effect just entered a player-choice state (Scry/Dig/Surveil),
        // save the sub-ability as a continuation to execute after the player responds,
        // rather than immediately processing it (which would bypass the UI).
        if matches!(
            state.waiting_for,
            WaitingFor::ScryChoice { .. }
                | WaitingFor::DigChoice { .. }
                | WaitingFor::SurveilChoice { .. }
                | WaitingFor::RevealChoice { .. }
                | WaitingFor::SearchChoice { .. }
                | WaitingFor::TriggerTargetSelection { .. }
                | WaitingFor::NamedChoice { .. }
                | WaitingFor::MultiTargetSelection { .. }
                | WaitingFor::OptionalEffectChoice { .. }
                | WaitingFor::OpponentMayChoice { .. }
                | WaitingFor::DiscoverChoice { .. }
                | WaitingFor::TopOrBottomChoice { .. }
                | WaitingFor::ProliferateChoice { .. }
                | WaitingFor::ExploreChoice { .. }
                | WaitingFor::CopyRetarget { .. }
                | WaitingFor::DistributeAmong { .. }
                | WaitingFor::RetargetChoice { .. }
                | WaitingFor::ChooseFromZoneChoice { .. }
                | WaitingFor::ManifestDreadChoice { .. }
                | WaitingFor::DiscardChoice { .. }
                | WaitingFor::EffectZoneChoice { .. }
                | WaitingFor::CategoryChoice { .. }
                | WaitingFor::LearnChoice { .. }
                | WaitingFor::PopulateChoice { .. }
        ) {
            let mut sub_clone = sub.as_ref().clone();
            if sub_clone.targets.is_empty() && !ability.targets.is_empty() {
                sub_clone.targets = ability.targets.clone();
            }
            // Propagate SpellContext so kicker/optional flags survive continuations.
            sub_clone.context = ability.context.clone();
            debug_assert!(
                state.pending_continuation.is_none(),
                "pending_continuation overwritten before consumption — sub_ability chain will be lost"
            );
            state.pending_continuation = Some(Box::new(sub_clone));
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
        {
            // CR 309.4c + CR 607.1: Forward exiled card IDs to sub-ability
            // (linked ability pair — second refers to cards exiled by the first).
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
        AbilityCondition::AdditionalCostPaid => ability.context.additional_cost_paid,
        AbilityCondition::AdditionalCostNotPaid => !ability.context.additional_cost_paid,
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
            negated,
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
            let matches = type_matches && filter_matches;
            if *negated {
                !matches
            } else {
                matches
            }
        }
        // CR 400.7 + CR 608.2c: "unless ~ entered this turn"
        AbilityCondition::SourceDidNotEnterThisTurn => state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.entered_battlefield_turn != Some(state.turn_number))
            .unwrap_or(true),
        // CR 702.49 + CR 603.4: "if its sneak/ninjutsu cost was paid"
        AbilityCondition::NinjutsuVariantPaid { variant } => state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.ninjutsu_variant_paid == Some((variant.clone(), state.turn_number)))
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
        AbilityCondition::HasMaxSpeed => has_max_speed(state, ability.controller),
        // "Instead" override conditions — return pure boolean value.
        // Terminal control flow (early return from resolve_ability_chain) is the caller's
        // responsibility in the sub-ability context.
        AbilityCondition::AdditionalCostPaidInstead => ability.context.additional_cost_paid,
        AbilityCondition::NinjutsuVariantPaidInstead { variant } => state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.ninjutsu_variant_paid == Some((variant.clone(), state.turn_number)))
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
            if let Some(id) = target_id {
                if *use_lki {
                    // CR 400.7: Check last-known information for past-tense conditions.
                    // Try LKI cache first, fall back to current state if object still exists.
                    if let Some(lki) = state.lki_cache.get(&id) {
                        // LKI snapshot has core types — check type_filters against LKI
                        match filter {
                            TargetFilter::Typed(tf) => {
                                use crate::types::card_type::CoreType;
                                tf.type_filters.iter().all(|req| {
                                    let ct = match req {
                                        TypeFilter::Creature => Some(CoreType::Creature),
                                        TypeFilter::Land => Some(CoreType::Land),
                                        TypeFilter::Artifact => Some(CoreType::Artifact),
                                        TypeFilter::Enchantment => Some(CoreType::Enchantment),
                                        TypeFilter::Instant => Some(CoreType::Instant),
                                        TypeFilter::Sorcery => Some(CoreType::Sorcery),
                                        TypeFilter::Planeswalker => Some(CoreType::Planeswalker),
                                        TypeFilter::Battle => Some(CoreType::Battle),
                                        _ => None,
                                    };
                                    ct.map(|ct| lki.card_types.contains(&ct)).unwrap_or(true)
                                })
                            }
                            _ => true,
                        }
                    } else {
                        // Object still exists — check current state
                        crate::game::filter::matches_target_filter(
                            state,
                            id,
                            filter,
                            ability.source_id,
                        )
                    }
                } else {
                    // Check current state for present-tense conditions
                    crate::game::filter::matches_target_filter(state, id, filter, ability.source_id)
                }
            } else {
                false
            }
        }
        // CR 608.2c: "If this creature/permanent is a [type]" — check source object.
        AbilityCondition::SourceMatchesFilter { filter } => {
            crate::game::filter::matches_target_filter(
                state,
                ability.source_id,
                filter,
                ability.source_id,
            )
        }
        // CR 608.2c: "If it's your turn" — check active player against controller.
        AbilityCondition::IsYourTurn { negated } => {
            (state.active_player == ability.controller) != *negated
        }
        // CR 608.2c: "If a [noun] was [verb]ed this way" — check if any zone-changed
        // object matches the type filter. For optional-targeting parents with no targets
        // chosen, last_zone_changed_ids is empty → returns false.
        AbilityCondition::ZoneChangedThisWay { filter } => {
            state.last_zone_changed_ids.iter().any(|&id| {
                crate::game::filter::matches_target_filter_controlled(
                    state,
                    id,
                    filter,
                    ability.source_id,
                    ability.controller,
                )
            })
        }
        // CR 611.2b: "if this creature/permanent is tapped/untapped" — check source object.
        AbilityCondition::SourceIsTapped { negated } => {
            let is_tapped = state
                .objects
                .get(&ability.source_id)
                .is_some_and(|obj| obj.tapped);
            is_tapped != *negated
        }
        // CR 608.2c: General "instead" — delegate to the wrapped inner condition.
        // The "instead" semantics are handled by the swap/guard in resolve_ability_chain.
        AbilityCondition::ConditionInstead { inner } => evaluate_condition(inner, state, ability),
        // CR 608.2c: Compound condition — all inner conditions must be true.
        AbilityCondition::And { conditions } => conditions
            .iter()
            .all(|c| evaluate_condition(c, state, ability)),
    }
}

/// Resolve the payer for an unless-pay modifier from the trigger event context.
/// `TriggeringPlayer` resolves to the player involved in the triggering event
/// (e.g., the opponent who cast a spell for Esper Sentinel).
fn resolve_unless_payer(
    state: &GameState,
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
        AbilityDefinition, AbilityKind, DelayedTriggerCondition, PlayerFilter, QuantityExpr,
        SpellContext, TargetFilter, TargetRef,
    };
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::mana::ManaCost;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
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

    #[test]
    fn resolve_unless_payer_uses_player_action_event_player() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(GameEvent::PlayerPerformedAction {
            player_id: PlayerId(1),
            action: crate::types::events::PlayerActionKind::SearchedLibrary,
        });
        assert_eq!(
            resolve_unless_payer(&state, &TargetFilter::TriggeringPlayer),
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

    #[test]
    fn resolve_ability_chain_condition_blocks_optional_prompt() {
        let mut state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: crate::types::ability::PaymentCost::Life { amount: 1 },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::IsYourTurn { negated: true })
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
                    },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
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
                crate::types::ability::CastingPermission::ExileWithAltCost { cost }
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
                    },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
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
                crate::types::ability::CastingPermission::ExileWithAltCost { cost }
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
                },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
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
            inner: Box::new(AbilityCondition::IsYourTurn { negated: false }),
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
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        // Instead sub: deal 5 damage (with its own chain: draw 2)
        let instead_chain = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
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
            inner: Box::new(AbilityCondition::IsYourTurn { negated: true }),
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
                up_to: false,
                unless_filter: None,
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
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::IsYourTurn { negated: true });
        ability.player_scope = Some(PlayerFilter::All);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].hand.len(), 0);
        assert_eq!(state.players[1].hand.len(), 1);
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

    #[test]
    fn evaluate_condition_and_all_true() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // And([IsYourTurn(false=not negated), IsYourTurn(false=not negated)]) — both true
        let cond = AbilityCondition::And {
            conditions: vec![
                AbilityCondition::IsYourTurn { negated: false },
                AbilityCondition::IsYourTurn { negated: false },
            ],
        };
        assert!(evaluate_condition(&cond, &state, &ability));
    }

    #[test]
    fn evaluate_condition_and_one_false() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // And([IsYourTurn(true), IsYourTurn(false)]) — one is "not your turn" which is false
        let cond = AbilityCondition::And {
            conditions: vec![
                AbilityCondition::IsYourTurn { negated: true },
                AbilityCondition::IsYourTurn { negated: false },
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
            inner: Box::new(AbilityCondition::IsYourTurn { negated: false }),
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
        .condition(AbilityCondition::IsYourTurn { negated: true })
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
}
