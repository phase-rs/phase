use crate::game::engine::EngineError;
use crate::types::events::GameEvent;
use crate::types::game_state::{AutoPassMode, GameState, WaitingFor};
use crate::types::player::PlayerId;

use super::players;
use super::precast_copy_shortcut;
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
    pub(crate) consumed_stack_entries: usize,
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
                PriorityPassOutcome {
                    waiting_for: turns::auto_advance(state, events),
                    consumed_stack_entries: 0,
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
                PriorityPassOutcome {
                    waiting_for: turns::auto_advance(state, events),
                    consumed_stack_entries: 0,
                }
            } else {
                // CR 117.4: Empty stack — advance to next phase.
                let _ = turns::advance_phase_once(state, events);
                PriorityPassOutcome {
                    waiting_for: turns::auto_advance(state, events),
                    consumed_stack_entries: 0,
                }
            }
        } else {
            // CR 117.4: Non-empty stack — resolve the next object. A batch-safe
            // run of identical token triggers collapses into one step that
            // consumes K entries (Tier 3); otherwise exactly one entry resolves.
            let consumed =
                super::stack::resolve_next_with_limit(state, events, stack_resolution_limit);

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
                        *initial_stack_len = initial_stack_len.saturating_sub(consumed as usize);
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
    use crate::types::identifiers::CardId;

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
}
