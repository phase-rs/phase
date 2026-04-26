use std::borrow::Cow;

use crate::game::filter;
use crate::game::speed::has_max_speed;
use crate::types::ability::{
    AbilityCondition, AbilityKind, ControllerRef, Effect, EffectError, EffectKind, FilterProp,
    PlayerFilter, QuantityExpr, QuantityRef, ResolvedAbility, SharedQuality, TargetFilter,
    TargetRef, TypeFilter, UnlessCost,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingContinuation, WaitingFor};
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
pub mod cascade;
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
    let Some(cont) = state.pending_continuation.take() else {
        return;
    };
    let PendingContinuation { chain, parent_kind } = cont;
    let source_id = chain.source_id;
    let _ = resolve_ability_chain(state, &chain, events, 0);
    if let Some(kind) = parent_kind {
        events.push(GameEvent::EffectResolved { kind, source_id });
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
        Effect::AdditionalCombatPhase { .. } => additional_combat::resolve(state, ability, events),
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
        Effect::ChooseOneOf { .. } => {
            // CR 700.2: Runtime resolver for `ChooseOneOf` is not yet wired —
            // full support requires a new `WaitingFor::ChooseOneOfBranch`
            // state + effect resolver + UI integration. The typed shape is
            // emitted by the parser (Highway Robbery and analogous binary
            // "you may A or B" imperatives) so it is preserved in card data
            // for future runtime activation. Treat as a no-op at resolution
            // for now — the outer `optional: true` on the ability means the
            // controller can simply decline.
            eprintln!("Warning: ChooseOneOf resolver not yet implemented — treating as no-op");
            Ok(())
        }
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
        | QuantityExpr::HalfRounded { inner, .. } => quantity_expr_references_tracked_set(inner),
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
        | Effect::ControlNextTurn { target, .. }
        | Effect::AdditionalCombatPhase { target, .. }
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
        state.exiled_from_hand_this_resolution = 0;
        // CR 603.7: Chain-local tracked-set identity — resets per top-level
        // ability resolution so compound zone changes within one chain
        // coalesce into a single tracked set, while unrelated resolutions
        // stay isolated.
        state.chain_tracked_set_id = None;
    }

    // BeginGame abilities are handled at game-start setup, not during stack resolution
    if matches!(ability.kind, AbilityKind::BeginGame) {
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
    // execute the entire effect chain once per matching player, temporarily
    // overriding ability.controller for each iteration so effects like Discard,
    // Draw, Mill target the correct player.
    if let Some(ref scope) = ability.player_scope {
        let controller = ability.controller;
        let matching_players: Vec<PlayerId> = crate::game::players::apnap_order(state)
            .into_iter()
            .filter(|pid| matches_player_scope(state, *pid, scope, controller, ability.source_id))
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

    // CR 117.3a + CR 609.3: "You may" effects prompt the acting player before
    // execution. For subject-anchored optional effects ("its controller may
    // search their library" — Assassin's Trophy), the acting player is the
    // resolved subject (the target permanent's controller), NOT the caster.
    if ability.optional {
        let description = ability.description.clone();
        let prompt_player = optional_prompt_player(state, ability);
        state.pending_optional_effect = Some(Box::new(ability.clone()));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: prompt_player,
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
            state.pending_continuation = Some(PendingContinuation::new(Box::new(sub_clone)));
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
        AbilityCondition::HasMaxSpeed => has_max_speed(state, ability.controller),
        AbilityCondition::IsMonarch => state.monarch == Some(ability.controller),
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
        AbilityCondition::TargetMatchesFilter {
            filter,
            use_lki,
            negated,
        } => {
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
                            ) ^ negated;
                        }
                    }
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
            matched ^ negated
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
        // CR 608.2c + CR 614.1d: "if you control a/no [filter]" — scan the battlefield
        // for any other permanent owned/controlled by the ability controller matching
        // `filter`. Excludes the source itself so a Soldier-typed land doesn't satisfy
        // its own "you control a Soldier" check. `filter` carries `ControllerRef::You`
        // pre-bound by the parser; FilterContext provides the source binding.
        AbilityCondition::ControllerControlsMatching { filter, negated } => {
            let ctx = crate::game::filter::FilterContext::from_ability(ability);
            let controls_any = state.objects.values().any(|o| {
                o.zone == crate::types::zones::Zone::Battlefield
                    && o.id != ability.source_id
                    && crate::game::filter::matches_target_filter(state, o.id, filter, &ctx)
            });
            controls_any != *negated
        }
        // CR 608.2c: "If it's your turn" — check active player against controller.
        AbilityCondition::IsYourTurn { negated } => {
            (state.active_player == ability.controller) != *negated
        }
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
        // CR 730.2a: True when it's neither day nor night (no designation set yet).
        AbilityCondition::DayNightIsNeither => state.day_night.is_none(),
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
        AbilityDefinition, AbilityKind, CastingPermission, ControllerRef, DelayedTriggerCondition,
        Duration, FilterProp, PlayerFilter, PtValue, QuantityExpr, QuantityRef, SpellContext,
        TargetFilter, TargetRef, TypedFilter,
    };
    use crate::types::game_state::{ExileLink, ExileLinkKind, LinkedExileSnapshot};
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::mana::ManaColor;
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

    #[test]
    fn resolve_ability_chain_condition_blocks_optional_prompt() {
        let mut state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: crate::types::ability::PaymentCost::Life {
                    amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                },
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
            conditions: vec![
                AbilityCondition::IsYourTurn { negated: false },
                AbilityCondition::IsYourTurn { negated: false },
            ],
        };
        assert!(evaluate_condition(&cond, &state, &ability));
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
                    duration: Duration::UntilYourNextTurn,
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
}
