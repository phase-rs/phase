//! Horde Magic — the self-piloting "Horde" seat's scripted turn runtime.
//!
//! Horde Magic is a casual cooperative variant (not DCI/CR-sanctioned) in which
//! a team of survivors faces a single automated "Horde" deck. The Horde reuses
//! CR mechanisms the engine already models: the one-vs-many topology of
//! Archenemy (CR 904) with the Horde in the archenemy seat, and shared team
//! turns (CR 805) for the survivor team. This module is the runtime sibling of
//! [`crate::game::archenemy`] / [`crate::game::planechase`]: it owns the Horde's
//! precombat-main "reveal and resolve" wave.
//!
//! ## Reveal-and-resolve wave
//!
//! At the start of the Horde's precombat main phase the Horde reveals cards from
//! the top of its library and either casts them (nontoken cards, for free — the
//! Cascade/Discover free-cast authority, CR 608.2g) or puts them onto the
//! battlefield (tokens — CR 111: a token is never cast, so a revealed library
//! token enters directly via [`reveal_library_token`]). A "wave" reveals a
//! ruleset-defined number of cards ([`WaveTermination`]).
//!
//! Because each free cast sets [`GameState::waiting_for`] and puts one spell on
//! the stack (the spell must then resolve through the normal priority/stack
//! loop), a wave cannot cast N cards in a single synchronous call. Instead:
//!
//!  - [`begin_wave`] runs as the Horde's precombat main begins (dispatched from
//!    `turns::finish_enter_phase`, mutually exclusive with the archenemy
//!    `set_in_motion` turn-based action) and seeds
//!    [`GameState::horde_wave_remaining`] with the wave size. It does NOT cast —
//!    setting `waiting_for` inside `finish_enter_phase` would be clobbered by the
//!    phase executor's own priority grant.
//!  - [`maybe_reveal_next`] reveals-and-resolves exactly ONE card and decrements
//!    the counter. It is invoked (a) from the `PreCombatMain` arm of
//!    `turns::auto_advance` for the first card (mirroring the Paradigm
//!    turn-based-action hook), and (b) from the priority-grant seam in
//!    `engine::pass_priority_once_with_pipeline` after each Horde spell resolves
//!    and priority returns to the Horde with an empty stack, until the counter
//!    reaches zero.

use crate::game::effects::change_zone::{execute_zone_move, ZoneMoveResult};
use crate::game::effects::token::apply_create_token_after_replacement;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    CardPlayMode, CastFromZoneDriver, Effect, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card::{Rarity, TokenImageRef};
use crate::types::events::GameEvent;
use crate::types::format::{GameFormat, HordePostCombatActivation, WaveTermination};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{
    EtbTapState, ProposedEvent, TokenAttackRequest, TokenCharacteristics, TokenHostRequest,
    TokenSpec,
};
use crate::types::zones::Zone;
use std::collections::HashSet;

/// The PRIMARY Horde seat, if this is a Horde game. Live read via the topology
/// accessor (single authority, CR 904.2a-style), never a latched copy. For a
/// single-Horde game this is the sole Horde seat; for a two-Horde-force deck it
/// is the first (the one-vs-many archenemy). Prefer [`horde_seats`] (all seats)
/// for side-scoped logic (loss, no-life-total, opening-hand skip).
pub(crate) fn horde_seat(state: &GameState) -> Option<PlayerId> {
    if state.format_config.format != GameFormat::Horde {
        return None;
    }
    crate::game::topology::archenemy(state)
}

/// EVERY seat on the Horde side. A single-Horde game leaves
/// `GameState::horde_seats` empty and this falls back to the sole archenemy seat;
/// a two-Horde-force deck (LOTR Two Towers) lists both Horde seats there. Empty
/// for non-Horde formats. This is the single authority for "which seats are the
/// Horde" — side-scoped logic (loss, no-life-total, opening-hand skip) folds over
/// it rather than comparing against the one archenemy.
pub(crate) fn horde_seats(state: &GameState) -> Vec<PlayerId> {
    if state.format_config.format != GameFormat::Horde {
        return Vec::new();
    }
    if !state.horde_seats.is_empty() {
        return state.horde_seats.clone();
    }
    horde_seat(state).into_iter().collect()
}

/// True when `id` is one of this game's Horde seats.
pub(crate) fn is_horde_seat(state: &GameState, id: PlayerId) -> bool {
    horde_seats(state).contains(&id)
}

/// True when it is currently a Horde seat's turn. Used to gate the draw-step skip
/// and the wave. Tests membership so it holds for either seat of a two-Horde deck.
pub(crate) fn is_horde_turn(state: &GameState) -> bool {
    is_horde_seat(state, state.active_player)
}

/// True when `id` is a Horde seat whose ruleset grants the advanced-rule
/// infinite mana ("the Horde has infinite mana", for post-combat activation
/// costs). This is the PERSISTENT authority for Horde mana: derived from the
/// ruleset, so it can never expire. The step-end drain exempts these seats
/// from both the CR 500.4 pool-empty and the loop-axis de-realization, and
/// `mana_payment::refill_infinite_mana` keeps topping the pool after every
/// action — unlike a one-shot loop mark, which the first step transition
/// clears before the post-combat queue runs.
pub(crate) fn has_horde_infinite_mana(state: &GameState, id: PlayerId) -> bool {
    state
        .format_config
        .horde_ruleset
        .as_ref()
        .is_some_and(|r| r.post_combat_activation == HordePostCombatActivation::OncePerPermanent)
        && is_horde_seat(state, id)
}

/// The Horde seat whose turn it currently is — the ACTIVE commander. For a
/// two-Horde deck (LOTR "Two Towers") the commanders alternate turns (Reading B),
/// so this is `active_player`, not the primary seat; the wave, post-combat
/// activation, and the damage-mill clock all key off it so each commander reveals
/// from / mills its OWN library and activates only its OWN permanents. For a
/// single-Horde game it is the sole Horde seat. `None` when it is not a Horde turn.
pub(crate) fn active_horde_seat(state: &GameState) -> Option<PlayerId> {
    is_horde_turn(state).then_some(state.active_player)
}

/// True when this Horde game stages Bosses (Turtle Team-Up class): the ruleset
/// carries a `boss_stages` config. Boss-deck games keep a real per-stage life
/// total on the Horde seat instead of the classic mill race.
pub(crate) fn has_boss_stages(state: &GameState) -> bool {
    state
        .format_config
        .horde_ruleset
        .as_ref()
        .is_some_and(|r| r.boss_stages.is_some())
}

/// True when `id` is a Horde seat, which in this casual variant has no life
/// total. Damage/life loss it would suffer is redirected to milling (see
/// `effects::life`), and it is exempt from the CR 704.5a "0 or less life loses"
/// state-based action (`sba::collect_life_losers`). This is not a
/// CR-sanctioned rule — Horde Magic is a casual format — so the helper names
/// the *mechanism* (a seat with no life total) rather than citing a fictional
/// rule number. Every classic-Horde seat has no life total; boss-deck games
/// (Turtle Team-Up) keep a real per-stage life total instead.
pub(crate) fn player_has_no_life_total(state: &GameState, id: PlayerId) -> bool {
    is_horde_seat(state, id) && !has_boss_stages(state)
}

/// Redirect target for damage/life loss the Horde would suffer (CR 120.3a maps
/// combat damage to a player onto life loss; CR 119.3 direct loss): mill `count`
/// cards from the top of the Horde's library. The Horde has no life total, so
/// the milled count stands in for the "loss" and its emptying library is the
/// real clock ([`horde_is_defeated`]).
///
/// This is the single authority for the community *advanced* legendary rule
/// ([`HordeLegendaryDeath::EtbThenPhaseOut`]): a legendary PERMANENT card among
/// the milled cards is put onto the battlefield instead of into the graveyard
/// (its enters-the-battlefield triggers fire, CR 603.6) and then immediately
/// phases out (CR 702.26). Milling the Horde's boss therefore DEPLOYS it rather
/// than removing it, and it phases back in on the Horde's next untap
/// (CR 702.26a). Basic decks ([`HordeLegendaryDeath::Normal`], every currently
/// shipped deck) mill straight to the graveyard.
///
/// Outcome of the Horde damage→mill redirect ([`mill_from_loss`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MillFromLossOutcome {
    /// The redirect settled: the mill was applied, prevented to nothing, or
    /// transformed away from a mill. No prompt is parked.
    Settled,
    /// A replacement choice parked mid-redirect — either an event-level Mill
    /// choice (CR 616.1 ordering / an optional mill replacement) or a per-card
    /// CR 616.1 ordering choice inside the delivery batch. `state.waiting_for`
    /// names the prompt and the caller must surface its own pause, never
    /// proceed as if the redirect settled.
    Paused,
}

/// Called from `effects::life`'s no-life-total redirect. The redirect proposes
/// a real `ProposedEvent::Mill` through the replacement pipeline — the same
/// seam an ordinary mill resolves through — so event-level mill replacements
/// (the Bruvac doubling class) apply to the redirected count, and either pause
/// channel (event-level choice or per-card CR 616.1 ordering) propagates to
/// the caller as [`MillFromLossOutcome::Paused`] instead of being discarded.
///
/// Resume: an event-level pause completes through the shared
/// `handle_replacement_choice` Mill arm, which delivers the basic mill; a
/// per-card pause completes through `zone_pipeline`'s batch-tail drain, which
/// runs the stashed [`BatchCompletion`](crate::types::game_state::BatchCompletion)
/// once the tail delivers — so the advanced deployment work (ordinary `Milled`
/// events + the synchronous cohort phase-out) survives a per-card pause. One
/// opt-in-advanced edge stays documented rather than silently fixed: an
/// event-level pause under the advanced ruleset resumes as a basic mill
/// (milled legendaries are buried rather than deployed — reaching this needs
/// an INTERACTIVE mill replacement, since Bruvac-class automatic doublings
/// never pause).
pub(crate) fn mill_from_loss(
    state: &mut GameState,
    horde: PlayerId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> MillFromLossOutcome {
    use crate::types::format::HordeLegendaryDeath;

    let legendary_rule = state
        .format_config
        .horde_ruleset
        .as_ref()
        .map_or(HordeLegendaryDeath::Normal, |r| r.legendary_death);

    // CR 614 + CR 616.1: propose the redirect as a Mill EVENT first, mirroring
    // `effects::mill::resolve_mill`'s three-way match. Event-level mill
    // replacements (Bruvac: "twice that many instead") fold their count here;
    // a competing-choice pause parks here like any other mill.
    match replacement::replace_event(
        state,
        ProposedEvent::Mill {
            player_id: horde,
            count,
            destination: Zone::Graveyard,
            applied: HashSet::new(),
        },
        events,
    ) {
        ReplacementResult::Execute(ProposedEvent::Mill { count, .. }) => {
            // Basic decks: ordinary mill to the graveyard, routed through the
            // shared mill building block so per-card graveyard replacements
            // (Rest in Peace class) still consult exactly as an ordinary mill
            // would (CR 701.17a-b). A per-card pause (`false`) propagates.
            if legendary_rule == HordeLegendaryDeath::Normal {
                // `EffectError` has no carrier on this path, so an application
                // error maps to "delivered" (no pause) — mirroring the shared
                // Mill arm in `handle_replacement_choice`.
                let delivered = crate::game::effects::mill::apply_mill_after_replacement(
                    state,
                    ProposedEvent::Mill {
                        player_id: horde,
                        count,
                        destination: Zone::Graveyard,
                        applied: HashSet::new(),
                    },
                    events,
                )
                .unwrap_or(true);
                return if delivered {
                    MillFromLossOutcome::Settled
                } else {
                    MillFromLossOutcome::Paused
                };
            }
            mill_from_loss_advanced(state, horde, count, events)
        }
        // A replacement transformed the redirect away from a mill: settled,
        // mirroring `apply_mill_after_replacement`'s non-Mill `Ok(true)`.
        ReplacementResult::Execute(_) => MillFromLossOutcome::Settled,
        // Fully prevented: the redirect changed nothing. Settled.
        ReplacementResult::Prevented => MillFromLossOutcome::Settled,
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            MillFromLossOutcome::Paused
        }
    }
}

/// Advanced legendary rule ([`HordeLegendaryDeath::EtbThenPhaseOut`]) delivery
/// for [`mill_from_loss`]: `count` is the POST-replacement mill count. Takes
/// the top `count` cards (CR 701.17b: no more than the library holds), puts
/// legendary permanents onto the battlefield and buries the rest. The
/// post-delivery work (ordinary `Milled` events + the synchronous cohort
/// phase-out) rides the batch as a `BatchCompletion`, so a CR 616.1 pause
/// carries it across to the resumed tail instead of dropping it.
fn mill_from_loss_advanced(
    state: &mut GameState,
    horde: PlayerId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> MillFromLossOutcome {
    use crate::game::zone_pipeline::{
        move_objects_simultaneously_then, BatchMoveResult, ZoneMoveRequest,
    };
    use crate::types::game_state::BatchCompletion;

    let Some(player) = state.players.iter().find(|p| p.id == horde) else {
        return MillFromLossOutcome::Settled;
    };
    let count = (count as usize).min(player.library.len());
    let milled: Vec<ObjectId> = player.library.iter().take(count).copied().collect();
    if milled.is_empty() {
        return MillFromLossOutcome::Settled;
    }
    let legendaries: Vec<ObjectId> = milled
        .iter()
        .copied()
        .filter(|&id| is_legendary_permanent(state, id))
        .collect();
    let legendary_set: HashSet<ObjectId> = legendaries.iter().copied().collect();

    // One simultaneous batch (CR 701.17a): legendary permanents redirect to the
    // battlefield (the shared pipeline runs full ETB machinery + emits the
    // ZoneChanged that fires their ETB triggers); everything else goes to the
    // graveyard, still consulting per-card graveyard-move replacements.
    let reqs: Vec<ZoneMoveRequest> = milled
        .iter()
        .map(|&id| {
            let dest = if legendary_set.contains(&id) {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            };
            // The milled card itself anchors the Effect cause, mirroring the
            // graveyard mill batch in `effects::mill`.
            ZoneMoveRequest::effect(id, dest, id)
        })
        .collect();
    // CR 616.1: a per-card ordering pause parks `waiting_for` + the tail AND
    // the completion above; the resume drain delivers the tail and runs the
    // completion once, so the phase-out work survives the pause either way.
    if matches!(
        move_objects_simultaneously_then(
            state,
            reqs,
            Some(BatchCompletion::HordeMillFromLossDeliveryComplete {
                player_id: horde,
                milled: milled.clone(),
                legendary_cohort: legendaries.clone(),
            }),
            events,
        ),
        BatchMoveResult::NeedsChoice
    ) {
        return MillFromLossOutcome::Paused;
    }

    MillFromLossOutcome::Settled
}

/// Complete an advanced Horde damage-to-mill batch: publish the ordinary
/// `Milled` events, then synchronously phase out the deployed legendary
/// cohort. Runs after the batch's own trigger collection on both the
/// synchronous and resumed paths — printed ETBs are already collected (CR
/// 603.6), and no SBA check runs before this completion on either path, so
/// the legend rule (CR 704.5j, which has no pending-phase-out exception)
/// never observes the phased-in window.
pub(crate) fn complete_advanced_mill_from_loss(
    state: &mut GameState,
    player_id: PlayerId,
    milled: Vec<ObjectId>,
    legendary_cohort: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> crate::game::zone_pipeline::BatchMoveResult {
    // Ordinary mill event publication: the same per-card `Milled` events an
    // ordinary mill emits, so milled triggers (Zellix class) fire.
    let result =
        crate::game::effects::mill::complete_mill_delivery(state, player_id, milled, events);
    // Synchronous cohort phase-out (CR 702.26). A `Moved` replacement (Rest
    // in Peace class) could still have diverted a cohort member, so only
    // phase out what actually entered.
    for id in legendary_cohort {
        if state.battlefield.contains(&id) {
            crate::game::phasing::phase_out_object(
                state,
                id,
                crate::game::game_object::PhaseOutCause::Directly,
                events,
            );
        }
    }
    result
}

/// A card that is both Legendary (CR 205.4a supertype) and a permanent type
/// (CR 110.4a — only permanent cards can be put onto the battlefield). A
/// legendary instant/sorcery can't enter, so the advanced rule can't apply to
/// it and it mills normally.
fn is_legendary_permanent(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|obj| {
        obj.card_types
            .supertypes
            .contains(&crate::types::card_type::Supertype::Legendary)
            && obj
                .card_types
                .core_types
                .iter()
                .any(|t| t.is_permanent_type())
    })
}

/// True when `id` is the Horde seat, which draws NO opening hand and takes no
/// mulligan (`mulligan::start_mulligan`).
///
/// The Horde has no hand in this variant: it plays entirely off the top of its
/// library through the reveal-and-resolve wave ([`maybe_reveal_next`]) and has no
/// way to cast from hand. Dealing it an opening hand therefore strands real
/// threats where they can never be played, AND permanently removes them from the
/// library — which is the Horde's actual clock, since it loses by decking out
/// ([`horde_is_defeated`]). Casual-format rule (no CR number), so this names the
/// mechanism rather than citing a rule.
pub(crate) fn player_skips_opening_hand(state: &GameState, id: PlayerId) -> bool {
    is_horde_seat(state, id)
}

/// The Horde SIDE is defeated (and the survivors win) when EVERY Horde seat's
/// library is empty AND no Horde seat controls a creature on the battlefield.
/// This is the Horde-variant loss condition consumed by
/// `elimination::check_game_over` in place of the generic archenemy "still
/// living" check — a Horde seat has no life total, so it can never be eliminated
/// by the ordinary life/poison state-based actions. Casual-format rule (no CR
/// number); it stands in for the archenemy-alive predicate of CR 104.2a's win
/// check.
///
/// For a two-Horde-force deck (LOTR Two Towers) the fold is a conjunction: "The
/// Horde loses when both Horde libraries have no cards" — one Horde running out
/// while the other still has cards or creatures does NOT end the game.
///
/// Boss-deck games (Turtle Team-Up) take a different branch: the Boss seat
/// keeps a real per-stage life total, so defeat is plain elimination — the
/// final stage falling to zero lets the normal CR 704.5a loss proceed. The
/// library-empty clause must NOT apply here: the event deck empties routinely
/// mid-game and Villain Scheme! reshuffles it.
pub(crate) fn horde_is_defeated(state: &GameState) -> bool {
    let seats = horde_seats(state);
    if seats.is_empty() {
        return false;
    }
    if has_boss_stages(state) {
        return seats.iter().all(|seat| {
            state
                .players
                .iter()
                .find(|p| p.id == *seat)
                .is_some_and(|p| p.is_eliminated)
        });
    }
    let all_libraries_empty = seats.iter().all(|seat| {
        state
            .players
            .iter()
            .find(|p| p.id == *seat)
            .is_none_or(|p| p.library.is_empty())
    });
    // CR 702.26b: phased-out permanents are treated as though they don't
    // exist, so only PHASED-IN creatures keep the Horde alive. Without this a
    // single phased-out boss (the advanced rule phases every deployed boss out)
    // would hold the creature clause open forever and the Horde could never be
    // defeated by decking.
    let a_horde_controls_a_creature = state.battlefield.iter().any(|id| {
        state.objects.get(id).is_some_and(|obj| {
            seats.contains(&obj.controller)
                && !obj.is_phased_out()
                && obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Creature)
        })
    });
    all_libraries_empty && !a_horde_controls_a_creature
}

/// The wave policy for this Horde game, if any.
fn wave_policy(state: &GameState) -> Option<WaveTermination> {
    state.format_config.horde_ruleset.as_ref().map(|r| r.wave)
}

/// Seed value for the Horde's precombat-main wave counter.
///
/// - `FixedCount(n)`: the wave reveals exactly `n` cards, so seed `n`.
/// - `UntilNonToken` / `UntilRarityAtLeast(_)`: the wave reveals until a
///   terminating card resolves (the first non-token, or the first card at/above
///   a rarity threshold — see [`maybe_reveal_next`]). The counter is only a
///   *safety bound* for these — seed it with the Horde's current library size so
///   the loop still terminates if the library empties before the terminating
///   card appears.
///
/// Live-state bonuses layer on via [`boss_wave_bonus`] (the first consumer is
/// Turtle Team-Up's survivor-scaled Action phase).
fn wave_seed(state: &GameState) -> u32 {
    let base = match wave_policy(state) {
        Some(WaveTermination::FixedCount(n)) => n,
        Some(WaveTermination::UntilNonToken { .. })
        | Some(WaveTermination::UntilRarityAtLeast(_)) => active_horde_seat(state)
            .and_then(|horde| state.players.iter().find(|p| p.id == horde))
            .map_or(0, |p| p.library.len() as u32),
        None => 0,
    };
    base.saturating_add(boss_wave_bonus(state))
}

/// Extra wave cards from live boss-deck state (Turtle Team-Up): +1 with three
/// or more survivors (the "3 Events" Action phase), +1 while Krang, Dimension
/// X Overlord is deployed ("the bosses play an additional card each turn").
/// Zero for every classic deck.
fn boss_wave_bonus(state: &GameState) -> u32 {
    if !has_boss_stages(state) {
        return 0;
    }
    let survivors = state.players.len().saturating_sub(horde_seats(state).len());
    u32::from(survivors >= 3) + u32::from(crate::game::boss::is_krang_deployed(state))
}

/// How many NON-token cards must resolve to end this Horde turn's wave.
///
/// Only meaningful for `UntilNonToken`; every other policy terminates on a
/// different axis (a fixed card count, or a rarity threshold) and reports `0`.
/// The count is resolved against `turn_index` because a schedule may vary per
/// turn — `WaveCount::Snaking` ramps up and back down.
fn wave_nontoken_seed(state: &GameState, turn_index: u32) -> u32 {
    match wave_policy(state) {
        Some(WaveTermination::UntilNonToken { count }) => count.nontokens_for_turn(turn_index),
        _ => 0,
    }
}

/// Whether revealing-and-casting a NON-TOKEN card of the given rarity ends the
/// wave under `policy`.
///
/// - `UntilNonToken { .. }`: ends once the wave's required number of nontokens
///   have resolved — i.e. when the remaining count reaches zero AFTER this card
///   is accounted for. `nontokens_remaining_after` is that post-decrement value.
/// - `UntilRarityAtLeast(t)`: only a card whose rarity is at least `t` ends it; a
///   below-threshold (common) card is cast and the wave CONTINUES, exactly like
///   `FixedCount` — the per-card decrement in [`maybe_reveal_next`] already
///   accounts for it.
/// - `FixedCount` / no policy: never ends the wave here; the counter alone
///   governs.
///
/// A card of unknown rarity (`None`) never ends a rarity wave, so a card-data gap
/// fails safe toward "keep revealing" rather than stalling the Horde on turn one.
///
/// Split out as a pure function so the whole policy matrix is testable without
/// driving a free cast through the stack.
fn wave_ends_after_nontoken(
    policy: Option<WaveTermination>,
    rarity: Option<Rarity>,
    nontokens_remaining_after: u32,
) -> bool {
    match policy {
        Some(WaveTermination::UntilNonToken { .. }) => nontokens_remaining_after == 0,
        Some(WaveTermination::UntilRarityAtLeast(threshold)) => {
            rarity.is_some_and(|r| r >= threshold)
        }
        Some(WaveTermination::FixedCount(_)) | None => false,
    }
}

/// Seed the Horde's precombat-main wave counters. Called from
/// `turns::finish_enter_phase` as the Horde's precombat main begins, taking the
/// place of the (no-op) archenemy `set_in_motion` turn-based action for a Horde
/// game. This only sets the counters; the actual reveal-and-resolve is driven one
/// card at a time by [`maybe_reveal_next`] (see the module docs for why the cast
/// cannot happen here).
///
/// The Horde turn index advances here (and only here), so a per-turn wave
/// schedule (`WaveCount::Snaking`) reads a stable index for the whole wave.
pub(crate) fn begin_wave(state: &mut GameState, _events: &mut [GameEvent]) {
    if !is_horde_turn(state) {
        return;
    }
    let turn_index = state.horde_turn_index;
    state.horde_wave_remaining = wave_seed(state);
    state.horde_wave_nontokens_remaining = wave_nontoken_seed(state, turn_index);
    // Advance for the NEXT Horde turn; this turn's wave keeps `turn_index`.
    state.horde_turn_index = turn_index.saturating_add(1);

    // LOTR "Two Towers" (Reading B): the two commanders alternate Horde turns.
    // `begin_wave` runs only on a REAL Horde turn (a setup-skipped turn never
    // reaches its precombat main), so this is the skip-safe point to rotate the
    // active-commander cursor for the NEXT Horde turn. No-op for a single-Horde
    // game (`active_horde_index` stays 0, unread).
    let horde_seat_count = state.horde_seats.len();
    if horde_seat_count >= 2 {
        state.active_horde_index = (state.active_horde_index + 1) % horde_seat_count;
    }
}

/// If a Horde wave is in progress and the stack is empty, reveal-and-resolve
/// the next card and decrement the wave counter. Returns `Some(new_waiting_for)`
/// when a card was revealed (the caller must adopt it), or `None` when the wave
/// is not eligible to advance this beat (non-empty stack, counter exhausted, or
/// an empty library).
///
/// The gate is deliberately turn- and phase-agnostic: a positive budget outside
/// the Horde's precombat main can only be `play_top_cards` residue (Savanti
/// Romero's mid-combat "the bosses play a card", paused on a spell or a
/// hero-event prompt — the wave budget always exhausts before the precombat
/// main can end, and `begin_wave` reseeds only on Horde precombat entry), and
/// that residue must continue on the next empty-stack priority beat, not strand
/// until the next wave overwrites it.
pub(crate) fn maybe_reveal_next(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    // Gate: only advance the wave when the stack is clear (the previous card
    // resolved) and budget remains — or a parked tail is waiting. No
    // turn/phase gate (see the doc comment): a positive budget IS the
    // wave-active marker, in-window or residue. Pending resume work runs
    // independently of fresh budget: the reveal already consumed its budget
    // before parking, so a pause on the wave's LAST card would otherwise
    // strand its cast/dispatch after the replacement answer (the budget gate
    // below would close before the drain inside `reveal_and_resolve_top_card`
    // runs). The budget check inside the reveal path still stops cleanly
    // once the tail settles, so no extra card is revealed.
    if (state.horde_wave_remaining == 0 && state.pending_horde_reveal.is_none())
        || !state.stack.is_empty()
    {
        return None;
    }
    let horde = active_horde_seat(state)?;

    // Reveal-and-resolve cards until either the wave pauses on a nontoken spell
    // (which is put on the stack and resolved through the normal priority loop
    // before the `engine::pass_priority_once` re-entry seam re-invokes this
    // function), an Event parks a hero-choice prompt, a token-creation
    // replacement parks a choice prompt (same seam resumes both), or the wave
    // budget / library is exhausted.
    //
    // Tokens must be handled in a LOOP here rather than one-per-beat: a token is
    // never cast (CR 111) and enters the battlefield synchronously, so it never
    // goes on the stack and cannot pause the wave on the stack. The re-entry
    // seam only fires after a Horde SPELL resolves and priority returns to the
    // Horde with the stack becoming empty; a token that returned `None` (stack
    // already empty, `waiting_for` unchanged) would strand every later card in
    // the wave. So on a token we resolve its ETB and immediately `continue` to
    // the next card — unless its creation paused on a replacement choice, which
    // returns `Pause` for the seam like any other prompt.
    loop {
        match reveal_and_resolve_top_card(state, horde, events) {
            RevealOutcome::Continue => {}
            RevealOutcome::Pause(waiting) => return Some(*waiting),
            RevealOutcome::Done => return None,
        }
    }
}

/// Outcome of resolving one library card through the shared single-card wave
/// driver ([`reveal_and_resolve_top_card`]).
enum RevealOutcome {
    /// A token entered / amassed, or an Event resolved synchronously: the
    /// caller should reveal the next card.
    Continue,
    /// A nontoken spell went on the stack, an Event parked a hero-choice (or
    /// nested replacement) prompt, or a token reveal parked a token-creation
    /// replacement prompt: the wave pauses until it resolves, then the
    /// priority re-entry seam continues the remaining budget.
    /// Boxed: `WaitingFor` is ~1.7KB and the sibling variants carry nothing.
    Pause(Box<WaitingFor>),
    /// Nothing more to do (budget exhausted, empty library, redirected reveal,
    /// or a failed cast).
    Done,
}

/// Savanti Romero ("the bosses play a card") entry: seed `count` extra reveals
/// and drive them immediately, ungated (Savanti triggers on combat damage, mid
/// combat). On `Pause` (a nontoken spell went on the stack, or an Event parked
/// a hero-choice prompt) the remaining budget stays seeded: the priority
/// re-entry seam is turn- and phase-agnostic, so it continues the residue on
/// the next empty-stack priority beat, mid-combat included.
pub(crate) fn play_top_cards(
    state: &mut GameState,
    horde: PlayerId,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    state.horde_wave_remaining = state.horde_wave_remaining.saturating_add(count);
    loop {
        if state.horde_wave_remaining == 0 {
            break;
        }
        match reveal_and_resolve_top_card(state, horde, events) {
            RevealOutcome::Continue => {}
            RevealOutcome::Pause(_) | RevealOutcome::Done => break,
        }
    }
}

/// Reveal-and-resolve the top card of the Horde's library (ungated): consumes
/// one wave budget and dispatches Army-amass / ordinary-token ETB / Event
/// one-shot / nontoken free-cast. Shared by the gated wave loop
/// ([`maybe_reveal_next`]) and the mid-combat "play a card" entry
/// ([`play_top_cards`]).
fn reveal_and_resolve_top_card(
    state: &mut GameState,
    horde: PlayerId,
    events: &mut Vec<GameEvent>,
) -> RevealOutcome {
    // Resume a reveal parked mid-move by a CR 616.1 replacement choice. This
    // runs BEFORE the budget gate: the budget was already consumed for the
    // parked card, and the tail below must run even when nothing remains.
    if let Some(pending) = state.pending_horde_reveal.take() {
        return resume_pending_horde_reveal(state, horde, *pending, events);
    }
    // Wave budget exhausted (e.g. after the last card was a token that
    // decremented the counter to zero) — stop cleanly.
    if state.horde_wave_remaining == 0 {
        return RevealOutcome::Done;
    }

    // Reveal the top card of the Horde's library.
    let Some(card_id) = state
        .players
        .iter()
        .find(|p| p.id == horde)
        .and_then(|p| p.library.front().copied())
    else {
        // Empty library: nothing more to reveal this wave. The Horde loses by
        // decking out (checked by `horde_is_defeated`); here we simply end
        // the wave cleanly, zeroing the budget so an additive
        // `play_top_cards` seed (Krang/Savanti "play a card") cannot
        // accumulate a stale residue across turns.
        state.horde_wave_remaining = 0;
        return RevealOutcome::Done;
    };

    // Consume one from the wave budget regardless of the branch taken so the
    // wave always terminates.
    state.horde_wave_remaining = state.horde_wave_remaining.saturating_sub(1);

    let is_token = state.objects.get(&card_id).is_some_and(|obj| obj.is_token);

    if is_token {
        if state
            .objects
            .get(&card_id)
            .is_some_and(crate::game::boss::is_event)
        {
            // Turtle Team-Up: a revealed Event resolves its one-shot (no
            // stack) — synchronously when it needs no decisions, or pausing
            // the wave on a hero-choice prompt that the re-entry seam resumes.
            match resolve_revealed_event(state, horde, card_id, events) {
                crate::game::boss::HeroEventProgress::Settled => {}
                crate::game::boss::HeroEventProgress::Prompted => {
                    return RevealOutcome::Pause(Box::new(state.waiting_for.clone()));
                }
            }
        } else if let Some(subtype) = revealed_army_subtype(state, card_id) {
            // LOTR "Two Towers" rule "Orc Armies grow as a single army": a
            // revealed Army token does NOT enter as a fresh 0/0 (which would
            // die to SBAs immediately) — it AMASSES the Horde's single Army
            // instead. Any other token enters normally. The Amass can pause
            // on a replacement choice like an ordinary token reveal.
            return amass_revealed_army(state, horde, card_id, &subtype, events);
        } else {
            // CR 111 + CR 111.1: a token can never be cast, so a revealed library
            // token is put directly onto the battlefield under the Horde's
            // control. Enters synchronously (no stack) — unless a
            // token-creation replacement needs a choice (an Esix/Jinnie-style
            // "instead"), which parks the reveal and pauses the wave for the
            // priority re-entry seam.
            return reveal_library_token(state, card_id, horde, events);
        }
        return RevealOutcome::Continue;
    }

    // Capture the card's library rarity BEFORE it leaves the library, for the
    // `UntilRarityAtLeast` end-of-wave check below. (The object survives the
    // exile + free-cast, but reading it here keeps the rarity next to the
    // reveal and independent of what the cast does to the object.)
    let revealed_rarity = state
        .objects
        .get(&card_id)
        .and_then(|o| o.horde_library_rarity);

    // Nontoken: cast it for free during resolution. Mirror Cascade — move the
    // card to exile first (CR 601.2a: the reveal), then drive the free cast
    // through the single free-cast authority (`cast_from_zone::resolve`'s
    // `driver_free_cast` gate, CR 608.2g), which casts from the card's current
    // (exile) zone at zero cost. X-cost nontokens resolve at X = 0 (CR 601.2b),
    // exactly like Cascade/Discover. The exile runs the full zone-change
    // pipeline so replacements consult (Cascade's own shape — the card
    // attributes its own wave move; a turn-based action has no ability
    // source). A CR 616.1 choice parks the reveal in `pending_horde_reveal`
    // for the resume above.
    match execute_zone_move(
        state,
        card_id,
        Zone::Library,
        Zone::Exile,
        card_id,
        None,
        false,
        EtbTapState::Unspecified,
        false,
        None,
        &[],
        None,
        false,
        None,
        None,
        events,
    ) {
        ZoneMoveResult::Done => {}
        ZoneMoveResult::NeedsChoice(player) => {
            state.pending_horde_reveal = Some(Box::new(
                crate::types::game_state::PendingHordeReveal::NontokenCast {
                    card: card_id,
                    revealed_rarity,
                },
            ));
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            return RevealOutcome::Pause(Box::new(state.waiting_for.clone()));
        }
        // Non-battlefield destination: no Aura attachment choice can arise.
        ZoneMoveResult::NeedsAuraAttachmentChoice => {
            debug_assert!(false, "wave exile cannot need an attach choice");
            return RevealOutcome::Done;
        }
    }
    if state.objects.get(&card_id).map(|o| o.zone) != Some(Zone::Exile) {
        // A replacement effect redirected the reveal; do not attempt to cast.
        return RevealOutcome::Done;
    }

    cast_revealed_nontoken(state, horde, card_id, revealed_rarity, events)
}

/// Free-cast a revealed nontoken already in exile through the Cascade/Discover
/// authority, then apply the wave quota + end-of-wave policy. Shared by the
/// reveal path and the post-replacement resume — both reach here only with
/// the card verified in exile.
fn cast_revealed_nontoken(
    state: &mut GameState,
    horde: PlayerId,
    card_id: ObjectId,
    revealed_rarity: Option<Rarity>,
    events: &mut Vec<GameEvent>,
) -> RevealOutcome {
    let ability = free_cast_ability(card_id, horde);
    match crate::game::effects::cast_from_zone::resolve(state, &ability, events) {
        Ok(()) => {
            // This card is a resolved NON-token, so it counts against an
            // `UntilNonToken` wave's quota (saturating: other policies leave
            // the quota at 0 and are unaffected).
            state.horde_wave_nontokens_remaining =
                state.horde_wave_nontokens_remaining.saturating_sub(1);

            // Ending the wave clears the counter so the priority re-entry seam
            // (which re-invokes this function once the spell resolves) reveals
            // nothing more, leaving the rest of the library for the next Horde
            // turn. See [`wave_ends_after_nontoken`] for the policy matrix.
            if wave_ends_after_nontoken(
                wave_policy(state),
                revealed_rarity,
                state.horde_wave_nontokens_remaining,
            ) {
                state.horde_wave_remaining = 0;
            }
            RevealOutcome::Pause(Box::new(state.waiting_for.clone()))
        }
        Err(_) => RevealOutcome::Done,
    }
}

/// Resume a wave reveal parked mid-move by a CR 616.1 replacement choice. The
/// choice has resolved and the pipeline has delivered (or redirected) the
/// move; run only the parked tail — never consume budget or reveal again.
fn resume_pending_horde_reveal(
    state: &mut GameState,
    horde: PlayerId,
    pending: crate::types::game_state::PendingHordeReveal,
    events: &mut Vec<GameEvent>,
) -> RevealOutcome {
    use crate::types::game_state::PendingHordeReveal;
    match pending {
        PendingHordeReveal::NontokenCast {
            card,
            revealed_rarity,
        } => {
            if state.objects.get(&card).map(|o| o.zone) != Some(Zone::Exile) {
                // A replacement effect redirected the reveal; do not attempt to cast.
                return RevealOutcome::Done;
            }
            cast_revealed_nontoken(state, horde, card, revealed_rarity, events)
        }
        PendingHordeReveal::EventDispatch { card, fired } => {
            // The one-shot fires from wherever the pipeline delivered the
            // Event: pile redirection changes only where it lands.
            match crate::game::boss::resolve_boss_event(state, horde, card, &fired, 0, events) {
                crate::game::boss::HeroEventProgress::Settled => RevealOutcome::Continue,
                crate::game::boss::HeroEventProgress::Prompted => {
                    RevealOutcome::Pause(Box::new(state.waiting_for.clone()))
                }
            }
        }
        PendingHordeReveal::TokenCreation {
            card,
            before,
            pinned,
            revealed_name,
        } => {
            // The choice resolved; the shared CreateToken resume arm already
            // delivered (or prevented) the creation. Run the post-creation
            // tail over whatever it delivered — pinned-ability recovery,
            // Ninja grafts, placeholder removal — then continue the wave.
            complete_token_reveal(state, card, horde, &pinned, &before, &revealed_name);
            RevealOutcome::Continue
        }
        PendingHordeReveal::AmassArmy { card } => {
            // The Amass replacement choice resolved and its stashed
            // continuation already ran (counters placed). Remove the consumed
            // library placeholder, then continue the wave.
            // allow-raw-zone: consumed library placeholder destroyed after the Amass continuation settled (remove + object delete); not a CR 400.7 zone change, no ZoneChanged is emitted.
            crate::game::zones::remove_from_zone(state, card, Zone::Library, horde);
            state.objects.remove(&card);
            RevealOutcome::Continue
        }
    }
}

/// Turtle Team-Up: resolve a revealed Event. Banked Baxters fire on every
/// reveal (Baxter's Experiments itself included — it is an Event), so the
/// bank drains first and rides the dispatch as `fired`. The Event moves to
/// the Boss graveyard FIRST (so "this card" and graveyard reads see post-move
/// zones) — except Baxter's Experiments, which is banked beside the event
/// deck (exile; horde tokens never cease to exist) for a later event to fire.
/// A boss-deck library holds only tokens and Events, so the
/// budget-consumption + empty-library semantics match the token path exactly.
fn resolve_revealed_event(
    state: &mut GameState,
    horde: PlayerId,
    card_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> crate::game::boss::HeroEventProgress {
    let fired = std::mem::take(&mut state.baxter_set_aside);
    // The Event moves to its pile through the full zone-change pipeline so
    // replacements consult (a graveyard-replacement exiles would-be discards);
    // a CR 616.1 choice parks the dispatch in `pending_horde_reveal` for the
    // wave resume. Baxter's Experiments banks in exile for a later event to
    // fire; every other Event goes to the Boss graveyard (the event discard
    // pile). The Event attributes its own pile move; a turn-based action has
    // no ability source.
    let dest = if crate::game::boss::boss_name(state, card_id)
        .is_some_and(|name| name == "Event: Baxter's Experiments")
    {
        Zone::Exile
    } else {
        Zone::Graveyard
    };
    match execute_zone_move(
        state,
        card_id,
        Zone::Library,
        dest,
        card_id,
        None,
        false,
        EtbTapState::Unspecified,
        false,
        None,
        &[],
        None,
        false,
        None,
        None,
        events,
    ) {
        ZoneMoveResult::Done => {}
        ZoneMoveResult::NeedsChoice(player) => {
            state.pending_horde_reveal = Some(Box::new(
                crate::types::game_state::PendingHordeReveal::EventDispatch {
                    card: card_id,
                    fired: fired.clone(),
                },
            ));
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            return crate::game::boss::HeroEventProgress::Prompted;
        }
        // Non-battlefield destination: no Aura attachment choice can arise.
        ZoneMoveResult::NeedsAuraAttachmentChoice => {
            debug_assert!(false, "event pile move cannot need an attach choice");
            return crate::game::boss::HeroEventProgress::Settled;
        }
    }
    // The one-shot fires from wherever the pipeline delivered the Event (see
    // the resume path): pile redirection changes only where it lands.
    crate::game::boss::resolve_boss_event(state, horde, card_id, &fired, 0, events)
}

/// The Amass flavor subtype of a revealed library token when it is an Army
/// creature token (LOTR's "Orc Army" — subtypes `["Orc", "Army"]`), else `None`.
/// The flavor subtype ("Orc") is the non-"Army" creature subtype; a bare "Army"
/// token with no flavor returns `None` and reveals as an ordinary token. This is
/// the dispatch key in [`maybe_reveal_next`]: a revealed Army token amasses the
/// Horde's single Army rather than entering as a fresh (dying) 0/0 token.
fn revealed_army_subtype(state: &GameState, card_id: ObjectId) -> Option<String> {
    use crate::types::card_type::CoreType;
    let obj = state.objects.get(&card_id)?;
    if !obj.card_types.core_types.contains(&CoreType::Creature)
        || !obj.card_types.subtypes.iter().any(|s| s == "Army")
    {
        return None;
    }
    obj.card_types
        .subtypes
        .iter()
        .find(|s| *s != "Army")
        .cloned()
}

/// LOTR "Two Towers" rule "Orc Armies grow as a single army": a revealed Army
/// library token amasses the Horde's single Army by 1 (CR 701.47a) rather than
/// entering as a fresh 0/0 token that would die to state-based actions. Composes
/// the shared Amass resolver ([`crate::game::effects::amass::resolve`]): it grows
/// the Army the Horde already controls, or creates it (0/0, then a +1/+1 counter
/// → 1/1) if the Horde controls none.
///
/// Like [`reveal_library_token`], a replacement choice parked mid-Amass (an
/// Esix/Jinnie-style "instead" on the Army creation — the resolver reports
/// `Ok` either way, so a non-Priority `waiting_for` afterwards is the pause
/// signal) parks the reveal and pauses the wave; the placeholder is removed
/// only once the continuation settles. A settled Amass (created or prevented —
/// prevention consumes the reveal) removes the placeholder and continues.
fn amass_revealed_army(
    state: &mut GameState,
    horde: PlayerId,
    card_id: ObjectId,
    subtype: &str,
    events: &mut Vec<GameEvent>,
) -> RevealOutcome {
    use crate::types::ability::QuantityExpr;
    // CR 701.47a: Amass [subtype] 1 for the Horde. The revealed Orc Army card is
    // the amass source; it is removed below once the Amass settles (amass grows
    // a *different* object, the Horde's battlefield Army, so ordering is safe —
    // and on a pause the placeholder must survive until the resume).
    let ability = ResolvedAbility::new(
        Effect::Amass {
            subtype: subtype.to_string(),
            count: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        Vec::new(),
        card_id,
        horde,
    );
    let _ = crate::game::effects::amass::resolve(state, &ability, events);
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        // CR 614.1a: the Army creation (or a counter placement) needs a
        // choice. Park the reveal — the placeholder stays until the choice
        // resolves — and pause the wave for the priority re-entry seam.
        state.pending_horde_reveal = Some(Box::new(
            crate::types::game_state::PendingHordeReveal::AmassArmy { card: card_id },
        ));
        return RevealOutcome::Pause(Box::new(state.waiting_for.clone()));
    }
    // allow-raw-zone: consumed library placeholder destroyed after the Amass authority resolved (remove + object delete); not a CR 400.7 zone change, no ZoneChanged is emitted.
    crate::game::zones::remove_from_zone(state, card_id, Zone::Library, horde);
    state.objects.remove(&card_id);
    RevealOutcome::Continue
}

/// Reveal a token from the top of the Horde's library onto the battlefield.
///
/// CR 111: a token can never be cast, so a token in the Horde's library is put
/// directly onto the battlefield when revealed. A FRESH battlefield token is
/// created (CR 111.1 + CR 111.2 — the Horde is the token's owner and its
/// controller) from the library object's stored body characteristics, routed
/// through the ordinary token-creation replacement pipeline exactly like any
/// other created token — so ETB triggers fire and the Horde emblem's haste +
/// must-attack apply. The original library placeholder object is then removed so
/// no duplicate remains (the M2 "create + remove" lifecycle, chosen over a
/// literal library→battlefield move). In a classic deck the fresh battlefield
/// token does NOT carry `is_horde_deck_token` (it is a normal token via
/// `create_object`), so it will cease to exist normally (CR 704.5d) if it
/// later leaves the battlefield. In a boss-deck game it inherits the flag:
/// dead Ninjas must linger in the Boss graveyard for Shredder's Challenge,
/// so the flag (SBA exemption + movement carve-out) follows the instance.
fn reveal_library_token(
    state: &mut GameState,
    card_id: ObjectId,
    horde: PlayerId,
    events: &mut Vec<GameEvent>,
) -> RevealOutcome {
    let Some(obj) = state.objects.get(&card_id) else {
        return RevealOutcome::Continue;
    };
    // Reconstruct the token body from the revealed library object (mirrors the
    // GameObject → TokenCharacteristics projection used by `token_copy`). The body
    // (P/T, types, colors, keywords) round-trips through `TokenCharacteristics`.
    //
    // A NON-keyword ability the printed token carries (a triggered/activated
    // ability, e.g. the self-replicating Ooze's "when this dies, create two 1/1
    // Oozes") is NOT on the body — it lives in the preset catalog's `rules_text`,
    // and is recovered AFTER creation below from the token's pinned
    // `token_image_ref` (stamped by `create_horde_library_token`). Vanilla /
    // keyword-only tokens (Cyberman, Zombie, Metallic Sliver) have `None` here and
    // are unchanged.
    let pinned_token_ref = obj.token_image_ref.clone();
    let revealed_name = obj.name.clone();
    let characteristics = TokenCharacteristics {
        display_name: obj.name.clone(),
        power: obj.power,
        toughness: obj.toughness,
        loyalty: obj.loyalty,
        core_types: obj.card_types.core_types.clone(),
        subtypes: obj.card_types.subtypes.clone(),
        supertypes: obj.card_types.supertypes.clone(),
        colors: obj.color.clone(),
        keywords: obj.keywords.clone(),
    };
    let spec = TokenSpec {
        characteristics,
        script_name: obj.name.clone(),
        static_abilities: Vec::new(),
        enter_with_counters: Vec::new(),
        tapped: false,
        enters_attacking: false,
        attack_request: TokenAttackRequest::Unspecified,
        sacrifice_at: None,
        // Token-creation source (plumbing, not a rule): there is no card/ability
        // source for a Horde reveal (it is a turn-based action), so the library
        // placeholder — still present at apply time; removed just below — stands
        // in as the source for the token-art lookup. No delayed trigger or
        // `enters_attacking` references it, so its removal leaves nothing dangling.
        source_id: card_id,
        controller: horde,
        attach_to: TokenHostRequest::NotRequested,
    };
    let proposed = ProposedEvent::CreateToken {
        owner: horde,
        spec: Box::new(spec),
        copy: None,
        enter_tapped: EtbTapState::Unspecified,
        count: 1,
        appended: Vec::new(),
        applied: HashSet::new(),
    };
    // CR 614.16: token-creation replacement effects apply to the revealed token.
    // Snapshot the battlefield so we can identify the token this reveal creates
    // (the create fns return `bool`, not ids).
    let before: HashSet<ObjectId> = state.battlefield.iter().copied().collect();
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            apply_create_token_after_replacement(state, event, events);
            complete_token_reveal(
                state,
                card_id,
                horde,
                &pinned_token_ref,
                &before,
                &revealed_name,
            );
            RevealOutcome::Continue
        }
        // CR 609.3: prevention consumes the reveal — no token enters, but the
        // placeholder is still removed (the wave consumed the card).
        ReplacementResult::Prevented => {
            complete_token_reveal(
                state,
                card_id,
                horde,
                &pinned_token_ref,
                &before,
                &revealed_name,
            );
            RevealOutcome::Continue
        }
        // CR 614.1a: a token-creation replacement needs a choice — an
        // Esix/Jinnie-style "instead" substitute the Horde controls applies to
        // the reveal's token creation. Park the reveal (the placeholder stays
        // until the choice resolves) and pause the wave; the priority
        // re-entry seam resumes via `pending_horde_reveal`.
        ReplacementResult::NeedsChoice(player) => {
            state.pending_horde_reveal = Some(Box::new(
                crate::types::game_state::PendingHordeReveal::TokenCreation {
                    card: card_id,
                    before,
                    pinned: pinned_token_ref,
                    revealed_name,
                },
            ));
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            RevealOutcome::Pause(Box::new(state.waiting_for.clone()))
        }
    }
}

/// Post-creation tail of a revealed token: pinned-ability recovery, Ninja
/// grafts, the boss-deck flag, and placeholder removal. Shared by the
/// synchronous path and the post-replacement resume — both reach here only
/// after the CreateToken event settled (delivered or prevented).
fn complete_token_reveal(
    state: &mut GameState,
    card_id: ObjectId,
    horde: PlayerId,
    pinned_token_ref: &Option<TokenImageRef>,
    before: &HashSet<ObjectId>,
    revealed_name: &str,
) {
    // CR 111.3 + CR 111.4: recover the token's printed non-keyword abilities from
    // its pinned preset identity. The generic create path already ran
    // `inject_resolved_token_abilities`, but it matched identity by CHARACTERISTICS
    // (`find_exact_token_ref`), which is ambiguous for a subtype body like "Ooze"
    // (six catalog bodies) and cannot pick the right printing. Re-stamp the exact
    // pinned `token_image_ref` and re-run the SAME injection so the catalog
    // `rules_text` (the Ooze's dies trigger) is materialized from the correct
    // preset — the identical path that gives the SOS Pest its trigger. Skip when
    // the create path already resolved this exact preset (nothing to add — avoids
    // a double install) or when there is no pinned identity (vanilla/keyword
    // tokens: nothing to recover).
    //
    // Substitute guard: recovery re-stamps the REVEAL's identity, so it applies
    // only to products still carrying the revealed name. An accepted Esix-style
    // substitute creates copies of another creature, which must not inherit the
    // reveal's preset (their copiable values came from the chosen source).
    if let Some(pinned) = pinned_token_ref {
        let created: Vec<ObjectId> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| !before.contains(id))
            .collect();
        for id in created {
            if !state
                .objects
                .get(&id)
                .is_some_and(|o| o.name.as_str() == revealed_name)
            {
                continue;
            }
            let already_pinned = state
                .objects
                .get(&id)
                .and_then(|o| o.token_image_ref.as_ref())
                .is_some_and(|r| r.preset_id == pinned.preset_id);
            if already_pinned {
                continue;
            }
            if let Some(o) = state.objects.get_mut(&id) {
                o.token_image_ref = Some(pinned.clone());
            }
            crate::game::effects::token::inject_resolved_token_abilities(state, id);
        }
        // A materialized static ability (rare for Horde tokens, but possible)
        // affects the layer system; a triggered ability does not, but flushing is
        // cheap and keeps derived state correct in every case.
        crate::game::layers::flush_layers(state);
    }

    // Turtle Team-Up: graft revealed Ninjas' non-keyword statics. The catalog
    // `rules_text` carries them for display, but "can't block" / "can't be
    // blocked" lines don't materialize through token injection, so the code
    // synthesizer stamps them here. Name-keyed: every other deck's tokens
    // no-op, and grafted statics need the same layer flush as above.
    //
    // The same loop inherits the Horde-deck-token flag in boss-deck games:
    // dead Ninjas must linger in the Boss graveyard for Shredder's Challenge
    // (and ride Villain Scheme! back), so the flag — SBA exemption plus
    // movement carve-out — follows the instance past the battlefield. Classic
    // decks skip this: their revealed tokens stay normal tokens that cease
    // normally (CR 704.5d) on leaving the battlefield.
    let revealed: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| !before.contains(id))
        .collect();
    let boss_game = has_boss_stages(state);
    for id in revealed {
        crate::game::boss::graft_ninja_abilities(state, id);
        if boss_game {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.is_horde_deck_token = true;
            }
        }
    }
    crate::game::layers::flush_layers(state);

    // Remove the library placeholder so no duplicate token remains.
    // allow-raw-zone: placeholder destroyed after the fresh token entered via the token-creation pipeline (M2 create + remove); not a CR 400.7 zone change, no ZoneChanged is emitted.
    crate::game::zones::remove_from_zone(state, card_id, Zone::Library, horde);
    state.objects.remove(&card_id);
}

/// Build the synthetic `Effect::CastFromZone` that free-casts a single revealed
/// nontoken card during resolution (the Cascade/Discover authority). The card is
/// supplied as the sole explicit target so `cast_from_zone::resolve` routes it to
/// the `driver_free_cast` gate. The Horde is the controller so the spell resolves
/// (and any permanent enters) under the Horde's control.
fn free_cast_ability(card_id: ObjectId, horde: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::CastFromZone {
            target: TargetFilter::Any,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::DuringResolution,
            mana_spend_permission: None,
            additional_cost: None,
            cast_cost_modifier: None,
        },
        vec![TargetRef::Object(card_id)],
        card_id,
        horde,
    )
}

/// This Horde game's post-combat activation policy (`None` for basic decks and
/// non-Horde formats).
fn post_combat_activation_policy(
    state: &GameState,
) -> crate::types::format::HordePostCombatActivation {
    use crate::types::format::HordePostCombatActivation;
    state
        .format_config
        .horde_ruleset
        .as_ref()
        .map_or(HordePostCombatActivation::None, |r| {
            r.post_combat_activation
        })
}

/// Seed the Horde's post-combat activation queue as its post-combat main phase
/// begins — the sibling of [`begin_wave`] for the precombat wave, dispatched from
/// `turns::finish_enter_phase`.
///
/// Under the advanced `OncePerPermanent` rule the Horde activates each of its
/// permanents' abilities once after combat. Snapshotting the permanents it
/// controls NOW enforces "once per permanent per turn" structurally: a permanent
/// that enters later (e.g. from an activation's own effect) is not in the queue,
/// so it does not activate this turn. No-op for basic decks / non-Horde formats.
///
/// Caveat (shared with [`begin_wave`]): the snapshot is per post-combat-main
/// ENTRY, not per turn, so an *additional* post-combat main phase this turn (a
/// rare extra-phase effect) would re-seed and let each permanent activate again.
/// This mirrors the precombat wave's identical per-entry re-seed and is latent —
/// no shipped deck enables this axis. Promote to a per-turn latch if an advanced
/// deck ever pairs post-combat activation with extra-phase generation.
pub(crate) fn begin_post_combat_activation(state: &mut GameState, _events: &mut [GameEvent]) {
    use crate::types::format::HordePostCombatActivation;
    if !is_horde_turn(state)
        || post_combat_activation_policy(state) != HordePostCombatActivation::OncePerPermanent
    {
        return;
    }
    let Some(horde) = active_horde_seat(state) else {
        return;
    };
    // Battlefield order; each id is TRIED once, and its eligibility is re-checked
    // at activation time because the board changes as earlier abilities resolve.
    state.horde_postcombat_activation_queue = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.controller == horde))
        .collect();
}

/// The index of the first non-mana, non-loyalty activated ability the Horde can
/// legally activate on `source_id` right now, if any.
///
/// "Any OTHER ability" (the rule) excludes mana abilities (CR 605, the single
/// `is_mana_ability` authority); planeswalker loyalty abilities are their own
/// once-per-turn/sorcery-timing subsystem and are left out of this beat. Which
/// eligible ability to pick is the first by index — a stable engine default;
/// targets/modes are chosen by the Horde's AI seat at resolution. Mirrors the AI
/// legal-action idiom in `ai_support/candidates.rs`.
fn first_eligible_activated_ability(
    state: &GameState,
    horde: PlayerId,
    source_id: ObjectId,
    gates: &crate::game::restrictions::ActivationRestrictionStaticGates,
) -> Option<usize> {
    use crate::types::ability::{is_loyalty_ability_cost, AbilityKind};
    crate::game::casting::activated_ability_definitions(state, source_id)
        .into_iter()
        .find(|(i, def)| {
            def.kind == AbilityKind::Activated
                && !crate::game::mana_abilities::is_mana_ability(def)
                && !def.cost.as_ref().is_some_and(is_loyalty_ability_cost)
                && !tap_ability_summoning_sick_for_horde(state, source_id, def)
                && crate::game::casting::can_activate_ability_now_with_restriction_gates(
                    state, horde, source_id, *i, gates,
                )
        })
        .map(|(i, _)| i)
}

/// Hordemagic advanced rule: "Card-activated abilities have summoning sickness."
/// The Horde emblem grants Haste so its creatures can ATTACK the turn they enter
/// (and the wave deploys them mid-turn), but that Haste must NOT also lift the
/// CR 302.6 summoning-sickness gate on their `{T}`/`{Q}` activated abilities — the
/// rule explicitly keeps those sick. So gate tap-cost abilities on the haste-BLIND
/// summoning-sick flag rather than `can_activate_ability_now`'s haste-aware
/// [`crate::game::combat::has_summoning_sickness`]: a creature the Horde has not
/// controlled since the start of its turn (`obj.summoning_sick`) cannot use a
/// `{T}`/`{Q}` ability this turn, Haste notwithstanding. Non-creatures are never
/// summoning-sick (CR 302.6), and non-tap abilities aren't gated at all.
fn tap_ability_summoning_sick_for_horde(
    state: &GameState,
    source_id: ObjectId,
    def: &crate::types::ability::AbilityDefinition,
) -> bool {
    let has_tap_cost = crate::game::mana_sources::has_tap_component(&def.cost)
        || crate::game::mana_sources::has_untap_component(&def.cost);
    if !has_tap_cost {
        return false;
    }
    state.objects.get(&source_id).is_some_and(|obj| {
        obj.card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Creature)
            && obj.summoning_sick
    })
}

/// If the Horde is at an empty-stack priority window during its post-combat main
/// with a non-empty activation queue, activate the next eligible permanent's
/// ability and return the resulting `WaitingFor` (target/mode prompts are answered
/// by the Horde's AI seat, exactly like a free-cast wave spell). Returns `None`
/// when the beat is ineligible (not the Horde's turn, wrong phase, non-empty stack,
/// empty queue, or the rule is off) or when no queued permanent has a usable
/// ability left.
///
/// Mirrors [`maybe_reveal_next`]: permanents with no usable ability never pause
/// the beat (loop past them), and the function returns as soon as one ability is
/// announced (which sets `waiting_for` and must resolve before the next).
pub(crate) fn maybe_activate_next_ability(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    use crate::types::format::HordePostCombatActivation;
    if state.phase != Phase::PostCombatMain
        || !state.stack.is_empty()
        || !is_horde_turn(state)
        || state.horde_postcombat_activation_queue.is_empty()
        || post_combat_activation_policy(state) != HordePostCombatActivation::OncePerPermanent
    {
        return None;
    }
    let horde = active_horde_seat(state)?;

    // Hordemagic "infinite mana (for … activation costs)": top up the Horde's pool
    // so mana components are payable. Real non-mana costs (tap/sacrifice/pay-life)
    // are still paid by the activation path. No-op unless the Horde was flagged
    // unbounded — the flag is applied by `deck_loading::grant_horde_emblem` and
    // kept alive across step transitions by `has_horde_infinite_mana`.
    crate::game::mana_payment::refill_infinite_mana(state);

    let gates = crate::game::restrictions::ActivationRestrictionStaticGates::compute(state);

    // Pop permanents until one announces an ability or the queue empties. A
    // permanent with no usable ability never pauses the beat (mirrors the token
    // branch of `maybe_reveal_next`).
    while !state.horde_postcombat_activation_queue.is_empty() {
        let source_id = state.horde_postcombat_activation_queue.remove(0);
        let Some(ability_index) = first_eligible_activated_ability(state, horde, source_id, &gates)
        else {
            continue;
        };
        match crate::game::casting::handle_activate_ability(
            state,
            horde,
            source_id,
            ability_index,
            events,
        ) {
            // The ability is announced (on the stack); its target/mode WaitingFor
            // is answered by the AI seat, then it resolves and the priority seam
            // re-invokes this for the next permanent.
            Ok(waiting) => return Some(waiting),
            // Defensive: the non-mutating pre-check passed but activation still
            // failed (a subtle late illegality). Skip this permanent and continue.
            Err(_) => continue,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::format::{ChallengeDeck, FormatConfig, WaveCount};

    // The self-replicating Ooze: 2/2 green Ooze, "When this creature dies, create
    // two 1/1 green Ooze creature tokens." (M11 #5 body of the SLD #2819 token).
    const OOZE_PRESET_ID: &str = "6d30428c-f846-584a-8458-55de11d00213";
    // A plain vanilla 2/2 Zombie — the negative control.
    const ZOMBIE_PRESET_ID: &str = "011a9246-7f7c-50c7-ab99-3fc13469c13b";

    /// Build a library token from `preset_id`, reveal it through the real
    /// `create_horde_library_token` → `reveal_library_token` round-trip, and
    /// return the resulting battlefield token.
    fn reveal_preset_token(preset_id: &str) -> crate::game::game_object::GameObject {
        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::DndHorde.default_ruleset()),
            2,
            42,
        );
        let horde = horde_seat(&state).expect("horde seat");
        let preset = crate::game::token_presets::known_token_preset_by_id(preset_id)
            .expect("preset must exist in the catalog");
        let token_image_ref = Some(crate::game::deck_loading::horde_token_image_ref(preset));
        let library_id = crate::game::deck_loading::create_horde_library_token(
            &mut state,
            &preset.body,
            token_image_ref,
            horde,
        );

        let mut events = Vec::new();
        reveal_library_token(&mut state, library_id, horde, &mut events);

        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .find(|o| o.is_token && o.controller == horde)
            .cloned()
            .expect("the revealed token must be on the battlefield")
    }

    /// A Horde library token whose catalog printing carries a non-keyword
    /// triggered ability enters the battlefield WITH that ability. This is the
    /// token-abilities fix: the library token is stamped with its preset
    /// identity, so the apply path materializes the catalog `rules_text` dies
    /// trigger — the self-replicating Ooze the D&D Horde depends on.
    #[test]
    fn revealed_library_token_keeps_its_catalog_dies_trigger() {
        let ooze = reveal_preset_token(OOZE_PRESET_ID);
        assert_eq!(ooze.name, "Ooze");
        let dies_triggers: Vec<_> = ooze
            .trigger_definitions
            .as_slice()
            .iter()
            .filter(|t| {
                t.definition.mode == crate::types::triggers::TriggerMode::ChangesZone
                    && t.definition.destination == Some(Zone::Graveyard)
            })
            .collect();
        assert_eq!(
            dies_triggers.len(),
            1,
            "the revealed Ooze must carry EXACTLY one 'when this dies' trigger (no double \
             install), got triggers: {:?}",
            ooze.trigger_definitions.as_slice()
        );
    }

    /// Negative control: a vanilla token (no catalog `rules_text`) reveals with NO
    /// injected triggers, so the fix cannot spuriously grant abilities to the many
    /// plain tokens the Horde decks use (Zombie, Metallic Sliver, Human Soldier).
    #[test]
    fn revealed_vanilla_token_gains_no_triggers() {
        let zombie = reveal_preset_token(ZOMBIE_PRESET_ID);
        assert_eq!(zombie.name, "Zombie");
        assert!(
            zombie.trigger_definitions.as_slice().is_empty(),
            "a vanilla token must gain no triggers, got: {:?}",
            zombie.trigger_definitions.as_slice()
        );
    }

    /// Advanced legendary rule (`EtbThenPhaseOut`): when the Horde is damage-
    /// milled, a legendary PERMANENT card enters the battlefield (its ETB event
    /// fires) and immediately phases out (CR 702.26), while non-legendaries and
    /// non-permanent legendaries mill to the graveyard normally. This is the
    /// building-block test for the whole advanced Horde deck family.
    #[test]
    fn damage_mill_deploys_and_phases_out_the_hordes_legendaries() {
        use crate::game::zones::create_object;
        use crate::types::card_type::{CoreType, Supertype};
        use crate::types::format::HordeLegendaryDeath;
        use crate::types::identifiers::CardId;

        let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
        ruleset.legendary_death = HordeLegendaryDeath::EtbThenPhaseOut;
        let mut state = GameState::new(FormatConfig::horde(ruleset), 2, 42);
        let horde = horde_seat(&state).expect("horde seat");

        // A fresh Horde state has an empty library (deck loading is separate), so
        // these three cards are the entire top-of-library to mill.
        let boss = create_object(
            &mut state,
            CardId(9001),
            horde,
            "Boss".into(),
            Zone::Library,
        );
        {
            let o = state.objects.get_mut(&boss).unwrap();
            o.card_types.supertypes.push(Supertype::Legendary);
            o.card_types.core_types.push(CoreType::Creature);
        }
        let grunt = create_object(
            &mut state,
            CardId(9002),
            horde,
            "Grunt".into(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&grunt)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let bolt = create_object(
            &mut state,
            CardId(9003),
            horde,
            "Legendary Bolt".into(),
            Zone::Library,
        );
        {
            // Legendary but a non-permanent type — it can't enter the battlefield.
            let o = state.objects.get_mut(&bolt).unwrap();
            o.card_types.supertypes.push(Supertype::Legendary);
            o.card_types.core_types.push(CoreType::Instant);
        }

        let mut events = Vec::new();
        mill_from_loss(&mut state, horde, 3, &mut events);

        let in_horde_graveyard = |state: &GameState, id: ObjectId| {
            state
                .players
                .iter()
                .find(|p| p.id == horde)
                .unwrap()
                .graveyard
                .contains(&id)
        };

        // The redirect DEPLOYED the boss: it entered the battlefield, not the
        // graveyard — and the completion phased it out synchronously, before
        // this call returned (no trigger resolution needed).
        assert!(
            state.battlefield.contains(&boss),
            "the legendary boss must enter the battlefield"
        );
        assert!(
            !in_horde_graveyard(&state, boss),
            "the boss must not be milled to the graveyard"
        );
        assert!(
            state.objects[&boss].is_phased_out(),
            "the boss must phase out synchronously on deployment (CR 702.26)"
        );

        // Grunt (non-legendary) and Bolt (legendary but non-permanent) mill normally.
        assert!(
            in_horde_graveyard(&state, grunt) && !state.battlefield.contains(&grunt),
            "a non-legendary card mills to the graveyard"
        );
        assert!(
            in_horde_graveyard(&state, bolt) && !state.battlefield.contains(&bolt),
            "a legendary INSTANT can't enter the battlefield — it mills normally"
        );

        // CR 603.6: the boss's entering-the-battlefield event fired — its own
        // ETBs key off it.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } if *object_id == boss
            )),
            "the boss's enters-the-battlefield event must fire"
        );

        // CR 701.17a + CR 603.2c: the advanced delivery publishes the ordinary
        // `Milled` events — one per card that left the library.
        for id in [boss, grunt, bolt] {
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    GameEvent::Milled { object_id, .. } if *object_id == id
                )),
                "every milled card must publish its Milled event"
            );
        }
    }

    /// CR 704.5j + CR 702.26: two milled copies of one boss must BOTH survive
    /// and phase out — never hit `ChooseLegend` and annihilate. The completion
    /// phases both out synchronously, before the redirect returns, so no SBA
    /// check can observe the phased-in window (CR 704.5j has no
    /// pending-phase-out exception, so there is no exemption to consult).
    ///
    /// Revert guard: skipping the synchronous phase-out leaves both copies
    /// phased in, and the SBA pass parks `ChooseLegend` — the no-prompt
    /// assertion flips (and one copy would be annihilated on answer).
    #[test]
    fn duplicate_milled_bosses_survive_sbas_and_phase_out_together() {
        use crate::game::zones::create_object;
        use crate::types::card_type::{CoreType, Supertype};
        use crate::types::format::HordeLegendaryDeath;
        use crate::types::identifiers::CardId;

        let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
        ruleset.legendary_death = HordeLegendaryDeath::EtbThenPhaseOut;
        let mut state = GameState::new(FormatConfig::horde(ruleset), 2, 42);
        let horde = horde_seat(&state).expect("horde seat");

        // Two copies of one boss on top of the library (same name — the
        // legend-rule collision). 2/2 bodies so no other SBA has an opinion.
        let mut bosses = Vec::new();
        for (n, card) in [(1, 9001), (2, 9002)] {
            let boss = create_object(
                &mut state,
                CardId(card),
                horde,
                "Boss".into(),
                Zone::Library,
            );
            {
                let o = state.objects.get_mut(&boss).unwrap();
                o.card_types.supertypes.push(Supertype::Legendary);
                o.card_types.core_types.push(CoreType::Creature);
                o.base_card_types = o.card_types.clone();
                o.power = Some(2);
                o.toughness = Some(2);
                o.base_power = Some(2);
                o.base_toughness = Some(2);
            }
            bosses.push((n, boss));
        }

        // The redirect deploys both (production settle path).
        let mut events = Vec::new();
        let outcome = mill_from_loss(&mut state, horde, 2, &mut events);
        assert_eq!(
            outcome,
            MillFromLossOutcome::Settled,
            "no competing replacement — the redirect must settle"
        );
        for (n, boss) in &bosses {
            assert!(
                state.battlefield.contains(boss),
                "boss copy {n} must deploy to the battlefield"
            );
            assert!(
                state.objects[boss].is_phased_out(),
                "boss copy {n} must phase out synchronously on deployment (CR 702.26)"
            );
        }

        // The SBA check runs with both same-name bosses already phased out:
        // phased-out permanents are invisible to the rule (CR 702.26b), so no
        // ChooseLegend prompt parks and nothing is annihilated.
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseLegend { .. }),
            "both copies phased out before this SBA check — the legend rule \
             must not prompt, waiting_for={:?}",
            state.waiting_for
        );
        for (n, boss) in &bosses {
            assert!(
                state.battlefield.contains(boss),
                "boss copy {n} must still be on the battlefield (phased out)"
            );
        }
    }

    /// Negative control: under the basic rule (`Normal`, every shipped deck), a
    /// milled legendary is buried like any other card — the advanced deploy-and-
    /// phase-out behavior must be gated strictly on the opt-in axis.
    #[test]
    fn damage_mill_buries_legendaries_under_the_basic_rule() {
        use crate::game::zones::create_object;
        use crate::types::card_type::{CoreType, Supertype};
        use crate::types::identifiers::CardId;

        // default_ruleset() is `HordeLegendaryDeath::Normal` for every deck.
        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            2,
            42,
        );
        let horde = horde_seat(&state).expect("horde seat");
        let boss = create_object(
            &mut state,
            CardId(9001),
            horde,
            "Boss".into(),
            Zone::Library,
        );
        {
            let o = state.objects.get_mut(&boss).unwrap();
            o.card_types.supertypes.push(Supertype::Legendary);
            o.card_types.core_types.push(CoreType::Creature);
        }

        let mut events = Vec::new();
        mill_from_loss(&mut state, horde, 1, &mut events);

        assert!(
            !state.battlefield.contains(&boss),
            "the basic rule must not deploy milled legendaries"
        );
        assert!(
            state
                .players
                .iter()
                .find(|p| p.id == horde)
                .unwrap()
                .graveyard
                .contains(&boss),
            "the basic rule mills legendaries straight to the graveyard"
        );
    }

    /// Rules fidelity, the crux of the advanced rule: "ETB effects trigger, THEN
    /// immediately Phases Out." The milled legendary's OWN enters-the-battlefield
    /// trigger must be collected by the batch's eager scan (CR 603.6), which runs
    /// BEFORE the completion's synchronous phase-out. Asserted on the staged
    /// `deferred_triggers` queue directly — no manual `process_triggers`
    /// re-scan (a re-scan over the raw event vec would hide a collection miss,
    /// since production only consults unconsumed events).
    #[test]
    fn milled_legendarys_own_etb_still_fires_despite_phasing_out() {
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};
        use crate::types::card_type::{CoreType, Supertype};
        use crate::types::identifiers::CardId;
        use crate::types::triggers::TriggerMode;

        let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
        ruleset.legendary_death = crate::types::format::HordeLegendaryDeath::EtbThenPhaseOut;
        let mut state = GameState::new(FormatConfig::horde(ruleset), 2, 42);
        let horde = horde_seat(&state).expect("horde seat");

        let boss = create_object(
            &mut state,
            CardId(9001),
            horde,
            "Boss".into(),
            Zone::Library,
        );
        {
            let o = state.objects.get_mut(&boss).unwrap();
            o.card_types.supertypes.push(Supertype::Legendary);
            o.card_types.core_types.push(CoreType::Creature);
            // A plain, observable ETB trigger: "When this enters, draw a card."
            o.trigger_definitions.push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        let mut events = Vec::new();
        mill_from_loss(&mut state, horde, 1, &mut events);

        // The boss entered and the completion already phased it out — and the
        // eager scan collected its own ETB first. A minimal Horde state has no
        // other trigger sources, so a missing staged trigger means the ETB was
        // suppressed — the rules bug this guards.
        assert!(
            state.battlefield.contains(&boss),
            "the boss must enter the battlefield"
        );
        assert!(
            state.objects[&boss].is_phased_out(),
            "the boss must already be phased out (synchronous completion)"
        );
        assert!(
            state
                .deferred_triggers
                .iter()
                .any(|ctx| ctx.pending.source_id == boss),
            "the milled legendary's own ETB trigger must be collected by the \
             batch's eager scan, not suppressed by the phase-out"
        );
    }

    // ── Post-combat activation (advanced rule) ──────────────────────────────

    use crate::types::format::HordePostCombatActivation;

    /// Build a Horde game whose post-combat main phase is open, on the Horde's
    /// turn, with the given activation policy — the shared scaffold for the
    /// post-combat activation tests.
    fn horde_post_combat_game(policy: HordePostCombatActivation) -> (GameState, PlayerId) {
        let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
        ruleset.post_combat_activation = policy;
        let mut state = GameState::new(FormatConfig::horde(ruleset), 2, 42);
        let horde = horde_seat(&state).expect("horde seat");
        state.phase = Phase::PostCombatMain;
        state.active_player = horde;
        (state, horde)
    }

    /// Give the Horde a battlefield permanent carrying a single non-mana `{T}`
    /// activated ability (Proliferate — no targets, so it announces cleanly with
    /// no AI in the loop). Not summoning-sick, so the `{T}` cost is payable.
    fn add_horde_ability_permanent(state: &mut GameState, horde: PlayerId, name: &str) -> ObjectId {
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use std::sync::Arc;

        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, horde, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.summoning_sick = false;
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(AbilityKind::Activated, Effect::Proliferate)
                .cost(AbilityCost::Tap),
        );
        id
    }

    /// The advanced rule: during the Horde's post-combat main, the beat seeds the
    /// Horde's permanents and activates one's non-mana ability — announced on the
    /// stack, its `{T}` cost paid, and the permanent consumed (once per turn).
    #[test]
    fn post_combat_beat_activates_a_horde_permanents_ability() {
        let (mut state, horde) =
            horde_post_combat_game(HordePostCombatActivation::OncePerPermanent);
        let bot = add_horde_ability_permanent(&mut state, horde, "Ability Bot");

        let mut events = Vec::new();
        begin_post_combat_activation(&mut state, &mut events);
        assert_eq!(
            state.horde_postcombat_activation_queue,
            vec![bot],
            "the Horde permanent must be queued for post-combat activation"
        );

        let wf = maybe_activate_next_ability(&mut state, &mut events);
        assert!(
            wf.is_some(),
            "the beat must announce the permanent's ability"
        );
        assert!(
            !state.stack.is_empty(),
            "the activated ability must be on the stack"
        );
        assert!(
            state.objects[&bot].tapped,
            "the {{T}} activation cost must have tapped the source"
        );
        assert!(
            state.horde_postcombat_activation_queue.is_empty(),
            "the permanent must be consumed — one activation per permanent per turn"
        );
    }

    /// Negative control: with the basic `None` policy (every shipped deck) the
    /// beat is completely inert — nothing is queued, nothing is announced.
    #[test]
    fn post_combat_beat_is_inert_under_the_basic_rule() {
        let (mut state, horde) = horde_post_combat_game(HordePostCombatActivation::None);
        let bot = add_horde_ability_permanent(&mut state, horde, "Ability Bot");

        let mut events = Vec::new();
        begin_post_combat_activation(&mut state, &mut events);
        assert!(
            state.horde_postcombat_activation_queue.is_empty(),
            "the basic rule must not seed any activations"
        );
        assert!(
            maybe_activate_next_ability(&mut state, &mut events).is_none(),
            "the basic rule must not activate anything"
        );
        assert!(state.stack.is_empty());
        assert!(!state.objects[&bot].tapped);
    }

    /// "Any OTHER ability" = non-mana: a Horde permanent whose only activated
    /// ability is a mana ability must be skipped (never tapped for mana it can't
    /// spend), and the queue drains to nothing announced.
    #[test]
    fn post_combat_beat_skips_mana_only_permanents() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ManaProduction, QuantityExpr,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use std::sync::Arc;

        let (mut state, horde) =
            horde_post_combat_game(HordePostCombatActivation::OncePerPermanent);
        let card_id = CardId(state.next_object_id);
        let rock = create_object(
            &mut state,
            card_id,
            horde,
            "Mana Rock".into(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&rock).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.summoning_sick = false;
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
        }

        let mut events = Vec::new();
        begin_post_combat_activation(&mut state, &mut events);
        assert_eq!(
            state.horde_postcombat_activation_queue,
            vec![rock],
            "the permanent is queued (eligibility is decided at activation time)"
        );

        assert!(
            maybe_activate_next_ability(&mut state, &mut events).is_none(),
            "a mana-only permanent must be skipped — 'any OTHER ability' excludes mana abilities"
        );
        assert!(state.stack.is_empty(), "nothing may be announced");
        assert!(
            !state.objects[&rock].tapped,
            "the mana rock must NOT be tapped for mana the Horde can't use"
        );
        assert!(
            state.horde_postcombat_activation_queue.is_empty(),
            "the skipped permanent is still consumed from the queue"
        );
    }

    /// "Card-activated abilities have summoning sickness": a Horde creature that
    /// entered this turn cannot use its `{T}` ability post-combat — EVEN with the
    /// emblem's Haste (which only lets it attack). Revert guard: the creature is
    /// given Haste, so `can_activate_ability_now` alone would allow it; only the
    /// dedicated haste-blind gate keeps it summoning-sick. Non-tap abilities and
    /// creatures controlled since the turn began are unaffected (covered above).
    #[test]
    fn post_combat_beat_keeps_tap_abilities_summoning_sick_despite_haste() {
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::keywords::Keyword;
        use std::sync::Arc;

        let (mut state, horde) =
            horde_post_combat_game(HordePostCombatActivation::OncePerPermanent);
        let card_id = CardId(state.next_object_id);
        let sick = create_object(
            &mut state,
            card_id,
            horde,
            "Fresh Recruit".into(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&sick).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // Entered this turn: summoning-sick. Haste (as the emblem grants) lets
            // it attack but must NOT lift the {T}-ability summoning-sickness gate.
            obj.summoning_sick = true;
            obj.base_keywords.push(Keyword::Haste);
            obj.keywords.push(Keyword::Haste);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(AbilityKind::Activated, Effect::Proliferate)
                    .cost(AbilityCost::Tap),
            );
        }

        let mut events = Vec::new();
        begin_post_combat_activation(&mut state, &mut events);
        assert!(
            maybe_activate_next_ability(&mut state, &mut events).is_none(),
            "a summoning-sick creature's {{T}} ability must not activate the turn it entered, \
             Haste notwithstanding"
        );
        assert!(
            !state.objects[&sick].tapped,
            "the summoning-sick creature must not be tapped"
        );
    }

    /// "Horde has infinite mana (for … activation costs)" is granted at Horde
    /// setup, but only for decks that actually activate abilities post-combat.
    /// Observable as a filled mana pool after `grant_horde_emblem` (which tops up
    /// the flagged pool). A basic-rule Horde must get no such pool.
    #[test]
    fn horde_gets_infinite_mana_only_under_the_post_combat_rule() {
        use crate::game::deck_loading::grant_horde_emblem;

        let (mut on, horde) = horde_post_combat_game(HordePostCombatActivation::OncePerPermanent);
        grant_horde_emblem(&mut on, horde, true);
        assert!(
            !on.players
                .iter()
                .find(|p| p.id == horde)
                .unwrap()
                .mana_pool
                .is_empty(),
            "the post-combat rule must grant the Horde infinite mana (a filled pool)"
        );

        let (mut off, horde2) = horde_post_combat_game(HordePostCombatActivation::None);
        grant_horde_emblem(&mut off, horde2, true);
        assert!(
            off.players
                .iter()
                .find(|p| p.id == horde2)
                .unwrap()
                .mana_pool
                .is_empty(),
            "a basic-rule Horde must not be granted infinite mana"
        );
    }

    // ── Two-Horde-seat side (LOTR Two Towers foundation) ────────────────────

    /// Both designated Horde seats are recognized as Horde (no life total, skip
    /// the opening hand); a non-designated seat is a survivor. With the set empty
    /// the game falls back to the single archenemy seat (single-Horde unchanged).
    #[test]
    fn both_horde_seats_are_recognized_and_survivors_are_not() {
        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            3,
            42,
        );
        state.horde_seats = vec![PlayerId(0), PlayerId(1)];

        for seat in [PlayerId(0), PlayerId(1)] {
            assert!(is_horde_seat(&state, seat), "{seat:?} must be a Horde seat");
            assert!(
                player_has_no_life_total(&state, seat),
                "{seat:?} has no life total"
            );
            assert!(
                player_skips_opening_hand(&state, seat),
                "{seat:?} skips the opening hand"
            );
        }
        assert!(
            !is_horde_seat(&state, PlayerId(2)),
            "the survivor is not a Horde seat"
        );
        assert!(
            !player_has_no_life_total(&state, PlayerId(2)),
            "the survivor keeps its life total"
        );

        // Fallback: empty set → the one-vs-many archenemy is the sole Horde seat.
        state.horde_seats.clear();
        assert_eq!(
            horde_seats(&state),
            vec![PlayerId(0)],
            "a single-Horde game resolves the sole archenemy seat"
        );
    }

    /// Collective loss: a two-Horde side is defeated ONLY when EVERY Horde
    /// library is empty AND no Horde seat controls a creature. One Horde running
    /// dry while the other still has cards (or a creature) does NOT end the game.
    #[test]
    fn two_horde_side_loses_only_when_both_libraries_empty() {
        use crate::game::zones::{create_object, move_to_zone};
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            3,
            42,
        );
        state.horde_seats = vec![PlayerId(0), PlayerId(1)];

        // One library card for each Horde seat.
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sauron card".into(),
            Zone::Library,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Saruman card".into(),
            Zone::Library,
        );

        assert!(
            !horde_is_defeated(&state),
            "not defeated while both libraries have cards"
        );

        // Empty ONE library — still NOT defeated (the collective rule's whole point).
        let mut ev = Vec::new();
        move_to_zone(&mut state, a, Zone::Exile, &mut ev);
        assert!(
            !horde_is_defeated(&state),
            "one empty Horde library must NOT defeat the two-Horde side"
        );

        // Empty the other too, with no Horde creatures → defeated.
        move_to_zone(&mut state, b, Zone::Exile, &mut ev);
        assert!(
            horde_is_defeated(&state),
            "both libraries empty and no Horde creature → the Horde side is defeated"
        );

        // A creature controlled by EITHER Horde seat keeps the side alive.
        let goblin = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Uruk".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&goblin)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        assert!(
            !horde_is_defeated(&state),
            "a creature controlled by either Horde seat keeps the side undefeated"
        );
    }

    /// The rarity-wave gate is a `>=` comparison, so the `Ord` derive on
    /// [`Rarity`] must order least-rare first. Pin it: if the enum is ever
    /// reordered, every `UntilRarityAtLeast` threshold silently changes meaning.
    #[test]
    fn rarity_orders_least_rare_first() {
        assert!(Rarity::Common < Rarity::Uncommon);
        assert!(Rarity::Uncommon < Rarity::Rare);
        assert!(Rarity::Rare < Rarity::Mythic);
    }

    /// The D&D Horde's rule: "a wave ends when an Uncommon, Rare, or Mythic
    /// enters" — commons keep it going, uncommon-or-better caps it.
    #[test]
    fn rarity_wave_ends_only_at_or_above_the_threshold() {
        let policy = Some(WaveTermination::UntilRarityAtLeast(Rarity::Uncommon));

        assert!(!wave_ends_after_nontoken(policy, Some(Rarity::Common), 0));
        assert!(wave_ends_after_nontoken(policy, Some(Rarity::Uncommon), 0));
        assert!(wave_ends_after_nontoken(policy, Some(Rarity::Rare), 0));
        assert!(wave_ends_after_nontoken(policy, Some(Rarity::Mythic), 0));
    }

    /// The threshold is a parameter, not a constant — a deck may cap its waves
    /// only on rares.
    #[test]
    fn rarity_wave_threshold_is_parameterized() {
        let rare_only = Some(WaveTermination::UntilRarityAtLeast(Rarity::Rare));

        assert!(!wave_ends_after_nontoken(
            rare_only,
            Some(Rarity::Common),
            0
        ));
        assert!(!wave_ends_after_nontoken(
            rare_only,
            Some(Rarity::Uncommon),
            0
        ));
        assert!(wave_ends_after_nontoken(rare_only, Some(Rarity::Rare), 0));
        assert!(wave_ends_after_nontoken(rare_only, Some(Rarity::Mythic), 0));
    }

    /// A card-data gap must fail safe toward "keep revealing" rather than
    /// stalling the Horde on an unknown-rarity card.
    #[test]
    fn unknown_rarity_never_ends_a_rarity_wave() {
        let policy = Some(WaveTermination::UntilRarityAtLeast(Rarity::Uncommon));
        assert!(!wave_ends_after_nontoken(policy, None, 0));
    }

    /// `UntilNonToken` is rarity-blind — it ends on the nontoken QUOTA, not on
    /// what the card is. With the quota exhausted (0 remaining) any rarity ends
    /// the wave; this is the Cyberman `Fixed(1)` behavior, unchanged.
    #[test]
    fn until_nontoken_is_rarity_blind_once_the_quota_is_met() {
        let policy = Some(WaveTermination::UntilNonToken {
            count: WaveCount::Fixed(1),
        });

        assert!(wave_ends_after_nontoken(policy, Some(Rarity::Common), 0));
        assert!(wave_ends_after_nontoken(policy, Some(Rarity::Mythic), 0));
        assert!(wave_ends_after_nontoken(policy, None, 0));
    }

    /// A multi-nontoken wave keeps going while the quota is unmet, regardless of
    /// rarity — the Zombies Horde's Wave 2 and Wave 3 turns.
    #[test]
    fn until_nontoken_continues_while_quota_remains() {
        let policy = Some(WaveTermination::UntilNonToken {
            count: WaveCount::Snaking { min: 1, max: 3 },
        });

        assert!(!wave_ends_after_nontoken(policy, Some(Rarity::Mythic), 2));
        assert!(!wave_ends_after_nontoken(policy, Some(Rarity::Common), 1));
        assert!(wave_ends_after_nontoken(policy, Some(Rarity::Common), 0));
    }

    /// `FixedCount` waves are governed solely by the counter — casting a
    /// non-token never short-circuits them, at any rarity.
    #[test]
    fn fixed_count_and_absent_policy_never_end_the_wave_here() {
        let fixed = Some(WaveTermination::FixedCount(2));

        assert!(!wave_ends_after_nontoken(fixed, Some(Rarity::Common), 0));
        assert!(!wave_ends_after_nontoken(fixed, Some(Rarity::Mythic), 0));
        assert!(!wave_ends_after_nontoken(fixed, None, 0));
        assert!(!wave_ends_after_nontoken(None, Some(Rarity::Mythic), 0));
    }

    // ── Only-Defenders-block combat rule ────────────────────────────────────

    /// Basic Horde rule "only Horde creatures with Defender can block": the
    /// game-start emblem gives every NON-Defender creature the Horde controls a
    /// `CantBlock` static (CR 509.1b), so it is excluded from the Horde's legal
    /// blockers, while a Defender creature stays a valid blocker.
    #[test]
    fn horde_non_defender_creatures_cant_block_only_defenders_can() {
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::keywords::Keyword;

        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            2,
            42,
        );
        let horde = horde_seat(&state).expect("horde seat");
        let survivor = state
            .players
            .iter()
            .map(|p| p.id)
            .find(|&id| id != horde)
            .expect("a survivor seat");

        // Grant the Horde its game-start emblem (haste + forced attackers +
        // only-Defenders-block).
        crate::game::deck_loading::grant_horde_emblem(&mut state, horde, true);

        // Two Horde creatures: a plain attacker and a Defender wall.
        let grunt = create_object(
            &mut state,
            CardId(9101),
            horde,
            "Grunt".into(),
            Zone::Battlefield,
        );
        let wall = create_object(
            &mut state,
            CardId(9102),
            horde,
            "Wall".into(),
            Zone::Battlefield,
        );
        for &id in &[grunt, wall] {
            let o = state.objects.get_mut(&id).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.power = Some(2);
            o.toughness = Some(2);
            o.summoning_sick = false;
        }
        {
            // base_keywords is the layer base, so the Defender survives flush_layers.
            let o = state.objects.get_mut(&wall).unwrap();
            o.keywords.push(Keyword::Defender);
            o.base_keywords.push(Keyword::Defender);
        }
        crate::game::layers::flush_layers(&mut state);

        // A survivor attacks the Horde; the Horde must now declare blockers.
        let attacker = create_object(
            &mut state,
            CardId(9103),
            survivor,
            "Raider".into(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&attacker).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.power = Some(2);
            o.toughness = Some(2);
            o.summoning_sick = false;
        }
        state.combat = Some(crate::game::combat::CombatState {
            attackers: vec![crate::game::combat::AttackerInfo::attacking_player(
                attacker, horde,
            )],
            ..Default::default()
        });

        let valid = crate::game::combat::get_valid_block_targets_for_player(&state, horde);
        assert!(
            !valid.contains_key(&grunt),
            "a non-Defender Horde creature can't block (only Defenders block)"
        );
        assert!(
            valid.contains_key(&wall),
            "a Defender Horde creature remains a legal blocker"
        );
    }

    /// Negative control: with `forced_attackers = false` the aggressive-combat
    /// package is off, so a non-Defender Horde creature CAN block normally — the
    /// restriction is strictly gated on the ruleset axis.
    #[test]
    fn horde_without_forced_attackers_can_block_normally() {
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            2,
            42,
        );
        let horde = horde_seat(&state).expect("horde seat");
        let survivor = state
            .players
            .iter()
            .map(|p| p.id)
            .find(|&id| id != horde)
            .expect("a survivor seat");

        crate::game::deck_loading::grant_horde_emblem(&mut state, horde, false);

        let grunt = create_object(
            &mut state,
            CardId(9101),
            horde,
            "Grunt".into(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&grunt).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.power = Some(2);
            o.toughness = Some(2);
            o.summoning_sick = false;
        }
        crate::game::layers::flush_layers(&mut state);

        let attacker = create_object(
            &mut state,
            CardId(9103),
            survivor,
            "Raider".into(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&attacker).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.power = Some(2);
            o.toughness = Some(2);
            o.summoning_sick = false;
        }
        state.combat = Some(crate::game::combat::CombatState {
            attackers: vec![crate::game::combat::AttackerInfo::attacking_player(
                attacker, horde,
            )],
            ..Default::default()
        });

        let valid = crate::game::combat::get_valid_block_targets_for_player(&state, horde);
        assert!(
            valid.contains_key(&grunt),
            "without forced attackers, a non-Defender Horde creature blocks normally"
        );
    }

    // ── Bounce → top of the Horde's library ─────────────────────────────────

    /// Basic Horde rule: a Horde-owned permanent returned to the Horde's hand goes
    /// on TOP of the Horde's library instead (the Horde has no hand).
    #[test]
    fn horde_owned_permanent_bounced_to_hand_goes_to_top_of_library() {
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            2,
            42,
        );
        let horde = horde_seat(&state).expect("horde seat");

        // A marker already on top of the library, to prove the bounced card lands
        // ABOVE it (top), not at the bottom.
        let existing = create_object(
            &mut state,
            CardId(8000),
            horde,
            "Existing".into(),
            Zone::Library,
        );

        let creature = create_object(
            &mut state,
            CardId(8001),
            horde,
            "Horde Beast".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, creature, Zone::Hand, &mut events);

        let horde_player = state.players.iter().find(|p| p.id == horde).unwrap();
        assert!(
            !horde_player.hand.contains(&creature),
            "the Horde never holds a card in hand"
        );
        assert_eq!(
            horde_player.library.front(),
            Some(&creature),
            "the bounced Horde permanent goes on TOP of the Horde's library"
        );
        assert_eq!(state.objects[&creature].zone, Zone::Library);
        assert!(
            horde_player.library.contains(&existing),
            "the pre-existing library card is still present (pushed below the new top)"
        );
    }

    /// Negative control: a SURVIVOR's bounced permanent goes to their hand
    /// normally — the redirect is strictly owner-scoped to the Horde.
    #[test]
    fn survivor_permanent_bounced_to_hand_is_not_redirected() {
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            2,
            42,
        );
        let horde = horde_seat(&state).expect("horde seat");
        let survivor = state
            .players
            .iter()
            .map(|p| p.id)
            .find(|&id| id != horde)
            .expect("a survivor seat");

        let creature = create_object(
            &mut state,
            CardId(8010),
            survivor,
            "Survivor Bear".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, creature, Zone::Hand, &mut events);

        let survivor_player = state.players.iter().find(|p| p.id == survivor).unwrap();
        assert!(
            survivor_player.hand.contains(&creature),
            "a survivor's bounced card goes to their hand normally"
        );
        assert_eq!(state.objects[&creature].zone, Zone::Hand);
    }

    // ── LOTR Orc Army: revealed Army tokens amass one shared army ────────────

    /// LOTR "Two Towers" rule "Orc Armies grow as a single army": a revealed Orc
    /// Army library token amasses the Horde's SINGLE Army (CR 701.47a) instead of
    /// entering as a fresh 0/0 that dies to SBAs. A second revealed Orc Army grows
    /// the SAME army — it never multiplies into a second one.
    #[test]
    fn revealed_orc_army_amasses_the_hordes_single_army() {
        use crate::game::deck_loading::{create_horde_library_token, horde_token_image_ref};
        use crate::types::counter::CounterType;

        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            2,
            42,
        );
        let horde = horde_seat(&state).expect("horde seat");

        let preset = crate::game::token_presets::known_token_presets()
            .iter()
            .find(|p| p.body.display_name == "Orc Army")
            .expect("Orc Army preset");
        let a1 = create_horde_library_token(
            &mut state,
            &preset.body,
            Some(horde_token_image_ref(preset)),
            horde,
        );
        let a2 = create_horde_library_token(
            &mut state,
            &preset.body,
            Some(horde_token_image_ref(preset)),
            horde,
        );

        // Dispatch key: an Orc Army token amasses the "Orc" army.
        assert_eq!(revealed_army_subtype(&state, a1).as_deref(), Some("Orc"));

        let armies = |state: &GameState| -> Vec<ObjectId> {
            state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|o| {
                        o.controller == horde && o.card_types.subtypes.iter().any(|s| s == "Army")
                    })
                })
                .collect()
        };

        let mut events = Vec::new();
        amass_revealed_army(&mut state, horde, a1, "Orc", &mut events);

        let first = armies(&state);
        assert_eq!(first.len(), 1, "one Orc Army entered");
        assert_eq!(
            state.objects[&first[0]]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "amass 1 put a +1/+1 counter (0/0 -> 1/1)"
        );
        assert!(
            state.objects[&first[0]]
                .card_types
                .subtypes
                .contains(&"Orc".to_string()),
            "the amassed army is an Orc Army"
        );
        assert!(
            !state.objects.contains_key(&a1),
            "the library placeholder is consumed, not materialized"
        );

        // The second Orc Army grows the SAME army, not a new one.
        amass_revealed_army(&mut state, horde, a2, "Orc", &mut events);
        let second = armies(&state);
        assert_eq!(
            second.len(),
            1,
            "still ONE Orc Army — it grew, not multiplied"
        );
        assert_eq!(
            state.objects[&second[0]]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2),
            "the single army grew to two +1/+1 counters (2/2)"
        );
    }

    // ── Two-Horde per-seat setup ─────────────────────────────────────────────

    /// A two-Horde game (LOTR Two Towers) grants EACH Horde seat its own game-start
    /// emblem — both commanders control their own creatures, so each needs the
    /// haste + forced-attackers + only-Defenders-block package.
    #[test]
    fn two_horde_game_grants_each_seat_an_emblem() {
        let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
        ruleset.co_horde_decks = vec![ChallengeDeck::CybermanHorde];
        let mut state = GameState::new(FormatConfig::horde(ruleset), 4, 42);

        let seats = horde_seats(&state);
        assert_eq!(seats.len(), 2, "two Horde seats");
        for &seat in &seats {
            crate::game::deck_loading::grant_horde_emblem(&mut state, seat, true);
        }

        for &seat in &seats {
            assert!(
                state
                    .objects
                    .values()
                    .any(|o| o.controller == seat && o.emblem_source.is_some()),
                "Horde seat {seat:?} controls its own emblem"
            );
        }
    }

    /// Reading B: `begin_wave` is the skip-safe hook that rotates the active-Horde
    /// cursor — it runs only on a real Horde turn, so each Horde turn hands the next
    /// one to the OTHER commander (Sauron ↔ Saruman).
    #[test]
    fn begin_wave_rotates_the_active_horde_commander() {
        let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
        ruleset.co_horde_decks = vec![ChallengeDeck::CybermanHorde];
        let mut state = GameState::new(FormatConfig::horde(ruleset), 4, 42);
        state.phase = Phase::PreCombatMain;

        assert_eq!(state.active_horde_index, 0);

        // Sauron's (seat 0) Horde turn advances the cursor to Saruman.
        state.active_player = state.horde_seats[0];
        let mut events = Vec::new();
        begin_wave(&mut state, &mut events);
        assert_eq!(state.active_horde_index, 1);

        // Saruman's (seat 1) Horde turn wraps it back (mod the seat count).
        state.active_player = state.horde_seats[1];
        begin_wave(&mut state, &mut events);
        assert_eq!(state.active_horde_index, 0);
    }

    /// `active_horde_seat` is the commander whose turn it is — so the wave/mill draw
    /// from that commander's own library, not always the primary's.
    #[test]
    fn active_horde_seat_is_the_current_commander() {
        let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
        ruleset.co_horde_decks = vec![ChallengeDeck::CybermanHorde];
        let mut state = GameState::new(FormatConfig::horde(ruleset), 4, 42);

        state.active_player = state.horde_seats[0];
        assert_eq!(active_horde_seat(&state), Some(state.horde_seats[0]));
        state.active_player = state.horde_seats[1];
        assert_eq!(
            active_horde_seat(&state),
            Some(state.horde_seats[1]),
            "the OTHER commander drives its own turn"
        );
        state.active_player = PlayerId(2);
        assert_eq!(active_horde_seat(&state), None, "not a Horde turn");
    }

    /// Wave-reveal test game: Cyberman Horde, FixedCount wave, budget seeded,
    /// with the Horde seat active so the wave driver engages.
    fn wave_test_game(wave: u32) -> (GameState, PlayerId) {
        let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
        ruleset.wave = WaveTermination::FixedCount(wave);
        let mut state = GameState::new(FormatConfig::horde(ruleset), 2, 42);
        let horde = horde_seat(&state).expect("horde seat");
        state.active_player = horde;
        state.horde_wave_remaining = wave;
        (state, horde)
    }

    /// Vanilla castable 2/2 nontoken creature on `owner`'s library.
    fn add_wave_creature(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaCost;
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).expect("created");
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.mana_cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        id
    }

    /// Battlefield replacement source redirecting `card`'s moves destined for
    /// `from_dest` to `to`. `optional` selects Optional defs (accept = 0,
    /// decline = 1); otherwise Mandatory defs. One def applies silently
    /// (Mandatory) or pauses alone (Optional); two force a CR 616.1 ordering
    /// pause.
    fn redirect_source(
        state: &mut GameState,
        owner: PlayerId,
        card: ObjectId,
        from_dest: Zone,
        to: Zone,
        optional: bool,
        copies: usize,
    ) {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode,
        };
        use crate::types::identifiers::CardId;
        use crate::types::replacements::ReplacementEvent;
        let source = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Redirector".to_string(),
            Zone::Battlefield,
        );
        let def = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: to,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
            .destination_zone(from_dest)
            .valid_card(TargetFilter::SpecificObject { id: card });
        let def = if optional {
            def.mode(ReplacementMode::Optional { decline: None })
        } else {
            def
        };
        let obj = state.objects.get_mut(&source).expect("source");
        for _ in 0..copies {
            obj.replacement_definitions.push(def.clone());
        }
    }

    /// A CR 616.1 pause on the nontoken reveal exile parks the reveal; after
    /// the choice declines the redirect, the wave resumes and casts the card
    /// for free. Revert guard: the raw mover never prompts, so the
    /// ReplacementChoice assertion fails without the pipeline.
    #[test]
    fn wave_reveal_pause_for_choice_resumes_to_cast_after_decline() {
        use crate::types::actions::GameAction;
        let (mut state, horde) = wave_test_game(2);
        let card = add_wave_creature(&mut state, horde, "Wave Bear");
        state.players[horde.0 as usize].library = crate::im::vector![card];
        redirect_source(
            &mut state,
            horde,
            card,
            Zone::Exile,
            Zone::Graveyard,
            true,
            1,
        );

        let mut events = Vec::new();
        let paused = maybe_reveal_next(&mut state, &mut events);
        assert!(
            matches!(paused, Some(WaitingFor::ReplacementChoice { .. })),
            "the Optional redirect must pause the reveal, got {paused:?}"
        );
        assert!(
            matches!(
                state.pending_horde_reveal.as_deref(),
                Some(crate::types::game_state::PendingHordeReveal::NontokenCast {
                    card: pending,
                    ..
                }) if *pending == card
            ),
            "the parked card must ride the pending record"
        );
        assert_eq!(state.horde_wave_remaining, 1, "budget consumed once");

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("decline the redirect");
        let resumed = maybe_reveal_next(&mut state, &mut events);
        assert!(
            state.pending_horde_reveal.is_none(),
            "resume drains the record"
        );
        assert!(
            !matches!(resumed, Some(WaitingFor::ReplacementChoice { .. })),
            "no second prompt after the choice resolves"
        );
        assert_eq!(
            state.objects.get(&card).map(|o| o.zone),
            Some(Zone::Stack),
            "the declined redirect casts the card for free"
        );
        assert_eq!(
            state.horde_wave_remaining, 1,
            "resume spends no fresh budget"
        );
    }

    /// A replacement pause on the wave's LAST card resumes through public
    /// actions to completion. Budget 1: the reveal consumes it, parks on the
    /// Optional redirect, and — after the decline — the wave must still run
    /// the parked cast even though no fresh budget remains. The cast then
    /// resolves through priority passes: the bear enters, the stack empties,
    /// and the wave ends with nothing stranded.
    ///
    /// Revert guard: without pending-through-gate, the post-answer
    /// `maybe_reveal_next` returns `None` with the record still parked and
    /// the card uncast in exile — the zone + pending assertions flip.
    #[test]
    fn wave_reveal_last_card_pause_resumes_to_completion() {
        use crate::types::actions::GameAction;
        let (mut state, horde) = wave_test_game(1);
        let card = add_wave_creature(&mut state, horde, "Wave Bear");
        state.players[horde.0 as usize].library = crate::im::vector![card];
        redirect_source(
            &mut state,
            horde,
            card,
            Zone::Exile,
            Zone::Graveyard,
            true,
            1,
        );

        let mut events = Vec::new();
        let paused = maybe_reveal_next(&mut state, &mut events);
        assert!(
            matches!(paused, Some(WaitingFor::ReplacementChoice { .. })),
            "the Optional redirect must pause the reveal, got {paused:?}"
        );
        assert_eq!(
            state.horde_wave_remaining, 0,
            "the lone reveal consumed the whole budget"
        );
        assert!(
            state.pending_horde_reveal.is_some(),
            "the last-card reveal parked its tail"
        );

        // Decline through the public action path.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("decline the redirect");

        // The re-entry seam resumes the parked tail despite zero budget.
        let _resumed = maybe_reveal_next(&mut state, &mut events);
        assert!(
            state.pending_horde_reveal.is_none(),
            "resume drains the record"
        );
        assert_eq!(
            state.objects.get(&card).map(|o| o.zone),
            Some(Zone::Stack),
            "the declined redirect casts the card for free"
        );

        // Drive the cast to completion through priority passes.
        let mut guard = 0;
        while !state.stack.is_empty() && guard < 8 {
            crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                .expect("pass priority");
            guard += 1;
        }
        assert!(state.stack.is_empty(), "the free cast resolves");
        assert_eq!(
            state.objects.get(&card).map(|o| o.zone),
            Some(Zone::Battlefield),
            "the revealed bear enters the battlefield"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "completion boundary is Priority, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            state.horde_wave_remaining, 0,
            "the wave ends with the budget spent — nothing revealed past it"
        );
        assert!(
            state.players[horde.0 as usize].library.is_empty(),
            "the one-card library is empty after its reveal"
        );
    }

    /// Accepting the redirect on a nontoken reveal sends the card to the
    /// replacement zone and ends the wave without casting.
    #[test]
    fn wave_reveal_pause_for_choice_redirect_accept_skips_cast() {
        use crate::types::actions::GameAction;
        let (mut state, horde) = wave_test_game(2);
        let card = add_wave_creature(&mut state, horde, "Wave Bear");
        state.players[horde.0 as usize].library = crate::im::vector![card];
        redirect_source(
            &mut state,
            horde,
            card,
            Zone::Exile,
            Zone::Graveyard,
            true,
            1,
        );

        let mut events = Vec::new();
        let paused = maybe_reveal_next(&mut state, &mut events);
        assert!(
            matches!(paused, Some(WaitingFor::ReplacementChoice { .. })),
            "the Optional redirect must pause the reveal, got {paused:?}"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 0 },
        )
        .expect("accept the redirect");
        let resumed = maybe_reveal_next(&mut state, &mut events);
        assert!(resumed.is_none(), "a redirected reveal ends the wave");
        assert!(
            state.pending_horde_reveal.is_none(),
            "resume drains the record"
        );
        assert_eq!(
            state.objects.get(&card).map(|o| o.zone),
            Some(Zone::Graveyard),
            "the accepted redirect moves the card instead of casting it"
        );
        assert!(state.stack.is_empty(), "nothing is cast after a redirect");
    }

    /// A lone mandatory replacement applies silently (no pause): the nontoken
    /// reveal lands in the replacement zone and the wave ends without casting.
    /// This is the Rest-in-Peace shape the raw mover ignored.
    #[test]
    fn wave_reveal_single_mandatory_redirect_applies_without_pause() {
        let (mut state, horde) = wave_test_game(2);
        let card = add_wave_creature(&mut state, horde, "Wave Bear");
        state.players[horde.0 as usize].library = crate::im::vector![card];
        redirect_source(
            &mut state,
            horde,
            card,
            Zone::Exile,
            Zone::Graveyard,
            false,
            1,
        );

        let mut events = Vec::new();
        let outcome = maybe_reveal_next(&mut state, &mut events);
        assert!(
            outcome.is_none(),
            "one mandatory replacement applies silently, got {outcome:?}"
        );
        assert!(state.pending_horde_reveal.is_none(), "no pause, no record");
        assert_eq!(
            state.objects.get(&card).map(|o| o.zone),
            Some(Zone::Graveyard),
            "the silent redirect moves the card instead of casting it"
        );
        assert!(state.stack.is_empty(), "nothing is cast after a redirect");
    }

    /// Two matching mandatory replacements pause for CR 616.1 ordering; after
    /// the order is chosen the redirect applies and the wave ends uncast.
    #[test]
    fn wave_reveal_ordering_choice_applies_redirect_after_answer() {
        use crate::types::actions::GameAction;
        let (mut state, horde) = wave_test_game(2);
        let card = add_wave_creature(&mut state, horde, "Wave Bear");
        state.players[horde.0 as usize].library = crate::im::vector![card];
        redirect_source(
            &mut state,
            horde,
            card,
            Zone::Exile,
            Zone::Graveyard,
            false,
            2,
        );

        let mut events = Vec::new();
        let paused = maybe_reveal_next(&mut state, &mut events);
        assert!(
            matches!(paused, Some(WaitingFor::ReplacementChoice { .. })),
            "two matching replacements must pause for ordering, got {paused:?}"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 0 },
        )
        .expect("choose the first replacement");
        let resumed = maybe_reveal_next(&mut state, &mut events);
        assert!(resumed.is_none(), "a redirected reveal ends the wave");
        assert!(
            state.pending_horde_reveal.is_none(),
            "resume drains the record"
        );
        assert_eq!(
            state.objects.get(&card).map(|o| o.zone),
            Some(Zone::Graveyard),
            "the ordered redirect moves the card instead of casting it"
        );
        assert!(state.stack.is_empty(), "nothing is cast after a redirect");
    }

    /// A lone mandatory graveyard replacement exiles a would-be discarded
    /// Event silently, and the dispatch still runs from exile: Karai's
    /// Planning drains each hero for 3 and feeds the Bosses.
    #[test]
    fn revealed_event_discard_consults_replacements_and_dispatches() {
        use crate::database::card_db::CardDatabase;
        use crate::game::deck_loading::load_horde_library;
        let ruleset = ChallengeDeck::TurtleTeamUpHorde.default_ruleset();
        let mut state = GameState::new(FormatConfig::horde(ruleset), 2, 42);
        let horde = horde_seat(&state).expect("horde seat");
        state.active_player = horde;
        load_horde_library(
            &mut state,
            &CardDatabase::default(),
            horde,
            ChallengeDeck::TurtleTeamUpHorde,
        );
        let event = state.players[horde.0 as usize]
            .library
            .iter()
            .copied()
            .find(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.name == "Event: Karai's Planning")
            })
            .expect("the event deck holds Karai's Planning");
        state.players[horde.0 as usize].library = crate::im::vector![event];
        state.horde_wave_remaining = 1;
        redirect_source(
            &mut state,
            horde,
            event,
            Zone::Graveyard,
            Zone::Exile,
            false,
            1,
        );
        let hero = PlayerId(1);
        let hero_life = state.players[hero.0 as usize].life;
        let boss_life = state.players[horde.0 as usize].life;

        let mut events = Vec::new();
        let outcome = maybe_reveal_next(&mut state, &mut events);
        assert!(
            outcome.is_none(),
            "one mandatory replacement applies silently, got {outcome:?}"
        );
        assert_eq!(
            state.objects.get(&event).map(|o| o.zone),
            Some(Zone::Exile),
            "the graveyard replacement exiles the would-be discard"
        );
        assert_eq!(
            state.players[hero.0 as usize].life,
            hero_life - 3,
            "the dispatched Planning still drains each hero"
        );
        assert_eq!(
            state.players[horde.0 as usize].life,
            boss_life + 3,
            "the dispatched Planning still feeds the Bosses"
        );
    }
}
