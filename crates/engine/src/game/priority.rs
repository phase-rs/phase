use std::borrow::Cow;

use crate::game::engine::EngineError;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::types::ability::{
    battlefield_departure_slot, extract_target_filter_from_effect, ResolvedAbility,
    TargetChoiceTiming, TargetFilter, TargetRef,
};
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    AutoPassMode, GameState, PriorityPassingMode, StackEntry, WaitingFor,
};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

use super::effects::stack_reach::resolution_board;
use super::effects::{bind_scoped_seat, player_scope_members};
use super::mana_sources;
use super::players;
use super::precast_copy_shortcut;
use super::targeting::{object_targets, resolve_event_context_target_for_event_or_state};
use super::turn_control;
use super::turns;

/// Handle a priority pass from the current priority player (CR 117.4).
///
/// Uses a BTreeSet (priority_passes) to track which players or shared-turn
/// team representatives have passed consecutively. CR 117.4 + CR 117.6 +
/// CR 805.5b: When all players/teams pass in succession, the top object on the
/// stack resolves (or the phase advances if the stack is empty).
/// Any non-pass action clears the set (handled by callers via `reset_priority`).
/// `current_seat` is the player who *holds* priority (the semantic seat), which
/// the caller must supply — it is NOT necessarily `state.priority_player`. Under
/// a turn-control effect (CR 723, e.g. Mindslaver) these differ: per CR 723.5
/// the controller makes the controlled player's decisions and per CR 723.8 still
/// makes their own, so `priority_player` (re-derived as the authorized submitter
/// by `sync_priority_player_from_waiting_for`) collapses onto the controller for
/// *both* seats. Tracking that submitter here would let `priority_passes` never
/// accumulate more than one entry, so "all players pass in succession" could
/// never be satisfied — an infinite soft-lock. Pass the seat from `waiting_for`.
pub fn handle_priority_pass(
    current_seat: PlayerId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    handle_priority_pass_with_limit(current_seat, state, events, None).waiting_for
}

/// The observable result of one CR 117.4 priority-pass boundary.
///
/// `consumed_stack_entries` is the exact count returned by the stack resolver,
/// rather than a before/after stack-length estimate. A resolution can create
/// triggers, so a length delta is not a trustworthy authorization cursor.
pub(crate) struct PriorityPassOutcome {
    pub(crate) waiting_for: WaitingFor,
    pub(crate) consumed_stack_entries: u32,
    /// A phase transition (the cleanup step, or a leave that ends the turn)
    /// deferred until a live resolution settles; the engine pipeline settles
    /// it and retries the transition once.
    pub(crate) transition_deferred: bool,
}

/// The pass leaves the step's priority window open while its phase transition
/// waits for a live resolution to settle.
fn deferred_transition_outcome(state: &GameState) -> PriorityPassOutcome {
    PriorityPassOutcome {
        waiting_for: state.waiting_for.clone(),
        consumed_stack_entries: 0,
        transition_deferred: true,
    }
}

pub(crate) fn handle_priority_pass_with_limit(
    current_seat: PlayerId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    stack_resolution_limit: Option<u32>,
) -> PriorityPassOutcome {
    let canonical_seat = super::topology::priority_pass_representative(state, current_seat);

    // Record this seat's pass (CR 117.4). CR 117.6 + CR 805.5b: In shared-team
    // turn games, teams rather than individual players have priority, so the
    // tracked pass seat is the team's representative.
    state.priority_passes.insert(canonical_seat);

    // Also maintain legacy counter for transition period
    state.priority_pass_count += 1;

    let participants = super::topology::priority_pass_participants(state);
    let living_count = participants.len();

    if state.priority_passes.len() >= living_count {
        // CR 117.4: All living players have passed consecutively.
        clear_priority_passes(state);

        if state.stack.is_empty() {
            // Cleanup -> Untap is owned by the phase interpreter, but only
            // after the resolution pipeline has retired every popped carrier
            // and typed continuation.  Returning the current Priority window
            // lets `engine::pass_priority_once_with_pipeline` perform that
            // settlement before asking the turn interpreter to retry.
            if state.phase == Phase::Cleanup && turns::phase_transition_requires_settlement(state) {
                return deferred_transition_outcome(state);
            }
            // CR 510.4: The combat damage step's turn-based action runs in two
            // sub-steps when a first-strike/double-strike creature is present. If
            // the first-strike sub-step paused on a CR 603.3b trigger-ordering
            // prompt (combat_damage.rs), `resolve_combat_damage` returned before
            // the MANDATORY second (regular) sub-step ran (`regular_damage_done ==
            // false`). Those ordered triggers have now resolved and all players
            // passed with an empty stack, but combat damage is INCOMPLETE — re-enter
            // the combat-damage turn-based action (auto_advance re-calls
            // resolve_combat_damage, which runs only the regular sub-step) instead
            // of advancing to end of combat (which would silently skip the regular
            // damage, violating CR 510.4). `regular_damage_done` is set true before
            // the regular sub-step's own trigger processing, so the gate fires at
            // most once and then advances normally — no infinite loop.
            let combat_damage_incomplete = state.phase == crate::types::phase::Phase::CombatDamage
                && state
                    .combat
                    .as_ref()
                    .is_some_and(|c| !c.regular_damage_done);
            if combat_damage_incomplete {
                let (waiting_for, transition_deferred) =
                    turns::auto_advance_reporting_deferral(state, events);
                PriorityPassOutcome {
                    waiting_for,
                    consumed_stack_entries: 0,
                    transition_deferred,
                }
            } else if state.phase == crate::types::phase::Phase::Cleanup {
                // CR 514.3a: Triggered abilities that triggered during the
                // cleanup step (e.g. Stolen Uniform's "when you lose control
                // of that Equipment this turn") have resolved and the stack is
                // empty — "another cleanup step begins", repeating the
                // CR 514.1/514.2 turn-based actions, rather than advancing to
                // the next turn. Re-enter `auto_advance`, whose Cleanup arm
                // re-runs `execute_cleanup`; once no further trigger fires it
                // returns `None` and advances normally (the until-EOT control
                // TCE is already pruned, so no new loss event re-fires — the
                // one-shot trigger is gone, guaranteeing termination).
                // CR 514.3a: "another cleanup step begins", re-running the cleanup arm
                // directly rather than through the turn machine's step entry.
                turns::record_step_begin(state, crate::types::phase::Phase::Cleanup);
                let (waiting_for, transition_deferred) =
                    turns::auto_advance_reporting_deferral(state, events);
                PriorityPassOutcome {
                    waiting_for,
                    consumed_stack_entries: 0,
                    transition_deferred,
                }
            } else {
                // CR 117.4: Empty stack — advance to next phase. CR 500.1 +
                // CR 500.8: a leave that ends the turn (the final step of a
                // unit added after the cleanup step) defers while a
                // resolution is live; the step's priority window stays open
                // for the engine pipeline's retry.
                match turns::advance_phase_once(state, events) {
                    turns::AdvancePhaseOnce::Deferred => {
                        return deferred_transition_outcome(state);
                    }
                    turns::AdvancePhaseOnce::Entry(_) => {}
                }
                let (waiting_for, transition_deferred) =
                    turns::auto_advance_reporting_deferral(state, events);
                PriorityPassOutcome {
                    waiting_for,
                    consumed_stack_entries: 0,
                    transition_deferred,
                }
            }
        } else {
            // CR 117.4: Non-empty stack — resolve the next object. A batch-safe
            // run of identical token triggers collapses into one step that
            // consumes K entries (Tier 3); otherwise exactly one entry resolves.
            let traced = super::play_trace::stack_before_resolution(state);
            let consumed =
                super::stack::resolve_next_with_limit(state, events, stack_resolution_limit);
            if let Some(before) = traced {
                super::play_trace::record_resolutions(state, &before, consumed);
            }

            // After resolve_next: the stack shrank by `consumed` entries.
            // Update auto-pass baselines by the SAME amount so trigger-growth
            // detection stays accurate across apply() calls (§7.2 / R6).
            let session_representative_auto_pass_keys = state
                .stack_resolution_session
                .as_ref()
                .map(|session| {
                    state
                        .auto_pass
                        .keys()
                        .copied()
                        .filter(|player| {
                            session.representatives.contains(
                                &super::topology::priority_pass_representative(state, *player),
                            )
                        })
                        .collect::<std::collections::BTreeSet<_>>()
                })
                .unwrap_or_default();
            for (&player, mode) in state.auto_pass.iter_mut() {
                if let AutoPassMode::UntilStackEmpty {
                    initial_stack_len, ..
                } = mode
                {
                    if !session_representative_auto_pass_keys.contains(&player) {
                        let consumed = usize::try_from(consumed)
                            .expect("a stack resolver count fits the engine's native index size");
                        *initial_stack_len = initial_stack_len.saturating_sub(consumed);
                    }
                }
            }

            // If resolve_top set an interactive WaitingFor (e.g. RevealChoice,
            // ScryChoice, SearchChoice), preserve it instead of overwriting
            // with Priority. Only reset to Priority if the effect didn't
            // request player interaction.
            let waiting_for = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                reset_priority(state);
                WaitingFor::Priority {
                    player: state.active_player,
                }
            } else {
                state.waiting_for.clone()
            };
            PriorityPassOutcome {
                waiting_for,
                consumed_stack_entries: consumed,
                transition_deferred: false,
            }
        }
    } else {
        // CR 117.3d + CR 117.6 + CR 805.5b: The player/team passed; priority
        // moves to the next player/team in turn order. Advance from the
        // semantic seat that just passed, canonicalized to its priority
        // representative, not from `priority_player` — under CR 723
        // turn-control the latter is the controller, which would mis-seat the
        // cursor.
        let next = next_priority_player(state, canonical_seat);
        state.priority_player = next;

        events.push(GameEvent::PriorityPassed { player_id: next });

        PriorityPassOutcome {
            waiting_for: WaitingFor::Priority { player: next },
            consumed_stack_entries: 0,
            transition_deferred: false,
        }
    }
}

/// CR 117.3d: "If a player has priority and chooses not to take any actions,
/// that player passes." Passing is available to the holder of any live priority
/// window; this is the single authority for the two conditions under which the
/// engine nevertheless refuses one.
///
/// CR 723.5: under a turn-control effect the authorized submitter for the
/// holder's seat is the controller, so the live `priority_player` must be
/// compared against that mapped submitter, never against the seat itself.
///
/// CR 732.2a-c: a shortened pre-cast shortcut proposal (CR 732.2a-b) obliges the
/// player who now has priority to "make a different game choice than what was
/// originally proposed" (CR 732.2c). The runtime models that obligation as a
/// divergence latch, and a bare pass can never discharge it.
///
/// Both the `(Priority, PassPriority)` reducer arm and
/// `pass_priority_structurally_legal` below call this, so no fast path can drift
/// from the reducer.
pub fn pass_priority_legality(state: &GameState, player: PlayerId) -> Result<(), EngineError> {
    if state.priority_player != turn_control::authorized_submitter_for_player(state, player) {
        return Err(EngineError::NotYourPriority);
    }
    if precast_copy_shortcut::blocks_pass(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "A shortened pre-cast shortcut requires a different meaningful action before passing"
                .to_string(),
        ));
    }
    Ok(())
}

/// `true` iff a bare `PassPriority` by `player` is legal **and** the pass
/// boundary is decidable without simulating it.
///
/// This is the single predicate shared by every structural fast path for a pass
/// — the AI legality hatch (`ai_support::filter`) and the `phase-ai` forward
/// projection. Both must ask exactly this question; a caller that re-derives a
/// weaker approximation reintroduces the drift `pass_priority_legality` exists to
/// prevent.
///
/// CR 118.3b + CR 119.4 + CR 616.1: a parked deferred-life or cost-move payment
/// root makes the pass boundary drain a continuation
/// (`engine::resume_pending_continuation_if_priority`, whose own annotation on
/// that seam cites these same three rules), and that drain's failure modes are
/// not modelled here. Both fallible `?` sites in the drain are gated on exactly
/// these two fields being `Some`, so refusing on either is a sound, O(1)
/// over-approximation for roots that are already parked when this runs.
///
/// **Scope limit — read before "simplifying" this.** This test is evaluated
/// BEFORE the pass boundary. The drain reads these fields AFTER
/// `handle_priority_pass_with_limit` has resolved the top of the stack (CR
/// 117.4), so a root parked BY that resolution is not visible here. The
/// mitigating argument is that parking such a root coincides with installing a
/// live non-`Priority` prompt, and each fallible drain site re-tests
/// `WaitingFor::Priority` immediately before firing, so it is skipped at that
/// boundary and caught here at the next window. The local half of that argument
/// is visible in `handle_priority_pass_with_limit` above: after a CR 117.4
/// resolution it returns `state.waiting_for.clone()` untouched and only resets
/// to `Priority` when the resolved effect requested no interaction. Two links
/// are read rather than proved, though — the `PaidWithDeferredSubstitution`
/// variant's doc contract in `game::life_costs`, and that the installed prompt
/// still stands at the drain instant (the drains are not the first thing after
/// the resolution: `engine::sync_waiting_for` and the Priority-gated infallible
/// `effects::drain_pending_continuation` / `effects::resume_resolution_frames`
/// run before them). So the remainder is a known, booked residual rather than a
/// closed window.
///
/// **Note the direction**: this is a test on the *fields*, not on
/// `ai_support::classify_payment_continuation`'s verdict — that verdict can be
/// `NotAffiliated` while a field is still `Some` (see
/// `ai_support::payment_continuation::classify_parked_cost_move_root` and
/// `classify_deferred_life_root`), so a verdict test would be strictly weaker
/// and would let a fallible drain through.
///
/// Conservative by design, mirroring `ai_support::structurally_valid_search_selection`:
/// `false` only costs a simulation, so any shape this does not fully model returns
/// `false` rather than guessing.
pub fn pass_priority_structurally_legal(state: &GameState, player: PlayerId) -> bool {
    if state.pending_deferred_life_cost_resume.is_some() || state.pending_cost_move_resume.is_some()
    {
        return false;
    }
    pass_priority_legality(state, player).is_ok()
}

/// CR 117.1 + CR 723.5: whether `player` has set Full Control, a standing
/// refusal to have any priority window passed for them. Preference ownership
/// follows the authorized submitter, as it does in `auto_pass_recommended`, so
/// a controlled seat answers with its controller's preference.
pub(crate) fn holds_full_control(state: &GameState, player: PlayerId) -> bool {
    state.priority_passing_mode(turn_control::authorized_submitter_for_player(state, player))
        == PriorityPassingMode::FullControl
}

/// The Standard ladder's standing answer for `player`'s priority window while
/// `window_top` is the top of the stack (CR 117.1 + CR 117.3d).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StandingPriorityPass {
    /// CR 117.1: Full Control refuses to have any window passed for this player.
    Withheld,
    /// CR 117.3d: the player stands committed to pass this window — a yield to
    /// the top triggered ability, or the player's own object on top when it is
    /// not `Held`.
    Granted,
    /// CR 117.1d + CR 605.3a: the player's own object is on top, but resolving
    /// it can move a permanent whose mana ability they can activate now off the
    /// battlefield (CR 400.7), so the window is kept for that activation. Kept
    /// unconditionally: the mana stays until the step ends (CR 106.4), and what
    /// it can pay for once the object resolves (a sorcery-speed spell after a
    /// karoo returns itself) is not castable now, so no castability test here
    /// can tell. Opponent objects are not covered; they fall to the ladder.
    Held,
    /// No standing answer; the window needs a fresh decision.
    Undecided,
}

/// CR 117.1 + CR 117.3d: the single authority for the Standard ladder's
/// standing rungs, shared by `ai_support::auto_pass_recommended` (a
/// recommendation the client submits as `PassPriority`) and the
/// stack-resolution session, which executes it for every window it passes
/// (`engine::stack_resolution_session_participant_authorizes_pass`), and by
/// the turn-boundary auto-pass interrupt (`engine::priority_auto_pass_decision`).
pub(crate) fn standing_priority_pass(
    state: &GameState,
    player: PlayerId,
    window_top: Option<&StackEntry>,
) -> StandingPriorityPass {
    if holds_full_control(state, player) {
        return StandingPriorityPass::Withheld;
    }
    match window_top {
        // CR 117.3d: a yield is a pre-committed pass.
        Some(top) if state.is_priority_yielded(player, top) => StandingPriorityPass::Granted,
        // An own object on top is the ladder's MTGA-parity standing pass (it
        // outranks castability, see `auto_pass_recommended`): whatever the
        // player could do now, they can still do once it resolves — except
        // activate a mana ability of a permanent it moves off the battlefield.
        Some(top) if top.controller == player => {
            if resolution_can_remove_activatable_mana_source(state, player, top) {
                StandingPriorityPass::Held
            } else {
                StandingPriorityPass::Granted
            }
        }
        Some(_) | None => StandingPriorityPass::Undecided,
    }
}

/// CR 117.1d + CR 605.3a + CR 400.7: whether resolving `top` can move a
/// permanent with a mana ability `player` can activate now off the battlefield.
/// Once it moves, the permanent is a new object (CR 400.7) and that mana is out
/// of reach, so this is the one window in which the activation is possible.
///
/// An instruction's battlefield departures are named by
/// [`battlefield_departure_slot`]; [`DepartureReach::of`] decides which objects
/// each one can move. The chain is read on the board `stack::resolve_top`
/// prepares just before its first instruction runs
/// (`stack_reach::resolution_board`), so trigger-event references, the event
/// batch and the resolving entry read exactly what resolution will.
fn resolution_can_remove_activatable_mana_source(
    state: &GameState,
    player: PlayerId,
    top: &StackEntry,
) -> bool {
    let Some(root) = top.ability() else {
        return false;
    };
    if !chain_has_battlefield_departure(root) {
        return false;
    }
    let sources: Vec<ObjectId> = mana_sources::activatable_mana_actions_for_player(state, player)
        .iter()
        .filter_map(GameAction::source_object)
        .collect();
    if sources.is_empty() {
        return false;
    }
    // `None`: `resolve_top` would not run the chain as it stands (a false
    // intervening-if, a missing required target), or a declared target is no
    // longer legal. A partly illegal target set still resolves for its legal
    // targets (CR 608.2b), so that case keeps the standing pass: an accepted
    // under-approximation.
    let Some((board, prepared)) = resolution_board(state, top, root) else {
        return false;
    };
    let mut departures = Vec::new();
    collect_battlefield_departures(&prepared, &[], &mut departures);
    let reaches: Vec<DepartureReach<'_>> = departures
        .into_iter()
        .filter_map(|departure| DepartureReach::of(&board, player, departure))
        .collect();
    sources
        .iter()
        .any(|&source| reaches.iter().any(|reach| reach.contains(&board, source)))
}

/// Whether any instruction in `ability`'s chain can move battlefield objects to
/// another zone — the structural gate before any board is read.
fn chain_has_battlefield_departure(ability: &ResolvedAbility) -> bool {
    battlefield_departure_slot(&ability.effect).is_some()
        || [&ability.sub_ability, &ability.else_ability]
            .into_iter()
            .flatten()
            .any(|branch| chain_has_battlefield_departure(branch))
}

/// The objects one battlefield-departure instruction can move, as far as they
/// are known before it resolves.
enum DepartureReach<'a> {
    /// Exactly these objects: the targets chosen for an announced slot, or the
    /// object an event reference names.
    Objects(Vec<ObjectId>),
    /// Anything `filter` describes, read in `context`'s frame.
    Described {
        filter: &'a TargetFilter,
        context: Box<Cow<'a, ResolvedAbility>>,
    },
}

impl<'a> DepartureReach<'a> {
    /// - An announced target slot (CR 115.1 + CR 601.2c / CR 602.2b /
    ///   CR 603.3d) moves only the objects chosen for it, which are fixed on the
    ///   stack; with none chosen it moves nothing.
    /// - A reference to a target an earlier instruction declared ("destroy
    ///   it", CR 608.2c) moves that target: the instruction's own targets when
    ///   it already holds some (an event referent seeded as the trigger was put
    ///   on the stack), else the targets its ancestors declared, which the
    ///   resolver hands down only as the chain resolves (a slot reference reads
    ///   them all). With none declared, "it" names an object the chain
    ///   produces as it resolves, so it reaches nothing foreseeable (an
    ///   accepted under-approximation).
    /// - A reference to the triggering event's object ("sacrifice that
    ///   creature", CR 608.2k) moves that object.
    /// - Otherwise the instruction's objects are chosen or determined as it
    ///   resolves (CR 608.2d), so anything its filter describes can be moved. A
    ///   `player_scope` instruction applies once per scoped player with that
    ///   player as its controller (CR 109.5), so it reaches `player`'s
    ///   permanents only when `player` is in scope, read from `player`'s own
    ///   iteration. Membership is read from the instruction's own targets, so a
    ///   scope anchored on a target declared above it reads none (an accepted
    ///   under-approximation).
    ///
    /// `None` when the instruction can move none of `player`'s permanents.
    fn of(state: &GameState, player: PlayerId, departure: Departure<'a>) -> Option<Self> {
        let Departure {
            ability,
            filter,
            ancestor_targets,
        } = departure;
        let announced_targets = ability.target_choice_timing == TargetChoiceTiming::Stack
            && extract_target_filter_from_effect(&ability.effect)
                .is_some_and(|slot| !slot.is_context_ref());
        if announced_targets {
            return Some(Self::Objects(object_targets(&ability.targets).collect()));
        }
        let parent_target = matches!(
            filter,
            TargetFilter::ParentTarget | TargetFilter::ParentTargetSlot { .. }
        );
        if parent_target {
            let own: Vec<ObjectId> = object_targets(&ability.targets).collect();
            if !own.is_empty() {
                return Some(Self::Objects(own));
            }
            if !ancestor_targets.is_empty() {
                return Some(Self::Objects(ancestor_targets));
            }
            // With nothing declared, "it" is an object the chain produces as it
            // resolves (the card it looked at or revealed) or one the resolver
            // falls back to; neither is foreseeable here.
            return None;
        }
        if filter.is_context_ref() {
            if let Some(TargetRef::Object(id)) = resolve_event_context_target_for_event_or_state(
                state,
                filter,
                ability.source_id,
                state.current_trigger_event.as_ref(),
            ) {
                return Some(Self::Objects(vec![id]));
            }
        }
        // `Any` is the operand of an instruction that names no population of its
        // own (the card a search found, put into a hand); it describes no
        // permanent this instruction moves off the battlefield. Likewise a
        // referent the chain itself produces as it resolves ("those cards" its
        // reveal publishes, "that token" it creates, CR 608.2c) names nothing
        // yet: read now, it would name an earlier resolution's objects.
        if is_chain_produced_referent(filter) {
            return None;
        }
        let Some(scope) = &ability.player_scope else {
            return Some(Self::Described {
                filter,
                context: Box::new(Cow::Borrowed(ability)),
            });
        };
        let apnap =
            players::apnap_order_from(state, ability.starting_with.clone(), ability.controller);
        if !player_scope_members(state, scope, ability, apnap).contains(&player) {
            return None;
        }
        Some(Self::Described {
            filter,
            context: Box::new(Cow::Owned(bind_scoped_seat(
                ability,
                ability.controller,
                player,
            ))),
        })
    }

    fn contains(&self, state: &GameState, object: ObjectId) -> bool {
        match self {
            Self::Objects(objects) => objects.contains(&object),
            Self::Described { filter, context } => {
                matches_target_filter(state, object, filter, &FilterContext::from_ability(context))
            }
        }
    }
}

/// CR 608.2c: an operand naming no population of its own (`Any`) or one the
/// resolving chain publishes as it goes: the chain's tracked-set sentinel and
/// the last created / revealed / zone-changed objects. A concrete tracked set,
/// published before this object was put on the stack, is not one of them.
fn is_chain_produced_referent(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::LastZoneChanged => true,
        TargetFilter::TrackedSet { id } | TargetFilter::TrackedSetFiltered { id, .. } => {
            *id == TrackedSetId(0)
        }
        _ => false,
    }
}

/// One instruction that can move battlefield objects to another zone.
struct Departure<'a> {
    ability: &'a ResolvedAbility,
    /// The slot naming the objects it can move.
    filter: &'a TargetFilter,
    /// Object targets declared by the instructions above it in its chain — what
    /// a parent-target reference ("it") inherits as the chain resolves.
    ancestor_targets: Vec<ObjectId>,
}

/// Every instruction in `ability`'s chain (its sub-abilities and else branches)
/// that can move battlefield objects to another zone. `ancestor_targets` are the
/// object targets declared above `ability`.
fn collect_battlefield_departures<'a>(
    ability: &'a ResolvedAbility,
    ancestor_targets: &[ObjectId],
    departures: &mut Vec<Departure<'a>>,
) {
    if let Some(filter) = battlefield_departure_slot(&ability.effect) {
        departures.push(Departure {
            ability,
            filter,
            ancestor_targets: ancestor_targets.to_vec(),
        });
    }
    let mut inherited = ancestor_targets.to_vec();
    inherited.extend(object_targets(&ability.targets));
    for branch in [&ability.sub_ability, &ability.else_ability]
        .into_iter()
        .flatten()
    {
        collect_battlefield_departures(branch, &inherited, departures);
    }
}

/// Determine the next player to receive priority, using APNAP order (CR 101.4).
///
/// `current` is the semantic seat that just passed (the player who held
/// priority), which under CR 723 turn-control is distinct from
/// `state.priority_player` (the authorized submitter). Callers must pass the
/// seat, not the submitter.
///
/// For non-team formats: next living player in seat order after `current`.
/// For shared-team-turn formats: CR 117.6 + CR 805.5b make priority and pass
/// bookkeeping team-level, ordered by each team's representative.
fn next_priority_player(state: &GameState, current: PlayerId) -> PlayerId {
    let canonical_current = super::topology::priority_pass_representative(state, current);
    // CR 101.4 + CR 117.3d: `participants` is APNAP order, which already honors
    // `turn_direction`, so the main walk below inherits the reversal. The two
    // fallbacks (passer not in `participants`, or all have passed) resolve "the
    // next player in turn order" (CR 117.3d) and so must also honor direction.
    let participants = super::topology::priority_pass_participants(state);
    let Some(current_idx) = participants.iter().position(|&id| id == canonical_current) else {
        return players::next_player_in_turn_order(state, canonical_current);
    };
    for offset in 1..=participants.len() {
        let idx = (current_idx + offset) % participants.len();
        let candidate = participants[idx];
        if !state.priority_passes.contains(&candidate) {
            return candidate;
        }
    }
    players::next_player_in_turn_order(state, canonical_current)
}

/// CR 117.4: Clear consecutive priority pass bookkeeping without changing who holds priority.
pub(crate) fn clear_priority_passes(state: &mut GameState) {
    state.priority_passes.clear();
    state.priority_pass_count = 0;
}

/// Reset priority bookkeeping and grant priority to the active player.
/// Callers own the concrete rule that grants priority for their flow.
pub fn reset_priority(state: &mut GameState) {
    state.priority_player = state.active_player;
    clear_priority_passes(state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::ResolvedAbility;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{CastingVariant, StackEntry};
    use crate::types::identifiers::{CardId, ObjectId, TriggerFiring};

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.priority_pass_count = 0;
        state.priority_passes.clear();
        state
    }

    fn setup_three_player() -> GameState {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.priority_passes.clear();
        state
    }

    // --- 2-player backward compatibility ---

    #[test]
    fn two_player_single_pass_gives_priority_to_opponent() {
        let mut state = setup();
        let mut events = Vec::new();

        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        assert_eq!(state.priority_player, PlayerId(1));
        assert!(state.priority_passes.contains(&PlayerId(0)));
    }

    #[test]
    fn two_player_both_pass_empty_stack_advances_phase() {
        let mut state = setup();
        state.priority_passes.insert(PlayerId(0));
        state.priority_pass_count = 1;
        state.priority_player = PlayerId(1);

        let mut events = Vec::new();
        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        // Should advance past combat to PostCombatMain
        assert!(matches!(result, WaitingFor::Priority { .. }));
    }

    #[test]
    fn empty_stack_phase_wrap_propagates_cleanup_deferral() {
        let mut state = setup();
        state.phase = Phase::End;
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        state.priority_passes.insert(PlayerId(0));
        state.priority_pass_count = 1;
        state.resolving_stack_entry = Some(StackEntry {
            id: ObjectId(1),
            source_id: ObjectId(1),
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
                source_id: ObjectId(1),
                ability: Box::new(ResolvedAbility::new(
                    crate::types::ability::Effect::NoOp,
                    vec![],
                    ObjectId(1),
                    PlayerId(0),
                )),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });
        state.resolving_trigger_firing = Some(TriggerFiring::Ordinary);

        let outcome =
            handle_priority_pass_with_limit(PlayerId(1), &mut state, &mut Vec::new(), None);

        assert!(outcome.transition_deferred);
        assert_eq!(state.phase, Phase::Cleanup);
        assert!(matches!(
            outcome.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    #[test]
    fn two_player_both_pass_non_empty_stack_resolves_top() {
        let mut state = setup();
        state.priority_passes.insert(PlayerId(0));
        state.priority_pass_count = 1;
        state.priority_player = PlayerId(1);

        use crate::game::zones::create_object;
        use crate::types::zones::Zone;
        let created_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );

        state.stack.push_back(StackEntry {
            id: created_id,
            source_id: created_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        state
            .objects
            .get_mut(&created_id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Instant);

        let mut events = Vec::new();
        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert!(state.priority_passes.is_empty());
        assert!(state.stack.is_empty());
    }

    #[test]
    fn priority_resets_to_active_player() {
        let mut state = setup();
        state.priority_player = PlayerId(1);
        state.priority_passes.insert(PlayerId(0));
        state.priority_passes.insert(PlayerId(1));
        state.priority_pass_count = 2;

        reset_priority(&mut state);

        assert_eq!(state.priority_player, PlayerId(0));
        assert!(state.priority_passes.is_empty());
        assert_eq!(state.priority_pass_count, 0);
    }

    #[test]
    fn clear_priority_passes_preserves_priority_player() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(1);
        state.priority_passes.insert(PlayerId(0));
        state.priority_passes.insert(PlayerId(1));
        state.priority_pass_count = 2;

        clear_priority_passes(&mut state);

        assert!(state.priority_passes.is_empty());
        assert_eq!(state.priority_pass_count, 0);
        assert_eq!(state.priority_player, PlayerId(1));
    }

    // --- 3-player N-player priority ---

    #[test]
    fn three_player_first_pass_does_not_resolve_stack() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        // P0 passes, priority goes to P1
        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        assert_eq!(state.priority_player, PlayerId(1));
        assert_eq!(state.priority_passes.len(), 1);
    }

    #[test]
    fn three_player_two_passes_does_not_resolve_stack() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        // P0 passes
        handle_priority_pass(state.priority_player, &mut state, &mut events);
        // P1 passes
        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        // Still not all 3 have passed, priority goes to P2
        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(2)
            }
        ));
        assert_eq!(state.priority_passes.len(), 2);
    }

    #[test]
    fn three_player_all_pass_advances_phase() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        // P0 passes
        handle_priority_pass(state.priority_player, &mut state, &mut events);
        // P1 passes
        handle_priority_pass(state.priority_player, &mut state, &mut events);
        // P2 passes - all 3 have passed
        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        // Should advance phase (empty stack)
        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert!(state.priority_passes.is_empty());
    }

    #[test]
    fn three_player_action_clears_priority_passes() {
        let mut state = setup_three_player();
        state.priority_passes.insert(PlayerId(0));
        state.priority_passes.insert(PlayerId(1));

        // Simulate an action resetting priority
        reset_priority(&mut state);

        assert!(state.priority_passes.is_empty());
        assert_eq!(state.priority_player, PlayerId(0));
    }

    #[test]
    fn three_player_skips_eliminated_player() {
        let mut state = setup_three_player();
        // Eliminate P1
        state.players[1].is_eliminated = true;
        state.eliminated_players.push(PlayerId(1));
        let mut events = Vec::new();

        // P0 passes
        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        // Should skip P1 and go to P2
        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(2)
            }
        ));
    }

    #[test]
    fn three_player_two_living_all_pass_resolves() {
        let mut state = setup_three_player();
        // Eliminate P1
        state.players[1].is_eliminated = true;
        state.eliminated_players.push(PlayerId(1));
        let mut events = Vec::new();

        // P0 passes -> P2
        handle_priority_pass(state.priority_player, &mut state, &mut events);
        // P2 passes -> both living players passed
        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        // Should advance phase (2 living players both passed)
        assert!(matches!(result, WaitingFor::Priority { .. }));
    }

    // --- 2HG team-based priority ---

    #[test]
    fn two_hg_priority_uses_team_apnap_order() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.priority_passes.clear();
        let mut events = Vec::new();

        // P0 (active team member) passes
        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        // CR 117.6 + CR 805.5b: priority is team-level in 2HG, so the active
        // team pass moves directly to the opposing team's representative.
        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(2)
            }
        ));
        assert_eq!(state.priority_player, PlayerId(2));
        assert!(state.priority_passes.contains(&PlayerId(0)));
        assert!(!state.priority_passes.contains(&PlayerId(1)));
    }

    #[test]
    fn two_hg_two_team_passes_advance_empty_stack() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.priority_passes.clear();
        let mut events = Vec::new();

        handle_priority_pass(state.priority_player, &mut state, &mut events); // active team
        let result = handle_priority_pass(state.priority_player, &mut state, &mut events); // opposing team

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert!(state.priority_passes.is_empty());
    }

    #[test]
    fn two_hg_stale_teammate_pass_canonicalizes_to_team_representative() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(1);
        state.priority_passes.clear();
        let mut events = Vec::new();

        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        assert_eq!(
            result,
            WaitingFor::Priority {
                player: PlayerId(2)
            }
        );
        assert!(state.priority_passes.contains(&PlayerId(0)));
        assert!(!state.priority_passes.contains(&PlayerId(1)));
    }

    // --- pass legality authority (CR 117.3d / CR 723.5 / CR 732.2c) ---

    /// T1. `pass_priority_legality` is the single expression of the reducer's two
    /// pass guards. Each arm is asserted separately so a delegation that drops
    /// one of them cannot pass on the strength of the other.
    #[test]
    fn pass_priority_legality_expresses_both_reducer_guards() {
        // (a) A fresh two-player state: P0 holds priority and is its own
        // authorized submitter, no divergence latch — the pass is legal.
        let state = setup();
        assert!(pass_priority_legality(&state, PlayerId(0)).is_ok());

        // (b) CR 723.5: `priority_player` no longer maps to the seat's
        // authorized submitter.
        let mut desynced = setup();
        desynced.priority_player = PlayerId(1);
        assert!(matches!(
            pass_priority_legality(&desynced, PlayerId(0)),
            Err(EngineError::NotYourPriority)
        ));

        // (c) CR 732.2c: a shortened pre-cast shortcut obliges its owner to make
        // a different game choice; a bare pass can never discharge it.
        let mut latched = setup();
        latched.precast_shortcut_runtime.must_diverge = Some(PlayerId(0));
        let Err(EngineError::ActionNotAllowed(message)) =
            pass_priority_legality(&latched, PlayerId(0))
        else {
            panic!("a latched divergence obligation must reject a bare pass");
        };
        assert!(
            message.contains("shortened pre-cast shortcut"),
            "the reducer's exact message must be preserved by the extraction, got {message:?}"
        );
    }

    /// T1b. `pass_priority_structurally_legal` is the authority PLUS the
    /// parked-continuation field gate — not an alias for
    /// `pass_priority_legality(..).is_ok()`.
    ///
    /// Each `false` case re-asserts that `pass_priority_legality` is still `Ok`
    /// in the same state. That paired positive is the non-vacuity partner: it
    /// proves the refusal came from the field gate and not from the authority,
    /// so collapsing the two functions into one goes red here.
    ///
    /// The gate is `Option::is_some` on the two fields, so it is deliberately
    /// variant-agnostic; the variants below are the cheapest constructible
    /// representatives. `DeferredLifeCostResume::ManaRoot` is one of the two
    /// fallible deferred-life roots that motivate the gate.
    #[test]
    fn pass_priority_structurally_legal_adds_the_parked_continuation_gate() {
        use crate::types::game_state::{
            DeferredLifeCostResume, ManaAbilityResume, PendingCostMoveResume,
        };

        let clean = setup();
        assert!(pass_priority_structurally_legal(&clean, PlayerId(0)));

        let mut deferred_life = setup();
        deferred_life.pending_deferred_life_cost_resume = Some(DeferredLifeCostResume::ManaRoot {
            player: PlayerId(0),
            resume: Box::new(ManaAbilityResume::Priority),
            remaining_life_payments: vec![],
            resume_at_resolution_depth: 0,
        });
        assert!(!pass_priority_structurally_legal(
            &deferred_life,
            PlayerId(0)
        ));
        assert!(
            pass_priority_legality(&deferred_life, PlayerId(0)).is_ok(),
            "the refusal must come from the field gate, not from the authority"
        );

        let mut cost_move = setup();
        cost_move.pending_cost_move_resume = Some(PendingCostMoveResume::LoyaltyActivation {
            player: PlayerId(0),
            pw_id: crate::types::identifiers::ObjectId(1),
            resolved: Box::new(ResolvedAbility::new(
                crate::types::ability::Effect::NoOp,
                vec![],
                crate::types::identifiers::ObjectId(1),
                PlayerId(0),
            )),
            ability_index: 0,
        });
        assert!(!pass_priority_structurally_legal(&cost_move, PlayerId(0)));
        assert!(
            pass_priority_legality(&cost_move, PlayerId(0)).is_ok(),
            "the refusal must come from the field gate, not from the authority"
        );
    }

    #[test]
    fn resolve_preserves_interactive_waiting_for() {
        use crate::game::zones::create_object;
        use crate::types::ability::{Effect, TargetFilter, TargetRef};
        use crate::types::zones::Zone;

        let mut state = setup();
        state.priority_passes.insert(PlayerId(0));
        state.priority_pass_count = 1;
        state.priority_player = PlayerId(1);

        // Create a triggered ability on the stack with RevealHand effect
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Deep-Cavern Bat".to_string(),
            Zone::Battlefield,
        );

        // Add a card to opponent's hand so RevealChoice is meaningful
        let hand_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        let _ = hand_card;

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Any,
                card_filter: TargetFilter::Any,
                count: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
                reveal: true,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source_id,
            PlayerId(0),
        );

        state.stack.push_back(StackEntry {
            id: source_id,
            source_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
                source_id,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });

        let mut events = Vec::new();
        let result = handle_priority_pass(state.priority_player, &mut state, &mut events);

        // RevealHand should set RevealChoice, and priority pass should preserve it
        assert!(
            matches!(result, WaitingFor::RevealChoice { .. }),
            "Expected RevealChoice, got {:?}",
            result
        );
    }

    fn spell_entry(id: u64, controller: PlayerId, card_id: CardId) -> StackEntry {
        StackEntry {
            id: ObjectId(id),
            source_id: ObjectId(id),
            controller,
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id,
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        }
    }

    /// A triggered ability whose latched source card is `card_id`.
    fn trigger_entry(id: u64, controller: PlayerId, card_id: CardId) -> StackEntry {
        let source_id = ObjectId(id);
        let mut ability = ResolvedAbility::new(
            crate::types::ability::Effect::NoOp,
            vec![],
            source_id,
            controller,
        );
        ability.set_test_trigger_source_recursive(2, card_id);
        StackEntry {
            id: source_id,
            source_id,
            controller,
            kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
                source_id,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        }
    }

    fn yield_all_copies(state: &mut GameState, card_id: CardId) {
        state.add_priority_yield(
            PlayerId(0),
            crate::types::game_state::YieldTarget::AllCopies {
                card_id,
                trigger_description: None,
            },
        );
    }

    #[test]
    fn standing_pass_is_withheld_under_full_control() {
        let mut state = setup();
        state
            .priority_passing_modes
            .insert(PlayerId(0), PriorityPassingMode::FullControl);
        let own_top = spell_entry(1, PlayerId(0), CardId(1));

        assert_eq!(
            standing_priority_pass(&state, PlayerId(0), Some(&own_top)),
            StandingPriorityPass::Withheld
        );
        assert_eq!(
            standing_priority_pass(&state, PlayerId(0), None),
            StandingPriorityPass::Withheld
        );
    }

    #[test]
    fn standing_pass_is_granted_for_an_own_object_or_a_yielded_trigger() {
        let mut state = setup();
        let own_top = spell_entry(1, PlayerId(0), CardId(1));
        assert_eq!(
            standing_priority_pass(&state, PlayerId(0), Some(&own_top)),
            StandingPriorityPass::Granted
        );

        let yielded_top = trigger_entry(2, PlayerId(1), CardId(77));
        yield_all_copies(&mut state, CardId(77));
        assert_eq!(
            standing_priority_pass(&state, PlayerId(0), Some(&yielded_top)),
            StandingPriorityPass::Granted
        );
    }

    #[test]
    fn standing_pass_is_undecided_without_a_standing_rung() {
        let mut state = setup();
        let opponent_top = trigger_entry(1, PlayerId(1), CardId(77));
        assert_eq!(
            standing_priority_pass(&state, PlayerId(0), Some(&opponent_top)),
            StandingPriorityPass::Undecided
        );
        assert_eq!(
            standing_priority_pass(&state, PlayerId(0), None),
            StandingPriorityPass::Undecided
        );

        // Reach guard: P1 controls this trigger, so only the yield rung can
        // grant it, and the stored yield does.
        yield_all_copies(&mut state, CardId(77));
        assert_eq!(
            standing_priority_pass(&state, PlayerId(0), Some(&opponent_top)),
            StandingPriorityPass::Granted
        );
        // Yields match only triggered abilities (`GameState::is_priority_yielded`),
        // so the same stored yield does not cover a spell from that card.
        let opponent_spell = spell_entry(2, PlayerId(1), CardId(77));
        assert_eq!(
            standing_priority_pass(&state, PlayerId(0), Some(&opponent_spell)),
            StandingPriorityPass::Undecided
        );
    }

    mod removable_mana_source {
        use super::*;
        use crate::game::scenario::GameScenario;
        use crate::types::ability::{
            BounceSelection, ControllerRef, Effect, PlayerFilter, TypedFilter,
        };
        use crate::types::game_state::StackEntryKind;
        use crate::types::mana::ManaColor;
        use crate::types::zones::{EtbTapState, Zone};

        const TRIGGER_SOURCE: ObjectId = ObjectId(900);

        /// P0 controls an untapped Forest (a mana source P0 can activate now)
        /// and P1 controls a Forest of its own.
        fn board() -> (GameState, ObjectId, ObjectId) {
            let mut scenario = GameScenario::new();
            let forest = scenario.add_basic_land(PlayerId(0), ManaColor::Green);
            let opponent_forest = scenario.add_basic_land(PlayerId(1), ManaColor::Green);
            (scenario.build().state().clone(), forest, opponent_forest)
        }

        fn your_lands() -> TargetFilter {
            TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You))
        }

        fn top(controller: PlayerId, source: ObjectId, ability: ResolvedAbility) -> StackEntry {
            StackEntry {
                id: ObjectId(901),
                source_id: source,
                controller,
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ability),
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            }
        }

        fn with_trigger_event(mut entry: StackEntry, event: GameEvent) -> StackEntry {
            if let StackEntryKind::TriggeredAbility { trigger_event, .. } = &mut entry.kind {
                *trigger_event = Some(event);
            }
            entry
        }

        fn own_top(effect: Effect, targets: Vec<TargetRef>) -> StackEntry {
            top(
                PlayerId(0),
                TRIGGER_SOURCE,
                ResolvedAbility::new(effect, targets, TRIGGER_SOURCE, PlayerId(0)),
            )
        }

        fn verdict(state: &GameState, top: &StackEntry) -> StandingPriorityPass {
            standing_priority_pass(state, PlayerId(0), Some(top))
        }

        /// Guildless Commons / karoo class: "return a land you control to its
        /// owner's hand", chosen as the trigger resolves.
        fn return_a_land_you_control() -> Effect {
            Effect::Bounce {
                target: your_lands(),
                destination: None,
                selection: BounceSelection::AtResolution,
            }
        }

        #[test]
        fn own_resolution_time_bounce_of_your_land_holds_while_the_land_can_tap() {
            let (mut state, forest, _) = board();
            let bounce = own_top(return_a_land_you_control(), vec![]);
            assert_eq!(verdict(&state, &bounce), StandingPriorityPass::Held);

            // Reach guard: the predicate consults activatability — with the
            // Forest tapped there is no mana left to lose, so the pass stands.
            state.objects.get_mut(&forest).unwrap().tapped = true;
            assert_eq!(verdict(&state, &bounce), StandingPriorityPass::Granted);
        }

        #[test]
        fn opponent_resolution_time_bounce_stays_undecided() {
            let (state, _, _) = board();
            let bounce = top(
                PlayerId(1),
                TRIGGER_SOURCE,
                ResolvedAbility::new(
                    return_a_land_you_control(),
                    vec![],
                    TRIGGER_SOURCE,
                    PlayerId(1),
                ),
            );
            assert_eq!(verdict(&state, &bounce), StandingPriorityPass::Undecided);
        }

        /// The classifier keeps the own-object pass for instructions that move
        /// nothing off the battlefield, even with an activatable land in play.
        #[test]
        fn own_non_departure_instructions_keep_the_pass() {
            let (state, _, _) = board();
            // Reach guard: this board does hold for a departure instruction.
            assert_eq!(
                verdict(&state, &own_top(return_a_land_you_control(), vec![])),
                StandingPriorityPass::Held
            );
            // Amulet of Vigor: "untap it".
            let untap = own_top(
                Effect::SetTapState {
                    target: TargetFilter::TriggeringSource,
                    scope: crate::types::ability::EffectScope::Single,
                    state: crate::types::ability::TapStateChange::Untap,
                },
                vec![],
            );
            assert_eq!(verdict(&state, &untap), StandingPriorityPass::Granted);

            // Patron of the Moon: lands from hand onto the battlefield.
            let put_lands = own_top(
                Effect::ChangeZone {
                    origin: Some(Zone::Hand),
                    destination: Zone::Battlefield,
                    target: TargetFilter::Typed(TypedFilter::land()),
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: EtbTapState::Tapped,
                    enters_attacking: false,
                    up_to: true,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
                vec![],
            );
            assert_eq!(verdict(&state, &put_lands), StandingPriorityPass::Granted);

            // A search's "put that card into your hand": no origin, an `Any`
            // operand naming the found card.
            let found_card_to_hand = own_top(
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Hand,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
                vec![],
            );
            assert_eq!(
                verdict(&state, &found_card_to_hand),
                StandingPriorityPass::Granted
            );
        }

        /// Coral Atoll class: the land's own trigger sacrifices it.
        #[test]
        fn own_self_sacrifice_of_the_mana_land_holds() {
            let (state, forest, _) = board();
            let sacrifice_self = top(
                PlayerId(0),
                forest,
                ResolvedAbility::new(
                    Effect::Sacrifice {
                        target: TargetFilter::SelfRef,
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        min_count: 0,
                    },
                    vec![],
                    forest,
                    PlayerId(0),
                ),
            );
            assert_eq!(verdict(&state, &sacrifice_self), StandingPriorityPass::Held);
        }

        /// CR 608.2k: "sacrifice that permanent" names the triggering event's
        /// object, which the trigger's stack entry carries.
        #[test]
        fn own_event_reference_departure_holds_only_for_the_named_mana_source() {
            let (mut state, forest, opponent_forest) = board();
            let sacrifice_that = |object_id| {
                with_trigger_event(
                    own_top(
                        Effect::Sacrifice {
                            target: TargetFilter::TriggeringSource,
                            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                            min_count: 0,
                        },
                        vec![],
                    ),
                    GameEvent::PermanentUntapped { object_id },
                )
            };
            assert_eq!(
                verdict(&state, &sacrifice_that(forest)),
                StandingPriorityPass::Held
            );
            assert_eq!(
                verdict(&state, &sacrifice_that(opponent_forest)),
                StandingPriorityPass::Granted
            );
            state.objects.get_mut(&forest).unwrap().tapped = true;
            assert_eq!(
                verdict(&state, &sacrifice_that(forest)),
                StandingPriorityPass::Granted
            );
        }

        /// CR 608.2k: a filter axis relative to the triggering event ("lands
        /// that player controls") is read in the resolver's frame.
        #[test]
        fn own_event_relative_mass_destruction_holds_only_for_that_player() {
            let (state, _, _) = board();
            let destroy_that_players_lands = |player_id| {
                with_trigger_event(
                    own_top(
                        Effect::DestroyAll {
                            target: TargetFilter::Typed(
                                TypedFilter::land().controller(ControllerRef::TriggeringPlayer),
                            ),
                            cant_regenerate: false,
                        },
                        vec![],
                    ),
                    GameEvent::CardsDrawn {
                        player_id,
                        count: 1,
                    },
                )
            };
            assert_eq!(
                verdict(&state, &destroy_that_players_lands(PlayerId(0))),
                StandingPriorityPass::Held
            );
            assert_eq!(
                verdict(&state, &destroy_that_players_lands(PlayerId(1))),
                StandingPriorityPass::Granted
            );
        }

        /// CR 608.2c: "target land's controller sacrifices it" names the target
        /// the first instruction declared; on the stack only the chain root
        /// carries it.
        #[test]
        fn own_parent_target_departure_holds_only_for_the_declared_land() {
            let (state, forest, opponent_forest) = board();
            let sacrifice_it = |target| {
                let mut ability = ResolvedAbility::new(
                    Effect::TargetOnly {
                        target: TargetFilter::Typed(TypedFilter::land()),
                    },
                    vec![TargetRef::Object(target)],
                    TRIGGER_SOURCE,
                    PlayerId(0),
                );
                ability.sub_ability = Some(Box::new(ResolvedAbility::new(
                    Effect::Sacrifice {
                        target: TargetFilter::ParentTarget,
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        min_count: 0,
                    },
                    vec![],
                    TRIGGER_SOURCE,
                    PlayerId(0),
                )));
                top(PlayerId(0), TRIGGER_SOURCE, ability)
            };
            assert_eq!(
                verdict(&state, &sacrifice_it(forest)),
                StandingPriorityPass::Held
            );
            assert_eq!(
                verdict(&state, &sacrifice_it(opponent_forest)),
                StandingPriorityPass::Granted
            );
        }

        /// "Tap target land. Return it to its owner's hand." — a `Bounce` of the
        /// parent target reads the declared land, not its filter.
        #[test]
        fn own_parent_target_bounce_holds_only_for_the_declared_land() {
            let (state, forest, opponent_forest) = board();
            let tap_then_return_it = |target| {
                let mut ability = ResolvedAbility::new(
                    Effect::TargetOnly {
                        target: TargetFilter::Typed(TypedFilter::land()),
                    },
                    vec![TargetRef::Object(target)],
                    TRIGGER_SOURCE,
                    PlayerId(0),
                );
                ability.sub_ability = Some(Box::new(ResolvedAbility::new(
                    Effect::Bounce {
                        target: TargetFilter::ParentTarget,
                        destination: None,
                        selection: BounceSelection::Targeted,
                    },
                    vec![],
                    TRIGGER_SOURCE,
                    PlayerId(0),
                )));
                top(PlayerId(0), TRIGGER_SOURCE, ability)
            };
            assert_eq!(
                verdict(&state, &tap_then_return_it(forest)),
                StandingPriorityPass::Held
            );
            assert_eq!(
                verdict(&state, &tap_then_return_it(opponent_forest)),
                StandingPriorityPass::Granted
            );
        }

        /// A target declared after the departing instruction is not what its
        /// "it" names, and with nothing declared above it "it" is an object the
        /// chain produces as it resolves ("look at the top card ... put it into
        /// your hand"), so it falls back to neither the event nor the source.
        #[test]
        fn undeclared_parent_target_reaches_nothing() {
            let (state, forest, _) = board();
            let sacrifice_it_otherwise_destroy = |source, later_target| {
                let mut ability = ResolvedAbility::new(
                    Effect::Sacrifice {
                        target: TargetFilter::ParentTarget,
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        min_count: 0,
                    },
                    vec![],
                    source,
                    PlayerId(0),
                );
                ability.else_ability = Some(Box::new(ResolvedAbility::new(
                    Effect::TargetOnly {
                        target: TargetFilter::Typed(TypedFilter::land()),
                    },
                    vec![TargetRef::Object(later_target)],
                    source,
                    PlayerId(0),
                )));
                top(PlayerId(0), source, ability)
            };
            // The else branch targets P0's Forest; the root "it" does not read it.
            assert_eq!(
                verdict(
                    &state,
                    &sacrifice_it_otherwise_destroy(TRIGGER_SOURCE, forest)
                ),
                StandingPriorityPass::Granted
            );
            // The Forest's own trigger, nothing declared: no source fallback.
            let found_it_to_hand = with_trigger_event(
                top(
                    PlayerId(0),
                    forest,
                    ResolvedAbility::new(
                        Effect::Bounce {
                            target: TargetFilter::ParentTarget,
                            destination: None,
                            selection: BounceSelection::Targeted,
                        },
                        vec![],
                        forest,
                        PlayerId(0),
                    ),
                ),
                GameEvent::PermanentUntapped { object_id: forest },
            );
            assert_eq!(
                verdict(&state, &found_it_to_hand),
                StandingPriorityPass::Granted
            );
            // Reach guard: the same board holds once a target is declared above.
            assert_eq!(
                verdict(&state, &own_top(return_a_land_you_control(), vec![])),
                StandingPriorityPass::Held
            );
        }

        /// CR 608.2c: "put the rest on the bottom" moves the cards this chain
        /// reveals, not whatever an earlier resolution last published.
        #[test]
        fn chain_published_referents_do_not_read_an_earlier_resolution() {
            let (mut state, forest, _) = board();
            let rest_on_bottom = own_top(
                Effect::ChangeZoneAll {
                    origin: None,
                    destination: Zone::Library,
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    enters_under: None,
                    enter_tapped: EtbTapState::Unspecified,
                    enters_attacking: false,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                    library_position: None,
                    library_shuffle: crate::types::ability::MassLibraryShuffleMode::default(),
                    random_order: false,
                },
                vec![],
            );
            assert_eq!(
                verdict(&state, &rest_on_bottom),
                StandingPriorityPass::Granted
            );
            // An earlier resolution published a set holding the Forest.
            crate::game::effects::publish_tracked_set(&mut state, vec![forest]);
            state.chain_tracked_set_id = None;
            assert_eq!(
                verdict(&state, &rest_on_bottom),
                StandingPriorityPass::Granted
            );
            // Reach guard: a concrete set published before this object holds.
            let concrete = crate::game::targeting::latest_tracked_set_id(&state)
                .expect("the set was published");
            let sacrifice_those = own_top(
                Effect::Sacrifice {
                    target: TargetFilter::TrackedSet { id: concrete },
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
                vec![],
            );
            assert_eq!(
                verdict(&state, &sacrifice_those),
                StandingPriorityPass::Held
            );
        }

        /// Armageddon class: an own mass destruction of lands.
        #[test]
        fn own_mass_destruction_of_lands_holds() {
            let (state, _, _) = board();
            let armageddon = own_top(
                Effect::DestroyAll {
                    target: TargetFilter::Typed(TypedFilter::land()),
                    cant_regenerate: false,
                },
                vec![],
            );
            assert_eq!(verdict(&state, &armageddon), StandingPriorityPass::Held);
        }

        /// An announced target slot moves only what was chosen for it.
        #[test]
        fn own_targeted_destruction_holds_only_for_the_chosen_land() {
            let (state, forest, opponent_forest) = board();
            let destroy = |targets| {
                own_top(
                    Effect::Destroy {
                        target: TargetFilter::Typed(TypedFilter::land()),
                        cant_regenerate: false,
                    },
                    targets,
                )
            };
            assert_eq!(
                verdict(&state, &destroy(vec![TargetRef::Object(forest)])),
                StandingPriorityPass::Held
            );
            assert_eq!(
                verdict(&state, &destroy(vec![TargetRef::Object(opponent_forest)])),
                StandingPriorityPass::Granted
            );
            // "Up to one target land" with no target chosen moves nothing.
            let mut up_to_one = destroy(vec![]);
            up_to_one.ability_mut().unwrap().optional_targeting = true;
            // Reach guard: the board is prepared, so the verdict comes from
            // the empty announced slot, not from a refused resolution.
            assert!(resolution_board(&state, &up_to_one, up_to_one.ability().unwrap()).is_some());
            assert_eq!(verdict(&state, &up_to_one), StandingPriorityPass::Granted);
        }

        /// CR 109.5: a `player_scope` sacrifice applies with each scoped player as
        /// its controller, so "you control" names the scoped player's lands.
        #[test]
        fn player_scoped_sacrifice_holds_only_when_you_are_in_scope() {
            let (state, _, _) = board();
            let scoped_sacrifice = |scope| {
                let mut ability = ResolvedAbility::new(
                    Effect::Sacrifice {
                        target: your_lands(),
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        min_count: 0,
                    },
                    vec![],
                    TRIGGER_SOURCE,
                    PlayerId(0),
                );
                ability.player_scope = Some(scope);
                top(PlayerId(0), TRIGGER_SOURCE, ability)
            };
            // Liliana's Triumph shape: "each opponent sacrifices".
            assert_eq!(
                verdict(&state, &scoped_sacrifice(PlayerFilter::Opponent)),
                StandingPriorityPass::Granted
            );
            // "Each player sacrifices".
            assert_eq!(
                verdict(&state, &scoped_sacrifice(PlayerFilter::All)),
                StandingPriorityPass::Held
            );
        }
    }
}
