use rand::Rng;
use std::collections::VecDeque;
use thiserror::Error;

use crate::types::ability::{EffectKind, KeywordAction, TargetRef};
#[cfg(test)]
use crate::types::ability::{EffectScope, TapStateChange};
use crate::types::actions::{
    GameAction, MayTriggerAutoChoiceOp, PriorityYieldOp, TriggerOrderTemplateOp,
};
use crate::types::events::{BendingType, ContestRound, GameEvent, ManaTapState, PlayerActionKind};
use crate::types::game_state::{
    ActionResult, AssistState, AutoPassMode, AutoPassRequest, CastOfferKind, ConvokeMode,
    CostResume, GameState, LandPlayRecord, LoopDetectionMode, MayTriggerAutoChoiceKey, PayCostKind,
    RetargetScope, StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::match_config::MatchType;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::ability_utils::{
    begin_target_selection_for_ability, build_target_slots, cap_distribution_target_slots,
    compute_unavailable_modes, has_legal_target_assignment_for_ability, modal_choice_for_player,
};
use super::casting;
use super::casting_costs;
use super::effects;
use super::engine_casting;
use super::engine_combat;
use super::engine_modes;
use super::engine_payment_choices;
use super::engine_priority;
use super::engine_replacement;
use super::engine_resolution_choices;
use super::engine_stack;
use super::mana_abilities;
use super::mana_payment;
use super::mana_sources;
use super::match_flow;
use super::mulligan;
use super::planeswalker;
use super::priority;
use super::public_state::{
    bump_state_revision, finalize_display_state, finalize_public_state, finalize_rules_state,
    mark_public_state_all_dirty, mark_public_state_from_events, sync_waiting_for,
};
use super::sba;
use super::splice;
use super::triggers;
use super::turn_control;
use super::turns;
use super::zones;

pub use super::engine_resolve_batch::{
    resolve_all_fast_forward, ResolveAllCallbackDecision, ResolveAllFastForwardResult,
};

#[derive(Debug, Clone, Error)]
pub enum EngineError {
    #[error("Invalid action: {0}")]
    InvalidAction(String),
    #[error("Wrong player")]
    WrongPlayer,
    #[error("Not your priority")]
    NotYourPriority,
    #[error("Action not allowed: {0}")]
    ActionNotAllowed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicFinalizeMode {
    Immediate,
    DeferredDisplay,
}

fn handle_unlock_room_door(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    door: crate::game::game_object::RoomDoor,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if state.active_player != player
        || !matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        || !state.stack.is_empty()
    {
        return Err(EngineError::ActionNotAllowed(
            "Room doors can be unlocked only as a main-phase special action with an empty stack"
                .to_string(),
        ));
    }

    let cost = {
        let obj = state
            .objects
            .get(&object_id)
            .ok_or_else(|| EngineError::InvalidAction("Room not found".to_string()))?;
        if obj.controller != player || obj.zone != Zone::Battlefield {
            return Err(EngineError::ActionNotAllowed(
                "Only the controller of a battlefield Room can unlock it".to_string(),
            ));
        }
        if !obj
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Room")
        {
            return Err(EngineError::ActionNotAllowed(
                "Object is not a Room".to_string(),
            ));
        }
        if obj.room_unlocks.unwrap_or_default().is_unlocked(door) {
            return Err(EngineError::ActionNotAllowed(
                "That door is already unlocked".to_string(),
            ));
        }
        match door {
            crate::game::game_object::RoomDoor::Left => obj.mana_cost.clone(),
            crate::game::game_object::RoomDoor::Right => obj
                .back_face
                .as_ref()
                .map(|face| face.mana_cost.clone())
                .ok_or_else(|| {
                    EngineError::ActionNotAllowed("Room has no right door face".to_string())
                })?,
        }
    };

    // CR 116.2m + CR 118.7a: Reduce the door's generic unlock cost by the
    // player's active `ReduceActionCost { action: UnlockDoor }` statics
    // (Inquisitive Glimmer — "Unlock costs you pay cost {1} less") before
    // payment. Single authority shared with the plot path.
    let cost = casting::apply_special_action_cost_reduction(
        state,
        player,
        crate::types::mana::SpecialAction::UnlockDoor,
        cost,
    );

    // CR 116.2m + CR 709.5e + CR 106.6: The unlock cost is a special action's
    // mana cost. Route payment through `PaymentContext::SpecialAction(UnlockDoor)`
    // so spend-restricted mana ("only to … unlock doors", Smoky Lounge) is
    // eligible here and spell/activation-restricted mana is correctly rejected.
    casting::pay_special_action_mana_cost(
        state,
        player,
        Some(object_id),
        &cost,
        crate::types::mana::SpecialAction::UnlockDoor,
        events,
    )?;

    super::room::unlock_door_designation(state, object_id, player, door, events);
    Ok(WaitingFor::Priority { player })
}

/// Public engine entrypoint. Every caller must supply the `actor` — the
/// `PlayerId` whose authenticated identity is making this action. The engine
/// rejects any action whose `actor` does not match `authorized_submitter(state)`
/// (with a narrow Concede exception — see `check_actor_authorization`).
///
/// # Safety contract (non-negotiable)
///
/// `actor` must come from a **trusted transport boundary**, never from
/// client-supplied payload data. Adapters that forward actions from a remote
/// peer (WebSocket server, P2P host) must tag the action with the PlayerId
/// associated with the *connection*, not a value copied out of the wire frame.
/// Otherwise a malicious peer can trivially spoof another player's identity.
///
/// Engine-internal simulation (AI search, legal-action probing) may use
/// [`apply_as_current`] which derives `actor` from the game state itself.
pub fn apply(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_action_boundary(state, actor, action, PublicFinalizeMode::Immediate)
}

/// Explicit-actor simulation apply: [`apply`] for throwaway forward-projection
/// clones the caller never renders (the AI velocity-policy `project_to`
/// look-ahead). Identical rules resolution to [`apply`], but in
/// `DeferredDisplay` mode it skips `finalize_display_state` — the board-global
/// mana-availability sweep whose frontend-only output no rules or
/// AI-evaluation path consults. See [`apply_as_current_for_simulation`] for the
/// actor-derived counterpart used by the search's `apply_candidate`; both keep
/// the projected/simulated game-logic state rules-correct while removing the
/// per-step O(battlefield) display sweep (#4798).
pub fn apply_for_simulation(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_action_boundary(state, actor, action, PublicFinalizeMode::DeferredDisplay)
}

pub(super) fn apply_action_boundary(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
    mode: PublicFinalizeMode,
) -> Result<ActionResult, EngineError> {
    apply_action_boundary_with_stack_limit(state, actor, action, mode, None)
}

pub(super) fn apply_action_boundary_with_stack_limit(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
    mode: PublicFinalizeMode,
    stack_resolution_limit: Option<u32>,
) -> Result<ActionResult, EngineError> {
    // Clear transient inter-effect state at the start of each player action.
    // last_effect_count is set by interactive handlers (e.g., DiscardChoice) and
    // consumed by sub_ability continuations via EventContextAmount fallback.
    state.last_effect_count = None;
    state.last_effect_counts_by_player.clear();
    state.exiled_from_hand_this_resolution = 0;
    state.die_result_this_resolution = None;
    state.consumed_before_priority_trigger_events.clear();
    check_actor_authorization(state, actor, &action)?;
    let mut result = match apply_action(state, actor, action, stack_resolution_limit) {
        Ok(result) => result,
        Err(err) => {
            state.consumed_before_priority_trigger_events.clear();
            return Err(err);
        }
    };
    state.consumed_before_priority_trigger_events.clear();
    reconcile_terminal_result(state, &mut result);
    bump_state_revision(state);
    sync_waiting_for(state, &result.waiting_for);
    run_auto_pass_loop(state, &mut result);
    reconcile_terminal_result(state, &mut result);
    // Debug "infinite mana" (CR 500.5 suppressed for flagged players): restore any
    // pool that a spend during this action depleted, before public state is
    // finalized and the next affordability probe runs. No-op when none flagged.
    super::mana_payment::refill_infinite_mana(state);
    remember_public_reveals(state, &result.events);
    // Targeted public-state dirty marking over the full accumulated event set
    // (the auto-pass loop appends events). `finalize_public_state` is the only
    // consumer of `public_state_dirty`, so marking once here over the complete
    // event stream is correct and cheapest.
    mark_public_state_from_events(state, &result.events);
    finalize_rules_state(state);
    result.waiting_for = state.waiting_for.clone();
    if matches!(mode, PublicFinalizeMode::Immediate) {
        finalize_display_state(state);
    }
    result.log_entries = super::log::resolve_log_entries(&result.events, state);
    Ok(result)
}

thread_local! {
    /// PR-3 (Option C): set while inside a legality/search simulation probe
    /// (`ai_support::SimulationFilter`'s clone-and-apply). Loop-shortcut detection
    /// (`reconcile_terminal_result` §3) and ring accumulation
    /// (`pass_priority_once_with_pipeline` §2) are TOP-LEVEL-ONLY — a hypothetical
    /// single-action probe is NOT a real CR 732.2a play sequence, so it must neither
    /// shortcut nor accumulate. Engine game logic is single-threaded (no rayon /
    /// par_iter / std::thread::spawn in the apply or legal_actions path), `apply()` is
    /// fully synchronous (no `.await` between set and restore), and the tokio server
    /// runs each apply synchronously within one task on one thread, so the RAII
    /// set/restore is balanced on a single thread within one call. Mirrors the in-engine
    /// thread-local idiom (`perf_counters.rs`, `layers.rs`, `quantity.rs`).
    static IN_SIMULATION_PROBE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// True while inside a `SimulationFilter` legality probe. Read by §2 and §3.
pub(crate) fn in_simulation_probe() -> bool {
    IN_SIMULATION_PROBE.with(|f| f.get())
}

/// RAII guard: sets the probe flag, restores the PREVIOUS value on drop (panic-safe,
/// nesting-correct — a probe that itself enumerates legal actions keeps the flag set).
#[must_use]
pub(crate) struct SimulationProbeGuard(bool);
impl SimulationProbeGuard {
    pub(crate) fn enter() -> Self {
        SimulationProbeGuard(IN_SIMULATION_PROBE.with(|f| f.replace(true)))
    }
}
impl Drop for SimulationProbeGuard {
    fn drop(&mut self) {
        IN_SIMULATION_PROBE.with(|f| f.set(self.0));
    }
}

fn reconcile_terminal_result(state: &mut GameState, result: &mut ActionResult) {
    // Safety net (fixes #962): If a player-loss SBA would eliminate a player,
    // run SBAs now. CR 704.3 normally checks SBAs when a player would receive
    // priority, but skipping them here can leave the engine waiting on a dead
    // player for a non-priority choice.
    //
    // The predicate lives in `sba` so it shares the same CR 101.2 "can't lose"
    // exception as the real player-loss SBA checks, and stays narrower than the
    // full SBA loop to avoid unrelated mid-resolution SBA prompts.
    if sba::has_pending_player_loss_sba(state) {
        sba::check_state_based_actions(state, &mut result.events);
        // SBA may have advanced waiting_for (e.g., GameOver, or Priority for
        // the next living player). Sync the result.
        result.waiting_for = state.waiting_for.clone();
    }

    super::elimination::ensure_game_over_if_terminal(state, &mut result.events);
    if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        match_flow::handle_game_over_transition(state);
        result.waiting_for = state.waiting_for.clone();
    }

    // CR 732.2a + CR 704.5a: shortcut a NET-PROGRESS mandatory cascade to its
    // determinate single-opponent loss. Runs AFTER the CR 704 state-based actions
    // above (CR 704.3 ordering), so a player ALREADY at 0 life loses via the real
    // 704.5a SBA first and this never preempts or double-fires a legitimate win — it
    // only fires when the game would otherwise grind on (high victim life, or mid-drain
    // before 0). The `!GameOver` guard makes it idempotent across the :196/:200 calls.
    if !matches!(state.waiting_for, WaitingFor::GameOver { .. })
        && matches!(state.waiting_for, WaitingFor::Priority { .. }) // a player would get priority (CR 704.3)
        // CR 732.2a: the mandatory-loop game-ending shortcut is gated behind the
        // user-controllable combo-detector opt-in. With `loop_detection == Off` (the
        // default) the engine NEVER resolves a mandatory loop to its determinate
        // outcome — the game simply continues as it did before the combo-detector
        // existed (the natural CR 704.5a SBA death still ends a real life drain, just
        // not as a shortcut). This is an intentional opt-in departure: new
        // game-changing functionality ships OFF so it can be developed safely
        // (issue #4603). When OFF the ring is also never populated (the sampler is
        // gated identically), so this conjunct is defense-in-depth, not the sole gate.
        // PR-7 Phase 3: `samples()` (not `is_on()`) so `Interactive` also enters. For
        // `Off` (false) and `On` (true) `samples() == is_on()`, so both are unchanged;
        // only `Interactive` newly enters, dispatched by the mode `match` in the body.
        && state.loop_detection.samples()
        && !state.stack.is_empty()
        && !state.loop_detect_ring.is_empty()
        // PR-3 Defect-2: loop-shortcut detection is TOP-LEVEL-ONLY. Inside a
        // `SimulationFilter` legality probe the flag is set, so §3 is skipped. This
        // enforces the invariant that a hypothetical single-action probe never runs
        // game-ending shortcut logic, and guards the
        // reconcile→§3→§9→legal_actions→SimulationFilter→reconcile path against
        // unbounded re-entry. (In the current architecture the §9 gate's pass-state
        // reset already makes those nested probes handoffs that do not re-resolve, so
        // the path is bounded even without this conjunct — see the impl report's
        // Defect-2 measurement — but the guard keeps the top-level-only invariant
        // explicit and robust to future §9/§2 changes.)
        && !in_simulation_probe()
    {
        // PR-7 Phase 3: dispatch the confirmed-loop body by mode. The `On` arm is the
        // pre-change block VERBATIM — byte-identical event stream, proven by the T-ON
        // golden captured from HEAD before this wrap. `Interactive` routes to the general
        // classification bridge (offer + APNAP window + CR 732.4 draw). `Off` is
        // unreachable: the `samples()` guard above excludes it.
        match state.loop_detection {
            LoopDetectionMode::On => {
                // Clone the Arc handles (cheap refcount bumps) to release the borrow on the
                // ring before the GameOver mutation below.
                let priors: Vec<std::sync::Arc<GameState>> =
                    state.loop_detect_ring.iter().cloned().collect();
                let cur = crate::analysis::resource::ResourceVector::snapshot(state);
                // Carry the matching cycle's `delta` out of the scan alongside the winner so
                // the ∞ producer below can name the loop's unbounded axes without recomputing.
                // INDEXED scan (not `find_map`) so the matched prior's ring index `k` is known:
                // the m9 controller-non-dip and R5-B2 faller-simultaneity checks consume the
                // SAME `frames[k..] ++ live` per-resolution window. On a candidate winner that
                // fails either seam gate, continue scanning older priors (fail-safe).
                if let Some((winner, delta)) = priors.iter().enumerate().find_map(|(k, prior)| {
                    let delta = crate::analysis::resource::ResourceVector::delta(
                        &crate::analysis::resource::ResourceVector::snapshot(prior),
                        &cur,
                    );
                    let winner = crate::analysis::loop_check::live_mandatory_loop_winner(
                        prior, state, &delta,
                    )?;
                    // The matched window: the prior frame at `k`, every subsequent ring frame,
                    // then the live state — all per-resolution, no gaps (a non-sampling beat
                    // clears the ring, so a confirmed window is gap-free).
                    let mut frames: Vec<&GameState> =
                        priors[k..].iter().map(|p| p.as_ref()).collect();
                    frames.push(state);
                    // CR 704.5a + CR 104.4a (m9): the winner (sole non-faller) must never dip
                    // across the window — a transient intra-cycle dip a net-delta check cannot
                    // see would kill it before the extrapolated win.
                    if !crate::analysis::loop_check::winner_life_never_dips(&frames, winner) {
                        return None;
                    }
                    // CR 704.3 + CR 800.4a + CR 104.2a (R5-B2): with ≥2 fallers, require
                    // pairwise-equal faller life at every frame so all cross lethal in ONE SBA
                    // batch (the first elimination is terminal — nothing past it is modeled).
                    let fallers: Vec<crate::types::player::PlayerId> = state
                        .players
                        .iter()
                        .filter(|p| !p.is_eliminated)
                        .map(|p| p.id)
                        .filter(|p| delta.life.get(p).copied().unwrap_or(0) < 0)
                        .collect();
                    if fallers.len() >= 2
                        && !crate::analysis::loop_check::fallers_lives_pairwise_equal(
                            &frames, &fallers,
                        )
                    {
                        return None;
                    }
                    Some((winner, delta))
                }) {
                    // CR 732.5: shortcut ONLY a loop NO living player can break. The gate runs
                    // ONCE after find_map (not per prior). At the per-beat drive this is the
                    // entire soundness firewall.
                    if no_living_player_has_meaningful_priority_action(state) {
                        // CR 732.2a: persist the confirmed loop's unbounded axes so
                        // `derive_views` projects the `∞` HUD rows. `winner` is the loop's
                        // controller (the non-faller); `unbounded_axes_for(winner)` returns the
                        // same axes `detect_loop` records in `LoopCertificate.unbounded`. This is
                        // the live producer of `unbounded_resources` for a detected loop (the
                        // debug `SetInfiniteMana` toggle is the only other producer). It runs
                        // only inside this OFF-gated block, so a default-OFF game never marks ∞.
                        state.mark_unbounded_loop(winner, &delta.unbounded_axes_for(winner));
                        result.events.push(GameEvent::GameOver {
                            winner: Some(winner),
                        });
                        state.waiting_for = WaitingFor::GameOver {
                            winner: Some(winner),
                        };
                        result.waiting_for = state.waiting_for.clone();
                        match_flow::handle_game_over_transition(state);
                    }
                }
            }
            LoopDetectionMode::Interactive => interactive_loop_bridge(state, result),
            LoopDetectionMode::Off => {
                unreachable!("reconcile shortcut body: samples() guard excludes Off")
            }
        }
    }

    // PR-7 Phase 4d-ii (CR 732.2a): the EMPTY-STACK dual of the ring-gated bridge above.
    // A self-returning (buyback) recast that creates an inert token settles with an EMPTY
    // stack, so the sampler clears the ring at that beat and the `!stack.is_empty()` bridge
    // is structurally unreachable for it. Detect it here by driving the captured recast on
    // a clone. Gated identically (opt-in + top-level-only) plus a cheap `last_recast_context`
    // precondition (set only on a buyback-paid, token-creating cast — so the clone-drive runs
    // ~never). INV-2: this OFFERS the interactive shortcut (never auto-resolves — CR 732.2a).
    if !matches!(state.waiting_for, WaitingFor::GameOver { .. })
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
        && state.stack.is_empty()
        && state.loop_detection.samples()
        && !in_simulation_probe()
        && state.last_recast_context.is_some()
    {
        if let Some((certificate, schema)) = try_offer_object_growth_shortcut(state) {
            let WaitingFor::Priority { player: proposer } = state.waiting_for else {
                unreachable!("guarded by matches!(Priority) above")
            };
            state.waiting_for = WaitingFor::LoopShortcut {
                proposer,
                predicted_winner: None,
                certificate,
                schema,
            };
            result.waiting_for = state.waiting_for.clone();
        }
    }
}

/// PR-7 Phase 3 (CR 732.2a/b/c + CR 732.4 + CR 704.5a): the `Interactive`-mode branch of
/// the reconcile shortcut block. Routes the SAME confirmed live loop signal the `On` arm
/// consumes through the GENERAL classification instead of only the lethal auto-win:
///
/// - **Path A — determinate lethal single-winner** (constant-depth OR ω growing cascade,
///   via the reused, UN-widened [`crate::analysis::loop_check::live_mandatory_loop_winner`]):
///   if the loop is mandatory (CR 732.5: no living player can break it) this AUTO-WINS
///   exactly as `On` does (mandatory winning drain). If it is OPTIONAL (some player could
///   respond) it OFFERS the interactive shortcut (CR 732.2a) via `WaitingFor::LoopShortcut`.
/// - **Path B — CR 732.4 all-mandatory, net-progress, no-loss draw**: a confirmed cycle
///   with no determinate winner that drives NO player toward a loss and that no living
///   player can break is a draw (CR 104.4b / 104.4f).
///
/// Everything else (staggered-pod losses, optional pure-advantage loops) falls through
/// with no action — the pre-feature halt/continue behavior. Runs inside the same
/// top-level-only `!in_simulation_probe()` guard as the `On` arm.
///
/// Multiplayer subset-lethality is safe by construction: [`find_live_loop_winner`] delegates
/// to [`crate::analysis::loop_check::live_mandatory_loop_winner`], which partitions the living
/// players into life-fallers vs non-fallers and requires EXACTLY one non-faller
/// (`nonfallers.len() == 1`; CR 104.2a — a winner is determinate only when every other living
/// player falls). A loop lethal to only SOME opponents leaves a surviving bystander as a
/// second non-faller ⇒ `None` ⇒ neither Path A (no winner) nor Path B (a life-loss axis is
/// present, so it is not a CR 732.4 no-loss draw) fires, and it falls through without crowning.
fn interactive_loop_bridge(state: &mut GameState, result: &mut ActionResult) {
    // CR 732.5 / CR 732.2b: is the loop mandatory (no living player has a meaningful
    // priority action that could break it)? The single mandatory-vs-optional signal the
    // engine already computes — not a new stored flag.
    let mandatory = no_living_player_has_meaningful_priority_action(state);

    // Path A: determinate lethal single-winner drain.
    if let Some((winner, delta, prior)) = find_live_loop_winner(state) {
        if mandatory {
            // FIRM #1 — mandatory winning drain: identical to the `On` auto-win.
            // CR 732.2a: mark the loop's unbounded axes; CR 704.5a: terminal GameOver.
            state.mark_unbounded_loop(winner, &delta.unbounded_axes_for(winner));
            result.events.push(GameEvent::GameOver {
                winner: Some(winner),
            });
            state.waiting_for = WaitingFor::GameOver {
                winner: Some(winner),
            };
            result.waiting_for = state.waiting_for.clone();
            match_flow::handle_game_over_transition(state);
        } else {
            // CR 732.2a: OPTIONAL winning drain — only the player with priority may propose
            // the shortcut. Keep that proposer distinct from the already-measured winner; a
            // loop can be detected during a different player's priority window.
            let certificate = build_cert(prior.as_ref(), state, &delta, winner);
            // CR 732.2a: a non-targeted drain reifies no per-iteration player choice ⇒ carry an
            // empty pin list; only the `iteration_count` (from `win_kind`) is populated.
            let WaitingFor::Priority { player: proposer } = state.waiting_for else {
                unreachable!("interactive bridge only runs during priority")
            };
            let schema = build_shortcut_schema(&[], certificate.win_kind, state, proposer);
            state.waiting_for = WaitingFor::LoopShortcut {
                proposer,
                predicted_winner: Some(winner),
                certificate,
                schema,
            };
            result.waiting_for = state.waiting_for.clone();
        }
        return;
    }

    // Path B: CR 732.4 all-mandatory, net-progress, no-loss draw. Only reached when Path A
    // found no determinate winner. `mandatory` gates it (CR 732.5); a loss axis or an
    // optional loop falls through to the pre-feature halt.
    if mandatory {
        let priors: Vec<std::sync::Arc<GameState>> =
            state.loop_detect_ring.iter().cloned().collect();
        let cur = crate::analysis::resource::ResourceVector::snapshot(state);
        for prior in &priors {
            let delta = crate::analysis::resource::ResourceVector::delta(
                &crate::analysis::resource::ResourceVector::snapshot(prior),
                &cur,
            );
            // CR 732.2a board-recurrence (constant-depth OR ω growing cascade) + net
            // progress + NO loss axis for anyone ⇒ the loop grinds forever with nobody
            // able to win or lose ⇒ CR 732.4 / 104.4b draw.
            if (crate::analysis::resource::loop_states_equal_modulo_resources(prior, state)
                || crate::analysis::resource::loop_states_cover_modulo_growth(prior, state))
                && delta.is_net_progress()
                && has_no_loss_axis(&delta)
            {
                result.events.push(GameEvent::GameOver { winner: None });
                state.waiting_for = WaitingFor::GameOver { winner: None };
                result.waiting_for = state.waiting_for.clone();
                match_flow::handle_game_over_transition(state);
                return;
            }
        }
    }
    // PR-7 Phase 4c (B5): OPTIONAL beneficial (non-winning) loop ⇒ revocable-∞ capability.
    // CR 104.4b: "Loops that contain an optional action don't result in a draw" — so an
    // optional net-progress no-loss loop is neither crowned (Path A: no faller) nor drawn
    // (Path B: !mandatory). It grinds under player control; record the unbounded capability
    // (mark_unbounded_loop) + its enablers so an enabler's departure REVOKES it (defuse hook
    // in zones.rs `apply_zone_exit_cleanup`). Reached only when Path A named no winner AND
    // the loop is OPTIONAL (a player can break it) — the pre-feature halt already applied
    // when Path B's `mandatory` gate excludes this branch, so this is a NEW arm, not a
    // narrowing of one.
    //
    // CR-FIDELITY NOTE: CR 104.4b grants the controller "no draw + player control", NOT a
    // persistent resource. The realization here reuses `unbounded_resources` /
    // `refill_infinite_mana`, which is a DOCUMENTED DEBUG-ONLY DEPARTURE FROM THE RULES
    // (mana_payment.rs top-up); reusing it for a real detected loop is team-lead's stated
    // design intent (in-scope). The mark means "this player can grind this axis unboundedly
    // under their own control", the closest live realization of CR 104.4b's grant.
    if !mandatory {
        let controller = state.active_player; // sampler gate is Priority{active_player}: the driver
        let priors: Vec<std::sync::Arc<GameState>> =
            state.loop_detect_ring.iter().cloned().collect();
        let cur = crate::analysis::resource::ResourceVector::snapshot(state);
        for prior in &priors {
            let delta = crate::analysis::resource::ResourceVector::delta(
                &crate::analysis::resource::ResourceVector::snapshot(prior),
                &cur,
            );
            // Same recurrence + net-progress predicate as Path B (byte-reused), minus the
            // `mandatory` gate. The object-growth disjunct is the SHARED-BUT-DORMANT arm
            // (empty residual today; lights up under 4a-live with no further edit).
            //
            // REDUNDANCY PROOF (R6, team-lead-verified): `has_no_loss_axis` (conjunct 3
            // below) is UNCONDITIONALLY REDUNDANT at this Path-C call site — every
            // self-loss axis it checks is already rejected by an EARLIER conjunct, so
            // removing it changes no Path-C outcome and a discriminating runtime test for
            // it HERE is unsatisfiable (waived; kept as documented defense-in-depth):
            //   - library↓ (self-mill): a card leaving the Library zone changes its
            //     `objects_content_eq` zone, so successive frames compare UNEQUAL and
            //     recurrence (conjunct 1) fails first — the loop never recurs, so this
            //     arm is never even reached.
            //   - life↓ (self-burn): life is a Consumed axis (`ResourceVector::components`),
            //     so `is_net_progress` (conjunct 2) returns false on any net-negative life
            //     (resource.rs ~:409, over all players) before conjunct 3 runs.
            //   - poison↑ (self-poison): `classify_win_kind` (conjunct 4) maps poison>0 to
            //     `WinKind::PoisonLoss`, not `Advantage`, so the `== Advantage` conjunct
            //     rejects it.
            // CONTRAST — the Path-B DRAW gate (:512-516 = recurrence + is_net_progress +
            // has_no_loss_axis, with NO `== Advantage` backstop) is DIFFERENT: there
            // `has_no_loss_axis` is the SOLE loss-axis veto and is LOAD-BEARING BY
            // CONSTRUCTION — it MUST NOT be removed. A poison loop reaching Path B satisfies
            // recurrence (poison is projected out at resource.rs:1995) AND is_net_progress
            // (poison is a Gained axis, which cannot make is_net_progress false), so without
            // this conjunct such a loop would be WRONGLY certified a CR 732.4 draw. (Path C's
            // poison redundancy comes ENTIRELY from its extra `== Advantage` conjunct, which
            // Path B lacks.) The Path-B veto is currently NOT runtime-discriminable: a
            // single-compound-trigger poison loop DOES reach the Path-B bridge, but the
            // "you gain N life and [each opponent gets a poison counter]" parser drop removes
            // the poison conjunct (card-build keeps only `GainLife`), so poison is 0 in the loop
            // delta at the gate → it draws as a benign lifegain loop and never exercises
            // has_no_loss_axis's poison veto. No constructible fixture carries poison>0 to the
            // Path-B gate (the 2-trigger form clears `loop_detect_ring` on its OrderTriggers
            // beats at engine.rs:1307; the single-compound-trigger form drops the poison at
            // parse). The runtime discriminator is therefore WAIVED as measured-unsatisfiable;
            // this in-code load-bearing-by-construction proof is the substitute. See the
            // `interactive_recurring_poison_is_not_drawn` Path-B behavioral test.
            if (crate::analysis::resource::loop_states_equal_modulo_resources(prior, state)
                || crate::analysis::resource::loop_states_cover_modulo_growth(prior, state)
                // CR 122.1 + CR 104.4b: OR a pure preserved-`Generic` counter-growth
                // cover (proliferate/charge Pentad Prism, burden The One Ring). Live
                // revocable-∞ mark ONLY — this Path-C arm routes to `mark_unbounded_loop`
                // + enabler registration below, which NEVER produces a GameOver; an
                // over-claim is a revocable capability, not a wrongful game-end.
                || crate::analysis::resource::loop_states_cover_modulo_counter_growth(
                    prior, state,
                ))
                && delta.is_net_progress()
                && has_no_loss_axis(&delta)
                && crate::analysis::loop_check::classify_win_kind(controller, &delta)
                    == crate::analysis::loop_check::WinKind::Advantage
            {
                let axes = delta.unbounded_axes_for(controller);
                if axes.is_empty() {
                    continue; // no unbounded axis for the driver ⇒ not this player's loop
                }
                // CR 104.4b: mark the revocable unbounded capability (idempotent set-union).
                state.mark_unbounded_loop(controller, &axes);
                // CR 110.1 + every-enabler: the stable recurring board is the enabler set.
                // battlefield_ids(prior) ∩ battlefield_ids(state) — complete for battlefield-
                // permanent enablers of a constant-depth loop, excludes intra-loop churn.
                let enablers: std::collections::BTreeSet<ObjectId> = prior
                    .battlefield
                    .iter()
                    .copied()
                    .filter(|id| state.battlefield.contains(id))
                    .collect();
                state.register_unbounded_loop_enablers(controller, enablers);
                return;
            }
        }
    }
    // else: staggered-pod loss / non-beneficial optional loop ⇒ no auto-resolve; fall
    // through to the pre-feature behavior (halt / continue).
}

/// PR-7 Phase 3: scan the live loop-detect ring for a determinate lethal single-winner,
/// applying the SAME per-frame window gates the `On` reconcile arm uses
/// ([`crate::analysis::loop_check::winner_life_never_dips`] +
/// [`crate::analysis::loop_check::fallers_lives_pairwise_equal`]). This is a deliberate,
/// isolated copy of the `On` arm's `find_map` scan — the `On` arm stays VERBATIM (byte-
/// identity gate), so it is not refactored to call this. Returns `(winner, per-cycle
/// delta, cycle-start frame)`; the frame feeds `board_delta` for the offer certificate.
fn find_live_loop_winner(
    state: &GameState,
) -> Option<(
    PlayerId,
    crate::analysis::resource::ResourceVector,
    std::sync::Arc<GameState>,
)> {
    let priors: Vec<std::sync::Arc<GameState>> = state.loop_detect_ring.iter().cloned().collect();
    let cur = crate::analysis::resource::ResourceVector::snapshot(state);
    priors.iter().enumerate().find_map(|(k, prior)| {
        let delta = crate::analysis::resource::ResourceVector::delta(
            &crate::analysis::resource::ResourceVector::snapshot(prior),
            &cur,
        );
        let winner = crate::analysis::loop_check::live_mandatory_loop_winner(prior, state, &delta)?;
        let mut frames: Vec<&GameState> = priors[k..].iter().map(|p| p.as_ref()).collect();
        frames.push(state);
        if !crate::analysis::loop_check::winner_life_never_dips(&frames, winner) {
            return None;
        }
        let fallers: Vec<PlayerId> = state
            .players
            .iter()
            .filter(|p| !p.is_eliminated)
            .map(|p| p.id)
            .filter(|p| delta.life.get(p).copied().unwrap_or(0) < 0)
            .collect();
        if fallers.len() >= 2
            && !crate::analysis::loop_check::fallers_lives_pairwise_equal(&frames, &fallers)
        {
            return None;
        }
        Some((winner, delta, prior.clone()))
    })
}

/// PR-7 Phase 3: build the offer certificate for an OPTIONAL winning drain. Fills the
/// residual via the SINGLE `board_delta` population seam (`loop_check.rs` invariant — NOT
/// `BoardDelta::default()`); empty for a constant-depth drain, non-empty for the ω growing
/// cascade where the Phase-4 materialization consumer reads it.
fn build_cert(
    prior: &GameState,
    state: &GameState,
    delta: &crate::analysis::resource::ResourceVector,
    winner: PlayerId,
) -> crate::analysis::loop_check::LoopCertificate {
    crate::analysis::loop_check::LoopCertificate {
        unbounded: delta.unbounded_axes_for(winner),
        win_kind: crate::analysis::loop_check::classify_win_kind(winner, delta),
        // The offer is only reached for an OPTIONAL loop.
        mandatory: false,
        residual_board_delta: crate::analysis::resource::board_delta(prior, state),
    }
}

/// CR 704.5a / CR 704.5c: a determinate lethal drain (0-or-less life / 10-poison) repeats
/// UntilLethal; every other CR 732.1b win seeds a `Fixed(1)` frontend count picker. Extracted
/// as a pure classifier so the exhaustive `WinKind` mapping is unit-testable without a
/// `GameState`.
fn shortcut_iteration_count(
    win_kind: crate::analysis::loop_check::WinKind,
) -> crate::analysis::decision_template::IterationCount {
    use crate::analysis::decision_template::IterationCount;
    use crate::analysis::loop_check::WinKind;
    match win_kind {
        WinKind::LethalDamage | WinKind::PoisonLoss => IterationCount::UntilLethal,
        WinKind::Decking | WinKind::ExtraTurns | WinKind::ImmediateWin | WinKind::Advantage => {
            IterationCount::Fixed(1)
        }
    }
}

/// CR 732.2a: build the READ-side decision schema for a loop-shortcut offer. `pins` is the
/// carried single-authority decision list (`build_recast_template` output for the object-growth
/// path; `&[]` for a non-targeted drain) — never re-derived here. Legal sets come from live
/// engine queries (`is_convoke_eligible`); the frontend computes nothing.
fn build_shortcut_schema(
    pins: &[crate::analysis::decision_template::PinnedDecision],
    win_kind: crate::analysis::loop_check::WinKind,
    state: &GameState,
    controller: PlayerId,
) -> crate::analysis::decision_template::ShortcutDecisionSchema {
    use crate::analysis::decision_template::{
        DecisionPoint, DecisionPointKind, PinnedDecision, ShortcutDecisionSchema,
    };
    let points: Vec<DecisionPoint> = pins
        .iter()
        .filter_map(|pin| match pin {
            // CR 603.3b: trigger ordering is not a loop-declaration choice — no read-side peer.
            PinnedDecision::Order { .. } => None,
            // CR 702.51a: the untapped creatures the controller may tap for convoke. Sorted by
            // the public inner id: `im::HashMap::values()` order is nondeterministic and this Vec
            // serializes to the wire (cf. `resolve_source`'s `min_by_key` for the same reason).
            PinnedDecision::ConvokeTaps { slot } => {
                let mut tappable: Vec<crate::types::identifiers::ObjectId> = state
                    .objects
                    .values()
                    .filter(|o| o.is_convoke_eligible(controller))
                    .map(|o| o.id)
                    .collect();
                tappable.sort_by_key(|id| id.0);
                Some(DecisionPoint {
                    slot: slot.clone(),
                    kind: DecisionPointKind::ConvokeTaps { tappable },
                })
            }
            // No Stage-1 offer path reifies a targeted / modal / may / unless decision (targeted
            // loops reach the offer only after the Stage-2 gate-relax). Fail-loud in dev,
            // fail-safe (drop) in prod — no producer emits one yet.
            PinnedDecision::Targets { .. }
            | PinnedDecision::Mode { .. }
            | PinnedDecision::MayChoice { .. }
            | PinnedDecision::UnlessBreak { .. } => {
                debug_assert!(
                    false,
                    "Stage-1 schema builder: only ConvokeTaps is reified; Targets/Mode/MayChoice/UnlessBreak are Stage-2 producers"
                );
                None
            }
        })
        .collect();
    // CR 702.51a: engine-owned total of untapped convoke-eligible creatures across every
    // ConvokeTaps point — the frontend renders this directly instead of re-deriving it from
    // `points` (display-layer purity). Identical predicate/sum to the deleted React reduce.
    let convoke_tappable_count = points
        .iter()
        .filter_map(|p| match &p.kind {
            DecisionPointKind::ConvokeTaps { tappable } => Some(tappable.len()),
            _ => None,
        })
        .sum();
    ShortcutDecisionSchema {
        iteration_count: shortcut_iteration_count(win_kind),
        points,
        convoke_tappable_count,
    }
}

/// CR 732.4 + CR 104.4b: a net-progress mandatory loop draws ONLY if it drives NO player
/// toward a loss — no life drain, no poison, no decking. Any loss axis means a determinate
/// loser (Path A) or a staggered pod (fall through), never a draw. The live delta comes
/// from two `snapshot`s, so `damage_dealt` is empty (state-fed) and life loss surfaces as
/// a negative `life` delta.
fn has_no_loss_axis(delta: &crate::analysis::resource::ResourceVector) -> bool {
    // CR 704.5c: poison is now per-victim (`delta.poison`); a rising poison on ANY
    // player is a loss axis that vetoes the CR 732.4 draw.
    delta.life.values().all(|&n| n >= 0)
        && delta.library_delta.values().all(|&n| n >= 0)
        && delta.poison.values().all(|&n| n <= 0)
}

/// CR 800.4a: the seat that should receive priority when a loop-shortcut resolution hands
/// priority back. Priority passes to the next player in turn order still in the game — the
/// active player if it is still in the game, otherwise the next living seat in turn order
/// (elimination does not advance `active_player` when a non-acting seat concedes during the
/// APNAP window, so `active_player` may be a departed player).
fn living_priority_seat(state: &GameState) -> PlayerId {
    if crate::game::players::is_alive(state, state.active_player) {
        state.active_player
    } else {
        crate::game::players::next_player_in_turn_order(state, state.active_player)
    }
}

/// CR 732.2c + CR 704.5a: apply a confirmed loop shortcut. Reached ONLY on the Accept path
/// (every living opponent accepted). CR 608.2b re-validation is satisfied BY CONSTRUCTION:
/// the offer confirmed `proposal.predicted_winner` as the determinate winner over public board
/// state, and between the offer and the final Accept the dispatch admits ONLY the protocol
/// actions (`DeclareShortcut`/`RespondToShortcut`), none of which touch the board — so the
/// loop is provably still intact and the predicted winner remains valid. (A live ring re-scan
/// here is unsound: intervening finalize/SBA/layer steps drift the paused state away from the
/// sampled ring frames. The Shorten path — where a real board action CAN break the loop —
/// deliberately hands priority instead of reaching here, and re-detection re-fires the bridge
/// LIVE on a later beat.) `UntilLethal` ⇒ mark the unbounded axes + declare the terminal win;
/// `Fixed(N)` ⇒ Phase-4b finite materialization (`materialize_fixed_shortcut`), which drives
/// N whole cycles atomically, commits + stops early on a cross-lethal `GameOver` mid-drive, and
/// falls back to manual play (priority to `living_priority_seat`) on any abort.
///
/// The consumption-time proposer/winner-liveness guard below catches a `Concede` (CR 104.3a)
/// or a `Debug` that ELIMINATES either authority inside the still-open APNAP window. A `Debug` action
/// that drifts the board WITHOUT killing the proposer (e.g. debug-removing a loop permanent)
/// is deliberately out of scope: `debug_mode` is sandbox god-mode that can already produce
/// arbitrarily inconsistent states, so loop-shortcut soundness under arbitrary debug mutation
/// is not a competitive-correctness obligation.
fn apply_confirmed_shortcut(
    state: &mut GameState,
    result: &mut ActionResult,
    proposal: &crate::analysis::loop_check::ShortcutProposal,
) {
    // CR 104.3a / CR 104.2a / CR 800.4a: re-validate the proposer and any latched winner at
    // consumption. `GameAction::Concede` (and a board-mutating `Debug`) bypass the WaitingFor
    // dispatch, so either authority can leave during the APNAP window. A departed proposer
    // invalidates the sequence they suggested; a departed predicted winner cannot be crowned.
    if !crate::game::players::is_alive(state, proposal.proposer)
        || proposal
            .predicted_winner
            .is_some_and(|winner| !crate::game::players::is_alive(state, winner))
    {
        priority::reset_priority(state);
        // CR 800.4a: priority passes to the next player in turn order still in the game.
        // The departed proposer may have been the active player (elimination does not advance
        // `active_player` when a non-acting seat concedes during the APNAP window), so route to
        // a LIVING holder rather than a possibly-departed `active_player`.
        let holder = living_priority_seat(state);
        state.waiting_for = WaitingFor::Priority { player: holder };
        result.waiting_for = state.waiting_for.clone();
        return;
    }
    match proposal.count {
        crate::analysis::decision_template::IterationCount::UntilLethal => {
            apply_until_lethal_shortcut(state, result, proposal)
        }
        crate::analysis::decision_template::IterationCount::Fixed(n) => {
            materialize_fixed_shortcut(state, result, proposal, n)
        }
    }
}

/// PR-7 Combo-UI Stage 2 (SOUNDNESS #2 — the E1 crown): CR 732.2a / CR 704.5a / CR 104.2a
/// win-derivation for a confirmed `UntilLethal` loop shortcut. NEVER an unconditional crown.
/// DRIVES one pin-faithful cycle of the confirmed loop, MEASURES the per-cycle
/// `ResourceVector::delta`, and re-runs the SAME offer-time authority
/// (`live_mandatory_loop_winner`) on the driven states. Crowns ONLY when that authority
/// names the proposer as the sole determinate winner; every other outcome (a subset-lethal
/// loop with >1 non-faller, an Advantage token-growth loop with no faller, an aborted drive)
/// falls back to manual play (CR 800.4a) — no wrong crown.
///
/// F2 hardening (crown SELF-soundness — a GameOver path must not depend on a future
/// hard-gated PR): for the ≥2-faller case, RE-VERIFY the offer's own
/// `fallers_lives_pairwise_equal` (CR 704.3 simultaneity) on the boundary/pre-drive faller
/// lives — a staggered-death unequal-absolute drain does NOT crown even though
/// `live_mandatory_loop_winner`'s ≥2-faller floor checks only per-cycle DELTAS.
///
/// SOUNDNESS FLAG (#20, belt+suspenders): when the PR-8 targeted-offer trigger reifies >2p
/// targeted loops, it should ALSO carry `fallers_lives_pairwise_equal` at OFFER time.
fn apply_until_lethal_shortcut(
    state: &mut GameState,
    result: &mut ActionResult,
    proposal: &crate::analysis::loop_check::ShortcutProposal,
) {
    // The board is unchanged since the offer (apply_confirmed_shortcut doc): `committed` is
    // the fully-committed pre-drive state to roll back to on any non-crown.
    let committed = state.clone();
    // The recurrence boundary: the loop's canonical per-cycle SETTLE beat
    // (`Priority{active_player}`), normalized — the same construction `materialize_fixed_shortcut`
    // captures (the cover/equal-modulo checks normalize internally, so this is a
    // self-contained canonical frame). `snapshot`'s life/poison/library axes are unaffected by
    // `normalize_for_loop`, so `before` is the pre-drive resource baseline.
    let boundary = {
        let mut seed = committed.clone();
        priority::reset_priority(&mut seed);
        seed.waiting_for = WaitingFor::Priority {
            player: seed.active_player,
        };
        seed.normalize_for_loop()
    };
    let before = crate::analysis::resource::ResourceVector::snapshot(&boundary);
    let period = shortcut_drive_period(proposal.template.as_ref());

    // DRIVE one representative cycle to produce the measured post-drive `work` state.
    let work: GameState = if let Some(ctx) = committed.last_recast_context.clone() {
        // Object-growth recast loop (buyback + convoke + affinity) declared `UntilLethal`
        // by the AI (which hardcodes it for every optional offer). Drive one real recast on a
        // clone under the re-entrancy guard; an inert Advantage token loop has NO life/poison
        // faller ⇒ `live_mandatory_loop_winner` returns None below ⇒ manual fallback (this is
        // the latent AI-mis-crown fix, first-class).
        let template = build_recast_template(&ctx);
        let _probe = SimulationProbeGuard::enter();
        let mut w = committed.clone();
        priority::reset_priority(&mut w);
        w.waiting_for = WaitingFor::Priority {
            player: ctx.controller,
        };
        match drive_recast_iteration(&mut w, &template, &ctx, 0) {
            Ok(()) => w,
            Err(RecastAbort) => {
                return until_lethal_fallback(state, result, committed);
            }
        }
    } else {
        // Drain loop (targeted Vito class, non-targeted Cleric class, ω-covering cascade).
        // Drive `period` whole cycles, injecting the pinned answers (CR 603.3b ordering / CR
        // 608.2b targets) at each mid-cycle prompt. A cross-lethal mid-drive already applies
        // the win to `work` (CR 704.5a SBA).
        let cap = auto_pass_loop_max_iterations(&committed);
        let mut running = committed.clone();
        for i in 0..period {
            match drive_one_shortcut_cycle(&running, &boundary, proposal.template.as_ref(), i, cap)
            {
                CycleOutcome::Recurred { state: s, .. } => running = *s,
                CycleOutcome::CrossLethal {
                    state: s,
                    winner,
                    mut events,
                } => {
                    // Commit + stop ONLY when the mid-drive lethal matches the winner measured
                    // at offer time; any other winner (or a draw) rolls back to manual play. `UntilLethal`
                    // IS unbounded ⇒ mark the axes on the committed state (contrast the
                    // finite `Fixed(N)` cross-lethal, which does not).
                    if let Some(winner) =
                        winner.filter(|winner| Some(*winner) == proposal.predicted_winner)
                    {
                        let mut w = *s;
                        w.mark_unbounded_loop(winner, &proposal.unbounded);
                        *state = w;
                        result.events.append(&mut events);
                        state.waiting_for = WaitingFor::GameOver {
                            winner: Some(winner),
                        };
                        result.waiting_for = state.waiting_for.clone();
                    } else {
                        until_lethal_fallback(state, result, committed);
                    }
                    return;
                }
                CycleOutcome::Abort => {
                    return until_lethal_fallback(state, result, committed);
                }
            }
        }
        running
    };

    // MEASURE + derive the winner via the offer-time authority, VERBATIM.
    let delta = crate::analysis::resource::ResourceVector::delta(
        &before,
        &crate::analysis::resource::ResourceVector::snapshot(&work),
    );
    match crate::analysis::loop_check::live_mandatory_loop_winner(&boundary, &work, &delta) {
        Some(winner) if Some(winner) == proposal.predicted_winner => {
            // F2 (CR 704.3 simultaneity): for ≥2 fallers, re-verify the offer's own pairwise
            // life-equality on the pre-drive faller lives. `live_mandatory_loop_winner`'s
            // ≥2-faller floor checks only per-cycle DELTAS, so a staggered-death unequal
            // ABSOLUTE-life drain would pass it — the offer's `fallers_lives_pairwise_equal`
            // is the missing absolute-life gate. Single-faller (2p) skips it (no simultaneity
            // to enforce); a non-targeted symmetric drain was certified pairwise-equal on the
            // frozen board, so it still passes.
            let fallers = fallers_of(&work, &delta);
            if fallers.len() >= 2
                && !crate::analysis::loop_check::fallers_lives_pairwise_equal(
                    &[&boundary],
                    &fallers,
                )
            {
                until_lethal_fallback(state, result, committed);
            } else {
                crown_until_lethal(state, result, proposal, winner);
            }
        }
        _ => until_lethal_fallback(state, result, committed),
    }
}

/// The faller partition of a measured per-cycle `delta`, over the living players of
/// `cycle_end` — EXACTLY the partition `live_mandatory_loop_winner` computes internally
/// (`delta.life<0 || delta.poison>0`). Exposed for the F2 ≥2-faller re-verification; NOT a
/// re-architecting of the win authority.
fn fallers_of(
    cycle_end: &GameState,
    delta: &crate::analysis::resource::ResourceVector,
) -> Vec<PlayerId> {
    cycle_end
        .players
        .iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| p.id)
        .filter(|p| {
            delta.life.get(p).copied().unwrap_or(0) < 0
                || delta.poison.get(p).copied().unwrap_or(0) > 0
        })
        .collect()
}

/// CR 732.2a + CR 704.5a: crown the measured winner of the confirmed
/// unbounded drain (the former UntilLethal-arm body). Persists the unbounded axes (the ∞ HUD
/// producer) and declares the CR 704.5a win.
fn crown_until_lethal(
    state: &mut GameState,
    result: &mut ActionResult,
    proposal: &crate::analysis::loop_check::ShortcutProposal,
    winner: PlayerId,
) {
    state.mark_unbounded_loop(winner, &proposal.unbounded);
    result.events.push(GameEvent::GameOver {
        winner: Some(winner),
    });
    state.waiting_for = WaitingFor::GameOver {
        winner: Some(winner),
    };
    result.waiting_for = state.waiting_for.clone();
    match_flow::handle_game_over_transition(state);
}

/// CR 800.4a: the E1 crown refused (no determinate winner / aborted drive) ⇒ roll back to the
/// pre-drive committed board and hand priority to the living seat for manual play. Clears the
/// loop-detect ring so this same `apply()` does not instantly re-offer the (now-declined)
/// loop; a later beat re-detects genuinely. Mirrors the `materialize_fixed_shortcut` abort
/// tail.
fn until_lethal_fallback(state: &mut GameState, result: &mut ActionResult, committed: GameState) {
    *state = committed;
    // CR 732.2c: a declined shortcut must not instantly re-offer the SAME loop in this same
    // `apply()`. Clear both re-offer signals: the drain offer's `loop_detect_ring` AND the
    // object-growth offer's `last_recast_context` routing signal (a non-drain object-growth
    // loop, e.g. an AI-declared UntilLethal on an inert Advantage recast, would otherwise
    // re-fire `try_offer_object_growth_shortcut` on the next reconcile and livelock). A later
    // real re-cast re-captures the context and re-detects genuinely.
    state.loop_detect_ring.clear();
    state.last_recast_context = None;
    priority::reset_priority(state);
    state.waiting_for = WaitingFor::Priority {
        player: living_priority_seat(state),
    };
    result.waiting_for = state.waiting_for.clone();
}

/// CR 732.2a: how many whole cycles one shortcut drive must aggregate before the measured
/// delta is complete. A `RoundRobin`/`Piecewise` target schedule rotates its OBJECT sources
/// over its length, so a full period is that length; every other pin (a `Constant` target, a
/// `Player` pin, a non-target pin, or no template at all) settles in ONE cycle. Returns the
/// max schedule length over the template's `Targets` pins, defaulting to 1.
///
/// DORMANT for every Stage-2 crownable loop (Ruling B): `TargetSchedule` rotates DecisionSource
/// objects, not players, and `live_mandatory_loop_winner` crowns on PLAYER fallers — an
/// object-rotating loop produces no player faller, so it never crowns; the only crownable >2p
/// player drain pins ALL opponents every cycle (`TargetPin::Player` is constant, period 1). The
/// seam is built for generality; a multi-cycle aggregation is fail-safe (an object loop reaching
/// the arm measures 1 cycle, finds no faller, does not crown).
///
/// CR 732.2a SAFETY LIMIT: the returned period is clamped to `MAX_SHORTCUT_CYCLES`. Both
/// consumers derive their `0..period` range from this one helper (`validate_pins` and
/// `apply_until_lethal_shortcut`), so the clamp bounds validate + drive coherently;
/// crown-soundness holds — every crownable loop has period 1, so the clamp only truncates a
/// hostile over-cap schedule into the conservative manual-fallback arm, never a mis-crown.
fn shortcut_drive_period(
    template: Option<&crate::analysis::decision_template::DecisionTemplate>,
) -> crate::analysis::decision_template::IterationIndex {
    use crate::analysis::decision_template::{PinnedDecision, TargetPin, TargetSchedule};
    let Some(t) = template else { return 1 };
    t.decisions
        .iter()
        .filter_map(|pin| match pin {
            PinnedDecision::Targets { targets, .. } => targets
                .iter()
                .map(|tp| match tp {
                    TargetPin::Scheduled(TargetSchedule::RoundRobin(v)) => v.len() as u32,
                    TargetPin::Scheduled(TargetSchedule::Piecewise(v)) => v.len() as u32,
                    TargetPin::Scheduled(TargetSchedule::Constant(_))
                    | TargetPin::ByIdentity(_)
                    | TargetPin::Player(_) => 1,
                })
                .max(),
            _ => None,
        })
        .max()
        .unwrap_or(1)
        // CR 732.2a SAFETY LIMIT: the drive period is STRUCTURALLY unbounded in the engine —
        // its length is the client template schedule's own length. On the WS transport the
        // 8 KB inbound-frame cap (phase-server/src/main.rs:409/1420) already bounds a hostile
        // schedule to a few hundred entries (~1-2 s stall, not a million-cycle remote DoS),
        // but in-process callers (WASM/Tauri/local) bypass that cap, so clamp here AT THE
        // SOURCE for every caller. Real schedules rotate over a handful of object sources
        // (period ≪ cap), so this is invisible to every legitimate loop; a clamped-shorter
        // drive measures a smaller (more conservative) delta ⇒ FEWER crowns / more manual
        // fallbacks, never a wrong crown.
        .clamp(1, MAX_SHORTCUT_CYCLES)
}

/// PR-7 Combo-UI Stage 2: the typed result of driving ONE whole loop-shortcut cycle on a
/// clone. Exhaustive at both call sites (`materialize_fixed_shortcut`, `apply_until_lethal_
/// shortcut`) — no silent `_` that could crown or roll back on an unhandled outcome.
enum CycleOutcome {
    /// The cycle recurred (constant-depth equal-modulo-resources or ω-covering) ⇒ `state` is
    /// the committed post-cycle board; `events` are its accumulated events.
    Recurred {
        state: Box<GameState>,
        events: Vec<GameEvent>,
    },
    /// CR 704.5a: the cycle crossed lethal mid-drive ⇒ the win is already applied to `state`
    /// (`waiting_for = GameOver{winner}`); `events` include the terminal `GameOver`.
    CrossLethal {
        state: Box<GameState>,
        winner: Option<PlayerId>,
        events: Vec<GameEvent>,
    },
    /// Runaway beat cap, an unpinned prompt, or an engine error ⇒ abort to manual play.
    Abort,
}

/// PR-7 Combo-UI Stage 2: drive ONE whole cycle of a confirmed loop shortcut on a fresh clone
/// of `committed`, seeded to the canonical settle beat (`Priority{active_player}`, the same
/// beat the detector ring samples). Recurrence is detected against `boundary` (normalized).
/// Behavior-identical to the former inline `materialize_fixed_shortcut` beat loop EXCEPT the
/// old `Ok(_) => break 'cycles` abort on a mid-cycle prompt now delegates to
/// [`inject_pinned_answer`] (CR 603.3b ordering / CR 608.2b pinned targets) and continues.
/// Uses the INTERNAL `apply_action` path throughout (via `pass_priority_once_with_pipeline`
/// and the injector), never the top-level reconcile boundary, so the detection hook cannot
/// recurse mid-drive.
fn drive_one_shortcut_cycle(
    committed: &GameState,
    boundary: &GameState,
    template: Option<&crate::analysis::decision_template::DecisionTemplate>,
    iteration: crate::analysis::decision_template::IterationIndex,
    cycle_beat_cap: usize,
) -> CycleOutcome {
    let mut work = committed.clone();
    priority::reset_priority(&mut work);
    work.waiting_for = WaitingFor::Priority {
        player: work.active_player,
    };
    let mut ev: Vec<GameEvent> = Vec::new();
    let mut beat = 0usize;

    loop {
        beat += 1;
        if beat > cycle_beat_cap {
            return CycleOutcome::Abort; // runaway backstop
        }
        // A FRESH per-beat buffer (see the former inline note): reusing one growing buffer
        // would make `run_post_action_pipeline` re-scan prior beats' events and re-fire
        // already-consumed triggers.
        let mut beat_events: Vec<GameEvent> = Vec::new();
        match pass_priority_once_with_pipeline(&mut work, &mut beat_events, None) {
            // Cross-lethal: COMMIT + STOP. The GameOver event + transition are already in
            // `work`/`beat_events`.
            Ok(WaitingFor::GameOver { winner }) => {
                ev.append(&mut beat_events);
                return CycleOutcome::CrossLethal {
                    state: Box::new(work),
                    winner,
                    events: ev,
                };
            }
            // Active-player settle beat: cycle complete iff the board recurred (constant-depth
            // equal-modulo-resources OR ω-covering growth).
            Ok(WaitingFor::Priority { player }) if player == work.active_player => {
                ev.append(&mut beat_events);
                let norm = work.normalize_for_loop();
                if crate::analysis::resource::loop_states_equal_modulo_resources(boundary, &norm)
                    || crate::analysis::resource::loop_states_cover_modulo_growth(boundary, &norm)
                {
                    return CycleOutcome::Recurred {
                        state: Box::new(work),
                        events: ev,
                    };
                }
                continue; // active beat, not yet recurred ⇒ keep driving within the cap
            }
            // Opponent's mid-cycle priority window ⇒ keep driving.
            Ok(WaitingFor::Priority { .. }) => {
                ev.append(&mut beat_events);
                continue;
            }
            // Any OTHER prompt (OrderTriggers / TriggerTargetSelection / …): answer it from the
            // pins and continue. An unpinned prompt fails closed ⇒ abort to manual.
            Ok(other) => {
                ev.append(&mut beat_events);
                match inject_pinned_answer(&mut work, template, iteration, &other) {
                    Ok(()) => continue,
                    Err(RecastAbort) => return CycleOutcome::Abort,
                }
            }
            Err(_) => return CycleOutcome::Abort, // engine error ⇒ abort to manual
        }
    }
}

/// PR-7 Combo-UI Stage 2: answer ONE mid-drive prompt during a loop-shortcut cycle, using the
/// INTERNAL reconcile-free `apply_action` path (mirrors `drive_recast_iteration`, so the
/// detection hook cannot recurse mid-drive). Fail-closed: any prompt kind with no Stage-2
/// producer ⇒ `Err(RecastAbort)`.
///
/// There is deliberately NO top-level `template.ok_or(...)` guard: the `OrderTriggers` arm is
/// TEMPLATE-INDEPENDENT (the real 2p Vito drive raises OrderTriggers with a `template = None`
/// declaration, and the forced-unique target auto-selects at dispatch), so a top guard would
/// wrongly abort it. The template guard lives INSIDE the `TriggerTargetSelection` arm, the only
/// arm that consumes pins.
fn inject_pinned_answer(
    work: &mut GameState,
    template: Option<&crate::analysis::decision_template::DecisionTemplate>,
    iteration: crate::analysis::decision_template::IterationIndex,
    prompt: &WaitingFor,
) -> Result<(), RecastAbort> {
    use crate::analysis::decision_template::{ConcreteDecision, ConcreteTarget};
    match prompt {
        // CR 603.3b / CR 732.2a: auto-order the confirmed shortcut's simultaneous
        // same-controller triggers by identity order (0..len). Template-INDEPENDENT and
        // delta-safe: the per-cycle net drain is order-invariant (both opponents drain
        // regardless of order; pins fix WHICH opponent, not the ordering). Answered via the
        // INTERNAL `handle_order_triggers` (`apply_action`), NOT `drain_order_triggers_with_
        // identity` — the latter routes through `reconcile_terminal_result`, which would
        // re-enter the loop-detection/offer hook mid-drive and could crown via a different
        // authority, bypassing E1's own measure.
        WaitingFor::OrderTriggers { player, triggers } => {
            let order: Vec<usize> = (0..triggers.len()).collect();
            apply_action(work, *player, GameAction::OrderTriggers { order }, None)
                .map_err(|_| RecastAbort)?;
            Ok(())
        }
        // CR 608.2b: choose this trigger's targets from the pin whose source matches the
        // prompt's `source_id` (per-source, so two distinct drainers pinned to distinct
        // opponents each receive the correct target). The template guard lives HERE.
        WaitingFor::TriggerTargetSelection {
            player, source_id, ..
        } => {
            let template = template.ok_or(RecastAbort)?;
            let source_id = source_id.ok_or(RecastAbort)?;
            let decisions = crate::analysis::decision_template::resolve(template, iteration, work)
                .map_err(|_| RecastAbort)?;
            let targets = decisions
                .into_iter()
                .find_map(|d| match d {
                    ConcreteDecision::Targets { slot, targets }
                        if crate::analysis::decision_template::resolve_source(
                            &slot.source,
                            work,
                        ) == Some(source_id) =>
                    {
                        Some(targets)
                    }
                    _ => None,
                })
                .ok_or(RecastAbort)?;
            let refs: Vec<TargetRef> = targets
                .into_iter()
                .map(|t| match t {
                    ConcreteTarget::Object(id) => TargetRef::Object(id),
                    ConcreteTarget::Player(p) => TargetRef::Player(p),
                })
                .collect();
            apply_action(
                work,
                *player,
                GameAction::SelectTargets { targets: refs },
                None,
            )
            .map_err(|_| RecastAbort)?;
            Ok(())
        }
        // CR 732.2a "no conditional actions": any other prompt (mode / may / unless / X) has
        // no Stage-2 pin producer ⇒ fail-closed.
        _ => Err(RecastAbort),
    }
}

/// PR-7 Phase 4b: CR 732.2a finite materialization of a confirmed `Fixed(N)` loop
/// shortcut. Drives `n` whole cycles of the constant-depth (or ω-covering) loop,
/// committing atomically per cycle. If a cycle crosses lethal, the win arrives
/// mid-drive already applied to `work` (CR 704.5a via `run_post_action_pipeline`'s
/// SBA pass) ⇒ COMMIT + STOP, un-clamped — `n` may be ≥ the true cycles-to-lethal
/// (CR 732.2a "a specified number of times" places no upper bound relative to the
/// board). Any unexpected prompt / stale-incarnation replay failure (CR 400.7) /
/// runaway beat count ⇒ abort to manual play: roll back to the last fully-committed
/// cycle and hand priority to the living seat (CR 800.4a) — exactly the pre-4b
/// decline-stub behavior, never a wrong crown.
fn materialize_fixed_shortcut(
    state: &mut GameState,
    result: &mut ActionResult,
    proposal: &crate::analysis::loop_check::ShortcutProposal,
    n: u32,
) {
    // PR-7 Phase 4d-ii (CR 732.2a): an object-growth recast loop (buyback + convoke +
    // affinity) settles with an EMPTY stack and grows the board, so the per-beat
    // auto-pass drive below never recognizes its recurrence. Route it to the recast
    // INJECTOR instead, which drives one real recast per cycle on a clone. The presence
    // of `last_recast_context` (set only on a buyback-paid, token-creating cast) is the
    // routing signal; the `ctx` rides `state.last_recast_context` (carried on the clone
    // since the offer). The drain path below is left byte-identical for every other loop.
    if let Some(ctx) = state.last_recast_context.clone() {
        materialize_object_growth_shortcut(state, result, &ctx, n);
        return;
    }

    let template = proposal.template.clone();

    // Last fully-completed cycle (clean owned O(1) rollback); starts at the offer state —
    // `apply_confirmed_shortcut`'s doc comment establishes the board is unchanged since the
    // offer (Declare/Accept touch only the protocol, never the board).
    let mut committed = state.clone();

    // The recurrence boundary is the loop's canonical per-cycle SETTLE beat —
    // `Priority{active_player}` — the same beat-kind the detector ring samples
    // (`resolved_this_beat` gate above). `committed.waiting_for` is still
    // `RespondToShortcut`/`LoopShortcut` at entry (never `Priority`), so seed a settled
    // priority beat before capturing the boundary. `reset_priority` zeroes
    // `priority_pass_count` and sets `priority_player`; `waiting_for` is set explicitly
    // here (reset_priority does not touch it). `loop_states_equal_modulo_resources` /
    // `loop_states_cover_modulo_growth` both normalize internally (`project_out_resources`
    // → `normalize_for_loop`), so the extra `.normalize_for_loop()` here is redundant with
    // that internal call but harmless (idempotent) — kept for a self-contained boundary
    // value whose `waiting_for`/ring fields are already canonical at the call sites below.
    let boundary = {
        let mut seed = committed.clone();
        priority::reset_priority(&mut seed);
        seed.waiting_for = WaitingFor::Priority {
            player: seed.active_player,
        };
        seed.normalize_for_loop()
    };

    let cycle_beat_cap = auto_pass_loop_max_iterations(&committed);

    'cycles: for i in 0..n {
        // CR 732.2a predictability firewall: `predictability_gate(t, &[])` is a WIRED
        // FORMAL no-op this phase — empty `required_slots` ⇒ always `Ok`
        // (decision_template.rs). The loop-body slot enumerator that would populate
        // `required_slots` ships with the deferred mid-drive injector; a choice-free
        // drain has no slots to pin. The REAL load-bearing firewall is `resolve` below.
        if let Some(t) = &template {
            if crate::analysis::decision_template::predictability_gate(t, &[]).is_err() {
                break 'cycles; // unreachable with &[]; wired for the injector phase
            }
            // CR 608.2b (target-legality re-check) + CR 400.7 (object incarnation
            // re-bind): re-resolve every pinned decision against the last COMMITTED
            // board before driving this cycle. Stale/absent source ⇒ IllegalTarget /
            // MissingSource ⇒ abort to manual play.
            if crate::analysis::decision_template::resolve(t, i, &committed).is_err() {
                break 'cycles;
            }
        }

        // Drive one whole cycle via the shared driver. Behavior-identical to the former
        // inline beat loop for a non-targeted `Fixed(N)` drain (which raises no mid-cycle
        // prompt, so the injector is inert); a targeted drive additionally answers each
        // OrderTriggers / target prompt from the pins.
        match drive_one_shortcut_cycle(&committed, &boundary, template.as_ref(), i, cycle_beat_cap)
        {
            CycleOutcome::Recurred {
                state: s,
                mut events,
            } => {
                committed = *s; // ATOMIC: commit state ...
                result.events.append(&mut events); // ... with its events together
                continue 'cycles;
            }
            // Cross-lethal: COMMIT + STOP. CR 704.5a: the win is already applied to `work`
            // (SBA → GameOver in `events`, `waiting_for = GameOver`). Do NOT roll back, NOT
            // `mark_unbounded_loop` (finite ≠ unbounded — contrast the UntilLethal arm).
            CycleOutcome::CrossLethal {
                state: s,
                winner,
                mut events,
            } => {
                *state = *s;
                result.events.append(&mut events);
                result.waiting_for = WaitingFor::GameOver { winner };
                return;
            }
            // Runaway cap / unpinned prompt / engine error ⇒ abort to manual. The aborting
            // cycle's events were already dropped (no partial-cycle event leak).
            CycleOutcome::Abort => break 'cycles,
        }
    }

    // Reached by: n cycles done with no cross-lethal, OR any abort (`break 'cycles`).
    // Commit the last WHOLE cycle; the aborting iteration's `ev` was already dropped (no
    // partial-cycle event leak). Ring-clear BEFORE handback so this same `apply()` does
    // not instantly re-emit a fresh offer for the same (now-interrupted) loop; a later
    // beat re-detects genuinely.
    *state = committed;
    state.loop_detect_ring.clear();
    priority::reset_priority(state);
    state.waiting_for = WaitingFor::Priority {
        player: living_priority_seat(state),
    };
    result.waiting_for = state.waiting_for.clone();
}

/// PR-7 Phase 4d-ii: the injector aborted a driven recast cycle ⇒ fall closed to manual
/// play. No payload — a marker so the drive loop is exhaustive over `WaitingFor` with an
/// explicit `Err` on any unpinned prompt (S1, CR 732.2a "no conditional actions").
#[derive(Debug)]
struct RecastAbort;

/// CR 601.2b + CR 608.2b + CR 400.7: drive ONE full recast iteration on the clone by
/// answering each mid-cast prompt from `template` (the ConvokeTaps pin) + `ctx` (the
/// buyback decision). Reuses the ENTIRE cast state machine via the INTERNAL `apply_action`
/// path (never the top-level `apply`/reconcile boundary, so the detection hook cannot
/// recurse), adding ZERO casting rules. EXHAUSTIVE over `WaitingFor`: any unpinned prompt
/// ⇒ `Err(RecastAbort)` ⇒ fail-closed to manual (no silent `_` that would fabricate a
/// bogus offer). `clone` MUST be at `Priority{ctx.controller}` with an empty stack.
fn drive_recast_iteration(
    clone: &mut GameState,
    template: &crate::analysis::decision_template::DecisionTemplate,
    ctx: &crate::types::game_state::RecastContext,
    iteration: crate::analysis::decision_template::IterationIndex,
) -> Result<(), RecastAbort> {
    // CR 400.7: re-find the recast card LIVE in its castable zone (a fresh incarnation on
    // each hand-return). Absent ⇒ abort (B3: a no-buyback recast went to the graveyard, so
    // the card is not here). Lowest ObjectId ⇒ deterministic.
    let recast_id = clone
        .objects
        .values()
        .filter(|o| {
            o.card_id == ctx.card_id && o.zone == ctx.from_zone && o.controller == ctx.controller
        })
        .map(|o| o.id)
        .min_by_key(|id| id.0)
        .ok_or(RecastAbort)?;
    apply_action(
        clone,
        ctx.controller,
        GameAction::CastSpell {
            object_id: recast_id,
            card_id: ctx.card_id,
            targets: vec![],
            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
        None,
    )
    .map_err(|_| RecastAbort)?;

    let beat_cap = auto_pass_loop_max_iterations(clone);
    for _ in 0..beat_cap {
        let actor = crate::game::turn_control::authorized_submitter(clone).ok_or(RecastAbort)?;
        match clone.waiting_for.clone() {
            // CR 601.2f/702.27a: re-pay (or decline) the buyback additional cost.
            WaitingFor::OptionalCostChoice { .. } => {
                apply_action(
                    clone,
                    actor,
                    GameAction::DecideOptionalCost {
                        pay: ctx.uses_buyback.pays(),
                    },
                    None,
                )
                .map_err(|_| RecastAbort)?;
            }
            // CR 601.2h + CR 702.51a/b: resolve the ConvokeTaps pin LIVE, tap each chosen
            // creature, then finalize the (now convoke-paid) cost. Affinity auto-reduces
            // the generic against the grown board with NO pin (CR 702.41a).
            WaitingFor::ManaPayment { .. } => {
                let decisions =
                    crate::analysis::decision_template::resolve(template, iteration, clone)
                        .map_err(|_| RecastAbort)?;
                use crate::analysis::decision_template::ConcreteDecision;
                for d in decisions {
                    // EXHAUSTIVE (mirrors the same-diff triggers.rs precedent): a recast
                    // template emits ONLY ConvokeTaps pins, so every other decision kind is
                    // unpinned for this class ⇒ fail-CLOSED abort. Listing the variants (no
                    // `_`) makes a future ConcreteDecision variant BUILD-BREAK here rather than
                    // be silently dropped.
                    match d {
                        ConcreteDecision::ConvokeTaps { creatures, .. } => {
                            for (object_id, mana_type) in creatures {
                                apply_action(
                                    clone,
                                    actor,
                                    GameAction::TapForConvoke {
                                        object_id,
                                        mana_type,
                                    },
                                    None,
                                )
                                .map_err(|_| RecastAbort)?;
                            }
                        }
                        ConcreteDecision::Order { .. }
                        | ConcreteDecision::Targets { .. }
                        | ConcreteDecision::Mode { .. }
                        | ConcreteDecision::MayChoice { .. }
                        | ConcreteDecision::UnlessBreak { .. } => return Err(RecastAbort),
                    }
                }
                apply_action(clone, actor, GameAction::PassPriority, None)
                    .map_err(|_| RecastAbort)?;
            }
            // CR 601.2i: the spell is on the stack ⇒ pass to let it resolve; an empty stack
            // at a priority beat is the per-cycle SETTLE boundary — iteration complete.
            WaitingFor::Priority { .. } => {
                if clone.stack.is_empty() {
                    return Ok(());
                }
                apply_action(clone, actor, GameAction::PassPriority, None)
                    .map_err(|_| RecastAbort)?;
            }
            // CR 732.2a "no conditional actions": any other prompt (target / mode / X /
            // may) is unpinned for this recast class ⇒ fail-closed abort.
            _ => return Err(RecastAbort),
        }
    }
    Err(RecastAbort)
}

/// CR 601.2h + CR 702.51a: the CR 732.2a decision template for a buyback+convoke recast
/// loop. Carries a single `ConvokeTaps` pin (when the recast pays convoke) whose slot is
/// the CARD-identity source (`AllCopies` — survives the per-iteration incarnation churn,
/// CR 400.7). The presence of the pin is the object-growth routing signal.
fn build_recast_template(
    ctx: &crate::types::game_state::RecastContext,
) -> crate::analysis::decision_template::DecisionTemplate {
    use crate::analysis::decision_template::{
        DecisionGroupKey, DecisionKind, DecisionSlot, IterationCount, PinnedDecision, ReplayMode,
    };
    let source = crate::types::game_state::YieldTarget::AllCopies {
        card_id: ctx.card_id,
        trigger_description: None,
    };
    let decisions = if ctx.convoke.is_some() {
        vec![PinnedDecision::ConvokeTaps {
            slot: DecisionSlot {
                source: source.clone(),
                index: 0,
            },
        }]
    } else {
        vec![]
    };
    crate::analysis::decision_template::DecisionTemplate {
        owner: ctx.controller,
        decisions,
        // The count is a placeholder — the real `Fixed(N)` comes from the proposer's
        // `DeclareShortcut`; nothing reads `template.replay.count`.
        replay: ReplayMode::Scheduled {
            count: IterationCount::Fixed(0),
        },
        key: DecisionGroupKey::from_sources(&[source], DecisionKind::LoopChoice),
    }
}

/// CR 400.7: normalize a settle frame for the object-growth board cover — strip the
/// self-returning recast card and clear the per-cycle token-id bookkeeping. Both churn a
/// FRESH ObjectId every cycle (the card via its hand→stack→hand round-trip; the
/// `last_created_token_ids` anaphora slot via each new token), which the id-keyed
/// stable-engine compare would read as a false board drift. The recast card's presence in
/// `ctx.from_zone` is a verified loop invariant (the hook precondition + the injector's
/// per-cycle re-find), and `last_created_token_ids` is pure ephemeral anaphora bookkeeping
/// (no observer reads it at the empty-stack settle beat), so clearing them identically from
/// every frame is fail-safe — any OTHER stable object still compares by id.
fn normalize_recast_frame(
    state: &GameState,
    ctx: &crate::types::game_state::RecastContext,
) -> GameState {
    let mut s = state.clone();
    let ids: Vec<ObjectId> = s
        .objects
        .values()
        .filter(|o| {
            o.card_id == ctx.card_id && o.zone == ctx.from_zone && o.controller == ctx.controller
        })
        .map(|o| o.id)
        .collect();
    for id in &ids {
        s.objects.remove(id);
    }
    if let Some(p) = s.players.iter_mut().find(|p| p.id == ctx.controller) {
        p.hand.retain(|id| !ids.contains(id)); // allow-raw-zone: prunes a discarded recast comparison-frame CLONE (fn takes &GameState, returns a normalized clone) - not a gameplay zone event
        p.graveyard.retain(|id| !ids.contains(id)); // allow-raw-zone: prunes a discarded recast comparison-frame CLONE (fn takes &GameState, returns a normalized clone) - not a gameplay zone event
        p.library.retain(|id| !ids.contains(id)); // allow-raw-zone: prunes a discarded recast comparison-frame CLONE (fn takes &GameState, returns a normalized clone) - not a gameplay zone event
    }
    // CR 608.2 anaphora / display bookkeeping: the "last created token / revealed /
    // zone-changed" id slots churn a fresh id each cycle. No observer reads them at the
    // empty-stack settle beat, so clearing them is fail-safe for the board cover.
    s.last_created_token_ids.clear();
    s.last_revealed_ids.clear();
    s.last_zone_changed_ids.clear();
    s
}

/// CR 111.10: the content class of the reproduced token — the single battlefield object
/// present in `after` but absent from `before` (the one predefined token the recast
/// creates). `None` unless EXACTLY one new battlefield object appeared (the target class
/// creates one Saproling; zero or several ⇒ not this shape ⇒ fail-closed).
fn derived_fodder_class(
    before: &GameState,
    after: &GameState,
) -> Option<crate::game::game_object::GameObject> {
    let mut new_ids = after
        .battlefield
        .iter()
        .copied()
        .filter(|id| !before.battlefield.contains(id));
    let id = new_ids.next()?;
    if new_ids.next().is_some() {
        return None;
    }
    after.objects.get(&id).cloned()
}

/// CR 732.2a: detect an object-growth recast loop by driving TWO iterations on a clone;
/// on success returns the offer certificate for the CALLER to install. Takes a SHARED
/// `&GameState` ⇒ a live write is TYPE-IMPOSSIBLE (INV-1); the sole live write
/// (`waiting_for = LoopShortcut`) is done by the mutable-borrow caller (INV-2: OFFER,
/// never auto-resolve, CR 732.2a). Both driven iterations run inside ONE
/// `SimulationProbeGuard` so the injector's internal `apply_action` never recurses into
/// this hook or any `!in_simulation_probe()`-gated shortcut logic.
fn try_offer_object_growth_shortcut(
    state: &GameState,
) -> Option<(
    crate::analysis::loop_check::LoopCertificate,
    crate::analysis::decision_template::ShortcutDecisionSchema,
)> {
    let ctx = state.last_recast_context.clone()?;
    let WaitingFor::Priority { player: caster } = state.waiting_for else {
        return None;
    };
    if ctx.controller != caster {
        return None;
    }
    // The recast card must be in its castable origin zone right now (recastable) —
    // capture it so the recast spell ability can be scanned for randomness below.
    let recast_obj = state.objects.values().find(|o| {
        o.card_id == ctx.card_id && o.zone == ctx.from_zone && o.controller == ctx.controller
    })?;
    // CR 732.2a: a shortcut "can't include conditional actions, where the outcome of a
    // game event determines the next action." A recast whose spell body bears an
    // auto-resolved coin flip (CR 705.1) / die roll (CR 706.1a) / random selection
    // (CR 701.9a/b) has more than one equally-likely outcome ⇒ not a legal shortcut.
    // Reject it STATICALLY, before driving (cheap + compile-time exhaustive over
    // `Effect`). Fail-closed: an undeterminable spell ability (no combined Spell def)
    // also does not offer. (A2 determinism gate — the static half; the post-drive
    // rng-position check below is the complete runtime backstop that additionally
    // catches external triggered/replacement randomness firing in the cycle.)
    let spell_def = crate::game::casting::combined_spell_ability_def(recast_obj)?;
    if crate::game::ability_scan::spell_ability_bears_randomness(&spell_def) {
        return None;
    }

    // Drive two iterations (three settle frames) under the re-entrancy guard.
    let _probe = SimulationProbeGuard::enter();
    let template = build_recast_template(&ctx);
    let s_n = state.clone();
    let mut clone = state.clone();
    drive_recast_iteration(&mut clone, &template, &ctx, 0).ok()?;
    let s_n1 = clone.clone();
    drive_recast_iteration(&mut clone, &template, &ctx, 1).ok()?;
    let s_n2 = clone;

    // CR 732.2a: any randomness CONSUMED during the deterministic detection drive means the
    // real loop is outcome-dependent (a coin flip CR 705.1 / die roll CR 706.1a / random
    // selection CR 701.9b / shuffle) and is not a predictable shortcut. The seeded ChaCha20
    // stream position advances iff randomness was drawn; the driven clone started as
    // `state.clone()` (an equal baseline), so a word-position delta disqualifies the offer.
    // This is the RUNTIME backstop to the static scan above: the fodder-cover's
    // `fire_time_conditions_read_growing_class` already rejects a randomness-bearing *permanent*
    // ability whose effect classifies `Axes::CONSERVATIVE` (`FlipCoin`/`RollDie`; a few
    // dice-adjacent effects like `RollToVisitAttractions` classify `Axes::NONE` and slip the
    // cover — this check catches those too), but it does NOT scan the resolving
    // recast *spell's* own body — so a coin flip in the recast body advances the RNG yet passes
    // the cover. This check closes that gap even when the static scan's `collect_effects` walk
    // misses a nested payload. Fail-closed / strictly-more-conservative (only turns OFFERs into
    // NO-OFFERs). (A2 determinism gate — discharges the b132ad9f8 "fail-closed-modulo-auto-
    // randomness" carry.)
    if s_n2.rng.get_word_pos() != state.rng.get_word_pos() {
        return None;
    }

    // CR 111.10: derive the reproduced token class from the first driven cycle, normalized
    // through the same per-object projection the cover applies to its frames (so the class
    // compares in the P/T-zeroed form, not the raw Some(1) form).
    let mut fodder = derived_fodder_class(&s_n, &s_n1)?;
    crate::analysis::resource::project_object_for_loop(&mut fodder);

    // CR 732.2a board recurrence on BOTH pairs: pure inert tapped-fodder growth. Normalize
    // each frame first (strip the self-returning recast card + clear churning token-id
    // bookkeeping, CR 400.7) so the id-keyed stable compare does not read fresh-each-cycle
    // ObjectIds as a board drift.
    let (cs_n, cs_n1, cs_n2) = (
        normalize_recast_frame(&s_n, &ctx),
        normalize_recast_frame(&s_n1, &ctx),
        normalize_recast_frame(&s_n2, &ctx),
    );
    if !crate::analysis::resource::loop_states_cover_modulo_fodder_growth(&cs_n, &cs_n1, &fodder)
        || !crate::analysis::resource::loop_states_cover_modulo_fodder_growth(
            &cs_n1, &cs_n2, &fodder,
        )
    {
        return None;
    }

    // CR 119 / CR 122.1 / CR 704.5g sign-check on the second pair (RAW un-projected frames):
    // net progress for the caster, no loss axis for anyone, every driving consumable
    // non-decreasing (energy / poison / player-counters / object-counters) and no
    // damage_marked increase.
    let mut delta = crate::analysis::resource::ResourceVector::delta(
        &crate::analysis::resource::ResourceVector::snapshot(&s_n1),
        &crate::analysis::resource::ResourceVector::snapshot(&s_n2),
    );
    // CR 111.10: `tokens_created` is an EVENT-fed axis (0 under a snapshot diff), but the
    // cover above already proved the battlefield grows ONLY by inert reproduced tokens, so
    // the battlefield growth IS the per-cycle tokens-created count — the unbounded axis. Feed
    // it so `net_progress_for` sees the progress and the certificate names TokensCreated.
    let board_growth = s_n2.battlefield.len() as i64 - s_n1.battlefield.len() as i64;
    if board_growth > 0 {
        delta.tokens_created += board_growth;
    }
    if !delta.net_progress_for(caster)
        || !has_no_loss_axis(&delta)
        || !crate::analysis::resource::driving_resources_non_decreasing(&s_n1, &s_n2, caster)
    {
        return None;
    }

    // CR 104.4b: only OFFER an OPTIONAL loop (a player could choose not to repeat it). A
    // mandatory board-growth loop would draw (Path B) / be handled elsewhere, not offered.
    if no_living_player_has_meaningful_priority_action(state) {
        return None;
    }

    let certificate = build_cert(&s_n1, &s_n2, &delta, caster);
    // CR 732.2a (CARRY, don't re-derive): the schema's decision list is the SAME
    // `build_recast_template` output driven above — `[ConvokeTaps]` when the recast has convoke,
    // else `[]`. Legal sets are derived against the live offer-time board.
    let schema = build_shortcut_schema(&template.decisions, certificate.win_kind, state, caster);
    Some((certificate, schema))
}

/// PR-7 Phase 4d-ii (CR 732.2a): materialize a confirmed `Fixed(N)` object-growth recast
/// shortcut by driving N real recast cycles on a clone via the injector, committing each
/// completed cycle atomically. The recurrence boundary is the injector's OWN settle
/// condition (`Priority` + empty stack) — one successful `drive_recast_iteration` IS one
/// materialized cycle of real game actions, so the result is ground-truth (contrast the
/// drain path, which re-derives recurrence from `loop_states_*`). Any injector abort
/// (e.g. the loop stopped being sustainable — CR 702.51b unpayable convoke) ⇒ commit the
/// completed cycles + hand priority back (CR 800.4a). Runs under the re-entrancy guard.
fn materialize_object_growth_shortcut(
    state: &mut GameState,
    result: &mut ActionResult,
    ctx: &crate::types::game_state::RecastContext,
    n: u32,
) {
    let template = build_recast_template(ctx);
    let _probe = SimulationProbeGuard::enter();
    // Last fully-completed cycle (owned O(1) rollback); the board is unchanged since the
    // offer (Declare/Accept touch only the protocol).
    let mut committed = state.clone();

    for i in 0..n {
        // Seed the caster's settle priority beat so the injector's first action (CastSpell)
        // is authorized (the entry `waiting_for` is RespondToShortcut / LoopShortcut).
        let mut work = committed.clone();
        priority::reset_priority(&mut work);
        work.priority_player = ctx.controller;
        work.waiting_for = WaitingFor::Priority {
            player: ctx.controller,
        };
        if drive_recast_iteration(&mut work, &template, ctx, i).is_err() {
            break; // loop no longer sustainable ⇒ keep the completed cycles
        }
        committed = work; // ATOMIC: one real recast cycle committed
    }

    *state = committed;
    // Ring-clear + consume the recast context BEFORE handback so this same `apply()` does
    // not instantly re-offer the just-materialized loop; a later manual recast re-arms the
    // context and a later beat re-detects genuinely.
    state.loop_detect_ring.clear();
    state.last_recast_context = None;
    priority::reset_priority(state);
    state.waiting_for = WaitingFor::Priority {
        player: living_priority_seat(state),
    };
    result.waiting_for = state.waiting_for.clone();
}

/// Immutable data from a `WaitingFor::LoopShortcut` offer, grouped for declaration handling.
struct LoopShortcutOffer<'a> {
    proposer: PlayerId,
    predicted_winner: Option<PlayerId>,
    certificate: &'a crate::analysis::loop_check::LoopCertificate,
    schema: &'a crate::analysis::decision_template::ShortcutDecisionSchema,
}

/// CR 732.2a: the proposer declared the loop shortcut. Build the public proposal and open
/// the APNAP accept-or-shorten window over the proposer's living opponents (turn order). No
/// opponents (solitaire / all eliminated) ⇒ take the shortcut immediately.
fn handle_declare_shortcut(
    state: &mut GameState,
    offer: LoopShortcutOffer<'_>,
    count: crate::analysis::decision_template::IterationCount,
    template: Option<crate::analysis::decision_template::DecisionTemplate>,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    let mut result = ActionResult {
        events: std::mem::take(events),
        waiting_for: state.waiting_for.clone(),
        log_entries: vec![],
    };
    // CR 732.2a fail-closed firewall: validate the declared pins against the offered schema
    // BEFORE `template` is moved into `proposal` and BEFORE APNAP opens. Coverage
    // (`predictability_gate`) and value-legality (`validate_pins`) both consult the SAME
    // single authority — the schema's exposed slots — so a rejection lands cleanly at a
    // manual-play handback (Priority to the living seat, no APNAP, no drive, no crown). The
    // offer window closes; a later beat re-detects the loop if it still closes. Validating
    // once at declare suffices: the board is frozen through Accept (apply_confirmed_shortcut
    // doc), and the drive's per-iteration `resolve` (CR 608.2b) is the runtime backstop.
    //
    // A CHOICE-FREE offer (empty schema — a non-targeted drain) exposes no decisions to
    // validate: its win derivation is pin-independent (the E1 measure is the authority), and
    // any template the caller supplies is inert for the drive (the loop raises no target
    // prompt). This preserves the established `Fixed(N)` drain behavior (the resolve-firewall
    // materialize tests drive a synthetic pin against the empty drain schema).
    if let Some(t) = &template {
        if !offer.schema.points.is_empty() {
            let required: Vec<crate::analysis::decision_template::DecisionSlot> =
                offer.schema.points.iter().map(|p| p.slot.clone()).collect();
            let period = shortcut_drive_period(Some(t));
            if crate::analysis::decision_template::predictability_gate(t, &required).is_err()
                || crate::analysis::decision_template::validate_pins(offer.schema, t, period, state)
                    .is_err()
            {
                priority::reset_priority(state);
                // CR 800.4a: hand priority to the next living seat.
                state.waiting_for = WaitingFor::Priority {
                    player: living_priority_seat(state),
                };
                result.waiting_for = state.waiting_for.clone();
                return Ok(result);
            }
        }
    }
    // CR 732.2a SAFETY LIMIT (see MAX_SHORTCUT_CYCLES): reject an over-cap Fixed count at
    // the single authority — BEFORE the proposal is built — into the same fail-closed
    // manual-play handback the pin validation above uses. This is THE catastrophic remote
    // vector: `Fixed(u32)` scalar-encodes up to ~4.3e9 cycles in ~10 bytes, sailing through
    // the 8 KB WS frame cap → one GameState clone + drive per cycle. Both confirmation paths
    // (solitaire-immediate below, APNAP Accept) consume this one proposal, and both drive
    // helpers (materialize_fixed_shortcut / materialize_object_growth_shortcut) read `n` from
    // it, so this one check bounds every Fixed drive on every transport. The drive helpers do
    // NOT re-check.
    // Exhaustive (no wildcard) so a future `IterationCount` variant — e.g. the reserved
    // `UntilResource`, which would carry its OWN unbounded count — build-breaks HERE and
    // forces a bound decision rather than silently regressing this cap.
    match &count {
        crate::analysis::decision_template::IterationCount::Fixed(n)
            if *n > MAX_SHORTCUT_CYCLES =>
        {
            priority::reset_priority(state);
            // CR 800.4a: hand priority to the next living seat.
            state.waiting_for = WaitingFor::Priority {
                player: living_priority_seat(state),
            };
            result.waiting_for = state.waiting_for.clone();
            return Ok(result);
        }
        // Under-cap `Fixed` and `UntilLethal` (period-bounded by `shortcut_drive_period`)
        // proceed to the proposal.
        crate::analysis::decision_template::IterationCount::Fixed(_)
        | crate::analysis::decision_template::IterationCount::UntilLethal => {}
    }
    let proposal = crate::analysis::loop_check::ShortcutProposal {
        proposer: offer.proposer,
        predicted_winner: offer.predicted_winner,
        count,
        unbounded: offer.certificate.unbounded.clone(),
        win_kind: offer.certificate.win_kind,
        template,
    };
    // CR 732.2b: living opponents in APNAP turn order, starting after the proposer.
    let opps: Vec<PlayerId> = crate::game::players::apnap_order_from(
        state,
        Some(crate::types::ability::ControllerRef::You),
        offer.proposer,
    )
    .into_iter()
    .filter(|&p| p != offer.proposer)
    .collect();
    if let Some((&first, rest)) = opps.split_first() {
        state.waiting_for = WaitingFor::RespondToShortcut {
            player: first,
            remaining_players: rest.to_vec(),
            proposal,
        };
        result.waiting_for = state.waiting_for.clone();
    } else {
        // CR 732.2c: nobody else to poll ⇒ take the shortcut.
        apply_confirmed_shortcut(state, &mut result, &proposal);
    }
    Ok(result)
}

/// CR 732.2a: the priority holder MAY decline the auto-offered loop shortcut — "the player
/// with priority may suggest a shortcut" makes suggesting optional, so forcing a proposal is
/// wrong. Restore ordinary priority (the living seat, mirroring the `handle_declare_shortcut`
/// pin-rejection handback) so the post-return reconcile hands the controller a normal window
/// instead of re-nagging the SAME offer. This is the `until_lethal_fallback` tail minus the
/// board rollback: decline is pre-drive, so no board mutation ever occurred.
///
/// Re-offer suppression, by seam:
/// - Interactive bridge (Seam 1, `find_live_loop_winner` reads `loop_detect_ring`, gated by
///   `!stack.is_empty()`): suppressed by the GENERAL deliberate-action invariant, not by this
///   handler. `apply_action` (engine.rs:3006-3011) invalidates `loop_detect_ring` for every
///   deliberate (non-`PassPriority`/`OrderTriggers`) action; `DeclineShortcut` is a deliberate
///   break, so the ring is already empty before this handler runs. Seam-1 suppression is the
///   shared invariant every cast/activate/play-land relies on — the handler does NOT re-clear
///   the ring (re-clearing would special-case `DeclineShortcut` to distrust an engine-wide
///   invariant). The interactive e2e's "no re-offer" assertion guards this end-to-end: a future
///   regression excluding `DeclineShortcut` from that allowlist would fail it loudly.
/// - Object-growth (Seam 2, gated by `last_recast_context.is_some()`): the deliberate-action
///   clear does NOT touch `last_recast_context`, so `state.last_recast_context = None` here is
///   the genuinely load-bearing suppressor — without it the post-return reconcile re-fires
///   `try_offer_object_growth_shortcut` within this same `apply()`.
///
/// A genuine re-recurrence or a fresh re-cast re-arms the offer naturally. Proposer-only
/// authorization is enforced upstream by `check_actor_authorization`
/// (`WaitingFor::acting_player` == `LoopShortcut.proposer`), so offer fields are unused here.
fn handle_decline_shortcut(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    let mut result = ActionResult {
        events: std::mem::take(events),
        waiting_for: state.waiting_for.clone(),
        log_entries: vec![],
    };
    // Seam 1 (loop_detect_ring) is already invalidated by apply_action's deliberate-action
    // ring-clear (engine.rs:3006-3011) — see doc. Only Seam 2 is the handler's gap:
    state.last_recast_context = None; // Seam 2: load-bearing object-growth offer-gate clear (CR 732.2a)
    priority::reset_priority(state);
    state.waiting_for = WaitingFor::Priority {
        player: living_priority_seat(state),
    };
    result.waiting_for = state.waiting_for.clone();
    Ok(result)
}

/// CR 732.2b/c: one opponent answered the shortcut offer. Mirrors the
/// `OpponentMayChoice`/`UnlessPayment` APNAP fan-out (drain-one-advance via
/// `remaining_players`). Accept advances to the next opponent, or — when the last accepts —
/// takes the shortcut. Shorten conservatively hands THAT opponent a real priority window
/// (CR 732.2c "a different choice"); the shortcut is NOT auto-applied, and a later beat
/// re-detects the loop (a fresh offer if it still closes, normal play if broken).
fn handle_respond_to_shortcut(
    state: &mut GameState,
    player: PlayerId,
    remaining_players: Vec<PlayerId>,
    proposal: crate::analysis::loop_check::ShortcutProposal,
    response: crate::analysis::loop_check::ShortcutResponse,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    let mut result = ActionResult {
        events: std::mem::take(events),
        waiting_for: state.waiting_for.clone(),
        log_entries: vec![],
    };
    match response {
        crate::analysis::loop_check::ShortcutResponse::Accept => {
            // CR 800.4a: never advance the offer onto a player who has left the game. A
            // queued opponent can concede AFTER the window opened (Concede bypasses the
            // `WaitingFor` dispatch, so `remaining_players` is never self-healed), so drop
            // any departed seats before advancing. CR 732.2b: the queue is already in APNAP
            // turn order, so the first surviving remainder is the next living opponent.
            let mut living = remaining_players
                .into_iter()
                .filter(|&p| crate::game::players::is_alive(state, p));
            if let Some(next) = living.next() {
                state.waiting_for = WaitingFor::RespondToShortcut {
                    player: next,
                    remaining_players: living.collect(),
                    proposal,
                };
                result.waiting_for = state.waiting_for.clone();
            } else {
                // CR 732.2c: the last living opponent accepted ⇒ take the shortcut
                // (F1 re-validates the proposer's own liveness before crowning).
                apply_confirmed_shortcut(state, &mut result, &proposal);
            }
        }
        crate::analysis::loop_check::ShortcutResponse::Shorten { .. } => {
            // CR 732.2c (Phase-3 conservative): hand this opponent a real priority window
            // instead of taking the shortcut. Finite-K materialization is Phase 4.
            priority::reset_priority(state);
            state.priority_player = player;
            state.waiting_for = WaitingFor::Priority { player };
            result.waiting_for = state.waiting_for.clone();
        }
    }
    Ok(result)
}

fn remember_public_reveals(state: &mut GameState, events: &[GameEvent]) {
    for event in events {
        if let GameEvent::CardsRevealed { card_ids, .. } = event {
            state.public_revealed_cards.extend(card_ids.iter().copied());
        }
    }
}

/// Engine-level authorization guard. Any *game action* must come from the
/// `authorized_submitter` for the current `WaitingFor` (which already accounts
/// for turn-decision-controller effects like Mindslaver). Two exception classes:
///
/// - `Concede` self-authenticates via its own `player_id` field — but we still
///   require it to match `actor` so a player cannot concede someone else on
///   their behalf (CR 104.3a).
/// - **Preference actions** (SetPhaseStops, CancelAutoPass) are per-player UI
///   settings. They have no CR semantics, mutate only the submitter's own
///   preference slot, and may legitimately fire at any time — e.g. the human
///   toggles a phase stop while the AI holds priority. The downstream handlers
///   route by `actor`, so any seat may set its own preferences regardless of
///   `WaitingFor`. `SetAutoPass` is deliberately NOT exempt: its handler
///   stores the mode for the `WaitingFor::Priority` player and immediately
///   passes that priority, so it must come from the authorized submitter.
fn check_actor_authorization(
    state: &GameState,
    actor: PlayerId,
    action: &GameAction,
) -> Result<(), EngineError> {
    if let GameAction::Concede { player_id } = action {
        // CR 104.3a: A player may concede at any time — but only themselves.
        if *player_id != actor {
            return Err(EngineError::WrongPlayer);
        }
        return Ok(());
    }
    if matches!(
        action,
        GameAction::SetPhaseStops { .. }
            | GameAction::SetPriorityYield { .. }
            | GameAction::SetMayTriggerAutoChoice { .. }
            | GameAction::SetTriggerOrderTemplate { .. }
            | GameAction::CancelAutoPass
            | GameAction::Debug(_)
            | GameAction::GrantDebugPermission { .. }
            | GameAction::RevokeDebugPermission { .. }
            | GameAction::ReorderHand { .. }
    ) {
        return Ok(());
    }
    // CR 103.5: For simultaneous-decision states (MulliganDecision,
    // OpeningHandBottomCards), authorize against the full pending set so any
    // pending player may submit in any order. Falls back to single-player
    // semantics for every other variant.
    let authorized = turn_control::authorized_submitters(state);
    if !authorized.is_empty() && !authorized.contains(&actor) {
        return Err(EngineError::WrongPlayer);
    }
    Ok(())
}

/// Engine-internal convenience: apply `action` as the player the engine is
/// currently waiting on. Intended for simulation (AI search, legal-action
/// probing) and tests — *not* for transport adapters, which must pass a
/// transport-authenticated `actor` to [`apply`] directly.
///
/// For [`GameAction::Concede`] the concede payload's `player_id` is used as
/// the actor, so tests can concede any player without first maneuvering the
/// `WaitingFor` state onto that player.
pub fn apply_as_current(
    state: &mut GameState,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_as_current_with_mode(state, action, PublicFinalizeMode::Immediate)
}

/// Simulation-apply variant of [`apply_as_current`] for throwaway clones that
/// are never rendered: either the caller discards the mutated state (the AI
/// `SimulationFilter` legality oracle reads only `.is_ok()`) or it keeps the
/// state solely to read *game-logic* fields for evaluation (the AI search
/// rollout/expansion). `finalize_rules_state` still runs, so the result is
/// rules-correct; only `finalize_display_state` — the board-global
/// `derive_display_state` sweep computing frontend-only hints (mana
/// availability `has_mana_ability`/`available_mana_pips`, devotion,
/// summoning-sickness display) that no rules, enumeration, or AI-evaluation
/// path consults — is skipped. On a large board this removes an
/// O(battlefield) mana sweep from every legality probe AND every AI search
/// node expansion; that per-node sweep, compounded across the un-timed
/// `resolveAll` batch loop, was the AI-vs-AI "won't advance" wedge (#4798).
pub fn apply_as_current_for_simulation(
    state: &mut GameState,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_as_current_with_mode(state, action, PublicFinalizeMode::DeferredDisplay)
}

fn apply_as_current_with_mode(
    state: &mut GameState,
    action: GameAction,
    mode: PublicFinalizeMode,
) -> Result<ActionResult, EngineError> {
    let actor = match &action {
        GameAction::Concede { player_id } => *player_id,
        // CR 103.5: For simultaneous-decision states, pick the first pending
        // player as the simulation representative. `authorized_submitters`
        // returns the full set; `first()` is deterministic (seat-ordered).
        _ => {
            let submitters = turn_control::authorized_submitters(state);
            submitters.first().copied().ok_or_else(|| {
                EngineError::InvalidAction(
                    "apply_as_current: no authorized submitter (game over?)".to_string(),
                )
            })?
        }
    };
    apply_action_boundary(state, actor, action, mode)
}

pub(super) fn resume_pending_continuation_if_priority(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        effects::drain_pending_continuation(state, events);
    }
    Ok(())
}

/// Decision emitted by the auto-pass loop's per-iteration check.
enum AutoPassDecision {
    /// No active auto-pass — leave the loop and let the frontend take over.
    Exit,
    /// Auto-pass completed or was interrupted (opponent action, phase stop,
    /// stack terminator). Clear the flag and exit.
    Finish,
    /// Continue passing priority for this iteration.
    Pass,
}

/// Classify what the auto-pass loop should do for `player` at the current
/// priority window.
///
/// Interrupts (MTGA-style): `UntilStackEmpty` bails when the stack empties or
/// grows beyond the baseline (trigger or opponent spell); `UntilTurnBoundary`
/// bails when an opponent-controlled object is on top of the stack or when the
/// current phase is in the user-supplied `phase_stops` list. The per-window
/// interrupt logic is boundary-agnostic — both `EndOfCurrentTurn` and
/// `MyNextTurnStart` behave identically within a priority window.
fn priority_auto_pass_decision(state: &GameState, player: PlayerId) -> AutoPassDecision {
    let Some(mode) = state.auto_pass.get(&player) else {
        return AutoPassDecision::Exit;
    };
    match mode {
        AutoPassMode::UntilStackEmpty { initial_stack_len } => {
            if state.stack.is_empty() || state.stack.len() > *initial_stack_len {
                AutoPassDecision::Finish
            } else {
                AutoPassDecision::Pass
            }
        }
        AutoPassMode::UntilTurnBoundary { .. } => {
            // CR 117.3d: An opponent-controlled top-of-stack normally ends the
            // session so the player can respond — unless they have pre-committed
            // to yield priority for that exact triggered ability, in which case
            // the session keeps auto-passing through it.
            let opponent_on_stack = state.stack.last().is_some_and(|top| {
                top.controller != player && !state.is_priority_yielded(player, top)
            });
            if opponent_on_stack || state.phase_stop_hit(player) {
                AutoPassDecision::Finish
            } else {
                AutoPassDecision::Pass
            }
        }
    }
}

/// True when `player` has an active turn-boundary auto-pass session (either
/// boundary). Both `EndOfCurrentTurn` and `MyNextTurnStart` drive the
/// DeclareAttackers/DeclareBlockers empty auto-submit arms, since both
/// auto-submit empty attackers within the current turn.
fn end_of_turn_active(state: &GameState, player: PlayerId) -> bool {
    matches!(
        state.auto_pass.get(&player),
        Some(AutoPassMode::UntilTurnBoundary { .. })
    )
}

fn pass_priority_once_with_pipeline(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    stack_resolution_limit: Option<u32>,
) -> Result<WaitingFor, EngineError> {
    if let WaitingFor::Priority { player } = &state.waiting_for {
        if super::precast_copy_shortcut::blocks_pass(state, *player) {
            return Ok(state.waiting_for.clone());
        }
    }
    state.cancelled_casts.clear();
    // CR 117.4 + 608.1: When all players pass in succession the stack begins
    // resolving; at that moment the AI guard against re-activating pending
    // abilities is no longer needed.
    state.pending_activations.clear();

    let stack_was_empty = state.stack.is_empty();
    // PR-3 (Option C) Defect-1: capture the pre-pipeline stack frame for the §2
    // loop-shortcut window maintenance below. `stack_top_before` is the resolving
    // entry's id; a real resolution this beat replaces the top with a different id
    // (every refilled trigger gets a fresh monotonic ObjectId), whereas a bare
    // priority handoff leaves it unchanged.
    let stack_len_before = state.stack.len();
    let stack_top_before = state.stack.last().map(|e| e.id);
    // CR 117.4 + CR 723.5/723.8: pass the *seat* that holds priority, not
    // `priority_player` — under turn-control the latter is the authorized
    // submitter (the controller), which would mis-count consecutive passes and
    // soft-lock the game.
    let current_seat = turn_control::priority_seat(state);
    let wf = priority::handle_priority_pass_with_limit(
        current_seat,
        state,
        events,
        stack_resolution_limit,
    );
    sync_waiting_for(state, &wf);

    // CR 608.2 + CR 117.4: Drain any pending continuation queued during the
    // priority pass (e.g. effects that chain a sub-resolution after the parent
    // settles) while the stack is still in its post-resolution state. Without
    // this drain, a continuation queued after a no-choice effect would sit
    // until an unrelated action, by which point referenced stack objects may
    // have left the stack.
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        effects::drain_pending_continuation(state, events);
    }

    let skip_triggers =
        stack_was_empty && !state.stack.is_empty() && state.phase == Phase::CombatDamage;

    let wf = engine_priority::run_post_action_pipeline(
        state,
        events,
        &state.waiting_for.clone(),
        skip_triggers,
        false,
    )?;
    sync_waiting_for(state, &wf);

    // PR-3 (Option C) CR 732.2a loop-shortcut window accumulation — relocated here
    // (PR3 Defect-1 fix). The refilling trigger is placed by
    // `run_post_action_pipeline` (CR 603.3 / CR 704.3: triggered abilities waiting to
    // go on the stack are put there the next time a player would receive priority),
    // which runs above — AFTER the resolution seam in `handle_priority_pass_with_limit`.
    // Sampling here is the only frame where a self-refilling cascade is already
    // non-shrinking (the refilled trigger is on the stack).
    //
    // RESOLUTION-OCCURRED GATE. `resolved_this_beat` is true iff there WAS a top entry
    // at function entry and it is no longer the top — i.e. a stack entry was actually
    // resolved/consumed this beat. A bare priority handoff (the active player passes,
    // priority moves on, stack untouched) leaves the top unchanged ⇒
    // `resolved_this_beat == false` ⇒ the ring is LEFT INTACT so accumulation survives
    // across the handoff beats that separate resolutions under the per-beat drive. A
    // naive `len >= before` gate would false-positive on those handoffs; a strict
    // clear-on-handoff would destroy the accumulation — both are wrong. This gate
    // samples only on a real resolution and touches the ring only then.
    let resolved_this_beat =
        stack_top_before.is_some() && state.stack.last().map(|e| e.id) != stack_top_before;
    // CR 732.2a: sample the loop-detection ring ONLY when the user-controllable
    // combo-detector is enabled. With `loop_detection == Off` (the default) the ring
    // is never populated, so the engine pays none of the per-resolution
    // `normalize_for_loop` clone cost and the reconcile-seam shortcut (which guards on
    // a non-empty ring AND the same flag) can never fire — exact pre-detector behavior.
    // PR-7 Phase 3: `samples()` so `Interactive` populates the ring identically to `On`;
    // `Off` (false) and `On` (true) are byte-preserved (`samples() == is_on()` for both).
    if resolved_this_beat && !in_simulation_probe() && state.loop_detection.samples() {
        // REFILL gate: a self-refilling MANDATORY cascade holds the stack non-empty and
        // non-shrinking across the resolution, settling at a non-interactive priority
        // window reset to the active player (the canonical modulo-comparison point —
        // `project_out_resources` compares phase/priority exactly). A normal multi-spell
        // stack SHRINKS; an interactive effect opens a non-Priority window; a finite
        // chain drains to empty — all three fall to the clear arm.
        if !state.stack.is_empty()
            && state.stack.len() >= stack_len_before
            && matches!(wf, WaitingFor::Priority { player } if player == state.active_player)
        {
            state.record_loop_detect_sample();
        } else if !matches!(wf, WaitingFor::OrderTriggers { .. }) {
            state.loop_detect_ring.clear();
        }
        // CR 603.3b + CR 732.2a: leave the ring intact on the mandatory trigger-ordering
        // window — ordering simultaneous triggers is a forced step of putting them on the
        // stack (staged in pending_trigger_order, so the stack is momentarily shrunk/empty
        // here), not a settle or deliberate break. Preserving the Priority{active} samples
        // across the beat lets a self-refilling multi-trigger loop reach CR 732.2a detection.
    }
    // No else-branch: a bare handoff or an empty-stack pass-to-advance-phase does NOT
    // touch the ring (leave-intact), so accumulation survives the inter-resolution beats.

    Ok(wf)
}

fn active_until_stack_empty_requester(state: &GameState) -> Option<PlayerId> {
    state.auto_pass.iter().find_map(|(player, mode)| {
        matches!(mode, AutoPassMode::UntilStackEmpty { .. }).then_some(*player)
    })
}

fn priority_player_has_meaningful_action(state: &GameState) -> bool {
    let mut probe_state = state.clone();
    probe_state.auto_pass.clear();
    super::layers::flush_layers(&mut probe_state);
    let player = match probe_state.waiting_for {
        WaitingFor::Priority { player } => player,
        _ => probe_state.priority_player,
    };
    let probe = super::casting::PriorityCastProbe::from_flushed_state(probe_state, player);
    // The probe always has `waiting_for == Priority` at both call sites, so the
    // flat priority-action path is byte-identical to what `legal_actions` yielded
    // — it drops only the unused spell-cost object-walk and grouped-map build.
    let actions = crate::ai_support::flat_priority_actions_with_probe(probe.state(), Some(&probe));
    crate::ai_support::has_meaningful_priority_action(probe.state(), &actions)
}

/// CR 732.5: no player can be forced to keep looping if ANY of them could take an
/// action that ends the loop. The cap-path [`priority_player_has_meaningful_action`]
/// checks only the CURRENT priority holder; the loop-shortcut WIN designates a
/// LOSER, so its gate must be stronger — the would-be loop-breaker (a victim whose
/// priority is auto-passed by a stale `UntilStackEmpty`/`UntilTurnBoundary` session,
/// which `priority_auto_pass_decision` Passes WITHOUT a meaningful check) need NOT
/// hold priority at the modulo-match iteration. Probe EVERY living player as the
/// priority holder (`legal_actions`/`has_meaningful_priority_action` key off
/// `waiting_for`). Conservative: if anyone has a meaningful action this returns
/// `false` and the cascade falls through to the existing halt (priority preserved) —
/// fail-safe toward the status quo, never a wrong win.
fn no_living_player_has_meaningful_priority_action(state: &GameState) -> bool {
    state.players.iter().filter(|p| !p.is_eliminated).all(|p| {
        let mut probe_state = state.clone();
        probe_state.auto_pass.clear();
        probe_state.priority_player = p.id;
        probe_state.waiting_for = WaitingFor::Priority { player: p.id };
        super::layers::flush_layers(&mut probe_state);
        let probe = super::casting::PriorityCastProbe::from_flushed_state(probe_state, p.id);
        let actions =
            crate::ai_support::flat_priority_actions_with_probe(probe.state(), Some(&probe));
        !crate::ai_support::has_meaningful_priority_action(probe.state(), &actions)
    })
}

fn finish_completed_or_interrupted_until_stack_empty_sessions(state: &mut GameState) -> bool {
    let finished: Vec<PlayerId> = state
        .auto_pass
        .iter()
        .filter_map(|(player, mode)| match mode {
            AutoPassMode::UntilStackEmpty { initial_stack_len }
                if state.stack.is_empty() || state.stack.len() > *initial_stack_len =>
            {
                Some(*player)
            }
            _ => None,
        })
        .collect();

    for player in &finished {
        state.auto_pass.remove(player);
    }

    !finished.is_empty()
}

// CR 732.2a SAFETY LIMIT: a shortcut is "a loop that repeats a specified number of times";
// the CR places NO board-relative upper bound, so this is an engine implementation cap
// against an absurd/hostile count — NOT a rules constraint. It bounds both a `Fixed(n)`
// cycle count (handle_declare_shortcut) and a template drive period (shortcut_drive_period).
// Motivating vector: a `u32` count scalar-encodes up to ~4.3e9 cycles in ~10 JSON bytes, so
// it sails through the 8 KB inbound WS frame cap (phase-server/src/main.rs:409/1420) yet
// would force ~4.3e9 GameState clones — a byte cap cannot see it, only this count cap can.
// 1_000 is generous vs any honest Fixed count (~10x KCI-style loops); worst-case bounded
// cost is 1_000 cycles x <=10_000 beats = 1e7.
const MAX_SHORTCUT_CYCLES: u32 = 1_000;

fn auto_pass_loop_max_iterations(state: &GameState) -> usize {
    let living_players = state
        .players
        .iter()
        .filter(|player| !player.is_eliminated)
        .count()
        .max(1);
    state
        .stack
        .len()
        .saturating_mul(living_players)
        .saturating_mul(2)
        .saturating_add(16)
        .clamp(500, 10_000)
}

#[cfg(test)]
#[path = "engine_auto_pass_decision_tests.rs"]
mod auto_pass_decision_tests;

/// Auto-pass loop: when a player has an auto-pass flag and receives priority,
/// automatically pass for them until the goal condition is met or interrupted.
fn run_auto_pass_loop(state: &mut GameState, result: &mut ActionResult) {
    // CR 732.2: per-dispatch resource ceilings for a runaway mandatory cascade.
    // Sized above the largest legitimate single-dispatch burst (a Scute Swarm
    // landfall copies every Scute in one resolution — tested boards reach ~2,936
    // permanents) yet far below the WASM linear-memory exhaustion threshold
    // (hundreds of thousands of objects). The iteration cap below is the
    // sustained-growth backstop; these deltas catch heavy-per-iteration loops.
    const MAX_EVENT_GROWTH: usize = 50_000;
    const MAX_OBJECT_GROWTH: usize = 16_000;
    let events_baseline = result.events.len();
    let objects_baseline = state.objects.len();

    // CR 104.4b: bounded-state mandatory-loop detection. Fingerprinting starts
    // only after this many mandatory iterations (normal resolution settles far
    // sooner, so it pays nothing); stored normalized snapshots are capped so a
    // non-repeating mandatory sequence falls through to the Phase-1 backstop.
    const FINGERPRINT_AFTER_ITERS: usize = 32;
    const MAX_LOOP_WINDOW: usize = 128;
    let mut mandatory_iters = 0usize;
    let mut loop_window: VecDeque<(u64, GameState)> = VecDeque::new();

    let max_iterations = auto_pass_loop_max_iterations(state);
    let mut iteration = 0usize;
    loop {
        // CR 732.2: the iteration cap was exhausted while a mandatory cascade is
        // still in flight (priority unsettled, non-empty stack, no meaningful
        // action) — halt gracefully, the same way the growth ceilings do, rather
        // than fall through and leave the game mid-cascade. Reached ONLY on true
        // exhaustion: every productive exit below uses `break`, leaving the loop
        // without passing this guard, so a normal short resolution never trips it.
        if iteration >= max_iterations {
            if matches!(result.waiting_for, WaitingFor::Priority { .. })
                && !state.stack.is_empty()
                && !priority_player_has_meaningful_action(state)
            {
                emit_resolution_halt(state, result);
            }
            break;
        }
        iteration += 1;

        match &result.waiting_for {
            WaitingFor::Priority { player } => {
                let player = *player;
                if super::precast_copy_shortcut::blocks_pass(state, player) {
                    break;
                }
                let decision = priority_auto_pass_decision(state, player);
                match decision {
                    AutoPassDecision::Exit => {
                        let Some(requester) = active_until_stack_empty_requester(state) else {
                            break;
                        };
                        if requester == player {
                            break;
                        }
                        if finish_completed_or_interrupted_until_stack_empty_sessions(state) {
                            break;
                        }
                        if priority_player_has_meaningful_action(state) {
                            break;
                        }
                    }
                    AutoPassDecision::Finish => {
                        state.auto_pass.remove(&player);
                        break;
                    }
                    AutoPassDecision::Pass => {}
                }

                let mut events = Vec::new();
                match pass_priority_once_with_pipeline(state, &mut events, None) {
                    Ok(wf) => {
                        let stack_empty_or_grew =
                            finish_completed_or_interrupted_until_stack_empty_sessions(state);
                        result.events.extend(events);
                        result.waiting_for = wf;
                        // CR 732.2: a mandatory cascade growing the board or
                        // event stream past the resource ceiling cannot settle —
                        // halt gracefully rather than exhaust WASM memory.
                        if result.events.len().saturating_sub(events_baseline) > MAX_EVENT_GROWTH
                            || state.objects.len().saturating_sub(objects_baseline)
                                > MAX_OBJECT_GROWTH
                        {
                            emit_resolution_halt(state, result);
                            return;
                        }

                        // CR 104.4b: detect a repeating mandatory loop. Every
                        // iteration here is mandatory by construction (a
                        // meaningful action would have broken the loop), so the
                        // window never spans an optional action. A cheap
                        // fingerprint pre-filters; a true repeat is CONFIRMED by
                        // deep state equality before any draw, so a fingerprint
                        // collision can never cause a wrongful draw.
                        mandatory_iters += 1;
                        if mandatory_iters >= FINGERPRINT_AFTER_ITERS
                            && matches!(result.waiting_for, WaitingFor::Priority { .. })
                        {
                            let fingerprint = state.loop_fingerprint();
                            let normalized = state.normalize_for_loop();
                            if loop_window.iter().any(|(fp, prior)| {
                                *fp == fingerprint
                                    && crate::types::game_state::loop_states_equal(
                                        &normalized,
                                        prior,
                                    )
                            }) {
                                // CR 104.4b + CR 732.4: a mandatory action
                                // repeated a prior state with no way to stop — a
                                // draw. CR 801.16: limited-range partial draw N/A
                                // while format_config.range_of_influence is None.
                                result.events.push(GameEvent::GameOver { winner: None });
                                result.waiting_for = WaitingFor::GameOver { winner: None };
                                state.waiting_for = WaitingFor::GameOver { winner: None };
                                match_flow::handle_game_over_transition(state);
                                return;
                            }

                            // PR-3 (Option C): the NET-PROGRESS mandatory-loop WIN
                            // shortcut is NOT duplicated here. `run_auto_pass_loop`
                            // resolves via `pass_priority_once_with_pipeline` (:1339),
                            // whose §2 maintenance accumulates the persisted
                            // `loop_detect_ring` across these internal iterations, but
                            // `reconcile_terminal_result` (the §3 win site) is NOT called
                            // inside this loop — only at :200 AFTER it returns. So the §3
                            // shortcut does NOT accelerate this auto-pass grind: this loop
                            // runs its own net-progress drive to the natural CR 704.5a
                            // death (or the strict CR 104.4b DRAW block above) on its own.
                            // The accelerated path is the per-beat repeated
                            // `apply(PassPriority)` drive (the production frontend
                            // default), where §3 runs after every beat. Keeping a second
                            // win site here would create two divergent detectors.

                            // CR 104.4b: a sliding window of the most recent
                            // MAX_LOOP_WINDOW distinct states. A fill-once-and-stop
                            // buffer never records the cycle of a loop whose
                            // repeating phase begins after a long mandatory preamble
                            // (more than MAX_LOOP_WINDOW transient states), silently
                            // downgrading that bounded-state draw to a Phase-1 halt.
                            // Evicting the oldest keeps any period <= MAX_LOOP_WINDOW
                            // detectable regardless of when the cycle starts; the
                            // deep loop_states_equal confirmation above still gates
                            // every draw, so eviction never risks a wrongful draw.
                            if loop_window.len() == MAX_LOOP_WINDOW {
                                loop_window.pop_front();
                            }
                            loop_window.push_back((fingerprint, normalized));
                        }

                        if stack_empty_or_grew {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            // UntilTurnBoundary: auto-submit empty attackers unless the user
            // flagged this phase as a stop.
            WaitingFor::DeclareAttackers { player, .. }
                if end_of_turn_active(state, *player) && !state.phase_stop_hit(*player) =>
            {
                let mut events = Vec::new();
                match engine_combat::handle_empty_attackers(state, &mut events) {
                    Ok(wf) => {
                        sync_waiting_for(state, &wf);
                        result.events.extend(events);
                        result.waiting_for = wf;
                    }
                    Err(_) => break,
                }
            }

            // Auto-submit empty blockers only when there's nothing to choose.
            // CR 509.1 says the turn-based action still runs when no legal blocks
            // are available, and CR 117.1c requires the active player to receive
            // priority during the step (instants and Ninjutsu-family activations
            // per CR 702.49 — notably Sneak, which is restricted to this step).
            // A phase stop on Declare Blockers overrides this even without an
            // auto-pass session: if the player explicitly asked to pause here,
            // honor it.
            WaitingFor::DeclareBlockers {
                player,
                valid_blocker_ids,
                ..
            } if !state.phase_stop_hit(*player)
                && (valid_blocker_ids.is_empty()
                    || !super::combat::has_attackers_in_play(state)) =>
            {
                let mut events = Vec::new();
                match engine_combat::handle_empty_blockers(state, *player, &mut events) {
                    Ok(wf) => {
                        sync_waiting_for(state, &wf);
                        result.events.extend(events);
                        result.waiting_for = wf;
                    }
                    Err(_) => break,
                }
            }

            // Non-auto-passable WaitingFor (interactive choice, game over, etc.)
            _ => break,
        }
    }
}

/// CR 732.2: settle a runaway mandatory cascade gracefully. Pauses resolution,
/// returns priority to the active player, and emits a non-fatal `ResolutionHalted`
/// log event so the UI/log explains why the cascade stopped. Reached three ways:
/// the event-growth ceiling, the object-growth ceiling, and iteration-cap
/// exhaustion. NOT a draw — a net-progress loop is a CR 732.2 shortcut the engine
/// cannot infer an iteration count for; a *repeating* state is a separate CR
/// 104.4b draw.
fn emit_resolution_halt(state: &mut GameState, result: &mut ActionResult) {
    // Diagnostic-only: the in-flight cascade's distinct stack-source ids.
    let mut involved: Vec<ObjectId> = state.stack.iter().map(|e| e.source_id).collect();
    involved.sort_unstable_by_key(|id| id.0);
    involved.dedup();
    result.events.push(GameEvent::ResolutionHalted { involved });

    priority::reset_priority(state);
    let wf = WaitingFor::Priority {
        player: state.active_player,
    };
    state.waiting_for = wf.clone();
    result.waiting_for = wf;
}

/// CR 707.10c: Finalize a `CopyRetarget` flow — write the slot-derived targets
/// back onto the copy's stack entry, emit `EffectResolved`, hand priority back
/// to the chooser, and drain any pending continuation queued during resolution.
fn finalize_copy_retarget(
    state: &mut GameState,
    player: PlayerId,
    copy_id: ObjectId,
    slots: &[crate::types::game_state::CopyTargetSlot],
    effect_kind: crate::types::ability::EffectKind,
    effect_source_id: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let paradigm_remaining_offers = match &state.waiting_for {
        WaitingFor::CopyRetarget {
            paradigm_remaining_offers,
            ..
        } => paradigm_remaining_offers.clone(),
        _ => None,
    };
    let targets: Vec<_> = slots
        .iter()
        .map(|slot| {
            slot.current.clone().ok_or_else(|| {
                EngineError::InvalidAction(
                    "Copy target selection has an unchosen target slot".to_string(),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(entry) = state.stack.iter_mut().find(|e| e.id == copy_id) {
        if let Some(ability) = entry.ability_mut() {
            ability.targets = targets;
        }
    }
    events.push(GameEvent::EffectResolved {
        kind: effect_kind,
        // Pre-metadata CopyRetarget saves omitted this field; those states were
        // generic copy-spell choices whose completion source is the copy.
        source_id: effect_source_id.unwrap_or(copy_id),
        subject: None,
    });
    // CR 707.10c + CR 603.2: Copy observers (Magecraft) must drain only after
    // the copy's targets are finalized, not while `CopyRetarget` is still open.
    if let Some(wf) =
        triggers::drain_deferred_triggers_after_stack_object_announcement(state, events)
    {
        if let Some(remaining) = paradigm_remaining_offers.filter(|offers| !offers.is_empty()) {
            effects::paradigm::stash_pending_remaining_offers(state, player, remaining);
        }
        state.waiting_for = wf;
        state.priority_player = player;
        effects::drain_pending_continuation(state, events);
        return Ok(());
    }
    state.waiting_for = if let Some(remaining) = paradigm_remaining_offers {
        effects::paradigm::waiting_after_remaining_offers(player, remaining)
    } else {
        WaitingFor::Priority { player }
    };
    state.priority_player = player;
    effects::drain_pending_continuation(state, events);
    Ok(())
}

fn apply_action(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
    stack_resolution_limit: Option<u32>,
) -> Result<ActionResult, EngineError> {
    // Clear stale revealed_cards from the previous action.
    // RevealTop reveals (e.g. Goblin Guide) are momentary — shown for one state update.
    // RevealHand reveals (e.g. Thoughtseize) persist through the RevealChoice interaction.
    // ManifestDread reveals persist through ManifestDreadChoice (cards come from WaitingFor).
    // CR 701.20b: DigChoice reveals (reveal-dig, e.g. Satyr Wayfinder) persist through
    // the selection — revealed cards remain public while the player chooses.
    if !matches!(
        state.waiting_for,
        WaitingFor::RevealChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::DigChoice { .. }
            // CR 700.3 + CR 701.20a: Fact or Fiction reveals persist through
            // both the opponent's partition step and the controller's pile
            // choice — the cards remain public while both players interact.
            | WaitingFor::SeparatePilesChooseOpponent { .. }
            | WaitingFor::SeparatePilesPartition { .. }
            | WaitingFor::SeparatePilesChoice { .. }
    ) {
        state.revealed_cards.clear();
    }

    // CR 701.20e: A bare "look at the top card" peek is visible to the looker
    // only until they act on it. The peek window must survive the action that
    // serves the dependent "you may reveal that card" optional (the looked-at
    // card is shown while that `OptionalEffectChoice` is pending) and any
    // `RevealChoice` opened by a private look-at-hand, then clear on the next
    // action boundary — mirroring the momentary `revealed_cards` reveal.
    if !matches!(
        state.waiting_for,
        WaitingFor::OptionalEffectChoice { .. } | WaitingFor::RevealChoice { .. }
    ) {
        state.private_look_ids.clear();
        state.private_look_player = None;
    }

    let mut events = Vec::new();
    let mut triggers_processed_inline = false;
    let mut skip_deferred_trigger_drain = false;

    // CancelAutoPass works from any WaitingFor state (player may cancel during
    // interactive choices). Routed by `actor` — previously used
    // `authorized_submitter(state)`, which silently cancelled the wrong player's
    // session when fired while an opponent held the prompt.
    if matches!(action, GameAction::CancelAutoPass) {
        state.auto_pass.remove(&actor);
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // SetPhaseStops propagates the player's phase-stop preference. Pure preference
    // state — no game logic, no WaitingFor transition. Works from any state so
    // frontends can sync on preference changes regardless of the current prompt.
    // Routed by `actor` so the human can update their own stops while the AI
    // holds priority (the previous "authorized_submitter" lookup rejected this
    // outright via the WrongPlayer guard, surfacing as an in-game dispatch error).
    if let GameAction::SetPhaseStops { stops } = &action {
        if stops.is_empty() {
            state.phase_stops.remove(&actor);
        } else {
            state.phase_stops.insert(actor, stops.clone());
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 117.3d: SetPriorityYield propagates the actor's standing priority-yield
    // preference — a pre-committed decision to pass priority while a class of
    // triggered ability resolves. Pure preference state, routed by `actor`, and
    // handled BEFORE the loop-ring clear and auto-pass session clearing below so
    // yields are exempt from that per-session teardown (CR 400.7: an `Add`
    // snapshots the source's latched identity from the on-stack trigger).
    if let GameAction::SetPriorityYield { op } = &action {
        match op {
            PriorityYieldOp::Add { source_id, scope } => {
                if let Some(target) = state.resolve_yield_target_from_stack(*source_id, *scope) {
                    state.add_priority_yield(actor, target);
                }
            }
            PriorityYieldOp::Remove { target } => {
                state.remove_priority_yield(actor, target);
            }
            PriorityYieldOp::ClearAll => {
                state.clear_priority_yields(actor);
            }
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 603.5: SetMayTriggerAutoChoice propagates the actor's stored "don't ask
    // again" auto-choices for optional ("may") triggers. Pure preference state,
    // routed by `actor`, and — like SetPriorityYield — handled before the
    // loop-ring / auto-pass teardown so it is a legal any-state mutation. Actor
    // scoping is enforced by overriding the key's player with `actor`, so a
    // player can only mutate their own preferences regardless of the payload.
    if let GameAction::SetMayTriggerAutoChoice { op } = &action {
        match op {
            MayTriggerAutoChoiceOp::Remove { key } => {
                let actor_key = MayTriggerAutoChoiceKey {
                    player: actor,
                    ..*key
                };
                state.remove_may_trigger_auto_choice(&actor_key);
            }
            MayTriggerAutoChoiceOp::ClearAll => {
                state.clear_may_trigger_auto_choices(actor);
            }
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 603.3b: Preferences are written only by a live `OrderTriggers` response.
    // This public action can only forget the actor's saved preferences and remains a
    // legal any-state, actor-scoped preference action.
    if let GameAction::SetTriggerOrderTemplate { op } = &action {
        match op {
            TriggerOrderTemplateOp::ClearAll => {
                state.clear_trigger_order_templates(actor);
            }
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 402.3: Hand order has no game-rules significance — ReorderHand is a
    // display-preference update on the actor's own hand. Validated as a strict
    // permutation of the current hand and applied with no event emission, no
    // WaitingFor transition, and no auto-pass / lands-tapped clearing. Mirrors
    // the SetPhaseStops / CancelAutoPass pattern: any-state, routed by `actor`.
    if let GameAction::ReorderHand { order } = &action {
        // Canonical accessor in this crate is direct indexing — see
        // `state.players[player.0 as usize]` throughout `ai_support/candidates.rs`,
        // `game/companion.rs`, and the existing test module. Bounds-check via
        // `len()` rather than swapping to `.get_mut()`, to stay idiomatic with
        // the rest of the file.
        if (actor.0 as usize) >= state.players.len() {
            return Err(EngineError::InvalidAction(format!(
                "ReorderHand: actor {:?} is not a valid player index",
                actor
            )));
        }
        let player = &mut state.players[actor.0 as usize];

        if order.len() != player.hand.len() {
            return Err(EngineError::InvalidAction(format!(
                "ReorderHand: expected {} ids, got {}",
                player.hand.len(),
                order.len()
            )));
        }

        // Permutation check: same multiset. Sort copies and compare — O(n log n)
        // is fine for hand sizes (typically <= 7, capped well under any realistic
        // limit by CR 402.2 and our zone semantics). ObjectId is not Ord, so
        // sort by the inner u64 key directly.
        let mut current: Vec<ObjectId> = player.hand.iter().copied().collect();
        let mut requested = order.clone();
        current.sort_unstable_by_key(|id| id.0);
        requested.sort_unstable_by_key(|id| id.0);
        if current != requested {
            return Err(EngineError::InvalidAction(
                "ReorderHand: order is not a permutation of the current hand".into(),
            ));
        }

        player.hand = order.iter().copied().collect();

        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 104.3a: A player may concede at any time. Concede bypasses the WaitingFor
    // dispatch entirely — there is no priority/state check. Eliminating the player
    // performs CR 800.4a object cleanup and advances `waiting_for` if the conceder
    // owned it (see `eliminate_player`).
    if let GameAction::Concede { player_id } = action {
        let mut events = Vec::new();
        super::elimination::eliminate_player(state, player_id, &mut events);
        return Ok(ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // Debug actions bypass WaitingFor dispatch — gated on debug_mode flag
    // (engine-level: the action runs) and `debug_permitted` (transport-level:
    // the player may submit). The transport layer (server-core / WASM) is
    // responsible for enforcing per-player permission; this engine check is
    // a defense-in-depth invariant — a player not in `debug_permitted` should
    // never have reached `apply`.
    if let GameAction::Debug(debug_action) = action {
        if !state.debug_mode {
            return Err(EngineError::InvalidAction(
                "Debug actions require debug_mode to be enabled".into(),
            ));
        }
        if !state.debug_permitted.is_empty() && !state.debug_permitted.contains(&actor) {
            return Err(EngineError::InvalidAction(
                "Debug actions require debug permission".into(),
            ));
        }
        let description = debug_action.describe(state);
        let mut result =
            super::engine_debug::apply_debug_action(state, actor, debug_action, &mut events)?;
        result
            .events
            .push(crate::types::events::GameEvent::DebugActionUsed {
                player_id: actor,
                description,
            });
        return Ok(result);
    }

    // Sandbox host-only grant/revoke of debug permission. server-core also
    // checks this at the transport boundary; the engine repeats the check as
    // defense-in-depth so WASM and P2P-host paths cannot be bypassed by a
    // malicious actor crafting the action shape directly. The host convention
    // (PlayerId(0)) is fixed across every transport — see
    // `crates/server-core/src/session.rs` `HOST_PLAYER`. Emits a public audit
    // event on success.
    const HOST_PLAYER: PlayerId = PlayerId(0);
    if matches!(
        action,
        GameAction::GrantDebugPermission { .. } | GameAction::RevokeDebugPermission { .. }
    ) {
        if !state.format_config.allow_debug_actions {
            return Err(EngineError::ActionNotAllowed(
                "Sandbox mode is not enabled for this game".to_string(),
            ));
        }
        if actor != HOST_PLAYER {
            return Err(EngineError::ActionNotAllowed(
                "Only the host can grant or revoke debug permission".to_string(),
            ));
        }
        if let GameAction::RevokeDebugPermission { player_id } = action {
            if player_id == HOST_PLAYER {
                return Err(EngineError::ActionNotAllowed(
                    "The host cannot revoke their own debug permission".to_string(),
                ));
            }
        }
    }
    if let GameAction::GrantDebugPermission { player_id } = action {
        state.debug_permitted.insert(player_id);
        events.push(crate::types::events::GameEvent::DebugPermissionGranted {
            host: actor,
            player_id,
        });
        return Ok(ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }
    if let GameAction::RevokeDebugPermission { player_id } = action {
        state.debug_permitted.remove(&player_id);
        events.push(crate::types::events::GameEvent::DebugPermissionRevoked {
            host: actor,
            player_id,
        });
        return Ok(ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // PR-3 (Option C): CR 732.2a loop-detection ring invalidation. Any deliberate
    // non-pass action (cast / activate / play-land) breaks a self-refilling mandatory
    // cascade, so the accumulated detection window is stale and must be dropped.
    // Placed AFTER every preference early-return (CancelAutoPass / SetPhaseStops /
    // ReorderHand / Debug / Grant- & RevokeDebugPermission) so a no-op preference
    // toggle never reaches here; PassPriority and OrderTriggers are the only actions
    // that CONTINUE a cascade and so must NOT clear (see the CR 603.3b note below).
    // `run_auto_pass_loop` and `resolve_all_fast_forward`
    // call the resolution seam directly (not via `apply_action`), so this clear does
    // not fire during their internal iterations — the ring accumulates correctly there.
    //
    // CR 603.3b + CR 732.2a: PassPriority AND OrderTriggers both CONTINUE a mandatory
    // cascade (OrderTriggers is the forced CR 603.3b placement of simultaneous triggers,
    // not a deliberate action). Every other action (cast/activate/play-land) is a
    // deliberate break and still invalidates the ring.
    if !matches!(
        action,
        GameAction::PassPriority | GameAction::OrderTriggers { .. }
    ) {
        state.loop_detect_ring.clear();
    }

    // Keep the semantic owner of the prompt before reducing it. Under turn
    // control this can differ from the authenticated submitter; a successful
    // action discharges a shortened shortcut only for that owner.
    let semantic_actor = state.waiting_for.acting_player().unwrap_or(actor);
    let action_for_divergence = action.clone();

    // Any deliberate player action (not auto-pass-related or a simple pass) cancels their auto-pass.
    // CR 103.5: Use the authenticated `actor` directly so the simultaneous mulligan
    // variants (where `authorized_submitter` is None when multiple players are pending)
    // still clear per-actor side-effect state correctly.
    match &action {
        GameAction::SetAutoPass { .. }
        | GameAction::PassPriority
        | GameAction::ReorderHand { .. } => {}
        _ => {
            state.auto_pass.remove(&actor);
        }
    }

    // Clear manual mana-tap tracking when the player commits to a non-mana action.
    // ActivateAbility is handled per-arm (only non-mana abilities clear tracking).
    match &action {
        GameAction::PassPriority
        | GameAction::PlayLand { .. }
        | GameAction::CastSpell { .. }
        | GameAction::Foretell { .. }
        | GameAction::CastSpellAsSneak { .. }
        | GameAction::CastSpellAsWebSlinging { .. }
        | GameAction::CastSpellForFree { .. }
        | GameAction::CastSpellAsMiracle { .. }
        | GameAction::CastSpellAsMadness { .. }
        | GameAction::CancelCast
        | GameAction::UnlockRoomDoor { .. }
        | GameAction::RollPlanarDie
        | GameAction::PayUnlessCost { .. }
        | GameAction::PayCombatTax { .. } => {
            state.lands_tapped_for_mana.remove(&actor);
        }
        _ => {}
    }

    // Validate and process action against current WaitingFor
    let waiting_for = match (&state.waiting_for.clone(), action) {
        (WaitingFor::Priority { player }, GameAction::PassPriority) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if super::precast_copy_shortcut::blocks_pass(state, *player) {
                return Err(EngineError::ActionNotAllowed(
                    "A shortened pre-cast shortcut requires a different meaningful action before passing"
                        .to_string(),
                ));
            }
            let wf = pass_priority_once_with_pipeline(state, &mut events, stack_resolution_limit)?;
            return Ok(ActionResult {
                events,
                waiting_for: wf,
                log_entries: vec![],
            });
        }
        (WaitingFor::Priority { player }, GameAction::PlayLand { object_id, card_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            state.cancelled_casts.clear();
            // CR 116.2a: Playing a land is a special action — sorcery-speed, once per turn, stack must be empty.
            // CR 305.2: Playing a land is a special action, not a spell.
            handle_play_land(state, *player, object_id, card_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::TapLandForMana { object_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            handle_tap_land_for_mana(state, *player, object_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::UntapLandForMana { object_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            handle_untap_land_for_mana(state, state.priority_player, object_id, &mut events)?;
            WaitingFor::Priority { player: *player }
        }
        (
            WaitingFor::Priority { player },
            GameAction::CastSpell {
                object_id,
                card_id,
                payment_mode,
                ..
            },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_cast_spell_with_payment_mode(
                state,
                *player,
                object_id,
                card_id,
                payment_mode,
                &mut events,
            )?
        }
        (WaitingFor::Priority { player }, GameAction::Foretell { object_id, card_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_foretell(state, *player, object_id, card_id, &mut events)?
        }
        // CR 602.1: Activated abilities have a cost and an effect, written as "[Cost]: [Effect.]"
        (
            WaitingFor::Priority { player },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            // Check if this is a mana ability -- resolve instantly without the stack
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                // CR 605.3b: Mana abilities resolve immediately without using the stack.
                let ability_def = obj.abilities[ability_index].clone();
                let is_land = obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Land);
                let wf = mana_abilities::activate_mana_ability(
                    state,
                    source_id,
                    *player,
                    ability_index,
                    &ability_def,
                    &mut events,
                    crate::types::game_state::ManaAbilityResume::Priority,
                    None,
                )?;
                // CR 605.3b: Track land mana taps for undo (UntapLandForMana),
                // matching the TapLandForMana path so dual lands are undoable
                // too. `ManaSourcePenalty::None` is the only variant that
                // allows undo — painlands (damage on resolution), pay-life
                // sources, and sacrifice sources all commit irreversible
                // state atomically with CR 605.3b resolution.
                if is_land && mana_sources::mana_ability_penalty(&ability_def).is_undoable() {
                    state
                        .lands_tapped_for_mana
                        .entry(state.priority_player)
                        .or_default()
                        .push(source_id);
                }
                wf
            } else if obj.loyalty.is_some()
                && ability_index < obj.abilities.len()
                && matches!(
                    obj.abilities[ability_index].cost,
                    Some(crate::types::ability::AbilityCost::Loyalty { .. })
                )
            {
                // CR 606.3: Loyalty abilities activate once per turn at sorcery speed.
                state.lands_tapped_for_mana.remove(player);
                planeswalker::handle_activate_loyalty(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?
            } else {
                // Non-mana activated ability — clear tracking
                state.lands_tapped_for_mana.remove(player);
                casting::handle_activate_ability(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?
            }
        }
        (WaitingFor::Priority { player }, GameAction::UnlockRoomDoor { object_id, door }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            handle_unlock_room_door(state, *player, object_id, door, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::RollPlanarDie) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            // CR 901.9 / CR 116.2i: Rolling the planar die as a special action
            // does not use the stack; the escalating cost is charged before the
            // roll and effect-caused rolls do not increment the counter.
            crate::game::planechase::take_paid_planar_die_action(state, *player, &mut events)?;
            WaitingFor::Priority { player: *player }
        }
        // CR 715.3a: Player chooses creature or Adventure face.
        (
            WaitingFor::CastOffer {
                player,
                kind:
                    CastOfferKind::Adventure {
                        object_id,
                        card_id,
                        payment_mode,
                    },
            },
            GameAction::ChooseAdventureFace { creature },
        ) => casting::handle_adventure_choice_with_payment_mode(
            state,
            *player,
            *object_id,
            *card_id,
            creature,
            *payment_mode,
            &mut events,
        )?,
        // CR 712.12 (land face) / CR 712.11b (spell face): Player chooses which
        // face of an MDFC to play (land) or cast (spell).
        (
            WaitingFor::ModalFaceChoice {
                player,
                object_id,
                card_id,
                payment_mode,
            },
            GameAction::ChooseModalFace { back_face },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if let Some(obj) = state.objects.get_mut(object_id) {
                if back_face {
                    // Swap to back face using existing primitives
                    let back = obj.back_face.take().expect("dual-faced card has back face");
                    let front_snapshot = super::printed_cards::snapshot_object_face(obj);
                    super::printed_cards::apply_back_face_to_object(obj, back);
                    obj.back_face = Some(front_snapshot);
                    // CR 712.8a (MDFC) / CR 709.3 (split): non-front face showing;
                    // `apply_zone_exit_cleanup` reverts when leaving the stack.
                    obj.modal_back_face = true;
                } else {
                    // Front face chosen — clear layout_kind so the intercept
                    // won't re-fire on re-entry into handle_play_land / handle_cast_spell.
                    if let Some(ref mut bf) = obj.back_face {
                        bf.layout_kind = None;
                    }
                }
                // After choosing either face, clear layout on the stashed other
                // half so cast/play re-entry does not re-prompt.
                if back_face {
                    if let Some(ref mut bf) = obj.back_face {
                        bf.layout_kind = None;
                    }
                }
            }
            // CR 712.12 / CR 712.11b: Route the re-entry by the now-active face's
            // type. A land face is put onto the battlefield via the play-land
            // special action (CR 712.12); a spell face is cast (CR 712.11b — Esika
            // // The Prismatic Bridge). After a swap
            // the new back_face (from snapshot_object_face) has layout_kind: None,
            // and a front-face choice clears it explicitly — so neither the
            // both-faces-land intercept nor the spell-face intercept re-fires.
            let active_is_land = state.objects.get(object_id).is_some_and(|obj| {
                obj.card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Land)
            });
            if active_is_land {
                handle_play_land(state, *player, *object_id, *card_id, &mut events)?
            } else {
                casting::handle_cast_spell_with_payment_mode(
                    state,
                    *player,
                    *object_id,
                    *card_id,
                    *payment_mode,
                    &mut events,
                )?
            }
        }
        // CR 118.9: Player chooses between the printed mana cost and the
        // keyword-granted alternative cost. The `keyword` axis on the waiting
        // state drives dispatch to the per-keyword post-payment handler
        // (CR 702.74a Evoke, CR 702.96a Overload, CR 702.103a Bestow,
        // CR 702.148a Cleave, custom Warp). Each keyword retains its own
        // resolver because post-payment semantics genuinely diverge — the
        // unification is purely at the player-decision layer.
        (
            WaitingFor::AlternativeCastChoice {
                player,
                object_id,
                card_id,
                payment_mode,
                keyword,
                ..
            },
            GameAction::ChooseAlternativeCast { choice },
        ) => {
            use crate::types::game_state::AlternativeCastKeyword;
            match keyword {
                AlternativeCastKeyword::Warp => casting::handle_warp_cost_choice_with_payment_mode(
                    state,
                    *player,
                    *object_id,
                    *card_id,
                    choice,
                    *payment_mode,
                    &mut events,
                )?,
                AlternativeCastKeyword::Evoke => {
                    casting::handle_evoke_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Emerge => {
                    casting::handle_emerge_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Dash => {
                    casting::handle_dash_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Blitz => {
                    casting::handle_blitz_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Spectacle => {
                    casting::handle_spectacle_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Prowl => {
                    casting::handle_prowl_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Overload => {
                    casting::handle_overload_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Bestow => {
                    casting::handle_bestow_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Awaken => {
                    casting::handle_awaken_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Mutate => {
                    // CR 702.140a: Handle the mutate alternative cost choice.
                    casting::handle_mutate_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Cleave => {
                    casting::handle_cleave_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::MoreThanMeetsTheEye => {
                    casting::handle_mtmte_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Impending => {
                    // CR 702.176a: Handle the impending alternative cost choice during casting.
                    casting::handle_impending_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Prototype => {
                    // CR 702.160a: Handle the prototype alternative cost choice during casting.
                    casting::handle_prototype_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::FaceDown => {
                    // CR 702.37c / CR 702.168b: Handle the "cast normally vs cast
                    // face down for {3}" choice for a Morph/Megamorph/Disguise card.
                    casting::handle_face_down_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
            }
        }
        (
            WaitingFor::CastingVariantChoice {
                player,
                object_id,
                card_id,
                payment_mode,
                options,
            },
            GameAction::ChooseCastingVariant { index },
        ) => casting::handle_casting_variant_choice_with_payment_mode(
            state,
            *player,
            *object_id,
            *card_id,
            options,
            index,
            *payment_mode,
            &mut events,
        )?,
        // CR 110.4: Player chose which permanent type slot to consume for a
        // multi-type graveyard cast via OncePerTurnPerPermanentType (Muldrotha).
        (
            WaitingFor::ChoosePermanentTypeSlot {
                player,
                object_id,
                card_id,
                source,
                payment_mode,
                ..
            },
            GameAction::ChoosePermanentTypeSlot { slot },
        ) => {
            let is_land_play = slot == crate::types::card_type::CoreType::Land;
            if is_land_play {
                state.pending_permanent_type_slot = Some((*source, slot));
                handle_play_land(state, *player, *object_id, *card_id, &mut events)?
            } else {
                casting::handle_permanent_type_slot_choice_with_payment_mode(
                    state,
                    *player,
                    *object_id,
                    *card_id,
                    *source,
                    slot,
                    *payment_mode,
                    &mut events,
                )?
            }
        }
        // CR 110.4: Cancel during slot choice — return to priority.
        (WaitingFor::ChoosePermanentTypeSlot { player, .. }, GameAction::CancelCast) => {
            WaitingFor::Priority { player: *player }
        }
        (WaitingFor::ModeChoice { player, .. }, GameAction::SelectModes { indices }) => {
            casting::handle_select_modes(state, *player, indices, &mut events)?
        }
        (
            WaitingFor::ModeChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        (WaitingFor::TargetSelection { player, .. }, GameAction::SelectTargets { targets }) => {
            engine_casting::handle_target_selection_select_targets(
                state,
                *player,
                targets,
                &mut events,
            )?
        }
        (WaitingFor::TargetSelection { player, .. }, GameAction::ChooseTarget { target }) => {
            engine_casting::handle_target_selection_choose_target(
                state,
                *player,
                target,
                &mut events,
            )?
        }
        (
            WaitingFor::TargetSelection {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        (
            WaitingFor::OptionalCostChoice {
                player,
                cost,
                pending_cast,
                ..
            },
            GameAction::DecideOptionalCost { pay },
        ) => engine_casting::handle_optional_cost_choice(
            state,
            *player,
            *pending_cast.clone(),
            cost,
            pay,
            &mut events,
        )?,
        (
            WaitingFor::OptionalCostChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 702.47a–e: Splice — caster reveals a card to splice onto the spell
        // (re-offering for the rest), or declines to finish and proceed to targets.
        (
            WaitingFor::SpliceOffer {
                player,
                pending_cast,
                eligible,
            },
            GameAction::RespondToSpliceOffer { card },
        ) => splice::resolve_offer(
            state,
            *player,
            *pending_cast.clone(),
            eligible.clone(),
            card,
            &mut events,
        )?,
        (
            WaitingFor::SpliceOffer {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 601.2b: Defiler cycle — player decides whether to pay life for mana reduction.
        (
            WaitingFor::DefilerPayment {
                player,
                life_cost,
                mana_reduction,
                pending_cast,
            },
            GameAction::DecideOptionalCost { pay },
        ) => engine_casting::handle_defiler_payment(
            state,
            *player,
            *pending_cast.clone(),
            *life_cost,
            mana_reduction,
            pay,
            &mut events,
        )?,
        (
            WaitingFor::DefilerPayment {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 118.3 + CR 601.2b + CR 605.3b: Player selected objects to pay a
        // cost. The single `PayCost` state dispatches on `kind` (which action)
        // and `resume` (spell-cast vs mana-ability pipeline) to the
        // appropriate authoritative handler.
        (
            WaitingFor::PayCost {
                player,
                kind:
                    PayCostKind::RemoveCounter {
                        counter_type,
                        count: counter_count,
                        selection,
                    },
                choices,
                resume,
                ..
            },
            GameAction::ChooseRemoveCounterCostDistribution { distribution },
        ) => match resume {
            CostResume::Spell {
                spell: pending_cast,
            }
            | CostResume::SpellCost {
                spell: pending_cast,
                ..
            } => {
                casting_costs::handle_remove_counter_distribution_for_cost(
                    state,
                    *player,
                    *pending_cast.clone(),
                    *counter_count,
                    counter_type.clone(),
                    *selection,
                    choices,
                    &distribution,
                    &mut events,
                )?
            }
            CostResume::ManaAbility {
                ..
            } => {
                return Err(EngineError::InvalidAction(
                    "Counter-cost distribution is not valid for mana abilities".to_string(),
                ));
            }
        },
        (
            WaitingFor::PayCost {
                player,
                kind,
                choices,
                count,
                min_count,
                resume,
            },
            GameAction::SelectCards { cards: chosen },
        ) => match resume {
            CostResume::Spell {
                spell: pending_cast,
            }
            | CostResume::SpellCost {
                spell: pending_cast,
                ..
            } => {
                let paid_cost = match resume {
                    CostResume::SpellCost { cost, source, .. } => {
                        Some(casting_costs::SpellCostPayment {
                            cost: cost.as_ref(),
                            source: *source,
                        })
                    }
                    _ => None,
                };
                match kind {
                PayCostKind::Discard => engine_casting::handle_discard_for_cost(
                    state,
                    *player,
                    *pending_cast.clone(),
                    *count,
                    choices,
                    &chosen,
                    &mut events,
                )?,
	                PayCostKind::Sacrifice => engine_casting::handle_sacrifice_for_cost(
	                    state,
	                    *player,
	                    *pending_cast.clone(),
	                    paid_cost,
	                    casting_costs::CostSelection {
	                        min_count: *min_count,
	                        count: *count,
	                        legal_permanents: choices,
	                        chosen: &chosen,
	                    },
	                    &mut events,
	                )?,
                PayCostKind::ReturnToHand => engine_casting::handle_return_to_hand_for_cost(
                    state,
                    *player,
                    *pending_cast.clone(),
                    *count,
                    choices,
                    &chosen,
                    &mut events,
                )?,
                PayCostKind::ExileFromZone { zone } => engine_casting::handle_exile_for_cost(
                    state,
                    *player,
                    *zone,
                    *pending_cast.clone(),
                    *count,
                    choices,
                    &chosen,
                    &mut events,
                )?,
                // CR 601.2h + CR 701.13: Exile a battlefield permanent the player
                // controls as an additional/alternative cost (Food Chain class).
                PayCostKind::ExilePermanent { filter } => {
                    engine_casting::handle_exile_permanent_for_cost(
                        state,
                        *player,
                        filter.clone(),
                        *pending_cast.clone(),
                        *count,
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                // CR 701.3d + CR 608.2k: Unattach a matching attachment from the
                // source as an activation cost (Captain America's Throw). The
                // handler snapshots the detached Equipment as the cost-referent,
                // then re-surfaces the deferred damage division.
                PayCostKind::UnattachFrom { filter } => {
                    casting_costs::handle_unattach_for_cost(
                        state,
                        *player,
                        filter,
                        *pending_cast.clone(),
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                // CR 702.167a/b: Craft materials exile across the
                // battlefield/graveyard union.
                PayCostKind::ExileMaterials { materials } => {
                    engine_casting::handle_exile_materials_for_cost(
                        state,
                        *player,
                        materials.clone(),
                        *pending_cast.clone(),
                        (*min_count, *count),
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                // CR 117.1 + CR 601.2b + CR 608.2c: Aggregate-threshold "exile
                // any number" cost (Baron Helmut Zemo's Boast); the handler
                // validates the threshold, exiles, publishes the tracked set, and
                // binds the resolving ability's tracked-set sentinel to it.
                PayCostKind::ExileAggregate {
                    zone,
                    function,
                    property,
                    comparator,
                    value,
                    filter,
                } => engine_casting::handle_exile_aggregate_for_cost(
                    state,
                    *player,
                    *zone,
                    *function,
                    *property,
                    *comparator,
                    *value,
                    filter,
                    *pending_cast.clone(),
                    choices,
                    &chosen,
                    &mut events,
                )?,
                PayCostKind::RemoveCounter {
                    counter_type,
                    count: counter_count,
                    selection,
                } => {
                    casting_costs::handle_remove_counter_for_cost(
                        state,
                        *player,
                        *pending_cast.clone(),
                        *counter_count,
                        counter_type.clone(),
                        *selection,
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                PayCostKind::TapCreatures { aggregate } => {
                    engine_casting::handle_tap_creatures_for_spell_cost(
                        state,
                        *player,
                        *pending_cast.clone(),
                        *count,
                        *aggregate,
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                PayCostKind::Behold { action } => engine_casting::handle_behold_for_cost(
                    state,
                    *player,
                    *pending_cast.clone(),
                    *count,
                    choices,
                    *action,
                    &chosen,
                    &mut events,
                )?,
                // ExileFromManaZone is mana-ability-only; never appears with a
                // spell-cast resume.
                PayCostKind::ExileFromManaZone { .. } => {
                    return Err(EngineError::InvalidAction(
                        "ExileFromManaZone cost cannot resume a spell cast".into(),
                    ));
                }
                }
            }
            CostResume::ManaAbility {
                mana_ability: pending_mana_ability,
            } => match kind {
                // CR 605.1a: mana-ability tap costs are always fixed-count; the
                // aggregate form never resumes a mana ability.
                PayCostKind::TapCreatures { .. } => {
                    engine_casting::handle_tap_creatures_for_mana_ability(
                        state,
                        *count,
                        choices,
                        pending_mana_ability,
                        &chosen,
                        &mut events,
                    )?
                }
                PayCostKind::Discard => engine_casting::handle_discard_for_mana_ability(
                    state,
                    *count,
                    choices,
                    pending_mana_ability,
                    &chosen,
                    &mut events,
                )?,
                PayCostKind::ExileFromManaZone { .. } => {
                    super::mana_abilities::handle_exile_for_mana_ability(
                        state,
                        *count,
                        choices,
                        pending_mana_ability,
                        &chosen,
                        &mut events,
                    )?
                }
                PayCostKind::Sacrifice => super::mana_abilities::handle_sacrifice_for_mana_ability(
                    state,
                    *count,
                    choices,
                    pending_mana_ability,
                    &chosen,
                    &mut events,
                )?,
                // ReturnToHand, ExileFromZone, RemoveCounter, and Behold do not
                // have mana-ability cost handlers wired today. If a future mana
                // ability uses one of these CR-valid cost shapes, add the
                // corresponding mana-ability handler instead of routing it
                // through the spell pipeline.
                PayCostKind::ReturnToHand
                | PayCostKind::ExileFromZone { .. }
                | PayCostKind::ExileMaterials { .. }
                | PayCostKind::ExilePermanent { .. }
                | PayCostKind::ExileAggregate { .. }
                | PayCostKind::RemoveCounter { .. }
                // CR 701.3d: an unattach-from cost is only ever surfaced via
                // `CostResume::Spell` (targeted activation), never as a mana
                // ability — unreachable here.
                | PayCostKind::UnattachFrom { .. }
                | PayCostKind::Behold { .. } => {
                    debug_assert!(
                        !matches!(kind, PayCostKind::UnattachFrom { .. }),
                        "UnattachFrom cost cannot resume a mana ability",
                    );
                    return Err(EngineError::InvalidAction(
                        "Cost kind cannot resume a mana ability".into(),
                    ));
                }
            },
        },
        // CR 601.2: Player backed out of a cost-payment choice. Only spell
        // casts can be cancelled; mana-ability cost payment has no cancel path.
        (
            WaitingFor::PayCost {
                player,
                resume:
                    CostResume::Spell {
                        spell: pending_cast,
                    }
                    | CostResume::SpellCost {
                        spell: pending_cast,
                        ..
                    },
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 118.3: Player selected permanents to sacrifice as cost.
        (
            WaitingFor::ActivationCostOneOfChoice {
                player,
                costs,
                pending_cast,
            },
            GameAction::ChooseActivationCostBranch { index },
        ) => engine_casting::handle_activation_cost_one_of_choice(
            state,
            *player,
            *pending_cast.clone(),
            costs,
            index,
            &mut events,
        )?,
        (
            WaitingFor::ActivationCostOneOfChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 601.2b + CR 701.4a: player chose the creature type for a pre-choice
        // behold cost; record it and resume behold payment.
        (
            WaitingFor::CostTypeChoice {
                player,
                options,
                pending_cast,
                ..
            },
            GameAction::ChooseOption { choice },
        ) => casting_costs::handle_cost_type_choice(
            state,
            *player,
            *pending_cast.clone(),
            options,
            &choice,
            &mut events,
        )?,
        (
            WaitingFor::CostTypeChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // Blight: player selected creature(s) to put -1/-1 counters on as cost.
        (
            WaitingFor::BlightChoice {
                player,
                counters,
                creatures,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => casting_costs::handle_blight_choice(
            state,
            *player,
            *pending_cast.clone(),
            *counters,
            creatures,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::BlightChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        (
            WaitingFor::ChooseManaColor {
                choice, context, ..
            },
            GameAction::ChooseManaColor {
                choice: chosen,
                count,
            },
        ) => {
            let events_before = events.len();
            let wf = match context {
                crate::types::game_state::ManaChoiceContext::ManaAbility(pending_mana_ability) => {
                    // CR 605.3a: validate the requested batch size BEFORE any mana
                    // is produced, so an out-of-range count rejects cleanly with
                    // no partial application. The cap is the just-activated source
                    // plus its choice-free identical twins.
                    if count as usize > pending_mana_ability.batch_siblings.len() + 1 {
                        return Err(EngineError::InvalidAction(format!(
                            "ChooseManaColor count {count} exceeds the {} batchable sources",
                            pending_mana_ability.batch_siblings.len() + 1
                        )));
                    }
                    let wf = engine_casting::handle_choose_mana_color(
                        state,
                        pending_mana_ability,
                        choice,
                        chosen.clone(),
                        &mut events,
                    )?;
                    // CR 605.3a: one color choice may bulk-activate the player's
                    // other identical, choice-free mana sources (their remaining
                    // Treasures, etc.) with the same color. Sibling cost/mana
                    // events append before the shared trigger scan below, so each
                    // sacrifice's observers fire exactly once.
                    if count > 1 {
                        engine_casting::batch_activate_mana_siblings(
                            state,
                            pending_mana_ability,
                            &chosen,
                            count,
                            &mut events,
                        )?;
                    }
                    wf
                }
                crate::types::game_state::ManaChoiceContext::ResolvingEffect(pending_effect) => {
                    effects::mana::handle_choose_mana_effect(
                        state,
                        pending_effect,
                        choice,
                        chosen.clone(),
                        &mut events,
                    )?
                }
            };
            // CR 603.2c + CR 605.4a: A mana color choice produces mana inline.
            // Scan its events for TapsForMana mana multipliers and for
            // cost-payment triggers HERE, because for `ManaPayment` /
            // `UnlessPayment` resumes the post-action pipeline is skipped
            // (it is guarded by `matches!(waiting_for, WaitingFor::Priority)`),
            // so this is the only scan site — and CR 605.4a requires the bonus
            // mana to enter the pool before the spell's payment step continues.
            // Do NOT "simplify" this scan away for non-Priority resumes.
            if events.len() > events_before {
                let mana_events: Vec<_> = events[events_before..].to_vec();
                super::triggers::process_triggers(state, &mana_events);
            }
            // CR 603.3b (#531): if the inline trigger scan paused on an
            // OrderTriggers prompt (controller has 2+ simultaneous TapsForMana
            // multipliers, etc.), surface that prompt instead of overwriting
            // it with the resume `wf` (Priority/ManaPayment). Preserve `wf`
            // so `handle_order_triggers` can resume the interrupted chain
            // after the ordered triggered mana abilities dispatch.
            if let Some(order_wf) =
                super::triggers::preserve_order_triggers_resume(state, wf.clone())
            {
                return Ok(ActionResult {
                    events,
                    waiting_for: order_wf,
                    log_entries: vec![],
                });
            }
            // CR 603.2c: For a `Priority` resume the post-action pipeline WOULD
            // re-scan these same events, double-firing the multiplier (issue
            // #443: Delighted Halfling under a mana multiplier yields 5 not 3).
            // Claim the scan via `triggers_processed_inline` — the same
            // mechanism `DeclareAttackers` uses — so the pipeline runs SBAs,
            // delayed/state triggers, and layers but skips the trigger re-scan.
            if matches!(wf, WaitingFor::Priority { .. }) {
                triggers_processed_inline = true;
            }
            wf
        }
        // CR 605.3a + CR 601.2h + CR 107.4e: Player submits the per-hybrid-shard
        // color vector for a mana-ability mana sub-cost (filter lands, etc.).
        (
            WaitingFor::PayManaAbilityMana {
                options,
                pending_mana_ability,
                ..
            },
            GameAction::PayManaAbilityMana { payment },
        ) => engine_casting::handle_pay_mana_ability_mana(
            state,
            options,
            pending_mana_ability,
            &payment,
            &mut events,
        )?,
        (
            WaitingFor::CollectEvidenceChoice {
                player,
                minimum_mana_value,
                cards: legal_cards,
                resume,
            },
            GameAction::SelectCards { cards: chosen },
        ) => super::effects::collect_evidence::handle_choice(
            state,
            *player,
            *minimum_mana_value,
            legal_cards,
            resume,
            &chosen,
            &mut events,
        )?,
        (WaitingFor::CollectEvidenceChoice { player, resume, .. }, GameAction::CancelCast) => {
            engine_casting::handle_collect_evidence_cancel(state, *player, resume, &mut events)
        }
        // CR 702.180b: Player chose which creature to tap for harmonize cost reduction.
        // CR 601.2b: Creature is tapped as part of paying the total cost.
        (
            WaitingFor::HarmonizeTapChoice {
                player,
                eligible_creatures,
                pending_cast,
            },
            GameAction::HarmonizeTap { creature_id },
        ) => engine_casting::handle_harmonize_tap_choice(
            state,
            *player,
            eligible_creatures,
            *pending_cast.clone(),
            creature_id,
            &mut events,
        )?,
        (
            WaitingFor::HarmonizeTapChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 608.2d: Player decided whether to perform an optional effect ("You may X").
        (WaitingFor::OptionalEffectChoice { .. }, GameAction::DecideOptionalEffect { accept }) => {
            engine_payment_choices::handle_optional_effect_choice(state, accept, &mut events)?
        }
        (
            WaitingFor::PairChoice {
                player,
                source_id,
                choices,
            },
            GameAction::ChoosePair { partner },
        ) => {
            if let Some(partner_id) = partner {
                if !choices.contains(&partner_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected Soulbond partner is not legal".to_string(),
                    ));
                }
                if super::pairing::is_unpaired_creature_you_control(state, *source_id, *player)
                    && super::pairing::is_unpaired_creature_you_control(state, partner_id, *player)
                {
                    super::pairing::pair_objects(state, *source_id, partner_id, *player);
                }
            }
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::PairWith,
                source_id: *source_id,
            subject: None,});
            state.waiting_for = WaitingFor::Priority { player: *player };
            state.priority_player = *player;
            effects::drain_pending_continuation(state, &mut events);
            state.waiting_for.clone()
        }
        (
            waiting_for @ WaitingFor::OptionalEffectChoice { .. },
            GameAction::DecideOptionalEffectAndRemember { choice },
        ) => engine_payment_choices::handle_optional_effect_choice_and_remember(
            state,
            waiting_for.clone(),
            choice,
            &mut events,
        )?,
        // CR 608.2d: Opponent decided on "any opponent may" effect.
        (
            waiting_for @ WaitingFor::OpponentMayChoice { .. },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            return engine_payment_choices::handle_opponent_may_choice(
                state,
                waiting_for.clone(),
                accept,
                &mut events,
            );
        }
        // CR 732.2a: the proposer declares the loop shortcut. The offered `schema` (the
        // declared-choices contract the fail-closed firewall validates the pins against) is
        // threaded through — no longer dropped by `..`.
        (
            WaitingFor::LoopShortcut {
                proposer,
                predicted_winner,
                certificate,
                schema,
            },
            GameAction::DeclareShortcut { count, template },
        ) => {
            return handle_declare_shortcut(
                state,
                LoopShortcutOffer {
                    proposer: *proposer,
                    predicted_winner: *predicted_winner,
                    certificate,
                    schema,
                },
                count,
                template,
                &mut events,
            );
        }
        // CR 732.2a: the proposer DECLINES the offered shortcut (suggesting is optional).
        // Proposer-only authorization is enforced upstream by `check_actor_authorization`, so
        // `proposer`/`certificate`/`schema` are unused here (`..`).
        (WaitingFor::LoopShortcut { .. }, GameAction::DeclineShortcut) => {
            return handle_decline_shortcut(state, &mut events);
        }
        // The finite pre-cast protocol is intentionally isolated from the
        // legacy generic loop-shortcut handlers above.
        (
            WaitingFor::PrecastCopyShortcutOffer { .. }
            | WaitingFor::RespondToPrecastCopyShortcut { .. },
            GameAction::PrecastCopyShortcut { epoch, response },
        ) => super::precast_copy_shortcut::handle(state, actor, epoch, response, &mut events)?,
        // CR 732.2b/c: an opponent answers the loop-shortcut offer.
        (
            WaitingFor::RespondToShortcut {
                player,
                remaining_players,
                proposal,
            },
            GameAction::RespondToShortcut { response },
        ) => {
            return handle_respond_to_shortcut(
                state,
                *player,
                remaining_players.clone(),
                proposal.clone(),
                response,
                &mut events,
            );
        }
        // CR 702.104a: The chosen opponent for a Tribute creature decided pay/decline.
        (
            waiting_for @ WaitingFor::TributeChoice { .. },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            return engine_payment_choices::handle_tribute_choice(
                state,
                waiting_for.clone(),
                accept,
                &mut events,
            );
        }
        // CR 118.12: Player decided whether to pay an "unless pays" cost.
        (waiting_for @ WaitingFor::UnlessPayment { .. }, GameAction::PayUnlessCost { pay }) => {
            return engine_payment_choices::handle_unless_payment(
                state,
                waiting_for.clone(),
                pay,
                &mut events,
            );
        }
        // CR 118.12a: Player chose **which** sub-cost of a disjunctive
        // unless-cost to pay (or declined to pay any). On a `Some(idx)`
        // choice, the handler swaps the multi-cost prompt for a single-cost
        // `WaitingFor::UnlessPayment` carrying the chosen branch. On `None`
        // it falls through to the effect-happens path the same way a `pay:
        // false` answer to `PayUnlessCost` would.
        (
            waiting_for @ WaitingFor::UnlessPaymentChooseCost { .. },
            GameAction::ChooseUnlessCostBranch { choice },
        ) => {
            return engine_payment_choices::handle_unless_payment_choose_cost(
                state,
                waiting_for.clone(),
                choice,
                &mut events,
            );
        }
        // CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: Player decided whether to
        // pay the locked-in combat tax. Resumes the paused attack/block declaration
        // with the matching sanitization per the accept/decline branch.
        (
            waiting_for @ WaitingFor::CombatTaxPayment { .. },
            GameAction::PayCombatTax { accept },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_pay_combat_tax(state, waiting_for.clone(), accept, &mut events)?
        }
        // Allow mana abilities during unless-payment choice (CR 118.12)
        (
            waiting_for @ WaitingFor::UnlessPayment { .. },
            GameAction::TapLandForMana { object_id },
        ) => engine_payment_choices::handle_unless_payment_tap_land_for_mana(
            state,
            waiting_for.clone(),
            object_id,
            &mut events,
        )?,
        (
            waiting_for @ WaitingFor::UnlessPayment { .. },
            GameAction::UntapLandForMana { object_id },
        ) => engine_payment_choices::handle_unless_payment_untap_land_for_mana(
            state,
            waiting_for.clone(),
            object_id,
            &mut events,
        )?,
        // Allow mana abilities during unless-payment choice (CR 118.12)
        (
            waiting_for @ WaitingFor::UnlessPayment { .. },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => engine_payment_choices::handle_unless_payment_activate_ability(
            state,
            waiting_for.clone(),
            source_id,
            ability_index,
            &mut events,
        )?,
        // CR 702.21a: Player selected a card to discard as ward cost payment.
        (
            waiting_for @ WaitingFor::WardDiscardChoice { .. },
            GameAction::SelectCards { cards: chosen },
        ) => engine_payment_choices::handle_ward_discard_choice(
            state,
            waiting_for.clone(),
            chosen,
            &mut events,
        )?,
        // CR 702.21a: Player selected a permanent to sacrifice as ward cost payment.
        (
            waiting_for @ WaitingFor::WardSacrificeChoice { .. },
            GameAction::SelectCards { cards: chosen },
        ) => engine_payment_choices::handle_ward_sacrifice_choice(
            state,
            waiting_for.clone(),
            chosen,
            &mut events,
        )?,
        // CR 118.12: Player selected a permanent to return to hand as unless cost.
        (
            waiting_for @ WaitingFor::UnlessBounceChoice { .. },
            GameAction::SelectCards { cards: chosen },
        ) => engine_payment_choices::handle_unless_bounce_choice(
            state,
            waiting_for.clone(),
            chosen,
            &mut events,
        )?,
        (WaitingFor::ManaPayment { player, .. }, GameAction::CancelCast) => {
            // CR 601.2i: Cancelling at mana payment rolls back the cast — pop
            // the stack entry placed at announcement and return the object to
            // its origin zone via `cancel_pending_cast`.
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)
                }
                None => WaitingFor::Priority { player },
            }
        }
        (WaitingFor::ChooseXValue { player, .. }, GameAction::CancelCast) => {
            // CR 601.2f + CR 601.2i: Caster may back out before committing to an
            // X value. Pop the stack entry placed at announcement and restore.
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)
                }
                None => WaitingFor::Priority { player },
            }
        }
        (WaitingFor::ChooseXValue { .. }, GameAction::PassPriority) => {
            // CR 601.2f: X must be chosen before the cast can proceed; passing priority
            // is not a legal way to skip this step.
            return Err(EngineError::ActionNotAllowed(
                "Cannot pass priority while choosing a value for X — commit with ChooseX or CancelCast."
                    .to_string(),
            ));
        }
        // CR 107.1b + CR 601.2f: Commit the chosen X value, then advance to mana payment.
        (
            WaitingFor::ChooseXValue {
                player,
                min,
                max,
                convoke_mode,
                ..
            },
            GameAction::ChooseX { value },
        ) => {
            if value < *min {
                return Err(EngineError::InvalidAction(format!(
                    "X={value} is below the minimum legal value of {min}",
                    min = *min,
                )));
            }
            if value > *max {
                return Err(EngineError::InvalidAction(format!(
                    "X={value} exceeds the maximum legal value of {max}",
                    max = *max,
                )));
            }
            let player = *player;
            let convoke_mode = *convoke_mode;
            if let Some(pending) = state.pending_cast.as_ref() {
                if pending.deferred_target_selection {
                    // CR 601.2c: A chosen X that determines target count must
                    // have a legal target assignment before it is locked into
                    // the pending cast.
                    // CR 601.2f: The same X value then determines the total cost.
                    let mut trial = pending.as_ref().clone();
                    trial.ability.set_chosen_x_recursive(value);
                    trial.cost.concretize_x(value);
                    let mut target_slots = build_target_slots(state, &trial.ability)?;
                    // CR 601.2c + CR 601.2d: clamp a divided spell's slots to the
                    // (now-known) pool so the legal-assignment probe matches what
                    // the controller will actually be offered (issue #2856).
                    cap_distribution_target_slots(
                        state,
                        &trial.ability,
                        trial.distribute.as_ref(),
                        &mut target_slots,
                    );
                    if !target_slots.is_empty()
                        && !has_legal_target_assignment_for_ability(
                            state,
                            &trial.ability,
                            &target_slots,
                            &trial.target_constraints,
                        )
                    {
                        return Err(EngineError::InvalidAction(format!(
                            "X={value} has no legal target assignment"
                        )));
                    }
                }
            }
            let pending = state.pending_cast.as_mut().ok_or_else(|| {
                EngineError::InvalidAction("No pending cast awaiting X".to_string())
            })?;
            pending.ability.set_chosen_x_recursive(value);
            pending.cost.concretize_x(value);
            let object_id = pending.object_id;
            events.push(GameEvent::XValueChosen {
                player,
                object_id,
                value,
            });
            // CR 601.2b + CR 601.2f: X is now locked in. Re-derive the full
            // concrete cost from the captured base — all reductions, target-
            // dependent modifiers, and Strive re-applied, with floors (Trinisphere
            // class) run LAST — against the now-concrete total, before payment is
            // determined. (Legacy/in-flight pending casts without a captured base
            // fall back to flooring the already-concretized cost.)
            casting::apply_post_x_cost_modifiers(state, player, object_id);
            casting_costs::enter_payment_step(state, player, convoke_mode, &mut events)?
        }
        // CR 702.132a: Assist — caster chooses another player to help pay generic,
        // or declines. `assist_state` was set to `Offered` when the offer was made,
        // so both branches simply (re)enter the payment step from where they resume.
        (
            WaitingFor::AssistChoosePlayer {
                player,
                candidates,
                max_generic,
                convoke_mode,
            },
            GameAction::ChooseAssistPlayer { player: chosen },
        ) => {
            let caster = *player;
            let convoke_mode = *convoke_mode;
            match chosen {
                None => {
                    // CR 702.132a: declining proceeds to normal payment by the caster.
                    casting_costs::enter_payment_step(state, caster, convoke_mode, &mut events)?
                }
                Some(p) => {
                    if !candidates.contains(&p) {
                        return Err(EngineError::InvalidAction(format!(
                            "Player {p:?} is not an eligible assist helper"
                        )));
                    }
                    WaitingFor::AssistPayment {
                        caster,
                        chosen: p,
                        max_generic: *max_generic,
                        convoke_mode,
                    }
                }
            }
        }
        (WaitingFor::AssistChoosePlayer { player, .. }, GameAction::CancelCast) => {
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)
                }
                None => WaitingFor::Priority { player },
            }
        }
        (WaitingFor::AssistChoosePlayer { .. }, GameAction::PassPriority) => {
            return Err(EngineError::ActionNotAllowed(
                "Must choose an assisting player or decline with ChooseAssistPlayer { player: None }, or CancelCast."
                    .to_string(),
            ));
        }
        // CR 702.132a: Assist — the chosen player commits how much generic mana to
        // pay. The caster's owed generic is reduced now, and the commitment is
        // recorded on the pending cast; the helper's sources are tapped only at
        // `finalize_cast` (the non-cancellable commit), so a later CancelCast can
        // never leak the helper's lands or spent mana.
        (
            WaitingFor::AssistPayment {
                caster,
                chosen,
                max_generic,
                convoke_mode,
            },
            GameAction::CommitAssistPayment { generic },
        ) => {
            let caster = *caster;
            let chosen = *chosen;
            let max_generic = *max_generic;
            let convoke_mode = *convoke_mode;
            if generic > max_generic {
                return Err(EngineError::InvalidAction(format!(
                    "Assist contribution {generic} exceeds the maximum {max_generic}"
                )));
            }
            if generic > 0 {
                use crate::types::mana::ManaCost;
                // CR 702.132a: validate the helper can actually produce the committed
                // generic (simulated auto-tap on a clone) before reducing the
                // caster's cost. No real taps happen here — see `apply_committed_assist`.
                let probe = ManaCost::Cost {
                    shards: Vec::new(),
                    generic,
                };
                let mut sim = state.clone();
                let mut sink = Vec::new();
                casting_costs::auto_tap_mana_sources(&mut sim, chosen, &probe, &mut sink, None);
                let feasible = sim
                    .players
                    .iter()
                    .find(|p| p.id == chosen)
                    .is_some_and(|p| mana_payment::can_pay(&p.mana_pool, &probe));
                if !feasible {
                    return Err(EngineError::InvalidAction(format!(
                        "Assisting player cannot produce {generic} generic mana"
                    )));
                }
                // Reduce the caster's owed generic and record the commitment; the
                // helper actually taps/spends at finalize.
                let pending = state.pending_cast.as_mut().ok_or_else(|| {
                    EngineError::InvalidAction("No pending cast for assist".to_string())
                })?;
                if let ManaCost::Cost { generic: owed, .. } = &mut pending.cost {
                    *owed = owed.saturating_sub(generic);
                }
                pending.assist_state = AssistState::Committed {
                    helper: chosen,
                    generic,
                };
            }
            casting_costs::enter_payment_step(state, caster, convoke_mode, &mut events)?
        }
        // CR 601.2h: Player has confirmed payment — delegate to the shared finalizer
        // that both this branch and the auto-pay path in `enter_payment_step` share.
        (WaitingFor::ManaPayment { player, .. }, GameAction::PassPriority) => {
            // CR 118.3a: `finalize_mana_payment` clears `active_payment_pins`
            // itself on every Ok/Err path, so no caller clear is needed.
            casting_costs::finalize_mana_payment(state, *player, &mut events)?
        }
        // CR 107.4f + CR 601.2f + CR 601.2h: Caster submitted per-shard Phyrexian
        // choices. Validate choice count + current affordability, then resume the
        // cast via `finalize_mana_payment_with_phyrexian_choices`.
        (
            WaitingFor::PhyrexianPayment {
                player,
                spell_object,
                shards,
            },
            GameAction::SubmitPhyrexianChoices { choices },
        ) => {
            let player = *player;
            let spell_object = *spell_object;
            let expected_len = shards.len();
            if choices.len() != expected_len {
                return Err(EngineError::InvalidAction(format!(
                    "Phyrexian choice count mismatch: expected {expected_len}, got {}",
                    choices.len()
                )));
            }
            // CR 118.3: Re-validate affordability against current state — life may have
            // dropped mid-cast (e.g., a life-loss replacement fired), so `PayLife` choices
            // on shards that now show `LifeOnly`/`ManaOrLife` must still have life available.
            {
                let pending_ref = state.pending_cast.as_ref().ok_or_else(|| {
                    EngineError::InvalidAction("No pending cast for Phyrexian payment".to_string())
                })?;
                let cost = pending_ref.cost.clone();
                let player_pool = state
                    .players
                    .iter()
                    .find(|p| p.id == player)
                    .map(|p| p.mana_pool.clone())
                    .ok_or_else(|| EngineError::InvalidAction("Player not found".to_string()))?;
                let activation_ability_index = pending_ref.activation_ability_index;
                let current_shards = if let Some(ability_index) = activation_ability_index {
                    let (source_types, source_subtypes) =
                        casting::activation_source_types(state, spell_object);
                    let activation_ctx = crate::types::mana::PaymentContext::Activation {
                        source_types: &source_types,
                        source_subtypes: &source_subtypes,
                        ability_tag: casting::activation_ability_tag(
                            state,
                            spell_object,
                            ability_index,
                        ),
                    };
                    let any_color = casting::player_can_spend_as_any_color_for_payment(
                        state,
                        player,
                        Some(spell_object),
                        Some(&activation_ctx),
                    );
                    let permissions = super::static_abilities::build_cost_permission_context(
                        state, player, any_color,
                    );
                    mana_payment::compute_phyrexian_shards(
                        &player_pool,
                        &cost,
                        Some(&activation_ctx),
                        permissions,
                    )
                } else {
                    let spell_meta = casting::build_spell_meta(state, player, spell_object);
                    let spell_ctx = spell_meta
                        .as_ref()
                        .map(crate::types::mana::PaymentContext::Spell);
                    let any_color = casting::player_can_spend_as_any_color_for_payment(
                        state,
                        player,
                        Some(spell_object),
                        spell_ctx.as_ref(),
                    );
                    let permissions = super::static_abilities::build_cost_permission_context(
                        state, player, any_color,
                    );
                    mana_payment::compute_phyrexian_shards(
                        &player_pool,
                        &cost,
                        spell_ctx.as_ref(),
                        permissions,
                    )
                };
                if current_shards.len() != expected_len {
                    return Err(EngineError::ActionNotAllowed(
                        "Phyrexian shard count changed during pause".to_string(),
                    ));
                }
                for (choice, shard) in choices.iter().zip(current_shards.iter()) {
                    match (choice, shard.options) {
                        (
                            crate::types::game_state::ShardChoice::PayLife,
                            crate::types::game_state::ShardOptions::ManaOnly,
                        ) => {
                            return Err(EngineError::ActionNotAllowed(
                                "Cannot pay life for shard — only mana available".to_string(),
                            ));
                        }
                        (
                            crate::types::game_state::ShardChoice::PayMana,
                            crate::types::game_state::ShardOptions::LifeOnly,
                        ) => {
                            return Err(EngineError::ActionNotAllowed(
                                "Cannot pay mana for shard — only life available".to_string(),
                            ));
                        }
                        _ => {}
                    }
                }
            }
            // CR 118.3a: `finalize_mana_payment_with_phyrexian_choices` clears
            // `active_payment_pins` itself on every Ok/Err path; no caller clear.
            casting_costs::finalize_mana_payment_with_phyrexian_choices(
                state,
                player,
                &choices,
                &mut events,
            )?
        }
        // CR 601.2i: CancelCast during Phyrexian payment rolls back the cast —
        // mirrors the ManaPayment CancelCast path.
        (WaitingFor::PhyrexianPayment { player, .. }, GameAction::CancelCast) => {
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)
                }
                None => WaitingFor::Priority { player },
            }
        }
        // Allow mana abilities during mana payment (mid-cast)
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                let events_before = events.len();
                let ability_def = obj.abilities[ability_index].clone();
                let wf = mana_abilities::activate_mana_ability(
                    state,
                    source_id,
                    *player,
                    ability_index,
                    &ability_def,
                    &mut events,
                    crate::types::game_state::ManaAbilityResume::ManaPayment {
                        convoke_mode: *convoke_mode,
                    },
                    None,
                )?;
                // CR 605.1b: Process TapsForMana triggers inline during mana payment
                // (same rationale as the TapLandForMana arm below).
                if events.len() > events_before {
                    let mana_events: Vec<_> = events[events_before..].to_vec();
                    super::triggers::process_triggers(state, &mana_events);
                }
                if let Some(order_wf) =
                    super::triggers::preserve_order_triggers_resume(state, wf.clone())
                {
                    return Ok(ActionResult {
                        events,
                        waiting_for: order_wf,
                        log_entries: vec![],
                    });
                }
                wf
            } else {
                return Err(EngineError::ActionNotAllowed(
                    "Only mana abilities can be activated during mana payment".to_string(),
                ));
            }
        }
        // Allow basic land tapping during mana payment
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::TapLandForMana { object_id },
        ) => {
            let events_before = events.len();
            handle_tap_land_for_mana(state, *player, object_id, &mut events)?;
            state
                .lands_tapped_for_mana
                .entry(state.priority_player)
                .or_default()
                .push(object_id);
            // CR 605.1b: TapsForMana triggered mana abilities (Wild Growth, Vorinclex,
            // Fertile Ground, Mana Flare class) must resolve inline when mana is
            // produced during cost payment. The ManaPayment path does not flow through
            // run_post_action_pipeline, so process triggers explicitly here so the
            // bonus mana reaches the pool before the payment check.
            if events.len() > events_before {
                let mana_events: Vec<_> = events[events_before..].to_vec();
                super::triggers::process_triggers(state, &mana_events);
            }
            let wf = WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: *convoke_mode,
            };
            if let Some(order_wf) =
                super::triggers::preserve_order_triggers_resume(state, wf.clone())
            {
                return Ok(ActionResult {
                    events,
                    waiting_for: order_wf,
                    log_entries: vec![],
                });
            }
            wf
        }
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::UntapLandForMana { object_id },
        ) => {
            handle_untap_land_for_mana(state, state.priority_player, object_id, &mut events)?;
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: *convoke_mode,
            }
        }
        // CR 118.3a: Pin a specific pool unit so the finalize spend prefers it.
        // Immediate-stage: records the hint on `pending_cast`, no stack push.
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::SpendPoolMana { pip_id },
        ) => {
            let (player, convoke_mode) = (*player, *convoke_mode);
            handle_spend_pool_mana(state, player, pip_id)?;
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            }
        }
        // CR 118.3a: Remove a previously-recorded pin (always legal).
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::UnspendPoolMana { pip_id },
        ) => {
            let (player, convoke_mode) = (*player, *convoke_mode);
            handle_unspend_pool_mana(state, pip_id);
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            }
        }
        // CR 702.51a / Waterbend: Tap a creature or artifact to pay mana.
        // CR 702.51a + CR 302.6: Convoke taps creatures to pay mana; summoning sickness
        // (CR 302.6) is not checked because convoke does not use the tap activated-ability mechanism.
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode:
                    Some(
                        mode @ (ConvokeMode::Convoke
                        | ConvokeMode::Waterbend
                        | ConvokeMode::Improvise),
                    ),
            },
            GameAction::TapForConvoke {
                object_id,
                mana_type,
            },
        ) => {
            let mode = *mode;
            let obj = state
                .objects
                .get(&object_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            let is_eligible = match mode {
                ConvokeMode::Convoke => obj.is_convoke_eligible(*player),
                ConvokeMode::Waterbend => obj.is_waterbend_eligible(*player),
                ConvokeMode::Improvise => obj.is_improvise_eligible(*player),
                // CR 702.66a: delve has a dedicated handler arm below (exile, not tap).
                ConvokeMode::Delve => unreachable!("delve uses its own ManaPayment arm"),
            };
            if !is_eligible {
                return Err(EngineError::ActionNotAllowed(
                    "Can only tap an eligible untapped permanent you control for convoke"
                        .to_string(),
                ));
            }
            let tapped_creature_for_convoke = mode == ConvokeMode::Convoke
                && obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Creature);
            // CR 702.51a: Validate color match for Convoke.
            let resolved_mana_type = match mode {
                ConvokeMode::Convoke => {
                    if let Some(color) = mana_sources::mana_type_to_color(mana_type) {
                        // Colored mana: creature must have that color
                        if !obj.color.contains(&color) {
                            return Err(EngineError::ActionNotAllowed(format!(
                                "Creature does not have color {:?} for convoke",
                                color
                            )));
                        }
                        mana_type
                    } else {
                        // Colorless: any creature can pay generic
                        crate::types::mana::ManaType::Colorless
                    }
                }
                // Waterbend always produces colorless
                ConvokeMode::Waterbend => crate::types::mana::ManaType::Colorless,
                // CR 702.126a: Improvise pays generic mana only — always colorless.
                ConvokeMode::Improvise => crate::types::mana::ManaType::Colorless,
                ConvokeMode::Delve => unreachable!("delve uses its own ManaPayment arm"),
            };
            // CR 701.26a + CR 508.1f: route the convoke tap through the single
            // authority so a "can't become tapped" creature is refused (no
            // summoning sickness check — CR 702.51a + CR 302.6).
            crate::game::restrictions::tap_permanent_for_cost(state, object_id, &mut events)?;
            let unit = match mode {
                ConvokeMode::Convoke => {
                    crate::types::mana::ManaUnit::convoke_payment(resolved_mana_type, object_id)
                }
                ConvokeMode::Waterbend => crate::types::mana::ManaUnit::new(
                    resolved_mana_type,
                    object_id,
                    false,
                    Vec::new(),
                ),
                // CR 702.126a/b: improvise mana exists only to pay this spell's
                // generic cost — `convoke_payment` carries the restriction that
                // keeps it from leaking into the pool as real mana.
                ConvokeMode::Improvise => {
                    crate::types::mana::ManaUnit::convoke_payment(resolved_mana_type, object_id)
                }
                ConvokeMode::Delve => unreachable!("delve uses its own ManaPayment arm"),
            };
            // CR 118.3a: stamp a pip id on pool entry. Convoke/improvise markers
            // are consumed by the shared algorithm and never pinned (the frontend
            // filters ConvokePayment units); Waterbend produces real pinnable mana.
            state.add_mana_to_pool(*player, unit);
            if mode == ConvokeMode::Waterbend {
                events.push(GameEvent::ManaAdded {
                    player_id: *player,
                    mana_type: resolved_mana_type,
                    source_id: object_id,
                    tap_state: ManaTapState::NotFromTap,
                });
            }
            if tapped_creature_for_convoke {
                let pending = state.pending_cast.as_mut().ok_or_else(|| {
                    EngineError::InvalidAction("No pending cast for convoke".to_string())
                })?;
                pending.convoked_creatures.push(object_id);
            }
            // Only emit waterbend event for Waterbend mode
            if mode == ConvokeMode::Waterbend {
                crate::game::bending::record_bending(
                    state,
                    &mut events,
                    BendingType::Water,
                    object_id,
                    *player,
                );
            }
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: Some(mode),
            }
        }
        // CR 702.66a: Delve — exile a card from the caster's graveyard to pay one
        // generic mana. Unlike convoke/improvise (which tap a permanent), the
        // source is a graveyard card that is exiled. The contribution is a
        // generic-only colorless marker (like Improvise) that can't leak into the
        // pool. (Tracking which cards were exiled — for Murktide Regent's "+1/+1
        // for each card exiled with it" — is a follow-up that also needs the
        // QuantityRef/parser wiring; the core payment is independent of it.)
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode: Some(ConvokeMode::Delve),
            },
            GameAction::TapForConvoke {
                object_id,
                mana_type,
            },
        ) => {
            let player = *player;
            if mana_type != crate::types::mana::ManaType::Colorless {
                return Err(EngineError::ActionNotAllowed(
                    "Delve can only pay generic mana".to_string(),
                ));
            }
            let eligible = state
                .objects
                .get(&object_id)
                .is_some_and(|o| o.is_delve_eligible(player));
            if !eligible {
                return Err(EngineError::ActionNotAllowed(
                    "Can only delve a card from your own graveyard".to_string(),
                ));
            }
            zones::move_to_zone(state, object_id, Zone::Exile, &mut events);
            // CR 702.66a + CR 607.2a: Delved cards are exiled "with" the spell
            // being cast (Murktide Regent ETB counters — issue #1322).
            if let Some(spell_id) = state.pending_cast.as_ref().map(|p| p.object_id) {
                crate::game::exile_links::push_tracked_by_source(state, object_id, spell_id);
            }
            // CR 118.3a: route through the stamping authority (delve marker is a
            // generic-only convoke marker, never pinned).
            state.add_mana_to_pool(
                player,
                crate::types::mana::ManaUnit::convoke_payment(
                    crate::types::mana::ManaType::Colorless,
                    object_id,
                ),
            );
            WaitingFor::ManaPayment {
                player,
                convoke_mode: Some(ConvokeMode::Delve),
            }
        }
        (WaitingFor::MulliganDecision { .. }, GameAction::MulliganDecision { choice }) => {
            // CR 103.5 + 103.5b: `actor` is already authorized as a member of
            // `pending` by `check_actor_authorization`. The mulligan module
            // resolves the per-player state update, transitioning the actor's
            // entry into `BottomCards` when a declare-point action still owes
            // bottoms, or advancing the flow when the pending set is empty.
            mulligan::handle_mulligan_decision(state, actor, choice, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (WaitingFor::MulliganDecision { .. }, GameAction::SelectCards { cards }) => {
            // CR 103.5: `actor` is already authorized as a member of `pending`.
            // A `SelectCards` submission resolves that player's owed
            // `BottomCards` sub-phase (rejected if their entry is in `Declare`).
            mulligan::handle_mulligan_bottom(state, actor, cards, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (WaitingFor::OpeningHandBottomCards { .. }, GameAction::SelectCards { cards }) => {
            // TL:R 906.6a/e: `actor` is already authorized as a member of
            // `pending`; no normal mulligan actions are available in this state.
            mulligan::handle_opening_hand_bottom(state, actor, cards, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (
            WaitingFor::DeclareAttackers { player, .. },
            GameAction::DeclareAttackers { attacks, bands },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_declare_attackers(state, *player, &attacks, &bands, &mut events)?
        }
        (
            WaitingFor::DeclareBlockers { player, .. },
            GameAction::DeclareBlockers { assignments },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_declare_blockers(state, *player, &assignments, &mut events)?
        }
        (
            WaitingFor::UntapChoice {
                player,
                candidates,
                chosen_not_to_untap,
            },
            GameAction::ChooseUntap { object_id, untap },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if !candidates.contains(&object_id) {
                return Err(EngineError::InvalidAction(
                    "Invalid untap choice object".to_string(),
                ));
            }

            let remaining: Vec<ObjectId> = candidates
                .iter()
                .copied()
                .filter(|candidate| candidate != &object_id)
                .collect();
            let mut declined = chosen_not_to_untap.clone();
            if !untap {
                declined.push(object_id);
            }

            if !remaining.is_empty() {
                WaitingFor::UntapChoice {
                    player: *player,
                    candidates: remaining,
                    chosen_not_to_untap: declined,
                }
            } else {
                // CR 502.3: Declines are recorded; now either surface the
                // required bounded `ChooseUntapSubset` prompt (a MaxUntapPerType
                // cap is over its limit after declines) or untap + advance. The
                // bridge advances the phase itself when it untaps, so only
                // resume `auto_advance` when no subset prompt was raised.
                let skipped: std::collections::HashSet<ObjectId> = declined.into_iter().collect();
                match turns::begin_untap_or_subset_prompt(state, &mut events, skipped) {
                    Some(prompt) => prompt,
                    None => turns::auto_advance(state, &mut events),
                }
            }
        }
        // CR 502.3: The active player directly determines which permanents untap
        // under a MaxUntapPerType cap (Smoke / Stoic Angel / Damping Field). The
        // chosen subset (`SelectCards`) must be a subset of the prompted `group`
        // and no larger than `max`; the unchosen complement is folded into the
        // declines and held tapped. Then the untap executes and the phase
        // advances. The enforcement clamp inside `execute_untap_with_choices`
        // remains as a safety net for any selection that slips past validation.
        (
            WaitingFor::ChooseUntapSubset { player, group, max },
            GameAction::SelectCards { cards: chosen },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if chosen.len() > *max {
                return Err(EngineError::InvalidAction(format!(
                    "Untap subset selects {} permanents but the cap allows {max}",
                    chosen.len()
                )));
            }
            let chosen_set: std::collections::HashSet<ObjectId> = chosen.iter().copied().collect();
            if chosen_set.len() != chosen.len() {
                return Err(EngineError::InvalidAction(
                    "Untap subset contains duplicate permanents".to_string(),
                ));
            }
            if let Some(bad) = chosen.iter().find(|id| !group.contains(id)) {
                return Err(EngineError::InvalidAction(format!(
                    "Untap subset object {bad:?} is not in the over-cap group"
                )));
            }
            // CR 502.3: the complement of the chosen set within the prompted
            // group stays tapped. Combine with the declines stashed from the
            // preceding optional-decline prompt.
            let mut skipped: std::collections::HashSet<ObjectId> =
                std::mem::take(&mut state.pending_untap_declines)
                    .into_iter()
                    .collect();
            for id in group {
                if !chosen_set.contains(id) {
                    skipped.insert(*id);
                }
            }
            match turns::begin_untap_or_subset_prompt(state, &mut events, skipped) {
                Some(prompt) => prompt,
                None => turns::auto_advance(state, &mut events),
            }
        }
        // CR 508.1g + CR 701.43d: the active player decides whether to pay the
        // optional "exert as it attacks" cost for the prompted attacker, one
        // attacker at a time. Triggers are deferred to `finish_declare_attackers`
        // (the buffered declaration + exert events fire together), so suppress
        // the epilogue's trigger pass for every step of the loop.
        (
            WaitingFor::ExertChoice {
                player,
                attacker,
                remaining,
            },
            GameAction::ChooseExert { exert },
        ) => {
            triggers_processed_inline = true;
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if exert {
                engine_combat::apply_attack_exert(state, *attacker, &mut events);
            }
            if let Some((next, rest)) = remaining.split_first() {
                WaitingFor::ExertChoice {
                    player: *player,
                    attacker: *next,
                    remaining: rest.to_vec(),
                }
            } else if let Some(waiting_for) =
                engine_combat::next_current_enlist_choice(state, *player)
            {
                waiting_for
            } else {
                engine_combat::finish_declare_attackers(state, &mut events, false)?
            }
        }
        // CR 508.1g + CR 702.154a: the active player may tap up to one eligible
        // creature for each Enlist instance as the source attacks. As with
        // exert, declaration/tap/enlist triggers are deferred until all optional
        // attack costs are decided.
        (
            WaitingFor::EnlistChoice {
                player,
                attacker,
                eligible,
                remaining,
            },
            GameAction::ChooseEnlist { target },
        ) => {
            triggers_processed_inline = true;
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if let Some(target) = target {
                if !eligible.contains(&target) {
                    return Err(EngineError::InvalidAction(format!(
                        "{target:?} is not an eligible Enlist target"
                    )));
                }
                engine_combat::apply_attack_enlist(state, *attacker, target, &mut events)?;
            }
            if let Some(waiting_for) =
                engine_combat::next_enlist_choice(state, *player, remaining.clone())
            {
                waiting_for
            } else {
                engine_combat::finish_declare_attackers(state, &mut events, false)?
            }
        }
        (WaitingFor::ReplacementChoice { .. }, GameAction::ChooseReplacement { index }) => {
            engine_replacement::handle_replacement_choice(state, index, &mut events)?
        }
        // CR 603.3b: Player submits the chosen order for their pending triggers.
        // `actor` is already authorized as the prompted player by
        // `check_actor_authorization` (via `WaitingFor::acting_player`).
        (WaitingFor::OrderTriggers { .. }, GameAction::OrderTriggers { order }) => {
            triggers::handle_order_triggers(state, order)?
        }
        // CR 707.9: Player chose a permanent to copy for "enter as a copy of" replacement.
        (
            waiting_for @ WaitingFor::CopyTargetChoice { .. },
            GameAction::ChooseTarget { target },
        ) => engine_replacement::handle_copy_target_choice(
            state,
            waiting_for.clone(),
            target,
            &mut events,
        )?,
        (
            WaitingFor::ExploreChoice {
                player,
                remaining,
                pending_effect,
                ..
            },
            GameAction::ChooseTarget { target },
        ) => {
            if turn_control::authorized_submitter(state) != Some(*player) {
                return Err(EngineError::WrongPlayer);
            }
            let chosen = match target {
                Some(TargetRef::Object(id)) => id,
                _ => {
                    return Err(EngineError::InvalidAction(
                        "Invalid explore choice".to_string(),
                    ));
                }
            };
            super::effects::explore::handle_choice(
                state,
                chosen,
                remaining,
                pending_effect.as_ref(),
                &mut events,
            )?
        }
        // CR 303.4 + CR 303.4f + CR 303.4g + CR 115.1: Player picked the
        // permanent to enchant for a return-as-Aura sub-effect or a non-spell
        // Aura battlefield entry. The picker is a CHOICE (not a target), so
        // the action shape mirrors
        // `WaitingFor::ExploreChoice` — `GameAction::ChooseTarget` with the
        // chosen `TargetRef` drawn from `legal_targets`.
        (
            WaitingFor::ReturnAsAuraTarget {
                player,
                source_id: _,
                returned_id,
                legal_targets,
                pending_effect,
            },
            GameAction::ChooseTarget { target },
        ) => {
            if turn_control::authorized_submitter(state) != Some(*player) {
                return Err(EngineError::WrongPlayer);
            }
            let chosen = match target {
                Some(target) if legal_targets.contains(&target) => target.clone(),
                _ => {
                    return Err(EngineError::InvalidAction(
                        "ReturnAsAuraTarget: invalid or missing legal target".to_string(),
                    ));
                }
            };
            let pending = pending_effect.clone();
            let returned = *returned_id;
            let active_player = *player;
            let (filter, grants) = match &pending.effect {
                crate::types::ability::Effect::ReturnAsAura {
                    enchant_filter,
                    grants,
                } => (enchant_filter.clone(), grants.clone()),
                _ => {
                    let old_target = match chosen {
                        TargetRef::Object(chosen_id) => {
                            super::effects::attach::attach_to(state, returned, chosen_id)
                        }
                        TargetRef::Player(chosen_player) => {
                            super::effects::attach::attach_to_player(state, returned, chosen_player)
                        }
                    };
                    if let Some(old_target) = old_target {
                        events.push(crate::types::events::GameEvent::Unattached {
                            attachment_id: returned,
                            old_target,
                        });
                    }
                    let resumes_change_zone_iteration =
                        state.pending_change_zone_iteration.is_some();
                    if !resumes_change_zone_iteration {
                        events.push(crate::types::events::GameEvent::EffectResolved {
                            kind: crate::types::ability::EffectKind::ChangeZone,
                            source_id: pending.source_id,
                        subject: None,});
                    }
                    state.waiting_for = WaitingFor::Priority {
                        player: active_player,
                    };
                    state.priority_player = active_player;
                    // CR 603.10a + CR 616.1: an aura-attachment pause can carry a
                    // deferred batch completion (a reveal-until / dig kept Aura
                    // whose entry paused before the rest pile was moved). Drain it
                    // here — the replacement-choice resume path drains it for the
                    // CR 616.1 case, but the aura-host resume is the ONLY drain
                    // site for an `NeedsAuraAttachmentChoice` pause.
                    if state.pending_batch_deliveries.is_some() {
                        super::zone_pipeline::drain_pending_batch_deliveries(state, &mut events);
                    }
                    effects::drain_pending_continuation(state, &mut events);
                    return Ok(ActionResult {
                        events,
                        waiting_for: state.waiting_for.clone(),
                        log_entries: vec![],
                    });
                }
            };
            let chosen = match chosen {
                TargetRef::Object(id) => id,
                TargetRef::Player(_) => {
                    return Err(EngineError::InvalidAction(
                        "ReturnAsAuraTarget: ReturnAsAura requires an object host".to_string(),
                    ));
                }
            };
            super::effects::return_as_aura::finalize_attach(
                state,
                pending.as_ref(),
                returned,
                chosen,
                &filter,
                grants,
                &mut events,
            )
            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            // After resolving the attach, return control to standard priority
            // flow under the picker's controller, then resume any chain that was
            // paused behind the picker.
            state.waiting_for = WaitingFor::Priority {
                player: active_player,
            };
            state.priority_player = active_player;
            // CR 603.10a + CR 616.1: drain a deferred batch completion parked
            // behind this aura-attachment pause (see the sibling path above).
            if state.pending_batch_deliveries.is_some() {
                super::zone_pipeline::drain_pending_batch_deliveries(state, &mut events);
            }
            effects::drain_pending_continuation(state, &mut events);
            state.waiting_for.clone()
        }
        (
            WaitingFor::EquipTarget {
                player,
                equipment_id,
                valid_targets,
            },
            GameAction::Equip {
                equipment_id: eq_id,
                target_id,
            },
        ) => {
            if eq_id != *equipment_id {
                return Err(EngineError::InvalidAction(
                    "Equipment ID mismatch".to_string(),
                ));
            }
            if !valid_targets.contains(&target_id) {
                return Err(EngineError::InvalidAction(
                    "Invalid equip target".to_string(),
                ));
            }
            let p = *player;
            push_keyword_action(
                state,
                p,
                eq_id,
                KeywordAction::Equip {
                    equipment_id: eq_id,
                    target_creature_id: target_id,
                },
                &mut events,
            )
        }
        (WaitingFor::Priority { player }, GameAction::Equip { equipment_id, .. }) => {
            let p = *player;
            handle_equip_activation(state, p, equipment_id, &mut events)?
        }
        // CR 702.122a: Crew activation from Priority
        (WaitingFor::Priority { player }, GameAction::CrewVehicle { vehicle_id, .. }) => {
            let p = *player;
            handle_crew_activation(state, p, vehicle_id, &mut events)?
        }
        // CR 702.122a: Crew creature selection from CrewVehicle state
        (
            WaitingFor::CrewVehicle {
                player,
                vehicle_id,
                crew_power,
                eligible_creatures,
                ..
            },
            GameAction::CrewVehicle {
                vehicle_id: _vid,
                creature_ids,
            },
        ) => handle_crew_announcement(
            state,
            *player,
            *vehicle_id,
            *crew_power,
            eligible_creatures,
            &creature_ids,
            &mut events,
        )?,
        // CR 602.2b + CR 601.2h: crew's tap cost is not paid until the
        // activation payment step, so backing out before creature selection is
        // complete restores priority with no state to undo.
        (WaitingFor::CrewVehicle { player, .. }, GameAction::CancelCast) => {
            WaitingFor::Priority { player: *player }
        }
        // CR 702.184a: Station activation from Priority — enters target-selection state.
        (
            WaitingFor::Priority { player },
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        ) => {
            let p = *player;
            handle_station_activation(state, p, spacecraft_id, &mut events)?
        }
        // CR 702.184a: Station creature selection — resolves the ability.
        (
            WaitingFor::StationTarget {
                player,
                spacecraft_id,
                eligible_creatures,
            },
            GameAction::ActivateStation {
                spacecraft_id: _sid,
                creature_id: Some(cid),
            },
        ) => handle_station_announcement(
            state,
            *player,
            *spacecraft_id,
            eligible_creatures,
            cid,
            &mut events,
        )?,
        // CR 702.171a: Saddle activation from Priority — enters target-selection state.
        (WaitingFor::Priority { player }, GameAction::SaddleMount { mount_id, .. }) => {
            let p = *player;
            handle_saddle_activation(state, p, mount_id, &mut events)?
        }
        // CR 702.171a: Saddle creature selection — announces, pays cost, pushes stack entry.
        (
            WaitingFor::SaddleMount {
                player,
                mount_id,
                saddle_power,
                eligible_creatures,
                ..
            },
            GameAction::SaddleMount {
                mount_id: _mid,
                creature_ids,
            },
        ) => handle_saddle_announcement(
            state,
            *player,
            *mount_id,
            *saddle_power,
            eligible_creatures,
            &creature_ids,
            &mut events,
        )?,
        // CR 601.2c: no cost is paid until the saddle announcement, so backing out
        // restores priority with no state to undo.
        (WaitingFor::SaddleMount { player, .. }, GameAction::CancelCast) => {
            WaitingFor::Priority { player: *player }
        }
        (WaitingFor::Priority { player }, GameAction::Transform { object_id }) => {
            let p = *player;
            let obj = state
                .objects
                .get(&object_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::InvalidAction(
                    "Object is not on the battlefield".to_string(),
                ));
            }
            if obj.controller != p {
                return Err(EngineError::InvalidAction(
                    "You don't control this permanent".to_string(),
                ));
            }
            if obj.back_face.is_none() {
                return Err(EngineError::InvalidAction(
                    "Card has no back face".to_string(),
                ));
            }
            super::transform::transform_permanent(state, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        // CR 702.49: Ninjutsu-family activation during combat
        (
            WaitingFor::Priority { player },
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id,
                creature_to_return,
            },
        ) => {
            let p = *player;
            super::keywords::activate_ninjutsu(
                state,
                p,
                ninjutsu_object_id,
                creature_to_return,
                &mut events,
            )
            .map_err(EngineError::InvalidAction)?;
            // CR 707.9 + CR 614.12a: battlefield entry may park on
            // `CopyTargetChoice` (enter-as-copy) or `ReplacementChoice` (optional
            // copy / CR 616.1 ordering); preserve the surfaced prompt instead of
            // clobbering it with Priority.
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                WaitingFor::Priority { player: p }
            } else {
                state.waiting_for.clone()
            }
        }
        // CR 702.190a: Sneak — cast a spell from hand during declare blockers
        // by paying the Sneak cost and returning an unblocked attacker.
        // Applies to any card type; permanent-spell placement (CR 702.190b)
        // is handled at resolution based on the variant's `placement`.
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellAsSneak {
                hand_object,
                card_id,
                creature_to_return,
                payment_mode,
            },
        ) => super::casting::handle_cast_spell_as_sneak_with_payment_mode(
            state,
            *player,
            hand_object,
            card_id,
            creature_to_return,
            payment_mode,
            &mut events,
        )?,
        // CR 702.188a: Web-slinging — cast a spell from hand by paying the
        // Web-slinging cost and returning a tapped creature you control.
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellAsWebSlinging {
                hand_object,
                card_id,
                creature_to_return,
                payment_mode,
            },
        ) => super::casting::handle_cast_spell_as_web_slinging_with_payment_mode(
            state,
            *player,
            hand_object,
            card_id,
            creature_to_return,
            payment_mode,
            &mut events,
        )?,
        // CR 601.2b + CR 118.9a: CastFromHandFree opt-in path — cast a hand
        // spell for free via a once-per-turn permission source (Zaffai).
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellForFree {
                object_id,
                card_id,
                source_id,
                payment_mode,
            },
        ) => super::casting::handle_cast_spell_for_free_with_payment_mode(
            state,
            *player,
            object_id,
            card_id,
            source_id,
            payment_mode,
            &mut events,
        )?,
        // CR 702.94a: Miracle reveal — accept path. The player reveals the card;
        // this creates a triggered ability ("When you reveal this card this way,
        // you may cast it for [miracle cost]") that goes on the stack. Opponents
        // can respond before the cast offer resolves.
        (
            WaitingFor::MiracleReveal {
                player,
                object_id,
                cost,
            },
            GameAction::CastSpellAsMiracle {
                object_id: action_obj,
                ..
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMiracle object_id does not match the outstanding miracle reveal"
                        .to_string(),
                ));
            }
            let p = *player;
            let source = *object_id;
            let miracle_cost = cost.clone();

            // CR 702.94a: Emit the reveal event.
            // CR 702.94a: Emit the reveal event.
            let card_name = state
                .objects
                .get(&source)
                .map(|o| o.name.clone())
                .unwrap_or_default();
            events.push(crate::types::events::GameEvent::CardsRevealed {
                player: p,
                card_ids: vec![source],
                card_names: vec![card_name],
            });

            // CR 702.94a: Push the miracle triggered ability onto the stack.
            // "When you reveal this card this way, you may cast it by paying
            // [miracle cost] rather than its mana cost."
            let ability = crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::MiracleCast { cost: miracle_cost },
                vec![],
                source,
                p,
            );
            let trigger = super::triggers::PendingTrigger {
                source_id: source,
                controller: p,
                condition: None,
                ability,
                timestamp: 0,
                target_constraints: vec![],
                distribute: None,
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: Some("Miracle — you may cast this card".to_string()),
                may_trigger_origin: None,
                subject_match_count: None,
                die_result: None,
            };
            super::triggers::push_pending_trigger_to_stack(state, trigger, &mut events);

            // Return to priority so the trigger can be responded to.
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
                false,
            )?
        }
        // CR 702.94a: Miracle reveal — decline path. Reuses the generic
        // DecideOptionalEffect decline; flushes the next pending miracle
        // offer or returns to Priority. Flip `waiting_for` out of MiracleReveal
        // before running the pipeline so its Priority-gated path (line 46 of
        // engine_priority) engages and the flush has a chance to pop the next
        // offer.
        (
            WaitingFor::MiracleReveal { player, .. },
            GameAction::DecideOptionalEffect { accept: false },
        ) => {
            let p = *player;
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
                false,
            )?
        }
        // CR 702.94a + CR 608.2g: Miracle cast offer — the miracle triggered
        // ability has resolved. The player may now cast for the miracle cost.
        // This cast happens during trigger resolution, so timing restrictions
        // do not apply (CR 608.2g).
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Miracle { object_id, cost },
            },
            GameAction::CastSpellAsMiracle {
                object_id: action_obj,
                card_id,
                payment_mode,
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMiracle object_id does not match miracle cast offer".to_string(),
                ));
            }
            let p = *player;
            let obj = action_obj;
            // CR 702.94a + CR 608.2g: forward the cost latched at offer-enqueue as
            // the sole cost authority — live keywords are not re-read (the granting
            // source may have left the battlefield, CR 608.2b).
            let latched_cost = Some(cost.clone());
            super::casting::handle_cast_spell_as_miracle_with_payment_mode(
                state,
                p,
                obj,
                card_id,
                payment_mode,
                latched_cost,
                &mut events,
            )?
        }
        // CR 702.94a: Miracle cast offer — decline. Resume resolution.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Miracle { .. },
            },
            GameAction::DecideOptionalEffect { accept: false },
        ) => {
            let p = *player;
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
                false,
            )?
        }
        // CR 702.35a: Madness cast offer — the madness triggered ability has
        // resolved. The player may now cast the exiled card for its madness cost.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Madness { object_id, .. },
            },
            GameAction::CastSpellAsMadness {
                object_id: action_obj,
                card_id,
                payment_mode,
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMadness object_id does not match madness cast offer".to_string(),
                ));
            }
            let p = *player;
            let obj = action_obj;
            super::casting::handle_cast_spell_as_madness_with_payment_mode(
                state,
                p,
                obj,
                card_id,
                payment_mode,
                &mut events,
            )?
        }
        // CR 702.35a: Madness decline — put the exiled card into its owner's graveyard.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Madness { object_id, .. },
            },
            GameAction::DecideOptionalEffect { accept: false },
        ) => {
            let p = *player;
            let obj = *object_id;
            // CR 702.35a + CR 614.6: a declined madness card is put into its
            // owner's graveyard from exile — route it through the zone-change
            // pipeline so a `Moved` graveyard→exile redirect (Rest in Peace /
            // Leyline of the Void) fires on it. The raw `move_to_zone` never
            // proposed the inner ZoneChange, silently dropping those redirects.
            // The card moves itself (no external source), so it anchors its own
            // attribution. A CR 616.1 ordering choice (two simultaneous
            // redirects) is parked centrally by `move_object`; bail before
            // overwriting `waiting_for` / running the post-action pipeline so the
            // parked prompt is not clobbered (its resume runs the pipeline).
            match super::zone_pipeline::move_object(
                state,
                super::zone_pipeline::ZoneMoveRequest::effect(obj, Zone::Graveyard, obj),
                &mut events,
            ) {
                super::zone_pipeline::ZoneMoveResult::Done => {
                    state.waiting_for = WaitingFor::Priority { player: p };
                    super::engine_priority::run_post_action_pipeline(
                        state,
                        &mut events,
                        &WaitingFor::Priority { player: p },
                        true,
                        false,
                    )?
                }
                // The graveyard move paused on a CR 616.1 ordering choice; the
                // parked prompt is already in `state.waiting_for`. Evaluate the
                // arm to it (non-`Priority`), so the post-match block skips the
                // post-action pipeline and the prompt is surfaced intact — its
                // replacement-choice resume finishes the move and re-runs the
                // pipeline.
                super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
                | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                    state.waiting_for.clone()
                }
            }
        }
        (waiting_for, action) if engine_resolution_choices::handles(waiting_for) => {
            match engine_resolution_choices::handle_resolution_choice(
                state,
                waiting_for.clone(),
                action,
                &mut events,
            )? {
                engine_resolution_choices::ResolutionChoiceOutcome::WaitingFor(waiting_for) => {
                    waiting_for
                }
                engine_resolution_choices::ResolutionChoiceOutcome::WaitingForWithInlineTriggers(
                    waiting_for,
                ) => {
                    triggers_processed_inline = true;
                    waiting_for
                }
                engine_resolution_choices::ResolutionChoiceOutcome::WaitingForWithParkedObservers(
                    waiting_for,
                ) => {
                    triggers_processed_inline = true;
                    skip_deferred_trigger_drain = true;
                    waiting_for
                }
                engine_resolution_choices::ResolutionChoiceOutcome::ActionResult(result) => {
                    return Ok(result);
                }
            }
        }
        (WaitingFor::Priority { player }, GameAction::PlayFaceDown { object_id, card_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            // Validate object_id matches card_id and is in hand
            let valid = state.objects.get(&object_id).is_some_and(|obj| {
                obj.card_id == card_id && obj.owner == p && obj.zone == Zone::Hand
            });
            if !valid {
                return Err(EngineError::InvalidAction(
                    "Card not found in hand".to_string(),
                ));
            }
            super::morph::play_face_down(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (WaitingFor::Priority { player }, GameAction::TurnFaceUp { object_id, x }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            let announced_x = x;
            // CR 116.2b + CR 702.37e / CR 702.168d / CR 701.40b + CR 106.6: turning
            // a face-down permanent face up is a special action whose morph/disguise/
            // manifest cost must be paid *before* the flip. `turn_face_up_prepare`
            // validates the action and derives that cost; payment routes through
            // `PaymentContext::SpecialAction(TurnFaceUp)` so spend-restricted mana
            // ("only to turn permanents face up", Overgrown Zealot / Tin Street
            // Gossip) is eligible here while other-context mana is rejected. Mirrors
            // the `UnlockDoor` special-action handler.
            let cost = super::morph::turn_face_up_prepare(state, object_id, p)?;
            let mut cost = casting::apply_special_action_cost_reduction(
                state,
                p,
                crate::types::mana::SpecialAction::TurnFaceUp,
                cost,
            );

            // CR 107.3d: "If a cost associated with a special action, such as a suspend
            // cost or a morph cost, has an {X} ... in it, the value of X is chosen by the
            // player taking the special action immediately before they pay that cost."
            // The announcement happens HERE — inside the action, with no priority window
            // between choosing X and paying it, exactly as the rule describes.
            //
            // Warbreak Trumpeter (Morph {X}{X}{R}), Bane of the Living (Morph {X}{B}{B})
            // and Aurelia's Vindicator (Disguise {X}{3}{W}) are the live faces.
            let has_x = casting_costs::cost_has_x(&cost);
            if has_x {
                // CR 118.3: a player can't announce an X they cannot pay for. The cap is
                // computed with `object_id: None` deliberately — this is a SPECIAL ACTION,
                // not a cast, so cast-time cost modifiers and floors must not apply (the
                // special-action reduction was already applied above).
                let max_x = casting_costs::max_x_value(state, p, &cost, None);
                if announced_x > max_x {
                    return Err(EngineError::InvalidAction(format!(
                        "X={announced_x} exceeds the maximum payable value of {max_x} for this \
                         turn-face-up cost"
                    )));
                }
                // CR 107.1b + CR 601.2f: each `{X}` shard becomes `announced_x` generic, so
                // Warbreak Trumpeter's `{X}{X}{R}` costs 2X + {R}. Without this the X shards
                // reach mana payment unresolved and are dropped — the permanent flips for
                // its non-X remainder alone.
                cost.concretize_x(announced_x);
            } else if announced_x != 0 {
                // A cost with no {X} admits no choice: CR 107.3d only grants one "if a cost
                // ... has an {X} ... in it". Reject rather than silently ignore, so a client
                // bug cannot masquerade as a legal flip.
                return Err(EngineError::InvalidAction(
                    "This permanent's turn-face-up cost has no {X}, so X must be 0".to_string(),
                ));
            }
            casting::pay_special_action_mana_cost(
                state,
                p,
                Some(object_id),
                &cost,
                crate::types::mana::SpecialAction::TurnFaceUp,
                &mut events,
            )?;

            // CR 702.37f (morph) / CR 702.168e (disguise): "If a permanent's morph cost
            // includes X, other abilities of that permanent may also refer to X. The value
            // of X in those abilities is equal to the value of X chosen as the morph special
            // action was taken." Publish the announced X on the source-keyed carrier BEFORE
            // the flip emits `TurnedFaceUp`, so `triggers::build_triggered_ability` — the
            // single trigger-instantiation authority — stamps it onto the turn-face-up
            // trigger's `chosen_x`.
            //
            // The stamp must land at INSTANTIATION, not resolution: Aurelia's Vindicator
            // spends its X in `multi_target.max` ("exile up to X other target creatures"),
            // which is consumed during target selection, before the trigger ever resolves.
            //
            // Published only when the cost actually HAS an {X} (CR 107.3d grants a choice
            // only then). A no-X flip leaves the carrier untouched rather than clobbering it
            // with `Some((.., 0))`: an unrelated activated ability of ANOTHER object may be
            // on the stack with its own announced X in flight, and that value must survive.
            // The carrier is cleared at the start of the next `resolve_top`, so this
            // publication cannot outlive the trigger it is for.
            if has_x {
                state.announced_source_x = Some((object_id, announced_x));
            }

            super::morph::turn_face_up(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (
            WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                target_constraints,
                ..
            },
            GameAction::SelectTargets { targets },
        ) => engine_stack::handle_trigger_target_selection_select_targets(
            state,
            *player,
            target_slots,
            target_constraints,
            targets,
            &mut events,
        )?,
        (WaitingFor::TriggerTargetSelection { .. }, GameAction::ChooseTarget { target }) => {
            let waiting_for = state.waiting_for.clone();
            engine_stack::handle_trigger_target_selection_choose_target(
                state,
                waiting_for,
                target,
                &mut events,
            )?
        }
        (
            WaitingFor::BetweenGamesSideboard { player, .. },
            GameAction::SubmitSideboard { main, sideboard },
        ) => match_flow::handle_submit_sideboard(state, *player, main, sideboard, &mut events)
            .map_err(EngineError::InvalidAction)?,
        (
            WaitingFor::BetweenGamesChoosePlayDraw { player, .. },
            GameAction::ChoosePlayDraw { play_first },
        ) => match_flow::handle_choose_play_draw(state, *player, play_first, &mut events)
            .map_err(EngineError::InvalidAction)?,
        (
            waiting_for @ WaitingFor::AbilityModeChoice { .. },
            GameAction::SelectModes { indices },
        ) => engine_modes::handle_ability_mode_choice(
            state,
            waiting_for.clone(),
            indices,
            &mut events,
        )?,
        // CR 602.2b + CR 601.2b: The controller chooses modes for an activated modal
        // ability BEFORE any cost is paid, target is chosen, or stack object is created
        // (those steps run later in engine_modes::handle_activated_mode_choice). At this
        // pre-commit sub-step nothing has changed in the game state, so cancelling is a
        // pure rollback to priority — mirroring the modal-spell (ModeChoice, CancelCast)
        // and (ChoosePermanentTypeSlot, CancelCast) arms.
        // CR 603.3c: A modal *triggered* ability's entry is already on the stack when the
        // mode prompt appears; its controller MUST choose a mode. This arm is guarded to
        // is_activated: true, so the triggered case falls through to the catch-all reject.
        (
            WaitingFor::AbilityModeChoice {
                player,
                is_activated: true,
                ..
            },
            GameAction::CancelCast,
        ) => WaitingFor::Priority { player: *player },
        // CR 601.2c: Player selected targets from a multi-target set ("any number of").
        (WaitingFor::MultiTargetSelection { .. }, GameAction::SelectCards { cards: selected }) => {
            let waiting_for = state.waiting_for.clone();
            engine_stack::handle_multi_target_selection(state, waiting_for, &selected, &mut events)?
        }
        // CR 702.139a: Pre-game companion reveal
        (
            WaitingFor::CompanionReveal { player, .. },
            GameAction::DeclareCompanion { card_index },
        ) => super::companion::handle_declare_companion(state, *player, card_index, &mut events),
        // CR 702.139a: Special action — pay {3} to put companion into hand (see rule 116.2g).
        (WaitingFor::Priority { player }, GameAction::CompanionToHand) => {
            state.lands_tapped_for_mana.remove(player);
            super::companion::handle_companion_to_hand(state, *player, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        // CR 722.3c / CR 601.2: Prepare (Strixhaven) — cast a copy of the
        // prepared face through the normal spell-casting pipeline (costs,
        // targeting, and mode choices all run through casting.rs single
        // authority). Assign when WotC publishes SOS CR update.
        (WaitingFor::Priority { player }, GameAction::CastPreparedCopy { source }) => {
            let p = *player;
            // Validate controller.
            let src = source;
            let Some(obj) = state.objects.get(&src) else {
                return Err(EngineError::InvalidAction(format!(
                    "CastPreparedCopy: source {src:?} not found"
                )));
            };
            if obj.controller != p {
                return Err(EngineError::InvalidAction(
                    "CastPreparedCopy: source not controlled by acting player".to_string(),
                ));
            }
            effects::prepare::cast_prepared_copy(state, src, p, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        // CR 702.xxx: Paradigm (Strixhaven) — accept the turn-based offer to
        // cast a copy of an exiled paradigm source. Assign when WotC
        // publishes SOS CR update.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Paradigm { offers },
            },
            GameAction::CastParadigmCopy { source },
        ) => {
            let src = source;
            if !offers.contains(&src) {
                return Err(EngineError::InvalidAction(format!(
                    "CastParadigmCopy: source {src:?} not in current offer set"
                )));
            }
            let p = *player;
            let copy_id = effects::paradigm::cast_paradigm_copy(state, src, p, &mut events)
                .map_err(EngineError::InvalidAction)?;
            let remaining: Vec<ObjectId> = offers
                .iter()
                .copied()
                .filter(|id| *id != src)
                .collect();
            // CR 707.10c: If the paradigm spell has target slots, open target
            // selection via CopyRetarget. Otherwise re-offer any remaining
            // paradigm sources before returning to priority.
            if effects::prepare::open_copy_target_selection(
                state,
                copy_id,
                p,
                Some(remaining.clone()),
            )
            .map_err(EngineError::InvalidAction)?
            {
                state.waiting_for.clone()
            } else {
                effects::paradigm::waiting_after_remaining_offers(p, remaining)
            }
        }
        // CR 702.xxx: Paradigm (Strixhaven) — decline the turn-based offer.
        // Assign when WotC publishes SOS CR update.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Paradigm { .. },
            },
            GameAction::PassParadigmOffer,
        ) => WaitingFor::Priority { player: *player },
        (WaitingFor::Priority { player }, GameAction::SetAutoPass { mode }) => {
            if super::precast_copy_shortcut::blocks_pass(state, *player) {
                return Err(EngineError::ActionNotAllowed(
                    "A shortened pre-cast shortcut requires a different meaningful action before passing"
                        .to_string(),
                ));
            }
            // Convert request to stored mode, capturing engine state as needed.
            let stored_mode = match mode {
                AutoPassRequest::UntilStackEmpty => AutoPassMode::UntilStackEmpty {
                    initial_stack_len: state.stack.len(),
                },
                AutoPassRequest::UntilTurnBoundary { until } => {
                    AutoPassMode::UntilTurnBoundary { until }
                }
            };
            state.auto_pass.insert(*player, stored_mode);
            let wf = pass_priority_once_with_pipeline(state, &mut events, None)?;
            return Ok(ActionResult {
                events,
                waiting_for: wf,
                log_entries: vec![],
            });
        }
        // CR 701.34a: Proliferate — player selected targets to proliferate.
        (
            WaitingFor::ProliferateChoice { player, eligible },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let eligible_set = eligible.clone();
            // Validate all selected targets are in the eligible set.
            for t in &targets {
                if !eligible_set.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Selected target not eligible for proliferate".to_string(),
                    ));
                }
            }
            if !effects::proliferate::apply_proliferate(state, p, &targets, &mut events) {
                return Ok(ActionResult {
                    events,
                    waiting_for: state.waiting_for.clone(),
                    log_entries: vec![],
                });
            }
            // CR 701.34a: Emit player-action event so proliferate triggers fire.
            events.push(GameEvent::PlayerPerformedAction {
                player_id: p,
                action: PlayerActionKind::Proliferate,
            });
            let completion_source = state
                .pending_proliferate_actions
                .as_ref()
                .map(|pending| pending.source_id)
                .unwrap_or(ObjectId(0));
            if !effects::proliferate::resume_pending_proliferate_actions(state, &mut events) {
                return Ok(ActionResult {
                    events,
                    waiting_for: state.waiting_for.clone(),
                    log_entries: vec![],
                });
            }
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Proliferate,
                source_id: completion_source,
            subject: None,});
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::drain_pending_continuation(state, &mut events);
            state.waiting_for.clone()
        }
        // CR 701.56a: Time travel — player selected objects for the current phase
        // (remove a time counter, then add). Validate against the eligible set,
        // apply the per-object counter change, then advance to the add phase or
        // finish. Counter changes drive the existing suspend/vanishing triggers.
        (
            WaitingFor::TimeTravelChoice {
                player,
                eligible,
                phase,
            },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let phase = *phase;
            let eligible_set = eligible.clone();
            for t in &targets {
                if !eligible_set.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Selected object not eligible for time travel".to_string(),
                    ));
                }
            }
            effects::time_travel::apply_phase(state, p, &targets, phase, &mut events);

            if phase == crate::types::game_state::TimeTravelPhase::Remove {
                // CR 701.56a: after the remove phase, offer the add phase over the
                // still-eligible objects, excluding any just chosen to remove.
                let add_eligible: Vec<_> = effects::time_travel::eligible_objects(state, p)
                    .into_iter()
                    .filter(|t| !targets.contains(t))
                    .collect();
                if !add_eligible.is_empty() {
                    state.waiting_for = WaitingFor::TimeTravelChoice {
                        player: p,
                        eligible: add_eligible,
                        phase: crate::types::game_state::TimeTravelPhase::Add,
                    };
                    state.waiting_for.clone()
                } else {
                    events.push(GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::TimeTravel,
                        source_id: ObjectId(0),
                    subject: None,});
                    state.waiting_for = WaitingFor::Priority { player: p };
                    state.priority_player = p;
                    effects::drain_pending_continuation(state, &mut events);
                    state.waiting_for.clone()
                }
            } else {
                events.push(GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::TimeTravel,
                    source_id: ObjectId(0),
                subject: None,});
                state.waiting_for = WaitingFor::Priority { player: p };
                state.priority_player = p;
                effects::drain_pending_continuation(state, &mut events);
                state.waiting_for.clone()
            }
        }
        // CR 608.2c: ChooseObjectsIntoTrackedSet — player submitted their
        // battlefield-permanent selection. Publish a fresh tracked set so the
        // downstream `PayCost { ScaledMana }` and the `IfYouDo`/`Untap` tail
        // resolve against exactly this selection, then resume the chain.
        (
            WaitingFor::ChooseObjectsSelection {
                player,
                eligible,
                trigger_event,
            },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let eligible_set = eligible.clone();
            let pending_event = trigger_event.clone();
            // Validate all selected targets are in the eligible set.
            for t in &targets {
                if !eligible_set.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Selected target not eligible for object selection".to_string(),
                    ));
                }
            }
            // Map TargetRef → ObjectId. The eligible set is all battlefield
            // permanents, so every selected target is an Object.
            let ids: Vec<ObjectId> = targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    TargetRef::Player(_) => None,
                })
                .collect();
            // CR 603.7: Always allocate a fresh tracked set — a player-chosen
            // "those creatures" set is a new resolution scope. An empty
            // selection yields an empty fresh set (size 0).
            effects::publish_fresh_tracked_set(state, ids);
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::ChooseObjectsIntoTrackedSet,
                source_id: ObjectId(0), // Source not tracked through choice state
                subject: None,
            });
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            // CR 608.2: restore the triggering event so the stashed
            // `PayCost { ScaledMana, payer: TriggeringPlayer }` continuation
            // resolves the payer correctly — the trigger's resolution is still
            // in flight.
            // CR 603.2c + CR 608.2: the batched-trigger subject count is also
            // part of the trigger's resolution scope — mirror its save/restore
            // so an `EventContextAmount` inside the resumed continuation reads
            // the original "that many" instead of `None`.
            let previous_trigger_event = state.current_trigger_event.clone();
            let previous_trigger_match_count = state.current_trigger_match_count;
            state.current_trigger_event = pending_event;
            state.current_trigger_match_count = state.pending_optional_trigger_match_count.take();
            effects::drain_pending_continuation(state, &mut events);
            state.current_trigger_event = previous_trigger_event;
            state.current_trigger_match_count = previous_trigger_match_count;
            state.waiting_for.clone()
        }
        // CR 707.10c: Copy retarget — player chose target for the current slot
        // via battlefield click. Advances slot-by-slot; finalizes on the last slot.
        (
            WaitingFor::CopyRetarget {
                player,
                copy_id,
                target_slots,
                effect_kind,
                effect_source_id,
                current_slot,
                paradigm_remaining_offers,
            },
            GameAction::ChooseTarget { target },
        ) => {
            let p = *player;
            let cid = *copy_id;
            let slot_idx = *current_slot;
            if let Some(ref t) = target {
                let slot = &target_slots[slot_idx];
                // CR 707.10c: A retarget choice must produce a legal target. Both
                // `prepare::open_copy_target_selection` and `copy_spell::resolve`
                // populate `legal_alternatives` from `build_target_slots`, so an
                // empty list means "no legal alternative exists" — the caller
                // must use `KeepAllCopyTargets` (or send `target: None`).
                if !slot.legal_alternatives.contains(t) {
                    return Err(EngineError::InvalidAction(format!(
                        "Target {t:?} not a legal alternative for copy slot {slot_idx}"
                    )));
                }
            } else if target_slots[slot_idx].current.is_none() {
                return Err(EngineError::InvalidAction(format!(
                    "Copy target slot {slot_idx} has no current target to keep"
                )));
            }
            let mut updated_slots = target_slots.clone();
            if let Some(t) = target {
                updated_slots[slot_idx].current = Some(t.clone());
            }
            let next_slot = slot_idx + 1;
            if next_slot < updated_slots.len() {
                state.waiting_for = WaitingFor::CopyRetarget {
                    player: p,
                    copy_id: cid,
                    target_slots: updated_slots,
                    effect_kind: *effect_kind,
                    effect_source_id: *effect_source_id,
                    current_slot: next_slot,
                    paradigm_remaining_offers: paradigm_remaining_offers.clone(),
                };
            } else {
                finalize_copy_retarget(
                    state,
                    p,
                    cid,
                    &updated_slots,
                    *effect_kind,
                    *effect_source_id,
                    &mut events,
                )?;
            }
            state.waiting_for.clone()
        }
        // CR 707.10c: "Keep Current Targets" — accept every remaining slot's
        // current value in one action. Equivalent to dispatching
        // `ChooseTarget { target: None }` for each remaining slot, but resolved
        // server-side so the UI doesn't pay N round-trips. The slot-by-slot
        // `ChooseTarget` path above remains the single authority for the
        // per-slot legality/advance semantics.
        (
            WaitingFor::CopyRetarget {
                player,
                copy_id,
                target_slots,
                effect_kind,
                effect_source_id,
                ..
            },
            GameAction::KeepAllCopyTargets,
        ) => {
            let p = *player;
            let cid = *copy_id;
            let slots = target_slots.clone();
            finalize_copy_retarget(
                state,
                p,
                cid,
                &slots,
                *effect_kind,
                *effect_source_id,
                &mut events,
            )?;
            state.waiting_for.clone()
        }
        // CR 510.1c/d: Combat damage assignment from attacker to blockers.
        (
            WaitingFor::AssignCombatDamage {
                player,
                attacker_id,
                total_damage,
                blockers,
                assignment_modes,
                trample,
                defending_player,
                attack_target,
                pw_loyalty,
                pw_controller,
            },
            GameAction::AssignCombatDamage {
                mode,
                assignments,
                trample_damage,
                controller_damage,
            },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_assign_combat_damage(
                state,
                *player,
                *attacker_id,
                *total_damage,
                blockers,
                assignment_modes,
                *trample,
                *defending_player,
                attack_target,
                *pw_loyalty,
                *pw_controller,
                mode,
                &assignments,
                trample_damage,
                controller_damage,
                &mut events,
            )?
        }
        // CR 510.1d + CR 702.22k: A banded blocker's combat damage is divided by
        // the active player among the attackers it blocks.
        (
            WaitingFor::AssignBlockerDamage {
                player,
                blocker_id,
                total_damage,
                attackers,
            },
            GameAction::AssignBlockerDamage { assignments },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_assign_blocker_damage(
                state,
                *player,
                *blocker_id,
                *total_damage,
                attackers,
                &assignments,
                &mut events,
            )?
        }
        // CR 601.2d: Distribute among targets (casting-time distribution).
        (WaitingFor::DistributeAmong { player, .. }, GameAction::CancelCast) => {
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)
                }
                None => {
                    return Err(EngineError::InvalidAction(
                        "No pending cast to cancel during distribution".to_string(),
                    ));
                }
            }
        }
        (
            WaitingFor::DistributeAmong {
                player,
                total,
                targets,
                ..
            },
            GameAction::DistributeAmong { distribution },
        ) => {
            let p = *player;
            let expected_total = *total;

            // Validate: each target gets ≥ 1, and total matches.
            let actual_total: u32 = distribution.iter().map(|(_, a)| *a).sum();
            if actual_total != expected_total {
                return Err(EngineError::InvalidAction(format!(
                    "Distribution total {} != required {}",
                    actual_total, expected_total
                )));
            }
            for (t, amount) in &distribution {
                if *amount == 0 {
                    return Err(EngineError::InvalidAction(
                        "Each target must receive at least 1".to_string(),
                    ));
                }
                if !targets.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Distribution target not in legal set".to_string(),
                    ));
                }
            }

            // Store on the pending cast's resolved ability if we're mid-casting.
            // The distribution will be read during effect resolution.
            if let Some(pending) = state.pending_cast.as_mut() {
                pending.ability.distribution =
                    Some(distribution.iter().map(|(t, a)| (t.clone(), *a)).collect());
            }

            // CR 601.2d: Resume casting pipeline after distribution.
            if state.pending_cast.is_some() {
                let pending = state.pending_cast.take().unwrap();
                if let Some(ability_index) = pending.activation_ability_index {
                    // CR 602.2b + CR 601.2d: an activated ability that divides
                    // damage among targets goes on the stack as an ActivatedAbility
                    // after the division is announced — not as a spell (Captain
                    // America's Throw). `push_activated_ability_to_stack` pays the
                    // residual mana leg and no-ops the already-paid UnattachFrom.
                    // The spell-only cost-determination authority used in the `else`
                    // branch (`finish_pending_cast_cost_or_pay`) must NOT be reached
                    // here: it routes into `finalize_cast`, which would commit the
                    // source permanent to the stack as a spell.
                    casting_costs::push_activated_ability_to_stack(
                        state,
                        p,
                        pending.object_id,
                        ability_index,
                        pending.ability,
                        pending.activation_cost.as_ref(),
                        pending.activation_residual,
                        &mut events,
                    )?
                } else {
                    // CR 601.2c + CR 601.2d + CR 601.2f: Targets and their division are now
                    // committed, so the total cost — including any target-dependent
                    // surcharge (Strive, CR 207.2c) — is finally determinable. Route through
                    // the single cost-determination authority every other post-target-
                    // selection path uses (`casting_targets::handle_select_targets` /
                    // `handle_choose_target`) instead of calling `finalize_cast` directly
                    // with the stale cost that was locked in at `ChooseXValue` time, before
                    // targets (and hence any per-target surcharge) were known.
                    //
                    // CR 601.2h ("Unpayable costs can't be paid"): mirror
                    // `finalize_mana_payment`'s `pending_for_restore` pattern
                    // (casting_costs.rs ~8623-8627/8778-8787) — `finish_pending_cast_cost_or_pay`'s
                    // downstream chain has no restore-on-error wrapper of its own, and
                    // `state.pending_cast` is already `None` here (unlike
                    // `handle_select_targets`, whose `pending_cast` lives inside the
                    // `WaitingFor::TargetSelection` variant and so is never destructively
                    // taken). Without this clone-and-restore, a recomputed cost that turns
                    // out unpayable would return `Err` with `state.pending_cast` gone while
                    // `state.waiting_for` still reports `DistributeAmong` — a resubmitted
                    // `DistributeAmong` action would then fall through to the
                    // resolution-time continuation branch below instead of being cleanly
                    // rejected.
                    let pending_for_restore = pending.clone();
                    let ability = pending.ability.clone();
                    let cost = pending.cost.clone();
                    match casting_costs::finish_pending_cast_cost_or_pay(
                        state,
                        p,
                        *pending,
                        ability,
                        cost,
                        &mut events,
                    ) {
                        Ok(waiting_for) => waiting_for,
                        Err(err) => {
                            state.pending_cast = Some(pending_for_restore);
                            return Err(err);
                        }
                    }
                }
            } else if let Some(mut pending_trigger) = state.pending_trigger.take() {
                // CR 601.2d + CR 603.3d: Triggered abilities divide effects
                // while being put on the stack. The chosen per-target amounts
                // are resolution data on the resolved ability. The entry is
                // already on the stack (pushed at distribute-among pause time);
                // mutate its ability with the distribution and clear
                // `pending_trigger_entry` so the resolver may now fire it.
                pending_trigger.ability.distribution =
                    Some(distribution.iter().map(|(t, a)| (t.clone(), *a)).collect());
                if !triggers::finalize_pending_trigger_entry(state, &pending_trigger.ability) {
                    // Unexpected dangling cursor: the entry is no longer on the
                    // stack. Recover per CR 608.2b / CR 800.4a (a stack object
                    // that has left the stack does not resolve) — record the
                    // diagnostic, abandon, and return priority instead of
                    // panicking (re-normalized next pass; CR 117.3b would give
                    // the active player).
                    triggers::abandon_ceased_pending_trigger(state, &pending_trigger.ability);
                    priority::clear_priority_passes(state);
                    WaitingFor::Priority { player: p }
                } else {
                    priority::clear_priority_passes(state);
                    // CR 113.2c + CR 603.2 + CR 603.3b: Drain siblings deferred
                    // behind this distribute-among trigger so each independent
                    // instance reaches the stack (issue #416).
                    debug_assert!(
                        !triggers::is_pending_trigger_construction_active(state),
                        "deferred-trigger drain entered with construction still active",
                    );
                    if let Some(waiting_for) =
                        triggers::drain_deferred_trigger_queue(state, &mut events)
                    {
                        waiting_for
                    } else {
                        WaitingFor::Priority { player: p }
                    }
                }
            } else {
                // Resolution-time distribution continuation path.
                state.waiting_for = WaitingFor::Priority { player: p };
                state.priority_player = p;
                effects::drain_pending_continuation(state, &mut events);
                state.waiting_for.clone()
            }
        }
        (
            WaitingFor::MoveCountersDistribution {
                player,
                source_id,
                available,
                destinations,
                pending_effect,
                ..
            },
            GameAction::ChooseCounterMoveDistribution { selections },
        ) => {
            let p = *player;
            effects::counters::validate_and_queue_counter_move_distribution(
                state,
                &selections,
                *source_id,
                available,
                destinations,
                pending_effect,
            )
            .map_err(|err| EngineError::InvalidAction(err.to_string()))?;
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::counters::drain_pending_counter_moves(state, &mut events);
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                effects::drain_pending_continuation(state, &mut events);
            }
            state.waiting_for.clone()
        }
        // CR 107.1c + CR 608.2d: Submit the "remove any number of counters"
        // resolution-time selection (Rhys, the Evermore; Tetravus). ORDERING
        // INVARIANT: apply removals (stamping `last_effect_count`) BEFORE draining
        // the continuation, so a chained "create that many" rider reads the count.
        (
            WaitingFor::RemoveCountersChoice {
                player,
                source_id,
                available,
                pending_effect,
                ..
            },
            GameAction::ChooseCountersToRemove { selections },
        ) => {
            let p = *player;
            effects::counters::validate_and_queue_counter_removal(
                state,
                &selections,
                *source_id,
                available,
                pending_effect,
            )
            .map_err(|err| EngineError::InvalidAction(err.to_string()))?;
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::counters::drain_pending_counter_removals(state, &mut events);
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                effects::drain_pending_continuation(state, &mut events);
            }
            state.waiting_for.clone()
        }
        // CR 115.7: Retarget a spell or ability on the stack via the dialog
        // path — the multi-target (`All`-scope) UI submits every new target at
        // once.
        (
            WaitingFor::RetargetChoice {
                player,
                stack_entry_index,
                scope,
                current_targets,
                legal_new_targets,
                ..
            },
            GameAction::RetargetSpell { new_targets },
        ) => apply_retarget(
            state,
            &mut events,
            RetargetSubmission {
                player: *player,
                stack_entry_index: *stack_entry_index,
                scope,
                current_targets,
                legal_new_targets,
                new_targets,
            },
        )?,
        // CR 115.7: Retarget a single-target spell via a board click. The
        // universal `ChooseTarget` action — already consumed by every other
        // targeting state — drives single-target retargets (Bolt Bend,
        // Redirect, Misdirection) so the player picks the new target directly
        // on the battlefield instead of through a dialog.
        (
            WaitingFor::RetargetChoice {
                player,
                stack_entry_index,
                scope: RetargetScope::Single,
                current_targets,
                legal_new_targets,
                ..
            },
            GameAction::ChooseTarget { target: Some(t) },
        ) => apply_retarget(
            state,
            &mut events,
            RetargetSubmission {
                player: *player,
                stack_entry_index: *stack_entry_index,
                scope: &RetargetScope::Single,
                current_targets,
                legal_new_targets,
                new_targets: vec![t],
            },
        )?,
        (waiting, action) => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cannot perform {:?} while waiting for {:?}",
                action, waiting
            )));
        }
    };

    // A shortened shortcut is discharged only by an action the normal reducer
    // accepted. In particular, a rejected cast/land attempt must leave the
    // CR 732.2c divergence requirement armed; preference actions returned
    // earlier and priority passes never reach this successful-reducer seam.
    super::precast_copy_shortcut::note_meaningful_action(
        state,
        semantic_actor,
        &action_for_divergence,
    );

    // Run post-action pipeline (SBAs, triggers, layers) and check for terminal states.
    // When triggers were already processed inline (e.g., DeclareAttackers, combat damage),
    // pass the flag to skip the trigger scan but still run SBAs, delayed triggers, and layers.
    if matches!(waiting_for, WaitingFor::Priority { .. }) {
        // Sync state.waiting_for before the pipeline so SBA/trigger checks see
        // the action's result, not the pre-action state (fixes stale TargetSelection
        // after CancelCast).
        state.waiting_for = waiting_for.clone();
        let wf = engine_priority::run_post_action_pipeline(
            state,
            &mut events,
            &waiting_for,
            triggers_processed_inline,
            skip_deferred_trigger_drain,
        )?;
        state.waiting_for = wf.clone();
        return Ok(ActionResult {
            events,
            waiting_for: wf,
            log_entries: vec![],
        });
    }

    // CR 704.3 / CR 800.4: SBAs may have ended the game during phase auto-advance (e.g.,
    // combat damage step) before we reach this point. state.waiting_for is the authoritative
    // result — written directly by eliminate_player → check_game_over. Guard against
    // overwriting it with the computed `waiting_for` from auto_advance.
    if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        match_flow::handle_game_over_transition(state);
        let wf = state.waiting_for.clone();
        return Ok(ActionResult {
            events,
            waiting_for: wf,
            log_entries: vec![],
        });
    }

    state.waiting_for = waiting_for.clone();

    Ok(ActionResult {
        events,
        waiting_for,
        log_entries: vec![],
    })
}

struct RetargetSubmission<'a> {
    player: PlayerId,
    stack_entry_index: usize,
    scope: &'a RetargetScope,
    current_targets: &'a [TargetRef],
    legal_new_targets: &'a [TargetRef],
    new_targets: Vec<TargetRef>,
}

/// CR 115.7d: Apply a validated retarget to the stack entry, then hand priority
/// back to the retargeting player. Single authority for both retarget entry
/// points — the board-click (`ChooseTarget`) and dialog (`RetargetSpell`) paths
/// — so target validation and stack mutation can never drift apart.
fn apply_retarget(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    submission: RetargetSubmission<'_>,
) -> Result<WaitingFor, EngineError> {
    let RetargetSubmission {
        player,
        stack_entry_index,
        scope,
        current_targets,
        legal_new_targets,
        new_targets,
    } = submission;

    match scope {
        RetargetScope::Single => {
            if new_targets.len() != 1 {
                return Err(EngineError::InvalidAction(
                    "Retarget: single-target change requires exactly one target".to_string(),
                ));
            }
            if !legal_new_targets.contains(&new_targets[0]) {
                return Err(EngineError::InvalidAction(
                    "Retarget: chosen target not in legal alternatives".to_string(),
                ));
            }
        }
        RetargetScope::All => {
            if new_targets.len() != current_targets.len() {
                return Err(EngineError::InvalidAction(
                    "Retarget: choose-new-targets submission must preserve target count"
                        .to_string(),
                ));
            }
            // CR 115.7d: For "choose new targets", unchanged targets may remain
            // unchanged even if they are no longer legal. Changed targets still
            // must be legal alternatives.
            for (idx, target) in new_targets.iter().enumerate() {
                if current_targets.get(idx) == Some(target) {
                    continue;
                }
                if !legal_new_targets.contains(target) {
                    return Err(EngineError::InvalidAction(
                        "Retarget: chosen target not in legal alternatives".to_string(),
                    ));
                }
            }
        }
        RetargetScope::ForcedTo(_) => {
            return Err(EngineError::InvalidAction(
                "Retarget: forced retarget is not interactive".to_string(),
            ));
        }
    }

    if stack_entry_index < state.stack.len() {
        if let Some(ability) = state.stack[stack_entry_index].ability_mut() {
            ability.targets = new_targets;
        }
    } else {
        return Err(EngineError::InvalidAction(
            "Invalid stack entry index for retargeting".to_string(),
        ));
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChangeTargets,
        source_id: state
            .stack
            .get(stack_entry_index)
            .map(|e| e.source_id)
            .unwrap_or(ObjectId(0)),
        subject: None,
    });
    state.waiting_for = WaitingFor::Priority { player };
    state.priority_player = player;
    effects::drain_pending_continuation(state, events);
    Ok(state.waiting_for.clone())
}

/// Run state-based actions, exile returns, delayed triggers, and trigger processing
/// after an action that produced `WaitingFor::Priority`. Returns the resulting
/// `WaitingFor` state — may be terminal (GameOver, interactive choice) or
/// a continuation (Priority for next player/active player).
///
/// `default_wf` is the WaitingFor computed by the action handler, used as fallback
/// when no terminal/trigger/SBA outcome overrides it.
///
/// `skip_trigger_scan` — when `true`, skips the `process_triggers` call because
/// triggers were already processed inline (e.g., combat damage, declare attackers).
/// SBAs, exile returns, delayed triggers, and layer evaluation still run.
pub(super) fn begin_pending_trigger_target_selection(
    state: &mut GameState,
) -> Result<Option<WaitingFor>, EngineError> {
    let Some(trigger) = state.pending_trigger.as_ref() else {
        return Ok(None);
    };

    // CR 700.2b: Modal trigger — prompt for mode selection before stack.
    if let Some(ref modal) = trigger.modal {
        if !trigger.mode_abilities.is_empty() {
            let player = trigger.controller;
            let source_id = trigger.source_id;
            let mode_abilities = trigger.mode_abilities.clone();
            let trigger_event = trigger.trigger_event.clone();
            let trigger_events = if state.pending_trigger_event_batch.is_empty() {
                trigger_event.iter().cloned().collect::<Vec<_>>()
            } else {
                state.pending_trigger_event_batch.clone()
            };
            let subject_match_count = trigger.subject_match_count;
            let modal = modal_choice_for_player(
                state,
                player,
                source_id,
                modal,
                &crate::types::ability::SpellContext::default(),
            );
            let mut unavailable_modes = compute_unavailable_modes(state, source_id, &modal);
            let context_snapshot = super::triggers::push_trigger_event_context(
                state,
                trigger_event.as_ref(),
                &trigger_events,
                subject_match_count,
            );
            super::ability_utils::filter_modes_by_target_legality(
                state,
                source_id,
                player,
                &mode_abilities,
                &modal,
                &mut unavailable_modes,
            );
            super::triggers::restore_trigger_event_context(state, context_snapshot);
            let Some(modal) = super::ability_utils::modal_choice_with_target_assignment_limit(
                state,
                source_id,
                player,
                &modal,
                &mode_abilities,
                &unavailable_modes,
            ) else {
                if let Some(entry_id) = state.pending_trigger_entry.take() {
                    if state.stack.back().map(|e| e.id) == Some(entry_id) {
                        state.stack.pop_back();
                        state.stack_paid_facts.remove(&entry_id);
                        state.stack_trigger_event_batches.remove(&entry_id);
                    }
                }
                state.pending_trigger = None;
                return Ok(None);
            };

            // CR 700.2b (override) + CR 701.9b (analogous): "choose ... at
            // random" modal triggers (Cult of Skaro) are resolved inline by
            // `dispatch_pending_trigger_context` via `state.rng` — they clear
            // `modal` before this re-entry surfaces a `WaitingFor`, so reaching
            // here with a `Random` selection means the dispatcher was bypassed.
            // This router cannot thread `events` into the random resolver, so
            // emitting `AbilityModeChoice` would (wrongly) prompt the controller.
            // Drop the trigger defensively instead of prompting incorrectly.
            debug_assert!(
                !modal.selection.is_random(),
                "random modal trigger reached begin_pending_trigger_target_selection; \
                 dispatch_pending_trigger_context must resolve it inline",
            );
            if modal.selection.is_random() {
                if let Some(entry_id) = state.pending_trigger_entry.take() {
                    if state.stack.back().map(|e| e.id) == Some(entry_id) {
                        state.stack.pop_back();
                        state.stack_paid_facts.remove(&entry_id);
                        state.stack_trigger_event_batches.remove(&entry_id);
                    }
                }
                state.pending_trigger = None;
                return Ok(None);
            }

            // CR 700.2b + CR 603.3c: All modes unavailable (previously chosen
            // OR no legal targets) — ability cannot remain on the stack.
            // Under the "push first, choose second" contract, the entry may
            // already have been pushed by `dispatch_pending_trigger_context`;
            // remove it before clearing the cursor. The new flow filters this
            // case BEFORE pushing in the modal branch, so this is normally a
            // dead branch — kept as a defensive cleanup for any
            // delayed-revalidation paths.
            if unavailable_modes.len() >= modal.mode_count {
                if let Some(entry_id) = state.pending_trigger_entry.take() {
                    if state.stack.back().map(|e| e.id) == Some(entry_id) {
                        state.stack.pop_back();
                        state.stack_paid_facts.remove(&entry_id);
                        state.stack_trigger_event_batches.remove(&entry_id);
                    }
                }
                state.pending_trigger = None;
                return Ok(None);
            }

            return Ok(Some(WaitingFor::AbilityModeChoice {
                player,
                modal,
                source_id,
                mode_abilities,
                is_activated: false,
                ability_index: None,
                ability_cost: None,
                unavailable_modes,
            }));
        }
    }

    let ability = trigger.ability.clone();
    // CR 601.2c + CR 603.3d + CR 109.5: a targeted "of their choice" trigger routes
    // target selection to the scoped (upkeep) player, not the source's controller.
    let player = ability
        .target_chooser
        .as_ref()
        .and_then(|f| crate::game::targeting::resolve_effect_player_ref(state, &ability, f))
        .unwrap_or(trigger.controller);
    let source_id = trigger.source_id;
    let target_constraints = trigger.target_constraints.clone();
    let description = trigger.description.clone();
    let trigger_controller = trigger.controller;
    let trigger_event = trigger.trigger_event.clone();
    let trigger_events = if state.pending_trigger_event_batch.is_empty() {
        trigger_event.iter().cloned().collect::<Vec<_>>()
    } else {
        state.pending_trigger_event_batch.clone()
    };
    let subject_match_count = trigger.subject_match_count;
    let context_snapshot = super::triggers::push_trigger_event_context(
        state,
        trigger_event.as_ref(),
        &trigger_events,
        subject_match_count,
    );
    // CR 603.3d: "If a choice is required when the triggered ability goes on the
    // stack but no legal choices can be made for it ... the ability is simply
    // removed from the stack." `build_target_slots` returns `Err` ONLY to report
    // exactly that — every error site in `collect_target_slots` is a
    // `No legal targets available` `ActionNotAllowed`. A targeted trigger's
    // targets can be legal at "push first" dispatch yet become illegal here at
    // "choose second" when an effect earlier in the SAME simultaneous cascade
    // removed the only legal target (e.g. the artifact a Schema Thief token would
    // copy was destroyed by a damage trigger that resolved first). Map that to
    // the no-prompt drop path below — never propagate it and abort the in-flight
    // action, which would leave the game unable to pass priority (a soft-lock
    // freeze). Errors from `begin_target_selection_for_ability` are genuine
    // selection-invariant violations and MUST still propagate (via `?` below).
    let selection_result = match build_target_slots(state, &ability) {
        Ok(target_slots) if !target_slots.is_empty() => {
            begin_target_selection_for_ability(state, &ability, &target_slots, &target_constraints)
                .map(|selection| Some((target_slots, selection)))
        }
        // Empty target slots (no targeting), or CR 603.3d no-legal-target: no
        // prompt is needed/possible — fall through to the removal branch.
        Ok(_) | Err(_) => Ok(None),
    };
    super::triggers::restore_trigger_event_context(state, context_snapshot);
    let Some((target_slots, selection)) = selection_result? else {
        // CR 603.3d: No target prompt is required — empty target slots, or
        // `build_target_slots` reported no legal target at choose-time (mapped to
        // `Ok(None)` above). Symmetric to the modal `all-modes-unavailable`
        // branch above: if the "push first" dispatcher already pushed an
        // in-construction entry for this trigger, pop it before clearing the
        // cursor.
        if let Some(entry_id) = state.pending_trigger_entry.take() {
            if state.stack.back().map(|e| e.id) == Some(entry_id) {
                state.stack.pop_back();
                state.stack_paid_facts.remove(&entry_id);
                state.stack_trigger_event_batches.remove(&entry_id);
            }
        }
        state.pending_trigger = None;
        return Ok(None);
    };
    Ok(Some(WaitingFor::TriggerTargetSelection {
        player,
        trigger_controller: Some(trigger_controller),
        trigger_event,
        trigger_events,
        target_slots,
        mode_labels: Vec::new(),
        target_constraints,
        selection,
        source_id: Some(source_id),
        description,
    }))
}

/// CR 604.2 + CR 110.4: If a land was played from the graveyard via a
/// frequency-bounded permission source, record the appropriate per-turn slot
/// as used to prevent a second play/cast from the same source/slot this turn.
///
/// - `OncePerTurn` (Crucible-of-Worlds-class): record the source in
///   `graveyard_cast_permissions_used`.
/// - `OncePerTurnPerPermanentType` (Muldrotha-class): record the
///   `(source, slot_type)` pair in `graveyard_cast_permissions_used_per_type`.
///   The slot is picked here (not stashed beforehand) because lands take the
///   non-stack play-land path; the picker reads the live used-set so concurrent
///   frequency-bounded permissions are handled correctly.
/// - `Unlimited` (Crucible-of-Worlds-with-no-rider): no tracking.
fn record_graveyard_play_permission(
    state: &mut GameState,
    source: Option<ObjectId>,
    played_object: ObjectId,
) {
    let Some(source_id) = source else {
        return;
    };
    let Some(obj) = state.objects.get(&source_id) else {
        return;
    };
    let frequency =
        super::functioning_abilities::active_static_definitions(state, obj).find_map(|s| {
            match s.mode {
                StaticMode::GraveyardCastPermission { frequency, .. } => Some(frequency),
                _ => None,
            }
        });
    match frequency {
        Some(crate::types::statics::CastFrequency::OncePerTurn) => {
            state.graveyard_cast_permissions_used.insert(source_id);
        }
        Some(crate::types::statics::CastFrequency::OncePerTurnPerPermanentType) => {
            // CR 110.4: Use the player-chosen slot if one was stashed by the
            // ChoosePermanentTypeSlot dispatch (multi-type card). Otherwise
            // auto-pick (single-type card).
            let slot = state
                .pending_permanent_type_slot
                .take()
                .filter(|(src, _)| *src == source_id)
                .map(|(_, ct)| ct)
                .or_else(|| {
                    super::casting::pick_per_permanent_type_slot(state, source_id, played_object)
                });
            if let Some(slot) = slot {
                state
                    .graveyard_cast_permissions_used_per_type
                    .insert((source_id, slot));
            }
        }
        Some(crate::types::statics::CastFrequency::Unlimited) | None => {
            // Unlimited (Crucible of Worlds) or no permission: no tracking.
        }
    }
}

fn record_exile_play_permission(state: &mut GameState, source: Option<ObjectId>) {
    let Some(source_id) = source else {
        return;
    };
    state.exile_play_permissions_used.insert(source_id);
}

/// CR 305.1 + CR 116.2a + CR 401.5: Consume the per-turn slot when a
/// `OncePerTurn` `TopOfLibraryCastPermission { play_mode: Play }` authorizes a
/// land play from the library. Playing a land is a special action (CR 305.1,
/// CR 116.2a) — not a spell cast — so CR 601.2a does not apply here; CR 401.5
/// governs top-of-library visibility during the special action. Receives the
/// pre-captured `(src_id, frequency)` that was resolved BEFORE the zone change
/// — `top_of_library_permission_source` reads `library.front()`, which no
/// longer points to the played land after the land is delivered to the
/// battlefield. `Unlimited` permissions (Future Sight, Bolas's Citadel) do not
/// spend a slot.
fn record_top_of_library_land_permission(
    state: &mut GameState,
    src_id: ObjectId,
    frequency: crate::types::statics::CastFrequency,
) {
    if matches!(frequency, crate::types::statics::CastFrequency::OncePerTurn) {
        state.top_of_library_cast_permissions_used.insert(src_id);
    }
}

fn mark_land_played_from_zone(state: &mut GameState, object_id: ObjectId, zone: Zone) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.played_from_zone = Some(zone);
    }
}

fn record_land_played_from_zone(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    zone: Zone,
) {
    mark_land_played_from_zone(state, object_id, zone);
    state
        .lands_played_this_turn_by_player
        .entry(player)
        .or_default()
        .push_back(LandPlayRecord { from_zone: zone });
}

fn handle_play_land(
    state: &mut GameState,
    acting_player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate main phase
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Can only play lands during main phases".to_string(),
            ));
        }
    }

    // CR 305.2 + CR 505.6b: Validate land limit.
    // Base limit is max_lands_per_turn (normally 1), plus any additional drops
    // from static abilities like Exploration or Azusa.
    //
    // CR 805.4c: "Each player on a team may play a land during each of that
    // team's turns" — under the shared team turns option, the nonactive
    // teammate plays from their OWN hand against their OWN once-per-turn
    // allowance, not the turn's nominal resource owner (`active_player`).
    // `turn_resource_owner` stays correct for turn-control effects (CR 723,
    // e.g. Mindslaver), which always act on the active player's own
    // resources regardless of who submits the choice — that path is
    // unaffected since it never uses shared team turns.
    let player = if state.format_config.topology().has_shared_team_turns() {
        if !super::topology::team_members(state, state.active_player).contains(&acting_player) {
            return Err(EngineError::ActionNotAllowed(
                "Only the active team may play lands during its turn".to_string(),
            ));
        }
        acting_player
    } else {
        turn_control::turn_resource_owner(state)
    };
    // CR 305.2: "Can't play lands" suppresses the play-land special action outright.
    if super::static_abilities::player_has_static_other(state, player, "CantPlayLand") {
        return Err(EngineError::ActionNotAllowed(
            "Player is under a CantPlayLand static (CR 305.2)".to_string(),
        ));
    }
    // CR 116.2a + CR 305.1: A `ProhibitPlayFromZone` deny covers the play-land
    // half of "play" (a land play is a special action, not a cast), so this gate
    // is the land-side counterpart to the cast-gate check in
    // `casting::prepare_spell_cast` (Memory Vessel: "can't play cards from their
    // hand"). The card's current zone is the discriminator.
    if let Some(obj) = state.objects.get(&object_id) {
        if super::casting::is_blocked_by_prohibit_play_from_zone(state, obj, player) {
            return Err(EngineError::ActionNotAllowed(
                "A temporary effect prevents playing cards from this zone (CR 116.2a)".to_string(),
            ));
        }
    }
    let additional = super::static_abilities::additional_land_drops(state, player);
    let effective_limit = state.max_lands_per_turn.saturating_add(additional);
    // CR 805.4c: per-player land count under team turns (each teammate has
    // their own allowance); the legacy single-counter `lands_played_this_turn`
    // is correct outside team-based formats, where only the active player
    // ever plays lands during their own turn.
    let lands_played = if state.format_config.topology().has_shared_team_turns() {
        state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.lands_played_this_turn)
            .unwrap_or(0)
    } else {
        state.lands_played_this_turn
    };
    if lands_played >= effective_limit {
        return Err(EngineError::ActionNotAllowed(
            "Already played maximum lands this turn".to_string(),
        ));
    }

    // Validate that object_id exists in hand or graveyard (with permission)
    // or on top of library (with TopOfLibraryCastPermission { play_mode: Play })
    // and matches card_id.
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("priority player exists");
    let in_hand = player_data.hand.contains(&object_id);
    // CR 305.1 + CR 604.2: Check graveyard for play-from-graveyard permission
    // CR 604.2: Find graveyard play permission source (if any) for once-per-turn tracking.
    let gy_permission_source = if player_data.graveyard.contains(&object_id) {
        super::casting::graveyard_lands_playable_by_permission(state, player)
            .iter()
            .find(|(obj_id, _)| *obj_id == object_id)
            .map(|(_, source_id)| *source_id)
    } else {
        None
    };
    let in_graveyard_with_permission = gy_permission_source.is_some();

    // CR 401.5 + CR 305.1: Check top of library for
    // `TopOfLibraryCastPermission { play_mode: Play }` (Future Sight,
    // Bolas's Citadel, Magus of the Future, The Fourth Doctor).
    //
    // IMPORTANT: capture (src_id, frequency) HERE — before the zone change.
    // `top_of_library_permission_source` reads `library.front()`, which will
    // point to the next card once the land is delivered to the battlefield.
    // Recording in the post-delivery epilogue would always see the wrong top
    // card and silently skip the once-per-turn slot, allowing a OncePerTurn
    // permission to be reused indefinitely. CR 305.1 + CR 116.2a + CR 401.5:
    // land play is a special action, not a spell cast (CR 601.2a does not apply).
    let library_permission_src: Option<(ObjectId, crate::types::statics::CastFrequency)> =
        super::casting::top_of_library_permission_source(
            state,
            player,
            Some(crate::types::ability::CardPlayMode::Play),
        )
        .and_then(|(top_id, src_id, frequency, _)| {
            if top_id != object_id {
                return None;
            }
            // CR 305.1: only land cards qualify for the Play-permission path.
            let obj = state.objects.get(&top_id)?;
            if !obj
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
            {
                return None;
            }
            Some((src_id, frequency))
        });
    let in_library_with_permission = library_permission_src.is_some();
    let exile_permission_source = if state.exile.contains(&object_id) {
        super::casting::exile_lands_playable_by_permission(state, player)
            .iter()
            .find(|(obj_id, _)| *obj_id == object_id)
            .map(|(_, source_id)| *source_id)
    } else {
        None
    };
    let in_exile_with_permission = exile_permission_source.is_some();

    if !in_hand
        && !in_graveyard_with_permission
        && !in_library_with_permission
        && !in_exile_with_permission
    {
        return Err(EngineError::InvalidAction(
            "Card not found in hand, graveyard, exile, or library with play permission".to_string(),
        ));
    }
    if !state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.card_id == card_id)
    {
        return Err(EngineError::InvalidAction(
            "Card not found or card_id mismatch".to_string(),
        ));
    }

    // CR 110.4: For multi-type graveyard lands via OncePerTurnPerPermanentType,
    // prompt the player to choose which permanent type slot to consume. Skip
    // if a slot was already chosen (pending_permanent_type_slot is set).
    if in_graveyard_with_permission && state.pending_permanent_type_slot.is_none() {
        if let Some(source) = gy_permission_source {
            if let Some(src_obj) = state.objects.get(&source) {
                let is_per_type = super::functioning_abilities::active_static_definitions(
                    state, src_obj,
                )
                .any(|s| {
                    matches!(
                        s.mode,
                        StaticMode::GraveyardCastPermission {
                            frequency:
                                crate::types::statics::CastFrequency::OncePerTurnPerPermanentType,
                            ..
                        }
                    )
                });
                if is_per_type {
                    let slots =
                        super::casting::available_permanent_type_slots(state, source, object_id);
                    if slots.len() > 1 {
                        return Ok(WaitingFor::ChoosePermanentTypeSlot {
                            player,
                            object_id,
                            card_id,
                            source,
                            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
                            available_slots: slots,
                        });
                    }
                }
            }
        }
    }

    // CR 712.12: MDFC land face selection
    if let Some(obj) = state.objects.get(&object_id) {
        let is_modal = obj
            .back_face
            .as_ref()
            .is_some_and(|bf| bf.layout_kind == Some(crate::types::card::LayoutKind::Modal));
        let front_is_land = obj
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land);
        let back_is_land = obj.back_face.as_ref().is_some_and(|bf| {
            bf.card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
        });

        if is_modal && front_is_land && back_is_land {
            // Both faces are lands — player must choose which face to put into play.
            // The land path never consumes payment_mode (lands cost no mana), but
            // the field is required; Auto is the inert default.
            return Ok(WaitingFor::ModalFaceChoice {
                player,
                object_id,
                card_id,
                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            });
        }

        if is_modal && !front_is_land && back_is_land {
            // CR 712.12: Only back face is a land — auto-swap (player already chose "play as land")
            let obj = state.objects.get_mut(&object_id).unwrap();
            let back = obj.back_face.take().expect("MDFC has back face");
            let front_snapshot = super::printed_cards::snapshot_object_face(obj);
            super::printed_cards::apply_back_face_to_object(obj, back);
            obj.back_face = Some(front_snapshot);
            // CR 712.8a: Mark back-face so apply_zone_exit_cleanup reverts to front face
            // when this land leaves the battlefield. Do NOT set obj.transformed — MDFC
            // face selection is not transformation.
            obj.modal_back_face = true;
        }
    }

    // Determine origin zone for the zone change event
    let origin_zone = if in_hand {
        Zone::Hand
    } else if in_graveyard_with_permission {
        Zone::Graveyard
    } else if in_exile_with_permission {
        Zone::Exile
    } else {
        // CR 401.5: in_library_with_permission — the card moves Library → Battlefield.
        Zone::Library
    };

    // Route through the replacement pipeline (handles ETB replacements like shock lands)
    let mut proposed = crate::types::proposed_event::ProposedEvent::zone_change(
        object_id,
        origin_zone,
        Zone::Battlefield,
        None,
    );

    // CR 110.2 + CR 110.2a (GitHub #696): A played land's controller
    // defaults to whoever played it, not the card's owner. `player` is the
    // acting land-player already resolved above (turn_resource_owner, or
    // acting_player under shared team turns) — the same identity already
    // used throughout this function for hand/zone lookups, and the correct
    // one even under Mindslaver-style turn control (the turn's rightful
    // player controls what gets played on their turn, not whoever is
    // making the decisions). This is a no-op for the overwhelmingly common
    // owner==player case. A genuine self-ETB "enters under [X]'s control"
    // replacement (enters_under) still wins — it runs later in the same
    // replacement pipeline this event is routed through below, and
    // hard-overwrites this default unconditionally (identical safety
    // property to the stack.rs spell-cast seam this mirrors).
    if let crate::types::proposed_event::ProposedEvent::ZoneChange {
        controller_override,
        ..
    } = &mut proposed
    {
        *controller_override = Some(player);
    }

    // CR 306.5b + CR 310.4b + CR 614.1c: Seed the intrinsic "enters with N
    // counters" replacement for planeswalkers and battles entering the
    // battlefield via a play-from-zone action.
    if let Some(obj) = state.objects.get(&object_id) {
        let intrinsic = super::printed_cards::intrinsic_etb_counters(obj);
        if !intrinsic.is_empty() {
            if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                enter_with_counters,
                ..
            } = &mut proposed
            {
                enter_with_counters.extend(intrinsic);
            }
        }
    }

    // CR 614.1c: A land played via a `PlayFromExile` grant that carries
    // `land_enter_tapped` enters the battlefield tapped (Lightstall Inquisitor:
    // "Each land played this way enters tapped."). Seed the tap state on the
    // proposed event so the replacement pipeline applies it like any other
    // ETB-tapped land. Only the exile-play path can carry this grant field.
    if in_exile_with_permission {
        let enters_tapped = state
            .objects
            .get(&object_id)
            .is_some_and(|obj| super::casting::exile_play_land_enters_tapped(obj, player));
        if enters_tapped {
            if let Some(slot) = proposed.battlefield_entry_tap_state_mut() {
                *slot = crate::types::zones::EtbTapState::Tapped;
            }
        }
    }

    match super::replacement::replace_event(state, proposed, events) {
        super::replacement::ReplacementResult::Execute(event) => {
            if let crate::types::proposed_event::ProposedEvent::ZoneChange { object_id, .. } = event
            {
                // Phase B (PLAN §6.2 / §7): the divergent partial copy of
                // `deliver_replaced_zone_change` that used to live here is
                // dissolved — the post-`replace_event` event is a
                // `ReplacementResult::Execute` payload, sealed through the third
                // mint path (`approve_post_replacement`) and delivered by the
                // shared `zone_pipeline::deliver`. The land entry now gets the
                // FULL delivery tail the copy skipped (CR 614.1c
                // `EntersWithAdditionalCounters` statics snapshot, the CR 303.4f
                // `attach_to` host, `entered_via_ability_source` provenance, the
                // CR 701.24a library-shuffle arm). `drain = CallerEpilogue`: the
                // land-play epilogue below owns the `post_replacement_continuation`
                // drain (it clears `post_replacement_source` and runs the
                // land-specific accounting), so the tail must not also drain it.
                let Ok(approved) =
                    crate::game::zone_pipeline::ApprovedZoneChange::approve_post_replacement(event)
                else {
                    unreachable!("`if let ZoneChange` guarantees a ZoneChange payload");
                };
                match crate::game::zone_pipeline::deliver(
                    state,
                    approved,
                    crate::game::zone_pipeline::DeliveryCtx {
                        source_id: None,
                        exile_links: crate::game::zone_pipeline::ExileLinkSpec::default(),
                        drain: crate::types::game_state::PostReplacementDrainOwner::CallerEpilogue,
                        // This resume delivery is not a library placement.
                        library_placement: None,
                    },
                    events,
                ) {
                    crate::game::zone_pipeline::ZoneDeliveryResult::Done => {}
                    // CR 614.1c / CR 614.12a: the delivery tail parked a
                    // counter-replacement prompt and stashed the remaining tail
                    // (carrying `CallerEpilogue`). The land has already entered
                    // the battlefield (the move precedes the counter pause in the
                    // tail), so stamp the play origin now — matching the pre-token
                    // arm, which stamped before the `apply_etb_counters`
                    // early-return — then surface the parked prompt; the land
                    // epilogue must not run yet.
                    crate::game::zone_pipeline::ZoneDeliveryResult::NeedsChoice(_) => {
                        // CR 305.1 + CR 400.7i: stamp land-play provenance so
                        // effects can find the permanent the played land became.
                        mark_land_played_from_zone(state, object_id, origin_zone);
                        return Ok(state.waiting_for.clone());
                    }
                }
                // CR 305.1 + CR 400.7i: stamp land-play provenance ("where it
                // was played from") so effects can find the permanent the
                // played land became. Stamped fresh AFTER delivery (this site
                // records a brand-new origin); the stamp then survives until
                // battlefield EXIT (`reset_for_battlefield_exit`).
                mark_land_played_from_zone(state, object_id, origin_zone);
            }

            // CR 614.12a: Drain post-replacement side effects (e.g., "As this land
            // enters, choose a color") that were stashed by the pipeline when the
            // execute ability is non-modifier work (Choose, etc.). Without this,
            // the choice prompt would fire at a random later resolution point with
            // the wrong controller context.
            if state.has_post_replacement_drain() {
                state.clear_post_replacement_source();
                if let Some(next_waiting_for) =
                    engine_replacement::apply_pending_post_replacement_effect(
                        state,
                        Some(object_id),
                        None,
                        Some(crate::types::replacements::ReplacementEvent::Moved),
                        events,
                    )
                {
                    state.lands_played_this_turn += 1;
                    record_land_played_from_zone(state, player, object_id, origin_zone);
                    record_graveyard_play_permission(state, gy_permission_source, object_id);
                    record_exile_play_permission(state, exile_permission_source);
                    // CR 305.1 + CR 116.2a + CR 401.5: consume the once-per-turn
                    // library play slot using the pre-captured source (land play is
                    // a special action per CR 305.1/116.2a; CR 401.5 top-of-library
                    // visibility closes after the action; library.front() now points
                    // to the next card, not the played land).
                    if let Some((src_id, frequency)) = library_permission_src {
                        record_top_of_library_land_permission(state, src_id, frequency);
                    }
                    if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
                        p.lands_played_this_turn += 1;
                    }
                    priority::clear_priority_passes(state);
                    events.push(GameEvent::LandPlayed {
                        object_id,
                        player_id: player,
                        from_zone: origin_zone,
                    });
                    return Ok(next_waiting_for);
                }
            }
        }
        super::replacement::ReplacementResult::Prevented => {
            // Land play was prevented — don't increment counters
            return Ok(WaitingFor::Priority {
                player: state.priority_player,
            });
        }
        super::replacement::ReplacementResult::NeedsChoice(player) => {
            // A replacement needs player choice (e.g., shock land "pay 2 life?").
            // Increment counters now — the land play is committed, only the ETB
            // effect is pending.
            state.lands_played_this_turn += 1;
            record_land_played_from_zone(state, player, object_id, origin_zone);
            // CR 604.2: Record once-per-turn graveyard play permission usage.
            record_graveyard_play_permission(state, gy_permission_source, object_id);
            record_exile_play_permission(state, exile_permission_source);
            // CR 305.1 + CR 116.2a + CR 401.5: consume the once-per-turn library
            // play slot using the pre-captured source (land play is a special
            // action per CR 305.1/116.2a; CR 401.5 top-of-library visibility
            // closes after the action; library.front() now points to the next
            // card, not the played land).
            if let Some((src_id, frequency)) = library_permission_src {
                record_top_of_library_land_permission(state, src_id, frequency);
            }
            if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
                p.lands_played_this_turn += 1;
            }
            priority::clear_priority_passes(state);

            events.push(GameEvent::LandPlayed {
                object_id,
                player_id: player,
                from_zone: origin_zone,
            });

            return Ok(super::replacement::replacement_choice_waiting_for(
                player, state,
            ));
        }
    }

    // Increment land counter
    state.lands_played_this_turn += 1;
    record_land_played_from_zone(state, player, object_id, origin_zone);
    // CR 604.2: Record once-per-turn graveyard play permission usage.
    record_graveyard_play_permission(state, gy_permission_source, object_id);
    record_exile_play_permission(state, exile_permission_source);
    // CR 305.1 + CR 116.2a + CR 401.5: consume the once-per-turn library play
    // slot using the pre-captured source (land play is a special action per
    // CR 305.1/116.2a; CR 401.5 top-of-library visibility closes after the
    // action; library.front() now points to the next card, not the played
    // land — post-delivery re-lookup would fail).
    if let Some((src_id, frequency)) = library_permission_src {
        record_top_of_library_land_permission(state, src_id, frequency);
    }
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("priority player exists");
    player_data.lands_played_this_turn += 1;

    // Reset priority passes (action was taken)
    priority::clear_priority_passes(state);

    events.push(GameEvent::LandPlayed {
        object_id,
        player_id: player,
        from_zone: origin_zone,
    });

    // Player retains priority after playing a land
    Ok(WaitingFor::Priority { player })
}

pub(super) fn handle_tap_land_for_mana(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    // CR 117.1d + CR 605.3a: the player with priority, or the player making a
    // mana payment, activates their own mana abilities even during another
    // player's turn.
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Object is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::NotYourPriority);
    }
    if !obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return Err(EngineError::InvalidAction(
            "Object is not a land".to_string(),
        ));
    }
    if obj.tapped {
        return Err(EngineError::InvalidAction(
            "Land is already tapped".to_string(),
        ));
    }

    let mana_options = mana_sources::activatable_land_mana_options(state, object_id, player);
    if mana_options.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "Land has no activatable mana ability".to_string(),
        ));
    }
    // Lands with multiple mana options (dual lands, triomes, etc.) must use
    // ActivateAbility with a specific ability_index to select which color.
    // TapLandForMana is a convenience shortcut for single-option lands only.
    if mana_options.len() > 1 {
        return Err(EngineError::ActionNotAllowed(
            "Land has multiple mana options — use ActivateAbility to choose".to_string(),
        ));
    }
    let mana_option = mana_options.into_iter().next().unwrap();

    let ability_to_resolve = mana_option.ability_index.and_then(|ability_index| {
        state
            .objects
            .get(&object_id)
            .and_then(|land| land.abilities.get(ability_index))
            .cloned()
    });

    if let Some(ability_def) = ability_to_resolve {
        mana_abilities::resolve_mana_ability(state, object_id, player, &ability_def, events, None)?;
        // CR 605.3b: Only record for `UntapLandForMana` when the activation is
        // fully reversible — painlands / pay-life sources commit irreversible
        // state during inline resolution and must not be eligible for undo.
        if mana_option.penalty.is_undoable() {
            state
                .lands_tapped_for_mana
                .entry(player)
                .or_default()
                .push(object_id);
        }
    } else {
        // Legacy fallback for subtype-only lands.
        let obj = state.objects.get_mut(&object_id).unwrap();
        obj.tapped = true;
        events.push(GameEvent::PermanentTapped {
            object_id,
            caused_by: None,
        });
        mana_payment::produce_mana(
            state,
            object_id,
            mana_option.mana_type,
            player,
            true,
            events,
        );
        // CR 106.12 + CR 106.12a: a basic/subtype-only land's intrinsic mana
        // ability always includes `{T}`. Emit one `TappedForMana` per
        // resolution so `TapsForMana` triggers fire exactly once (mirrors the
        // ability-resolution path in `produce_mana_from_ability`).
        events.push(GameEvent::TappedForMana {
            player_id: player,
            source_id: object_id,
            produced: vec![mana_option.mana_type],
            tap_state: crate::types::events::ManaTapState::FromTap,
        });
        state
            .lands_tapped_for_mana
            .entry(player)
            .or_default()
            .push(object_id);
    }

    Ok(WaitingFor::Priority { player })
}

/// CR 605.3b: Reverse a manual land tap — untap source and remove its mana from pool.
/// Rejects if the land isn't tracked or its mana was already spent.
pub(super) fn handle_untap_land_for_mana(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    // Validate: object_id is in this player's lands_tapped_for_mana
    let tracked = state
        .lands_tapped_for_mana
        .get(&player)
        .is_some_and(|ids| ids.contains(&object_id));
    if !tracked {
        return Err(EngineError::InvalidAction(
            "Land was not manually tapped for mana".to_string(),
        ));
    }

    // CR 605.3: Mana abilities resolve immediately — once consumed, irreversible.
    // CR 605.1b: Aura/Equipment with a `TapsForMana` trigger that fired off this
    // land's tap (Fertile Ground / Wild Growth / Utopia Sprawl / Trace of
    // Abundance / Verdant Haven / Market Festival / Weirding Wood / Overgrowth
    // class) added their bonus mana to the same pool with `source_id = aura_id`,
    // not `source_id = land_id`. Refunding only the land's source would strand
    // the aura's mana in the pool, allowing an infinite tap-untap-tap exploit
    // (each cycle adds one bonus, refund only takes the land's mana). Walk every
    // active TapsForMana trigger whose `valid_card` matches the land and refund
    // mana keyed at the trigger's source object too. This preserves CR 605.3b
    // (mana abilities resolve immediately) — the manual-untap convenience is the
    // single irreversibility-bypass channel and must reverse all coupled mana,
    // not just the land's own contribution.
    let aura_sources: Vec<ObjectId> =
        super::mana_sources::aura_taps_for_mana_sources_for_land(state, object_id, player);
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    let removed = player_data.mana_pool.remove_from_source(object_id);
    if removed == 0 {
        return Err(EngineError::InvalidAction(
            "Mana from this source was already spent".to_string(),
        ));
    }
    for aura_id in &aura_sources {
        player_data.mana_pool.remove_from_source(*aura_id);
    }

    // CR 118.3a: an UntapLandForMana during ManaPayment can drain a pinned unit
    // out of the pool. Prune any dangling pins so the finalize spend never tries
    // to honor a pip that no longer exists. Done AFTER the `player_data` borrow
    // above ends so the immutable pool read and the `pending_cast` mutation don't
    // overlap a live `&mut`.
    if state.pending_cast.is_some() {
        let surviving: std::collections::HashSet<crate::types::mana::ManaPipId> = state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.mana_pool.mana.iter().map(|u| u.pip_id).collect())
            .unwrap_or_default();
        if let Some(pc) = state.pending_cast.as_mut() {
            pc.pinned_pool_units.retain(|id| surviving.contains(id));
        }
    }

    // Untap the land
    let obj = state
        .objects
        .get_mut(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    obj.tapped = false;
    events.push(GameEvent::PermanentUntapped { object_id });

    // Remove from tracking
    if let Some(ids) = state.lands_tapped_for_mana.get_mut(&player) {
        ids.retain(|&id| id != object_id);
        if ids.is_empty() {
            state.lands_tapped_for_mana.remove(&player);
        }
    }

    Ok(())
}

/// CR 118.3a: Record a player-directed pin on a specific pool unit so the
/// finalize spend prefers it. The unit stays in the pool — this is a priority
/// hint, not a removal. A pin is accepted only when the unit is eligible to pay
/// at least one shard (or a generic pip) of the full locked cost; otherwise the
/// pin could never be honored, so it is rejected (`ActionNotAllowed`).
pub(super) fn handle_spend_pool_mana(
    state: &mut GameState,
    player: PlayerId,
    pip_id: crate::types::mana::ManaPipId,
) -> Result<(), EngineError> {
    // The unit must currently exist in the player's pool.
    let unit = state
        .players
        .iter()
        .find(|p| p.id == player)
        .and_then(|p| p.mana_pool.mana.iter().find(|u| u.pip_id == pip_id))
        .cloned()
        .ok_or_else(|| {
            EngineError::ActionNotAllowed("No such mana unit in pool to pin".to_string())
        })?;

    let pending = state.pending_cast.as_ref().ok_or_else(|| {
        EngineError::ActionNotAllowed("No pending cast to pin mana for".to_string())
    })?;
    let object_id = pending.object_id;
    let cost = pending.cost.clone();
    let activation_ability_index = pending.activation_ability_index;

    // CR 118.3a: eligibility against the full LOCKED cost. Nothing is paid at pin
    // time, so there is no "currently-unpaid" subset — the unit qualifies if it
    // could pay any shard (or generic pip) of the whole cost under the SAME
    // spend-restriction context the finalize spend will use. A `pending_cast`
    // can be an activated ability, not just a spell (CR 602): mirror
    // `finalize_mana_payment` and build a `PaymentContext::Activation` so an
    // activation-restricted unit (`OnlyForActivation`, `allows_spell == false`)
    // is correctly eligible to pin when it can legally pay the activation.
    // Owned holders so the context's borrowed slices outlive the eligibility check.
    let spell_meta;
    let source_types;
    let source_subtypes;
    let ability_tag;
    let ctx = if let Some(ability_index) = activation_ability_index {
        let (types, subtypes) = super::casting::activation_source_types(state, object_id);
        source_types = types;
        source_subtypes = subtypes;
        ability_tag = super::casting::activation_ability_tag(state, object_id, ability_index);
        Some(crate::types::mana::PaymentContext::Activation {
            source_types: &source_types,
            source_subtypes: &source_subtypes,
            ability_tag,
        })
    } else {
        spell_meta = super::casting::build_spell_meta(state, player, object_id);
        spell_meta
            .as_ref()
            .map(crate::types::mana::PaymentContext::Spell)
    };

    if !mana_unit_eligible_for_cost(&unit, &cost, ctx.as_ref()) {
        return Err(EngineError::ActionNotAllowed(
            "Mana unit cannot pay any part of this cost".to_string(),
        ));
    }

    if let Some(pc) = state.pending_cast.as_mut() {
        if !pc.pinned_pool_units.contains(&pip_id) {
            pc.pinned_pool_units.push(pip_id);
        }
    }
    Ok(())
}

/// CR 118.3a: Remove a previously-recorded pin. Always legal — a no-op if the
/// pin is absent or there is no pending cast.
pub(super) fn handle_unspend_pool_mana(
    state: &mut GameState,
    pip_id: crate::types::mana::ManaPipId,
) {
    if let Some(pc) = state.pending_cast.as_mut() {
        pc.pinned_pool_units.retain(|id| *id != pip_id);
    }
}

/// CR 118.3a: True when `unit` could legally pay at least one shard or generic
/// pip of `cost` under the spell's spend-restriction context. Combines
/// restriction gating (`ManaRestriction::allows`) with shard color/attribute
/// matching (`shard_to_mana_type`) — the same predicates the spend funnel uses.
fn mana_unit_eligible_for_cost(
    unit: &crate::types::mana::ManaUnit,
    cost: &crate::types::mana::ManaCost,
    ctx: Option<&crate::types::mana::PaymentContext<'_>>,
) -> bool {
    use crate::types::mana::{ManaCost, ManaType};
    use mana_payment::ShardRequirement;

    // CR 106.6: a unit whose restrictions reject this context can pay nothing here.
    if let Some(ctx) = ctx {
        if !unit.restrictions.iter().all(|r| r.allows(ctx)) {
            return false;
        }
    }
    // Convoke/improvise/delve markers are creature-tap stand-ins, never pinned.
    if unit.is_convoke_payment() {
        return false;
    }

    let (shards, generic) = match cost {
        ManaCost::Cost { shards, generic } => (shards, *generic),
        // No-cost / self-referential costs have no payable pip.
        _ => return false,
    };

    // CR 107.4b: any unit can pay a generic pip ({N} or {X}).
    if generic > 0 {
        return true;
    }

    shards.iter().any(|&shard| {
        // CR 107.4: a unit pays a shard if its color (or attribute, for {S}/{Z})
        // is among those the shard accepts.
        let accepts = |c: ManaType| unit.color == c;
        match mana_payment::shard_to_mana_type(shard) {
            ShardRequirement::Single(mt) => accepts(mt),
            ShardRequirement::Hybrid(a, b) => accepts(a) || accepts(b),
            ShardRequirement::Phyrexian(c) => accepts(c),
            ShardRequirement::HybridPhyrexian(a, b) => accepts(a) || accepts(b),
            // {2/C} and {C/color}: payable with the color, or (for {2/C}) generic.
            ShardRequirement::TwoGenericHybrid(c) => accepts(c),
            ShardRequirement::ColorlessHybrid(c) => accepts(ManaType::Colorless) || accepts(c),
            ShardRequirement::Snow => unit.is_snow(),
            ShardRequirement::TwoOrMoreColorSource => unit.source_could_produce_two_or_more_colors,
            // {X} contributes nothing off the stack (CR 107.3); generic-payable
            // when X > 0 is already covered by the `generic` check above.
            ShardRequirement::X => false,
            ShardRequirement::TwoGenericHybridPhyrexian(c) => accepts(c),
        }
    })
}

fn handle_equip_activation(
    state: &mut GameState,
    player: PlayerId,
    equipment_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate sorcery-speed timing: main phase, empty stack, active player
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Equip can only be activated during main phases".to_string(),
            ));
        }
    }
    if !state.stack.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated when the stack is empty".to_string(),
        ));
    }
    if state.active_player != player {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated by the active player".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&equipment_id)
        .ok_or_else(|| EngineError::InvalidAction("Equipment not found".to_string()))?;

    // Validate it's an equipment on the battlefield controlled by player
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Equipment is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this equipment".to_string(),
        ));
    }
    if !obj.card_types.subtypes.contains(&"Equipment".to_string()) {
        return Err(EngineError::InvalidAction(
            "Object is not an equipment".to_string(),
        ));
    }

    // Find valid targets: creatures controlled by the equipping player on battlefield
    let valid_targets: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|o| {
                    o.controller == player
                        && o.card_types
                            .core_types
                            .contains(&crate::types::card_type::CoreType::Creature)
                })
                .unwrap_or(false)
        })
        .collect();

    if valid_targets.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "No valid creatures to equip".to_string(),
        ));
    }

    // If only one target, auto-equip: CR 113.3b still requires the stack entry
    // + priority window; we skip only the target-selection UI.
    if valid_targets.len() == 1 {
        let target_id = valid_targets[0];
        return Ok(push_keyword_action(
            state,
            player,
            equipment_id,
            KeywordAction::Equip {
                equipment_id,
                target_creature_id: target_id,
            },
            events,
        ));
    }

    priority::clear_priority_passes(state);
    Ok(WaitingFor::EquipTarget {
        player,
        equipment_id,
        valid_targets,
    })
}

/// CR 702.122a: Activate a Vehicle's crew ability from Priority.
/// Unlike Equip (CR 702.6a) and Saddle (CR 702.171a), Crew has NO "Activate only as a
/// sorcery" restriction — it can be activated any time the controller has priority.
fn is_tappable_creature_for_cost(state: &GameState, id: ObjectId, player: PlayerId) -> bool {
    state.objects.get(&id).is_some_and(|o| {
        o.controller == player
            && !o.tapped
            && o.card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Creature)
            && !crate::game::restrictions::object_cant_tap(state, id)
    })
}

/// CR 602.5b + CR 702.122a: "activate only once each turn" is keyed to the exact
/// object incarnation, so a Vehicle that leaves and returns (a new object per
/// CR 400.7) may be crewed again. Single authority for reading the crew-cadence
/// set — callers never touch `crew_activated_this_turn` directly.
pub(crate) fn crew_activated_this_turn_contains(state: &GameState, vehicle_id: ObjectId) -> bool {
    state
        .objects
        .get(&vehicle_id)
        .map(crate::types::identifiers::ObjectIncarnationRef::from_object)
        .is_some_and(|r| state.crew_activated_this_turn.contains(&r))
}

/// CR 602.5b + CR 702.122a: record a crew activation against the Vehicle's current
/// incarnation. Single authority for writing the crew-cadence set.
pub(crate) fn record_crew_activation(state: &mut GameState, vehicle_id: ObjectId) {
    if let Some(r) = state
        .objects
        .get(&vehicle_id)
        .map(crate::types::identifiers::ObjectIncarnationRef::from_object)
    {
        state.crew_activated_this_turn.insert(r);
    }
}

fn handle_crew_activation(
    state: &mut GameState,
    player: PlayerId,
    vehicle_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&vehicle_id)
        .ok_or_else(|| EngineError::InvalidAction("Vehicle not found".to_string()))?;

    // Validate it's a Vehicle on the battlefield controlled by player
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Vehicle is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this Vehicle".to_string(),
        ));
    }
    if !obj.card_types.subtypes.contains(&"Vehicle".to_string()) {
        return Err(EngineError::InvalidAction(
            "Object is not a Vehicle".to_string(),
        ));
    }

    // Extract crew power and once-each-turn cadence from keywords.
    let (crew_power, crew_once_per_turn) = obj
        .keywords
        .iter()
        .find_map(|kw| {
            if let crate::types::keywords::Keyword::Crew {
                power,
                once_per_turn,
            } = kw
            {
                // CR 602.5b: once_per_turn is `Some(OnlyOnceEachTurn)` when the
                // Vehicle's crew ability is limited to once each turn.
                let limited = matches!(
                    once_per_turn.as_deref(),
                    Some(crate::types::ability::ActivationRestriction::OnlyOnceEachTurn)
                );
                Some((*power, limited))
            } else {
                None
            }
        })
        .ok_or_else(|| EngineError::InvalidAction("Vehicle has no Crew keyword".to_string()))?;

    // CR 602.5b: "Activate only once each turn" — reject a second crew activation
    // of this Vehicle in the same turn.
    if crew_once_per_turn && crew_activated_this_turn_contains(state, vehicle_id) {
        return Err(EngineError::ActionNotAllowed(
            "This Vehicle's crew ability can be activated only once each turn".to_string(),
        ));
    }

    // CR 702.122d: Exclude creatures with "can't crew Vehicles".
    let eligible_creatures: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            id != vehicle_id
                && is_tappable_creature_for_cost(state, id, player)
                && !super::static_abilities::object_has_cant_crew(state, id)
        })
        .collect();

    // Validate total power of all eligible creatures can meet the threshold.
    // CR 702.122a: a creature's contribution may be modified ("as though its
    // power were N greater" / "using its toughness rather than its power"). The
    // per-creature contributions travel with the choice so the UI gates the
    // selection on the same adjusted values the engine validates against, rather
    // than re-deriving from raw power.
    let contributions: Vec<i32> = eligible_creatures
        .iter()
        .map(|&id| {
            super::static_abilities::object_crew_power_contribution(
                state,
                id,
                crate::types::statics::CrewAction::Crew,
            )
        })
        .collect();
    let total_power: i32 = contributions.iter().sum();

    if total_power < crew_power as i32 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough total power among eligible creatures to crew".to_string(),
        ));
    }

    let _ = events; // No events emitted during activation
    priority::clear_priority_passes(state);
    Ok(WaitingFor::CrewVehicle {
        player,
        vehicle_id,
        crew_power,
        eligible_creatures,
        contributions,
    })
}

/// CR 113.3b: Push an activated keyword ability onto the stack and reset
/// priority. Called by the *_announcement handlers after costs have been paid
/// and targets selected. The payload is resolved via `stack::resolve_top`
/// once all players pass priority.
fn push_keyword_action(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    action: KeywordAction,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    super::stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id,
            controller: player,
            kind: StackEntryKind::KeywordAction { action },
        },
        events,
    );
    priority::clear_priority_passes(state);
    WaitingFor::Priority { player }
}

/// CR 702.122a + CR 113.3b: Announce a Vehicle's crew ability. Pays the cost
/// (tap selected creatures) and pushes a `KeywordAction::Crew` stack entry.
/// The Vehicle animation happens at stack resolution, not here — opening a
/// priority window for counterspell-class effects (CR 113.3b).
fn handle_crew_announcement(
    state: &mut GameState,
    player: PlayerId,
    vehicle_id: ObjectId,
    crew_power: u32,
    eligible_creatures: &[ObjectId],
    creature_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if creature_ids.is_empty() {
        return Err(EngineError::InvalidAction(
            "Must select at least one creature to crew".to_string(),
        ));
    }

    // Validate Vehicle is still on battlefield and controlled by player
    let vehicle = state
        .objects
        .get(&vehicle_id)
        .ok_or_else(|| EngineError::InvalidAction("Vehicle no longer exists".to_string()))?;
    if vehicle.zone != Zone::Battlefield || vehicle.controller != player {
        return Err(EngineError::InvalidAction(
            "Vehicle is no longer valid for crewing".to_string(),
        ));
    }

    // Validate all creature_ids are in eligible_creatures
    for &cid in creature_ids {
        if !eligible_creatures.contains(&cid) {
            return Err(EngineError::InvalidAction(
                "Creature not in eligible list".to_string(),
            ));
        }
    }

    // Re-validate and read power of each creature BEFORE tapping (HarmonizeTap idiom)
    let mut total_power: i32 = 0;
    for &cid in creature_ids {
        let obj = state
            .objects
            .get(&cid)
            .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".to_string()))?;
        if obj.zone != Zone::Battlefield || obj.tapped {
            return Err(EngineError::InvalidAction(
                "Creature is no longer eligible for crewing".to_string(),
            ));
        }
        if crate::game::restrictions::object_cant_tap(state, cid) {
            return Err(EngineError::InvalidAction(
                "Creature can't become tapped".to_string(),
            ));
        }
        if super::static_abilities::object_has_cant_crew(state, cid) {
            return Err(EngineError::InvalidAction(
                "Creature can't crew Vehicles".to_string(),
            ));
        }
        // CR 702.122a: apply any crew power-contribution modifier.
        total_power += super::static_abilities::object_crew_power_contribution(
            state,
            cid,
            crate::types::statics::CrewAction::Crew,
        );
    }

    // CR 702.122a: Total power must meet threshold
    if total_power < crew_power as i32 {
        return Err(EngineError::InvalidAction(
            "Selected creatures' total power is less than crew requirement".to_string(),
        ));
    }

    // CR 701.26a + CR 702.122b + CR 508.1f: Tap each creature as cost payment —
    // creature "crews" the Vehicle. Routed through the single authority so a
    // "can't become tapped" creature is refused.
    for &cid in creature_ids {
        crate::game::restrictions::tap_permanent_for_cost(state, cid, events)?;
    }

    // CR 602.5b: Record this crew activation so an "Activate only once each turn"
    // Vehicle cannot be crewed a second time this turn. Cleared at turn start.
    record_crew_activation(state, vehicle_id);

    Ok(push_keyword_action(
        state,
        player,
        vehicle_id,
        KeywordAction::Crew {
            vehicle_id,
            paid_creature_ids: creature_ids.to_vec(),
        },
        events,
    ))
}

// ---------------------------------------------------------------------------
// CR 702.184a: Station — keyword action with per-card dispatch (mirrors Crew)
// ---------------------------------------------------------------------------

/// CR 702.184a: Activate a Spacecraft's station ability from Priority.
/// Per CR 702.184a: "Activate only as a sorcery." — the activation is rejected
/// outside the active player's main phase, empty stack, own priority.
fn handle_station_activation(
    state: &mut GameState,
    player: PlayerId,
    spacecraft_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&spacecraft_id)
        .ok_or_else(|| EngineError::InvalidAction("Spacecraft not found".to_string()))?;

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Spacecraft is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this Spacecraft".to_string(),
        ));
    }
    if !obj
        .keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Station))
    {
        return Err(EngineError::InvalidAction(
            "Object has no Station keyword".to_string(),
        ));
    }

    // CR 702.184a: "Activate only as a sorcery."
    if !super::restrictions::is_sorcery_speed_window(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "Station may only be activated as a sorcery".to_string(),
        ));
    }

    // CR 702.184a: "Tap another untapped creature you control" — the chosen
    // creature is NOT the Spacecraft, is a creature, is untapped, and is
    // controlled by the activating player.
    let eligible_creatures: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| id != spacecraft_id && is_tappable_creature_for_cost(state, id, player))
        .collect();

    if eligible_creatures.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "No eligible creatures to tap for Station".to_string(),
        ));
    }

    let _ = events; // No events emitted during activation (cost payment happens at resolution).
    priority::clear_priority_passes(state);
    Ok(WaitingFor::StationTarget {
        player,
        spacecraft_id,
        eligible_creatures,
    })
}

/// CR 702.184a + CR 113.3b: Announce Station. Pays the cost (tap the chosen
/// creature), snapshots its power per CR 113.7a, and pushes a
/// `KeywordAction::Station` stack entry. Charge counters are applied at
/// stack resolution, after a priority window (CR 113.3b).
fn handle_station_announcement(
    state: &mut GameState,
    player: PlayerId,
    spacecraft_id: ObjectId,
    eligible_creatures: &[ObjectId],
    creature_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 702.184a: Re-validate the chosen creature is still eligible (pending-effect
    // time gap between activation and resolution). Mirrors the HarmonizeTap idiom.
    if !eligible_creatures.contains(&creature_id) {
        return Err(EngineError::InvalidAction(
            "Creature not in eligible list".to_string(),
        ));
    }

    let spacecraft = state
        .objects
        .get(&spacecraft_id)
        .ok_or_else(|| EngineError::InvalidAction("Spacecraft no longer exists".to_string()))?;
    if spacecraft.zone != Zone::Battlefield || spacecraft.controller != player {
        return Err(EngineError::InvalidAction(
            "Spacecraft is no longer valid for stationing".to_string(),
        ));
    }

    let creature = state
        .objects
        .get(&creature_id)
        .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".to_string()))?;
    if creature.zone != Zone::Battlefield
        || creature.controller != player
        || creature.tapped
        || !creature
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Creature)
        || crate::game::restrictions::object_cant_tap(state, creature_id)
    {
        return Err(EngineError::InvalidAction(
            "Creature is no longer eligible for Station".to_string(),
        ));
    }

    // CR 702.184a + CR 113.7a: Snapshot the creature's power BEFORE tapping —
    // the counter count is determined at cost-payment time and survives the
    // creature leaving the battlefield before resolution. CR 702.184c:
    // static abilities may modify the contributed value ("stations
    // permanents as though its power were N greater"); the helper applies any
    // such modifier and otherwise reads `power`, the default per the rule.
    let snapshot_power = super::static_abilities::object_crew_power_contribution(
        state,
        creature_id,
        crate::types::statics::CrewAction::Station,
    );

    // CR 701.26a: Tap the creature as cost payment. Routed through the single
    // authority (CR 508.1f exempts attacker declaration) so a "can't become
    // tapped" creature is refused.
    crate::game::restrictions::tap_permanent_for_cost(state, creature_id, events)?;

    Ok(push_keyword_action(
        state,
        player,
        spacecraft_id,
        KeywordAction::Station {
            spacecraft_id,
            paid_creature_id: creature_id,
            snapshot_power,
        },
        events,
    ))
}

// ---------------------------------------------------------------------------
// CR 702.171a: Saddle — keyword action with per-card dispatch (mirrors Crew)
// ---------------------------------------------------------------------------

/// CR 702.171a: Activate a Mount's saddle ability from Priority.
/// Enforces the sorcery-speed gate: main phase, empty stack, active player.
fn handle_saddle_activation(
    state: &mut GameState,
    player: PlayerId,
    mount_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&mount_id)
        .ok_or_else(|| EngineError::InvalidAction("Mount not found".to_string()))?;

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Mount is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this Mount".to_string(),
        ));
    }

    // Extract saddle power from keywords — fails if this permanent has no Saddle keyword.
    let saddle_power = obj
        .keywords
        .iter()
        .find_map(|kw| {
            if let crate::types::keywords::Keyword::Saddle(n) = kw {
                Some(*n)
            } else {
                None
            }
        })
        .ok_or_else(|| EngineError::InvalidAction("Object has no Saddle keyword".to_string()))?;

    // CR 702.171a: "Activate only as a sorcery."
    if !super::restrictions::is_sorcery_speed_window(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "Saddle may only be activated as a sorcery".to_string(),
        ));
    }

    // CR 702.171a: "Tap any number of other untapped creatures you control."
    let eligible_creatures: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| id != mount_id && is_tappable_creature_for_cost(state, id, player))
        .collect();

    // CR 702.171a: a creature's saddle contribution may be modified.
    let contributions: Vec<i32> = eligible_creatures
        .iter()
        .map(|&id| {
            super::static_abilities::object_crew_power_contribution(
                state,
                id,
                crate::types::statics::CrewAction::Saddle,
            )
        })
        .collect();
    let total_power: i32 = contributions.iter().sum();

    if total_power < saddle_power as i32 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough total power among eligible creatures to saddle".to_string(),
        ));
    }

    let _ = events;
    priority::clear_priority_passes(state);
    Ok(WaitingFor::SaddleMount {
        player,
        mount_id,
        saddle_power,
        eligible_creatures,
        contributions,
    })
}

/// CR 702.171a + CR 113.3b: Announce Saddle. Pays the cost (tap selected
/// creatures) and pushes a `KeywordAction::Saddle` stack entry. The "becomes
/// saddled UEOT" designation is applied at stack resolution.
fn handle_saddle_announcement(
    state: &mut GameState,
    player: PlayerId,
    mount_id: ObjectId,
    saddle_power: u32,
    eligible_creatures: &[ObjectId],
    creature_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if creature_ids.is_empty() {
        return Err(EngineError::InvalidAction(
            "Must select at least one creature to saddle".to_string(),
        ));
    }

    let mount = state
        .objects
        .get(&mount_id)
        .ok_or_else(|| EngineError::InvalidAction("Mount no longer exists".to_string()))?;
    if mount.zone != Zone::Battlefield || mount.controller != player {
        return Err(EngineError::InvalidAction(
            "Mount is no longer valid for saddling".to_string(),
        ));
    }

    for &cid in creature_ids {
        if !eligible_creatures.contains(&cid) {
            return Err(EngineError::InvalidAction(
                "Creature not in eligible list".to_string(),
            ));
        }
    }

    let mut total_power: i32 = 0;
    for &cid in creature_ids {
        let obj = state
            .objects
            .get(&cid)
            .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".to_string()))?;
        if obj.zone != Zone::Battlefield || obj.tapped {
            return Err(EngineError::InvalidAction(
                "Creature is no longer eligible for saddling".to_string(),
            ));
        }
        if crate::game::restrictions::object_cant_tap(state, cid) {
            return Err(EngineError::InvalidAction(
                "Creature can't become tapped".to_string(),
            ));
        }
        // CR 702.171a: apply any saddle power-contribution modifier.
        total_power += super::static_abilities::object_crew_power_contribution(
            state,
            cid,
            crate::types::statics::CrewAction::Saddle,
        );
    }

    if total_power < saddle_power as i32 {
        return Err(EngineError::InvalidAction(
            "Selected creatures' total power is less than saddle requirement".to_string(),
        ));
    }

    // CR 701.26a + CR 702.171c + CR 508.1f: Tap each creature as cost payment —
    // creature "saddles" the Mount. Routed through the single authority so a
    // "can't become tapped" creature is refused.
    for &cid in creature_ids {
        crate::game::restrictions::tap_permanent_for_cost(state, cid, events)?;
    }

    Ok(push_keyword_action(
        state,
        player,
        mount_id,
        KeywordAction::Saddle {
            mount_id,
            paid_creature_ids: creature_ids.to_vec(),
        },
        events,
    ))
}

pub fn new_game(seed: u64) -> GameState {
    GameState::new_two_player(seed)
}

/// Maximum number of tie-break reroll rounds in the first-player contest.
///
/// Load-bearing safety cap: if every tied seat re-rolls the same value, the
/// tied group does not shrink, so an unbounded "reroll the tied group" loop
/// could spin forever on a degenerate RNG. After this many rounds the tie is
/// broken deterministically by lowest seat index (see `start_game`).
const FIRST_PLAYER_CONTEST_MAX_ROUNDS: usize = 16;

/// CR 103.1: run the starting-player roll-off and capture its round structure.
///
/// `roll_round` is called once per round with the current contender set (in
/// seat order) and returns each contender's d20 result. Round 1 = all seats;
/// each later round = the prior round's tied-max group (CR 103.1 reroll).
/// Returns the per-round structure and the winner: the unique max of the final
/// round, or the lowest seat index when still tied at
/// `FIRST_PLAYER_CONTEST_MAX_ROUNDS`.
///
/// The selection logic (contenders narrowing, max/top filtering, bounded cap,
/// lowest-seat fallback) is identical to the prior inline loop; the only change
/// is that each round's rolls are captured into a `ContestRound` instead of
/// pushed as flat `DieRolled` events.
fn build_contest_rounds(
    seat_order: &[PlayerId],
    mut roll_round: impl FnMut(&[PlayerId]) -> Vec<(PlayerId, u8)>,
) -> (Vec<ContestRound>, PlayerId) {
    let mut rounds: Vec<ContestRound> = Vec::new();

    // `contenders` is the set of seats still in the running. It starts as every
    // seat and, after each tie, narrows to the tied top group only.
    let mut contenders: Vec<PlayerId> = seat_order.to_vec();
    let mut starting_player: Option<PlayerId> = None;

    // BOUNDED tie loop. Each iteration rolls every contender; a unique high
    // roller wins. On a tie, `contenders` narrows to the tied top group and we
    // reroll just them. INVARIANT: if every tied seat re-rolls the same value
    // the group does NOT shrink, so this loop is bounded by
    // FIRST_PLAYER_CONTEST_MAX_ROUNDS rather than relying on the group ever
    // shrinking. If the cap is reached while still tied, the tie is broken
    // deterministically by lowest seat index below — the engine can never hang.
    for _round in 0..FIRST_PLAYER_CONTEST_MAX_ROUNDS {
        let rolls: Vec<(PlayerId, u8)> = roll_round(&contenders);
        let max_roll = rolls.iter().map(|&(_, r)| r).max().expect("non-empty");
        let top: Vec<PlayerId> = rolls
            .iter()
            .filter(|&&(_, r)| r == max_roll)
            .map(|&(seat, _)| seat)
            .collect();
        rounds.push(ContestRound { rolls });
        if top.len() == 1 {
            starting_player = Some(top[0]);
            break;
        }
        // Tie: reroll only the tied top group on the next round.
        contenders = top;
    }

    // Deterministic fallback: still tied at the cap → lowest seat index wins.
    let starting_player = starting_player.unwrap_or_else(|| {
        contenders
            .iter()
            .copied()
            .min()
            .expect("contenders is always non-empty")
    });

    (rounds, starting_player)
}

/// Start game with mulligan flow. If no cards in libraries, skips mulligan.
///
/// CR 103.1: At the start of game 1 of a match the players determine who takes
/// the first turn "using any mutually agreeable method (flipping a coin,
/// rolling dice, etc.)". This engine models that determination as an
/// authoritative d20 high-roll contest — one d20 per seat using the game's
/// seeded RNG (CR 706, rolling a die) — with ties rerolled among the tied top
/// group. NOTE ON FIDELITY: the literal CR 103.1 sequence is "contest winner
/// *chooses* who takes the first turn"; this engine collapses that to "contest
/// winner *becomes* the starting player" (it does not present a play/draw
/// choice here), an existing, accepted simplification — the annotation does not
/// claim the choose-step is implemented. Subsequent games in a multi-game match
/// route through `match_flow::start_next_game`, which uses `next_game_chooser`
/// instead, so this function is always the game-1 path.
///
/// The contest is surfaced as a single authoritative
/// `GameEvent::StartingPlayerContest` carrying the full round structure (round
/// 1 = all seats, each later round = the prior round's tied-max reroll group)
/// plus the engine's authoritative `winner`, so downstream consumers render the
/// contest round by round without re-deriving anything. It is inserted at the
/// front of the result, ahead of `GameStarted` → `TurnStarted`. This replaces
/// the prior flat per-roll `DieRolled` batch; in-game die rolls still emit
/// `DieRolled`.
///
/// DETERMINISM: the contest draws only from `state.rng` (the seeded
/// `ChaCha20Rng`), never thread/global RNG, so replays and AI search stay
/// deterministic. The RNG draw count and order are EXACTLY as before — one
/// `random_range(1..=20)` per contender per round, in seat order — so this
/// representation change introduces ZERO determinism shift relative to the
/// prior `DieRolled`-batch implementation. (It still differs from the original
/// single `random_range(0..len)` pick that predated the contest, an earlier,
/// accepted shift.)
///
/// Callers that need a deterministic starter (tests, fixed scenarios) must use
/// `start_game_with_starting_player` directly — that path runs no contest and
/// emits no `StartingPlayerContest` event.
pub fn start_game(state: &mut GameState) -> ActionResult {
    if state.seat_order.is_empty() {
        return start_game_with_starting_player(state, PlayerId(0));
    }

    if let Some(archenemy) = super::topology::archenemy(state) {
        // CR 904.6: The archenemy takes the first turn. Default Archenemy does
        // not run the CR 103.1 starting-player contest.
        return start_game_with_starting_player(state, archenemy);
    }

    // CR 103.1 / CR 706: roll one d20 per seat; the high roller becomes the
    // starting player. Draw order/count is identical to the prior
    // implementation — one `random_range(1..=20)` per contender, in seat order.
    let seat_order = state.seat_order.clone();
    let (rounds, starting_player) = build_contest_rounds(&seat_order, |contenders| {
        contenders
            .iter()
            .map(|&seat| (seat, state.rng.random_range(1..=20u8)))
            .collect()
    });

    let mut result = start_game_with_starting_player(state, starting_player);
    // CR 103.1: StartingPlayerContest → GameStarted → TurnStarted.
    result.events.insert(
        0,
        GameEvent::StartingPlayerContest {
            rounds,
            winner: starting_player,
        },
    );
    result
}

/// Start game with a specific player taking the first turn.
pub fn start_game_with_starting_player(
    state: &mut GameState,
    starting_player: PlayerId,
) -> ActionResult {
    let mut events = Vec::new();
    state.outside_game_cards_brought_in.clear();
    let starting_player = super::topology::archenemy(state).unwrap_or(starting_player);

    if state.match_config.match_type == MatchType::Bo3
        && state.players.len() != 2
        && super::topology::archenemy(state).is_none()
    {
        state.match_config.match_type = MatchType::Bo1;
    }

    events.push(GameEvent::GameStarted);

    // Begin the game: set turn 1
    state.turn_number = 1;
    state.active_player = starting_player;
    state.priority_player = starting_player;
    state.current_starting_player = starting_player;
    // First-game default chooser is the starting player; BO3 restarts can pre-set this.
    if state.next_game_chooser.is_none() {
        state.next_game_chooser = Some(starting_player);
    }
    // Rotate seat order so mulligan starts with the starting player.
    if let Some(idx) = state.seat_order.iter().position(|&p| p == starting_player) {
        state.seat_order.rotate_left(idx);
    }
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: starting_player,
        turn_number: 1,
    });

    // If players have cards in their libraries, start mulligan flow
    let has_libraries = state.players.iter().any(|p| !p.library.is_empty());
    let waiting_for = if has_libraries {
        // CR 702.139a: Check for eligible companions before mulligans.
        if let Some(companion_wf) = super::companion::check_all_companion_reveals(state) {
            companion_wf
        } else {
            mulligan::start_mulligan(state, &mut events)
        }
    } else {
        // No cards to mulligan with, skip straight to game
        crate::game::planechase::reveal_starting_plane(state);
        turns::auto_advance(state, &mut events)
    };

    state.waiting_for = waiting_for.clone();
    bump_state_revision(state);
    mark_public_state_all_dirty(state);
    finalize_public_state(state);

    let log_entries = super::log::resolve_log_entries(&events, state);
    ActionResult {
        events,
        waiting_for,
        log_entries,
    }
}

/// Start game without mulligan (for backward compatibility with existing tests).
pub fn start_game_skip_mulligan(state: &mut GameState) -> ActionResult {
    let mut events = Vec::new();
    state.outside_game_cards_brought_in.clear();
    let starting_player = super::topology::archenemy(state).unwrap_or(PlayerId(0));

    events.push(GameEvent::GameStarted);

    state.turn_number = 1;
    state.active_player = starting_player;
    state.priority_player = starting_player;
    state.current_starting_player = starting_player;
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: starting_player,
        turn_number: 1,
    });

    crate::game::planechase::reveal_starting_plane(state);
    let waiting_for = turns::auto_advance(state, &mut events);
    state.waiting_for = waiting_for.clone();
    bump_state_revision(state);
    mark_public_state_all_dirty(state);
    finalize_public_state(state);

    let log_entries = super::log::resolve_log_entries(&events, state);
    ActionResult {
        events,
        waiting_for,
        log_entries,
    }
}

/// CR 607.2a + CR 406.6: Check if any exile-return sources have left the battlefield.
/// If so, move the exiled cards back — linked abilities track which cards were exiled by the source.
pub(super) fn check_exile_returns(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let mut to_return: Vec<crate::types::game_state::ExileLink> = Vec::new();

    for event in events.iter() {
        if let GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Battlefield),
            ..
        } = event
        {
            // Find exile links where this object was the source and the exile
            // effect specified an automatic return when that source leaves.
            for link in &state.exile_links {
                if link.source_id == *object_id
                    && matches!(
                        &link.kind,
                        crate::types::game_state::ExileLinkKind::UntilSourceLeaves { .. }
                    )
                {
                    to_return.push(link.clone());
                }
            }
        }
    }

    if to_return.is_empty() {
        return;
    }

    // CR 610.3 + CR 614.6: Return each exiled card to its previous zone through
    // the zone-change pipeline so a battlefield return seeds enters-with-counters
    // statics (Hardened Scales class) and so a `Moved` redirect fires on any
    // non-battlefield return — the raw `move_to_zone` skipped the delivery tail.
    // Group by destination zone (CR 603.10a: cards returning to the same zone do
    // so simultaneously); within a group each card self-anchors its attribution
    // (CR 400.7 — the pre-pipeline raw move recorded no source).
    //
    // The spent `UntilSourceLeaves` links are dropped via a per-group
    // `RemoveExileLinks` completion so the cleanup runs exactly once after the
    // group's pile lands, even when a returned creature pauses on an as-enters /
    // aura-host choice (CR 303.4f / 616.1): the parked batch tail + completion
    // are drained by the replacement-choice / aura-attachment resume.
    // First-seen insertion order (not a HashMap) so group processing is
    // deterministic for the engine's reproducibility guarantee.
    let mut groups: Vec<(Zone, Vec<ObjectId>)> = Vec::new();
    for link in &to_return {
        let still_in_exile = state
            .objects
            .get(&link.exiled_id)
            .map(|obj| obj.zone == Zone::Exile)
            .unwrap_or(false);
        if !still_in_exile {
            continue;
        }
        let crate::types::game_state::ExileLinkKind::UntilSourceLeaves { return_zone } = &link.kind
        else {
            continue;
        };
        let return_zone = *return_zone;
        let gi = match groups.iter().position(|(zone, _)| *zone == return_zone) {
            Some(i) => i,
            None => {
                groups.push((return_zone, Vec::new()));
                groups.len() - 1
            }
        };
        if !groups[gi].1.contains(&link.exiled_id) {
            groups[gi].1.push(link.exiled_id);
        }
        // CR 730.3c: if the source exiled a MERGED permanent, it split into
        // multiple objects (CR 730.3). The implicit "return when the source
        // leaves" must bring back ALL of them, not just the tracked survivor —
        // the components are co-located in exile with the survivor and return to
        // the same zone. (A no-op when the exiled card was not a merged permanent.)
        let components = super::merge::co_split_components(state, link.exiled_id, &groups[gi].1);
        groups[gi].1.extend(components);
    }

    // Links for cards that already left exile (not returned by us) are still spent
    // and must be dropped now — only the IN-FLIGHT group ids ride their batch
    // completion. (The common case is a single battlefield group; a mid-group
    // pause defers only that group's cleanup, while any remaining groups process
    // after — `move_objects_simultaneously_then` parks the tail per group.)
    let returning_ids: std::collections::HashSet<ObjectId> = groups
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect();
    let returned_all: Vec<ObjectId> = to_return.iter().map(|l| l.exiled_id).collect();
    state.exile_links.retain(|link| {
        !returned_all.contains(&link.exiled_id) || returning_ids.contains(&link.exiled_id)
    });

    for (return_zone, ids) in groups {
        let reqs: Vec<_> = ids
            .iter()
            .map(|&id| super::zone_pipeline::ZoneMoveRequest::effect(id, return_zone, id))
            .collect();
        let completion =
            crate::types::game_state::BatchCompletion::RemoveExileLinks { returned_ids: ids };
        if matches!(
            super::zone_pipeline::move_objects_simultaneously_then(
                state,
                reqs,
                Some(completion),
                events,
            ),
            super::zone_pipeline::BatchMoveResult::NeedsChoice
        ) {
            // CR 616.1 / CR 303.4f: this group paused; its tail + cleanup are
            // parked and drained on resume. Stop processing further groups so a
            // later group's moves do not run over the parked prompt; the spent
            // links of any unprocessed group remain in `exile_links` until their
            // (now-gone) source re-checks — acceptable, as multi-destination
            // returns from one source-leaves event do not occur in the pool.
            return;
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "engine_trigger_target_tests.rs"]
mod trigger_target_tests;

#[cfg(test)]
#[path = "engine_exile_return_tests.rs"]
mod exile_return_tests;

#[cfg(test)]
#[path = "engine_phase_trigger_regression_tests.rs"]
mod phase_trigger_regression_tests;

#[cfg(test)]
#[path = "engine_crew_tests.rs"]
mod crew_tests;

#[cfg(test)]
#[path = "engine_station_tests.rs"]
mod station_tests;

#[cfg(test)]
#[path = "engine_keyword_action_stack_tests.rs"]
mod keyword_action_stack_tests;

#[cfg(test)]
#[path = "engine_mdfc_land_tests.rs"]
mod mdfc_land_tests;

#[cfg(test)]
mod shortcut_schema_tests {
    use super::shortcut_iteration_count;
    use crate::analysis::decision_template::IterationCount;
    use crate::analysis::loop_check::WinKind;

    /// T3: `iteration_count` is exhaustive over all six `WinKind`s — the two determinate-lethal
    /// axes (CR 704.5a life / CR 704.5c poison) map to `UntilLethal`; every other win seeds
    /// `Fixed(1)`. Revert-probe: swapping any arm flips the corresponding assertion.
    #[test]
    fn iteration_count_maps_every_win_kind() {
        assert_eq!(
            shortcut_iteration_count(WinKind::LethalDamage),
            IterationCount::UntilLethal
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::PoisonLoss),
            IterationCount::UntilLethal
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::Decking),
            IterationCount::Fixed(1)
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::ExtraTurns),
            IterationCount::Fixed(1)
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::ImmediateWin),
            IterationCount::Fixed(1)
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::Advantage),
            IterationCount::Fixed(1)
        );
    }
}

/// PR-7 Combo-UI Stage 2: the mid-drive pin injector (item 4) + the drive-period seam (item 6).
#[cfg(test)]
mod stage2_injector_tests {
    use super::*;
    use crate::analysis::decision_template::{
        DecisionGroupKey, DecisionKind, DecisionSlot, DecisionTemplate, IterationCount,
        PinnedDecision, ReplayMode, TargetPin, TargetSchedule,
    };
    use crate::game::scenario::GameScenario;
    use crate::types::game_state::{LoopDetectionMode, YieldTarget};

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);
    const P2: PlayerId = PlayerId(2);
    const TARGET_DRAIN: &str = "Whenever you gain life, target opponent loses that much life.";
    const FEEDBACK: &str = "Whenever an opponent loses life, you gain that much life.";
    const KICKOFF: &str = "You gain 1 life.";

    fn life(state: &GameState, p: PlayerId) -> i32 {
        state.players.iter().find(|pl| pl.id == p).unwrap().life
    }

    fn this_object(id: ObjectId) -> YieldTarget {
        YieldTarget::ThisObject {
            source_id: id,
            incarnation: None,
            trigger_description: None,
        }
    }

    /// A template routing two distinct drainers to two distinct opponents by source identity.
    fn two_drainer_template(
        d0: ObjectId,
        opp0: PlayerId,
        d1: ObjectId,
        opp1: PlayerId,
    ) -> DecisionTemplate {
        let s0 = this_object(d0);
        let s1 = this_object(d1);
        DecisionTemplate {
            owner: P0,
            decisions: vec![
                PinnedDecision::Targets {
                    slot: DecisionSlot {
                        source: s0.clone(),
                        index: 0,
                    },
                    targets: vec![TargetPin::Player(opp0)],
                },
                PinnedDecision::Targets {
                    slot: DecisionSlot {
                        source: s1.clone(),
                        index: 0,
                    },
                    targets: vec![TargetPin::Player(opp1)],
                },
            ],
            replay: ReplayMode::Scheduled {
                count: IterationCount::UntilLethal,
            },
            key: DecisionGroupKey::from_sources(&[s0, s1], DecisionKind::LoopChoice),
        }
    }

    /// Test F ⭐ (item 4 — per-source target routing, the two-authority claim): a 3p loop with
    /// TWO targeted drainers raises a `TriggerTargetSelection` per drainer (two legal opponents
    /// ⇒ not forced-unique) plus `OrderTriggers`. `inject_pinned_answer` matches EACH prompt's
    /// `source_id` to the pin for THAT drainer (not the first pin), so the two drainers hit
    /// DISTINCT opponents. Discriminator: P2 dropping proves per-source routing — a first-pin
    /// injector would drain only P1.
    #[test]
    fn injector_routes_pinned_targets_per_source() {
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        scenario.with_life(P1, 500);
        scenario.with_life(P2, 500);
        let drainer_a = scenario
            .add_creature_from_oracle(P0, "Drainer A", 1, 4, TARGET_DRAIN)
            .id();
        let drainer_b = scenario
            .add_creature_from_oracle(P0, "Drainer B", 2, 2, TARGET_DRAIN)
            .id();
        scenario.add_creature_from_oracle(P0, "Feedback", 3, 4, FEEDBACK);
        let kickoff = scenario
            .add_spell_to_hand_from_oracle(P0, "Kickoff", false, KICKOFF)
            .id();
        let mut runner = scenario.build();
        // Off: drive the raw cascade directly through the injector (no offer/auto-win path).
        runner.state_mut().loop_detection = LoopDetectionMode::Off;
        // Cast the seed lifegain via the INTERNAL path (the CastBuilder's auto-resolver cannot
        // satisfy the non-forced-unique 2-opponent target prompt — that is exactly the arm the
        // injector is under test for).
        let card_id = runner.state().objects.get(&kickoff).unwrap().card_id;
        apply_action(
            runner.state_mut(),
            P0,
            GameAction::CastSpell {
                object_id: kickoff,
                card_id,
                targets: vec![],
                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            },
            None,
        )
        .expect("cast the seed lifegain");

        let template = two_drainer_template(drainer_a, P1, drainer_b, P2);

        // The target each drainer's trigger actually got, read off the stack right after the
        // injector answered its prompt (independent of drain-resolution order).
        let target_on_stack = |state: &GameState, src: ObjectId| -> Option<Vec<TargetRef>> {
            state
                .stack
                .iter()
                .find(|e| e.source_id == src)
                .and_then(|e| match &e.kind {
                    crate::types::game_state::StackEntryKind::TriggeredAbility {
                        ability, ..
                    } => Some(ability.targets.clone()),
                    _ => None,
                })
        };
        let mut a_target: Option<Vec<TargetRef>> = None;
        let mut b_target: Option<Vec<TargetRef>> = None;

        for _ in 0..40 {
            let wf = runner.state().waiting_for.clone();
            match wf {
                WaitingFor::Priority { player } => {
                    apply_action(runner.state_mut(), player, GameAction::PassPriority, None)
                        .expect("pass priority");
                }
                WaitingFor::OrderTriggers { .. } => {
                    inject_pinned_answer(runner.state_mut(), None, 0, &wf)
                        .expect("OrderTriggers arm is template-INDEPENDENT (None is fine)");
                }
                WaitingFor::TriggerTargetSelection { source_id, .. } => {
                    // Guard: at a target prompt, a None template fails CLOSED (the guard lives
                    // in THIS arm, not at the top of the injector).
                    assert!(
                        inject_pinned_answer(&mut runner.state().clone(), None, 0, &wf).is_err(),
                        "template=None must abort the TriggerTargetSelection arm"
                    );
                    inject_pinned_answer(runner.state_mut(), Some(&template), 0, &wf)
                        .expect("pinned target injected");
                    let src = source_id.expect("targeted trigger has a source");
                    if src == drainer_a {
                        a_target = target_on_stack(runner.state(), src);
                    } else if src == drainer_b {
                        b_target = target_on_stack(runner.state(), src);
                    }
                }
                _ => break,
            }
            if a_target.is_some() && b_target.is_some() {
                break;
            }
        }

        // Per-source routing: each drainer's trigger got ITS OWN pinned opponent — a first-pin
        // injector would route both to P1.
        assert_eq!(
            a_target,
            Some(vec![TargetRef::Player(P1)]),
            "Drainer A's trigger routed to its pinned P1"
        );
        assert_eq!(
            b_target,
            Some(vec![TargetRef::Player(P2)]),
            "Drainer B's trigger routed to its pinned P2 (per-source, not first-pin)"
        );
    }

    /// Test F (production-path twin, item 4): drive a primed 3p targeted loop through the REAL
    /// `drive_one_shortcut_cycle` and confirm its `Ok(other)` arm routes to the injector. Both
    /// pinned opponents drain to death in the driven cycle ⇒ `CrossLethal{winner: Some(P0)}`,
    /// which is REACHABLE ONLY if each drainer's trigger hit its OWN pinned opponent (a
    /// first-pin injector would drain only P1, leaving P2 alive and no single winner).
    #[test]
    fn drive_one_cycle_reaches_injector_for_3p_targeted() {
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        scenario.with_life(P1, 400);
        scenario.with_life(P2, 400);
        let drainer_a = scenario
            .add_creature_from_oracle(P0, "Drainer A", 1, 4, TARGET_DRAIN)
            .id();
        let drainer_b = scenario
            .add_creature_from_oracle(P0, "Drainer B", 2, 2, TARGET_DRAIN)
            .id();
        scenario.add_creature_from_oracle(P0, "Feedback", 3, 4, FEEDBACK);
        let kickoff = scenario
            .add_spell_to_hand_from_oracle(P0, "Kickoff", false, KICKOFF)
            .id();
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Off;
        let card_id = runner.state().objects.get(&kickoff).unwrap().card_id;
        apply_action(
            runner.state_mut(),
            P0,
            GameAction::CastSpell {
                object_id: kickoff,
                card_id,
                targets: vec![],
                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            },
            None,
        )
        .expect("cast seed");

        // Prime: drive (targeting P1 for anything) until a Priority{P0} beat with a pending
        // cascade — the settle beat the drive re-fires from.
        let prime = two_drainer_template(drainer_a, P1, drainer_b, P1);
        let mut primed = false;
        for _ in 0..40 {
            let wf = runner.state().waiting_for.clone();
            match wf {
                WaitingFor::Priority { player }
                    if player == P0 && !runner.state().stack.is_empty() =>
                {
                    primed = true;
                    break;
                }
                WaitingFor::Priority { player } => {
                    apply_action(runner.state_mut(), player, GameAction::PassPriority, None)
                        .unwrap();
                }
                WaitingFor::OrderTriggers { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                    inject_pinned_answer(runner.state_mut(), Some(&prime), 0, &wf).unwrap();
                }
                _ => break,
            }
        }
        assert!(primed, "must reach a primed Priority{{P0}} settle beat");

        // Reset opponents to equal LOW life so the driven cycle crosses lethal (both die) —
        // reachable only if each drainer hits its own pinned opponent.
        for p in [P1, P2] {
            runner
                .state_mut()
                .players
                .iter_mut()
                .find(|pl| pl.id == p)
                .unwrap()
                .life = 8;
        }
        let committed = runner.state().clone();
        let boundary = {
            let mut seed = committed.clone();
            priority::reset_priority(&mut seed);
            seed.waiting_for = WaitingFor::Priority {
                player: seed.active_player,
            };
            seed.normalize_for_loop()
        };
        let template = two_drainer_template(drainer_a, P1, drainer_b, P2);
        let cap = auto_pass_loop_max_iterations(&committed);

        match drive_one_shortcut_cycle(&committed, &boundary, Some(&template), 0, cap) {
            CycleOutcome::CrossLethal { winner, state, .. } => {
                assert_eq!(
                    winner,
                    Some(P0),
                    "both pinned opponents drained to death ⇒ P0 sole winner (per-source \
                     routing through the production drive)"
                );
                assert!(
                    life(&state, P1) <= 0 && life(&state, P2) <= 0,
                    "both opponents at 0-or-less"
                );
            }
            CycleOutcome::Recurred { state, .. } => {
                assert!(
                    life(&state, P1) < 8 && life(&state, P2) < 8,
                    "both pinned opponents drained through drive_one_shortcut_cycle"
                );
            }
            CycleOutcome::Abort => panic!("the pinned drive must not abort"),
        }
    }

    /// Item 6: `shortcut_drive_period` = the max schedule length over the template's target
    /// pins (Constant/Player/ByIdentity ⇒ 1), defaulting to 1 (no template / non-target pins).
    #[test]
    fn shortcut_drive_period_is_schedule_max() {
        assert_eq!(shortcut_drive_period(None), 1, "no template ⇒ period 1");

        let a = this_object(ObjectId(1));
        let b = this_object(ObjectId(2));
        let c = this_object(ObjectId(3));
        let slot = DecisionSlot {
            source: a.clone(),
            index: 0,
        };
        let mk = |targets: Vec<TargetPin>| DecisionTemplate {
            owner: P0,
            decisions: vec![PinnedDecision::Targets {
                slot: slot.clone(),
                targets,
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::UntilLethal,
            },
            key: DecisionGroupKey::from_sources(std::slice::from_ref(&a), DecisionKind::LoopChoice),
        };

        let constant = mk(vec![TargetPin::Player(P1)]);
        assert_eq!(shortcut_drive_period(Some(&constant)), 1, "Player pin ⇒ 1");

        let rr = mk(vec![TargetPin::Scheduled(TargetSchedule::RoundRobin(
            vec![a.clone(), b.clone(), c.clone()],
        ))]);
        assert_eq!(shortcut_drive_period(Some(&rr)), 3, "RoundRobin(3) ⇒ 3");

        let pw = mk(vec![TargetPin::Scheduled(TargetSchedule::Piecewise(vec![
            (0, a.clone()),
            (5, b.clone()),
        ]))]);
        assert_eq!(shortcut_drive_period(Some(&pw)), 2, "Piecewise(2) ⇒ 2");

        // CR 732.2a SAFETY LIMIT: an over-cap schedule clamps to MAX_SHORTCUT_CYCLES.
        // Revert-probe: restore `.max(1)` (drop the `.clamp`) ⇒ returns MAX+5 (1005) ≠ 1000.
        let oversized = mk(vec![TargetPin::Scheduled(TargetSchedule::RoundRobin(
            vec![a.clone(); (MAX_SHORTCUT_CYCLES + 5) as usize],
        ))]);
        assert_eq!(
            shortcut_drive_period(Some(&oversized)),
            MAX_SHORTCUT_CYCLES,
            "RoundRobin(MAX+5) clamps to MAX_SHORTCUT_CYCLES"
        );
    }
}
