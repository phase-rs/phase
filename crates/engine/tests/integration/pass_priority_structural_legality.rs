//! `PassPriority` structural legality at the public `legal_actions` boundary.
//!
//! CR 117.3d ("If a player has priority and chooses not to take any actions,
//! that player passes") makes a bare pass an O(1) question, but before the
//! fifth `SimulationFilter` structural hatch it was answered by cloning the
//! whole `GameState` and performing the pass — at every priority window every
//! consumer of `legal_actions` reached. These tests pin the clone away with
//! exact `perf_counters` integers (never a timing assertion) and pin the hatch's
//! soundness across the CR 117.4 resolution seam.

use engine::ai_support::legal_actions;
use engine::game::engine::apply_for_simulation;
use engine::game::perf_counters;
use engine::game::priority::pass_priority_structurally_legal;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::turn_control;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastingVariant, GameState, PendingContinuation, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// T2. The hatch removes the clone at the exact call the projection makes.
///
/// **Fixture pin (i) + (ii):** both hands are empty AND the window is
/// `Phase::Upkeep`, not a main phase. `candidates::priority_actions_with_probe`
/// enumerates one `PlayLand` per playable land in hand whenever
/// `is_main_phase && stack_empty && is_active` and the land-drop budget is
/// unspent, and `PlayLand` has no structural hatch — so it would reach
/// `fallback_simulation` and increment `state_clone_for_legality` even with a
/// correct implementation. Do not relax the `assert_eq!` below to an
/// inequality; repin the fixture instead.
///
/// `contains(PassPriority)` is the reach-guard: it passes either way by design
/// and exists so the counter assertion cannot be satisfied vacuously by an
/// empty action list.
#[test]
fn legal_actions_answers_a_bare_pass_without_cloning_the_state() {
    let runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Upkeep);
        scenario.build()
    };
    let state = runner.state();
    assert!(state.players[P0.0 as usize].hand.is_empty());
    assert!(state.stack.is_empty());

    perf_counters::reset();
    let actions = legal_actions(state);
    let counters = perf_counters::snapshot();

    assert!(
        actions.contains(&GameAction::PassPriority),
        "reach-guard: the pass must actually be enumerated at this window"
    );
    assert_eq!(
        counters.state_clone_for_legality, 0,
        "a bare pass must be decided structurally, with no legality clone"
    );
}

/// T5. Hostile, multi-authority: CR 723.5 turn control, where the seat that
/// holds priority (P1) is not the authorized submitter (P0).
///
/// **Fixture pin (ii):** the window is `Phase::Upkeep`, not a main phase, and
/// both hands are empty — so no `PlayLand` candidate is enumerated for P1.
///
/// This is the fixture that catches a predicate comparing `state.priority_player`
/// against the raw seat instead of `turn_control::authorized_submitter_for_player`
/// — the exact bug `turn_control_priority_softlock` documents. The
/// priority-movement assertion is the reach-guard proving this is a real,
/// reducer-accepted pass rather than a degenerate state.
#[test]
fn legal_actions_answers_a_turn_controlled_pass_without_cloning_the_state() {
    let mut runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Upkeep);
        scenario.build()
    };
    {
        let state = runner.state_mut();
        // CR 723: P0 controls P1's turn.
        state.active_player = P1;
        state.turn_decision_controller = Some(P0);
        state.priority_passes.clear();
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P1 });
    }
    assert!(runner.state().players[P1.0 as usize].hand.is_empty());
    assert_ne!(
        runner.state().priority_player,
        P1,
        "reach-guard: under turn control the submitter must not be the seat"
    );

    perf_counters::reset();
    let actions = legal_actions(runner.state());
    let counters = perf_counters::snapshot();

    assert!(
        actions.contains(&GameAction::PassPriority),
        "reach-guard: the controlled seat's pass must be enumerated"
    );
    assert_eq!(
        counters.state_clone_for_legality, 0,
        "a turn-controlled bare pass must be decided structurally too"
    );

    engine::game::engine::apply_as_current(runner.state_mut(), GameAction::PassPriority)
        .expect("the enumerated pass must be reducer-accepted");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "reach-guard: after the controlled seat passes, priority must move to P0's seat, got {:?}",
        runner.state().waiting_for
    );
}

/// T4b. The soundness property across the CR 117.4 resolution seam.
///
/// The hatch is evaluated BEFORE the pass boundary, but `pass_priority_once_with_pipeline`
/// runs `priority::handle_priority_pass_with_limit` FIRST — which, on the
/// all-players-passed pass, resolves the top of the stack and executes arbitrary
/// effect logic — and only then runs the continuation drains and
/// `run_post_action_pipeline`. Every fixture here therefore makes the accepted
/// pass the pass that actually resolves (or ends the phase), so a
/// post-resolution error surface reachable from a state the hatch accepts
/// surfaces here.
///
/// **Scope note — read before citing a green run here.**
/// *"Fixture (ii) drains through `effects::drain_pending_continuation`
/// (`engine.rs:5566`), which is called WITHOUT `?`. The only fallible drains are
/// `engine.rs:5584` and `:5613-5617`. This test therefore cannot falsify §3.4
/// residual 5, and a green run here is not evidence about it."*
///
/// Falsifying that residual needs a *fallible* root
/// (`DeferredLifeCostResume::{Cast, ManaRoot}` or
/// `PendingCostMoveResume::ManaAbilityPayment`) parked from INSIDE the
/// resolution, which no fixture below does — and which reachability could not be
/// established for this unit. This test is the standing falsifier for the
/// `run_post_action_pipeline` / token-realization residuals only.
///
/// Note this asserts the shared engine predicate
/// `game::priority::pass_priority_structurally_legal` rather than
/// `ai_support::filter::structurally_valid_pass_priority`, which is private to
/// its module and unreachable from an integration test. That predicate is the
/// hatch's entire rules judgement; the hatch's remaining two gates (window shape
/// and candidate-actor identity) hold by construction in every fixture here and
/// are pinned directly in `filter.rs`'s own tests.
#[test]
fn pass_priority_hatch_stays_sound_across_the_resolution_seam() {
    let mut accepted = 0usize;
    let mut rejected = 0usize;

    // (i) Baseline: the top of the stack is a plain no-choice spell. The seam
    // runs and nothing parks.
    {
        let mut state = all_players_passed_window();
        let spell = push_resolvable_spell(&mut state);
        let before = state.stack.len();
        assert_eq!(
            before, 1,
            "reach-guard: the fixture must have a stack object"
        );
        accept_tally(&state, &mut accepted, &mut rejected);
        let after = pass_and_expect_ok(&state);
        assert_resolved(&after, spell, before);
    }

    // (ii) The boundary additionally drains an ordinary ability continuation.
    // This is the INFALLIBLE `effects::drain_pending_continuation` mechanism —
    // see the scope note above.
    {
        let mut state = all_players_passed_window();
        let spell = push_resolvable_spell(&mut state);
        let continuation = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            engine::types::identifiers::ObjectId(9001),
            P0,
        );
        let parked = PendingContinuation::new(Box::new(continuation), &state);
        state.park_ability_continuation(parked);
        let before = state.stack.len();
        let life_before = state.players[P0.0 as usize].life;
        accept_tally(&state, &mut accepted, &mut rejected);
        let after = pass_and_expect_ok(&state);
        assert_resolved(&after, spell, before);
        assert_eq!(
            after.players[P0.0 as usize].life,
            life_before + 1,
            "reach-guard: the parked continuation must ALSO have drained at this boundary. \
             This is an additional guard on the drain, never a substitute for the \
             resolution guard above."
        );
    }

    // (iii) The all-passed pass ends the phase with an empty stack, so
    // `run_post_action_pipeline` places any phase triggers. There is no stack
    // object to resolve here, so the phase change is criterion 11a's equivalent
    // reach-guard that the seam did real work.
    {
        let state = all_players_passed_window();
        assert!(state.stack.is_empty());
        accept_tally(&state, &mut accepted, &mut rejected);
        let after = pass_and_expect_ok(&state);
        assert_ne!(
            after.phase, state.phase,
            "reach-guard: the all-passed empty-stack pass must end the phase"
        );
    }

    // Non-vacuity: one fixture the predicate must refuse, where the implication
    // holds vacuously. `must_diverge` is `pub(crate)`, so the refusal is driven
    // here by a CR 723.5 submitter desync instead; the latch half is pinned by
    // `pass_priority_hatch_honors_the_divergence_latch_owner` in `filter.rs`.
    {
        let mut state = all_players_passed_window();
        push_resolvable_spell(&mut state);
        state.priority_player = P1;
        accept_tally(&state, &mut accepted, &mut rejected);
        assert!(
            !pass_priority_structurally_legal(&state, P0),
            "a desynced submitter must be refused"
        );
    }

    assert!(
        accepted >= 3,
        "non-vacuity: a predicate stuck at `false` must not report green (accepted {accepted})"
    );
    assert!(
        rejected > 0,
        "non-vacuity: a predicate stuck at `true` must not report green"
    );
}

/// A two-player `Priority` window on P0 where P1 has already passed, so P0's
/// pass is the CR 117.4 all-players-passed pass.
fn all_players_passed_window() -> GameState {
    let mut runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.build()
    };
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_passes.clear();
        state.priority_passes.insert(P1);
        state.priority_pass_count = 1;
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }
    runner.state().clone()
}

/// Criterion 11(a): prove the accepted pass actually RESOLVED a stack object
/// rather than merely handing priority along.
///
/// Two independent observables, because stack depth alone is the weaker signal:
/// the stack shrank, AND the resolving spell reached its owner's graveyard,
/// which per CR 608.2n is the final step of an instant's resolution and which
/// nothing but a real resolution performs here. A fixture that silently degraded
/// into a bare priority handoff fails both.
fn assert_resolved(after: &GameState, spell: engine::types::identifiers::ObjectId, before: usize) {
    assert!(
        after.stack.len() < before,
        "criterion 11a: the accepted pass must actually resolve a stack object \
         (stack was {before}, is {})",
        after.stack.len()
    );
    assert!(
        after.players[P0.0 as usize].graveyard.contains(&spell),
        "criterion 11a: CR 608.2n — the resolved instant must have reached its \
         owner's graveyard, proving a real resolution and not a bare handoff"
    );
}

/// Put one plain, no-choice instant on the stack so the all-passed pass has an
/// object to resolve. Returns its `ObjectId` so the caller can prove it resolved.
fn push_resolvable_spell(state: &mut GameState) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(9100),
        P0,
        "Structural Legality Test Instant".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&id)
        .expect("just-created stack object")
        .card_types
        .core_types
        .push(engine::types::card_type::CoreType::Instant);
    state.stack.push_back(StackEntry {
        id,
        source_id: id,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(9100),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    id
}

/// Record whether the predicate accepted, and assert the soundness implication
/// when it did: an accepted pass must be one the authoritative boundary runs
/// without error.
fn accept_tally(state: &GameState, accepted: &mut usize, rejected: &mut usize) {
    if pass_priority_structurally_legal(state, P0) {
        *accepted += 1;
        let mut sim = state.clone();
        let actor = turn_control::authorized_submitter_for_player(state, P0);
        assert!(
            apply_for_simulation(&mut sim, actor, GameAction::PassPriority).is_ok(),
            "the predicate accepted a pass whose real boundary errors"
        );
        assert!(
            legal_actions(state).contains(&GameAction::PassPriority),
            "reach-guard: an accepted pass must reach the public legal_actions list"
        );
    } else {
        *rejected += 1;
    }
}

/// Perform the accepted pass for real and return the resulting state.
fn pass_and_expect_ok(state: &GameState) -> GameState {
    let mut applied = state.clone();
    let actor = turn_control::authorized_submitter_for_player(state, P0);
    apply_for_simulation(&mut applied, actor, GameAction::PassPriority)
        .expect("an accepted pass must apply cleanly");
    applied
}
