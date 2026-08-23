//! Discriminating tests for the `(EndCombat, DeclareAttackers)` wedge and the
//! un-drained `deferred_triggers` queue behind it.
//!
//! REACHABILITY NOTE (non-negotiable — read before editing any fixture here).
//! Every test in this module seeds its parked batch by calling the engine's own
//! collector, `triggers::collect_triggers_into_deferred(state, &[real
//! ZoneChanged event])`. Nothing here hand-constructs a `PendingTriggerContext`
//! and nothing here hand-assembles a corrupt `GameState`: the trigger contexts
//! are built by the real collector, from a real event whose `ZoneChangeRecord`
//! comes from the authoritative production constructor
//! (`GameObject::snapshot_for_zone_change`), against a real `GameObject` created
//! by `zones::create_object`. **Everything downstream of seeding is the
//! unmodified production pipeline** — `apply()` / `start_game_skip_mulligan()`
//! → reducer → `handle_declare_attackers` / `handle_priority_pass` →
//! `advance_after_empty_attackers` / `advance_phase_once` → `auto_advance` →
//! `execute_cleanup` → `sync_waiting_for`.
//!
//! Why a fully-public seeding path is not used: `run_post_action_pipeline`
//! drains at every `WaitingFor::Priority` boundary, so **on a fixed engine no
//! public action sequence can leave a queue parked across a boundary — the
//! reachable park IS the bug.** `collect_triggers_into_deferred` is
//! `pub(crate)`, which is why these live in an in-crate module rather than in
//! `tests/integration/`. The public-API positive controls that prove these
//! assertions are non-vacuous live in
//! `tests/integration/declare_attackers_end_combat_pairing.rs`.

use super::*;
use crate::game::scenario::GameScenario;
use crate::game::{engine, triggers, zones};
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::game_state::{StackEntryKind, ZoneChangeRecord};
use crate::types::identifiers::CardId;
use crate::types::zones::Zone;

/// Altar of the Brood's verbatim Oracle text (MTGJSON, via the repo's own
/// `card-data.json`). Per `/card-test`, fixtures are built from the real card's
/// exact text — a paraphrase can take a different parser branch and go green
/// while the real card stays broken.
///
/// The real card is a 1-mana **Artifact**. These fixtures build it as a
/// noncreature permanent, which is what both fixture constraints require: it is
/// SBA-safe (CR 704.5f cannot move a noncreature permanent for 0 toughness) and
/// it is not a legal attacker (so `advance_to_end_step` cannot park at
/// `Phase::DeclareAttackers`). The trigger condition names "another permanent
/// you control", never the source's own card type, so the source's type is not
/// load-bearing for what these tests measure.
const ALTAR_OF_THE_BROOD: &str =
    "Whenever another permanent you control enters, each opponent mills a card.";

/// Impact Tremors' verbatim Oracle text (MTGJSON, via `card-data.json`).
///
/// A **second, semantically distinct** observer of the same entry event, needed
/// by the CR 603.3b ordering rows. Two copies of one card will NOT raise an
/// ordering prompt: `strip_trigger_instance_identity` deliberately strips
/// per-instance object identity so genuinely indistinguishable triggers take
/// `TriggerOrderingDisposition::NoChoiceNeeded` — there is no choice to make
/// between two identical abilities. A real CR 603.3b choice needs two triggers
/// that actually differ.
///
/// Impact Tremors is an **Enchantment**, so it preserves the noncreature
/// fixture constraints (SBA-safe; not a legal attacker). Its condition names a
/// *creature* entering, which is why `battlefield_entry_event` builds a creature.
const IMPACT_TREMORS: &str =
    "Whenever a creature you control enters, this enchantment deals 1 damage to each opponent.";

/// Put a noncreature permanent carrying the Altar observer onto the battlefield
/// under `player`, and return its `ObjectId`.
fn add_altar(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let builder =
        scenario.add_enchantment_from_oracle(player, "Altar of the Brood", ALTAR_OF_THE_BROOD);
    builder.id()
}

/// Put the second, distinct observer onto the battlefield under `player`.
fn add_impact_tremors(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let builder = scenario.add_enchantment_from_oracle(player, "Impact Tremors", IMPACT_TREMORS);
    builder.id()
}

/// Build a **real** `GameEvent::ZoneChanged` for a permanent entering the
/// battlefield under `controller`, using the production record constructor.
///
/// This is the event an ordinary ETB emits; feeding it to the engine's own
/// collector is what parks a genuine observer context.
///
/// The entering permanent is a **2/2 creature**: that satisfies BOTH observers'
/// conditions (Altar of the Brood's "another permanent you control" and Impact
/// Tremors' "a creature you control"), and a positive toughness keeps it
/// SBA-safe under CR 704.5f so no fresh `ZoneChanged` is emitted behind the
/// test's back.
fn battlefield_entry_event(state: &mut GameState, controller: PlayerId) -> GameEvent {
    let card_id = CardId(state.next_object_id);
    let id = zones::create_object(
        state,
        card_id,
        controller,
        "Entering Creature".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).expect("entering object exists");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    let obj = state.objects.get(&id).expect("entering object exists");
    let record: ZoneChangeRecord =
        obj.snapshot_for_zone_change(id, Some(Zone::Hand), Zone::Battlefield);
    GameEvent::ZoneChanged {
        object_id: id,
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(record),
    }
}

/// Seed one parked observer context via the engine's own collector.
/// Returns the `ObjectId` of the permanent whose entry produced the event.
fn seed_parked_trigger(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let event = battlefield_entry_event(state, controller);
    let entering_id = match &event {
        GameEvent::ZoneChanged { object_id, .. } => *object_id,
        _ => unreachable!("battlefield_entry_event returns ZoneChanged"),
    };
    triggers::collect_triggers_into_deferred(state, std::slice::from_ref(&event));
    entering_id
}

/// The `source_id` of the single stack entry, for the "drained, not cleared"
/// assertions. Panics with the stack contents if the shape is wrong.
fn sole_stack_trigger_source(state: &GameState) -> ObjectId {
    assert_eq!(
        state.stack.len(),
        1,
        "expected exactly one stack entry, got {:?}",
        state.stack
    );
    let entry = state.stack.last().expect("stack has one entry");
    assert!(
        matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }),
        "top of stack must be a triggered ability, got {:?}",
        entry.kind
    );
    entry.source_id
}

/// Rows A1 / A2 / A3.
///
/// A1 (CR 603.3 + CR 117.5): a parked `deferred_triggers` batch is put on the
/// stack when the phase interpreter crosses a phase boundary.
/// A2: the queue is **drained, not cleared** — a `deferred_triggers.clear()`
/// band-aid leaves the stack empty and fails here.
/// A3 (CR 508.8 + CR 511.1): the stale `DeclareAttackers` prompt does not
/// survive the advance past `Phase::DeclareAttackers`.
///
/// DISCRIMINATION BOUNDARY (mandatory — do not relabel this test).
/// This test reds when **A-2** (the `current_trigger_prompt` narrowing) is
/// reverted. It is **green** with A-2 applied and A-1 reverted: with the echo
/// gone the arm falls through to `WaitingFor::Priority`, `apply_action`'s
/// post-action gate admits the pipeline, and `engine_priority.rs`'s
/// `Priority`-gated drain empties the queue one step later. It is therefore
/// **not** A-1's discriminating test —
/// `parked_queue_drains_at_first_upkeep_from_start_game` is.
#[test]
fn declare_no_attackers_with_parked_triggers_drains_and_leaves_declare_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let altar = add_altar(&mut scenario, PlayerId(0));
    // A real attacker, so `valid_attacker_ids` is non-empty and the prompt is a
    // genuine choice rather than a forced no-op.
    scenario.add_vanilla(PlayerId(0), 2, 2);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Install the CR 508.1 prompt with the production builder.
    state.waiting_for = combat::build_declare_attackers_waiting_for(state);

    let entering = seed_parked_trigger(state, PlayerId(0));
    let graveyard_before = state.players[1].graveyard.len();

    // --- Pre-action reach-guards: the negative assertions below cannot pass
    // --- vacuously via a rejected action or a phase that never advanced.
    assert_eq!(state.phase, Phase::DeclareAttackers);
    assert!(
        matches!(state.waiting_for, WaitingFor::DeclareAttackers { .. }),
        "fixture must start at the CR 508.1 declaration prompt, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.deferred_triggers.len(),
        1,
        "exactly one observer context must be parked"
    );
    assert!(
        state.stack.is_empty(),
        "fixture must start with an empty stack"
    );

    let result = engine::apply(
        runner.state_mut(),
        PlayerId(0),
        GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
    );
    assert!(
        result.is_ok(),
        "declaring no attackers must succeed: {result:?}"
    );

    let state = runner.state();
    // CR 508.8: with no attackers, declare blockers and combat damage are
    // skipped and the game advances to the end of combat step.
    assert_eq!(
        state.phase,
        Phase::EndCombat,
        "CR 508.8: an empty declaration advances past combat"
    );
    // A3 — CR 511.1: end of combat has no turn-based actions; the active player
    // gets priority. The stale declaration prompt must be gone.
    assert!(
        !matches!(state.waiting_for, WaitingFor::DeclareAttackers { .. }),
        "the CR 508.1 declaration prompt must not survive the CR 508.8 advance, got {:?}",
        state.waiting_for
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == state.active_player),
        "CR 511.1: expected Priority for the active player, got {:?}",
        state.waiting_for
    );
    // A1 — CR 603.3: the parked batch was put on the stack.
    assert!(
        state.deferred_triggers.is_empty(),
        "CR 603.3: the parked queue must be drained, still holds {:?}",
        state.deferred_triggers
    );
    // A2 — drained, NOT cleared: the ability is really on the stack, bound to
    // the permanent whose entry triggered it.
    assert_eq!(
        sole_stack_trigger_source(state),
        altar,
        "the stacked ability's source must be the observer, not {entering:?}"
    );
    // ...and it is UNRESOLVED: the arm returned priority without resolving it.
    assert_eq!(
        state.players[1].graveyard.len(),
        graveyard_before,
        "the ability is on the stack, not resolved — no mill has happened yet"
    );
}

/// Row A5 — hostile, non-empty declaration.
///
/// A parked queue plus a **real** attack declaration (`attacks_empty == false`)
/// also drains and reaches `Priority` at `Phase::DeclareAttackers` (CR 508.2).
/// This exercises `finish_declare_attackers`'s `else` arm, which never reaches
/// `advance_after_empty_attackers`.
///
/// **Non-regression row, not revert-failing.** This path returns `Priority`
/// directly, so the drain it exercises is `run_post_action_pipeline`'s —
/// pre-existing and expected green on main.
#[test]
fn declare_real_attacker_with_parked_triggers_drains_at_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    add_altar(&mut scenario, PlayerId(0));
    let attacker = scenario.add_vanilla(PlayerId(0), 2, 2);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = combat::build_declare_attackers_waiting_for(state);
    seed_parked_trigger(state, PlayerId(0));

    assert_eq!(state.phase, Phase::DeclareAttackers);
    assert_eq!(state.deferred_triggers.len(), 1);

    let result = engine::apply(
        runner.state_mut(),
        PlayerId(0),
        GameAction::DeclareAttackers {
            attacks: vec![(attacker, combat::AttackTarget::Player(PlayerId(1)))],
            bands: vec![],
        },
    );
    assert!(
        result.is_ok(),
        "declaring a real attacker must succeed: {result:?}"
    );

    let state = runner.state();
    // CR 508.2: the active player gets priority after the declaration; the
    // phase does NOT advance past declare attackers.
    assert_eq!(state.phase, Phase::DeclareAttackers);
    assert!(
        state.deferred_triggers.is_empty(),
        "CR 603.3: the parked queue must be drained, still holds {:?}",
        state.deferred_triggers
    );
}

/// Row A6 — hostile, multi-authority (CR 603.3b).
///
/// **Two** parked triggers under the **same controller** must raise a genuine
/// `OrderTriggers` prompt rather than being auto-ordered or dropped. This also
/// proves the prompt the arm returns is a **real** prompt produced by the
/// drain, not the stale echo.
///
/// The two observers must be **distinct cards**. Two copies of one card produce
/// byte-identical triggers, which `strip_trigger_instance_identity` recognises
/// as genuinely indistinguishable, so the engine correctly takes
/// `NoChoiceNeeded` and returns `Priority` — there is no ordering choice to
/// make between two identical abilities.
#[test]
fn two_parked_triggers_surface_cr_603_3b_ordering() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    add_altar(&mut scenario, PlayerId(0));
    add_impact_tremors(&mut scenario, PlayerId(0));
    scenario.add_vanilla(PlayerId(0), 2, 2);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = combat::build_declare_attackers_waiting_for(state);
    seed_parked_trigger(state, PlayerId(0));

    assert_eq!(
        state.deferred_triggers.len(),
        2,
        "two observers must each park a context"
    );

    let result = engine::apply(
        runner.state_mut(),
        PlayerId(0),
        GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
    );
    assert!(
        result.is_ok(),
        "declaring no attackers must succeed: {result:?}"
    );

    let state = runner.state();
    assert_eq!(state.phase, Phase::EndCombat, "CR 508.8");
    // CR 603.3b: two simultaneous triggers under one controller — that player
    // chooses the order they go on the stack.
    match &state.waiting_for {
        WaitingFor::OrderTriggers { player, triggers } => {
            assert_eq!(
                *player,
                PlayerId(0),
                "CR 603.3b: the controller of the triggers chooses their order"
            );
            assert_eq!(triggers.len(), 2, "both parked contexts must be offered");
        }
        other => panic!("CR 603.3b: expected an ordering prompt, got {other:?}"),
    }
}

/// **Row A11 — Unit A's discriminating test. This is the test the A-1 drain
/// requires; it must exist.**
///
/// `start_game_skip_mulligan` contains **no** `run_post_action_pipeline` call —
/// it calls `turns::auto_advance`, assigns `state.waiting_for` from the result,
/// finalizes public state, and returns an `ActionResult`. This is therefore the
/// discriminating test for the drain in `turns::process_phase_triggers`:
/// reverting that drain alone (leaving the `current_trigger_prompt` narrowing in
/// place) makes the `Upkeep` arm fall through to `Priority` with the queue still
/// parked, and **nothing downstream drains it**. Do not delete this test when
/// refactoring the declare-attackers tests.
///
/// CR 502.4: no player receives priority during the untap step, and any ability
/// that triggers then is held until the next time a player would receive
/// priority — usually upkeep. So `Upkeep` (CR 503.1) is the first arm on a new
/// turn that can settle the queue.
#[test]
fn parked_queue_drains_at_first_upkeep_from_start_game() {
    let mut scenario = GameScenario::new();
    let altar = add_altar(&mut scenario, PlayerId(0));
    let mut runner = scenario.build();
    let state = runner.state_mut();
    seed_parked_trigger(state, PlayerId(0));

    // --- Pre-call reach-guards.
    assert_eq!(
        state.deferred_triggers.len(),
        1,
        "exactly one observer context must be parked before the walk"
    );
    assert!(
        state.stack.is_empty(),
        "fixture must start with an empty stack"
    );

    // MANDATORY — arm the `waiting_for` reach-guard. `GameState::new`
    // initialises `waiting_for` to `Priority { PlayerId(0) }` and
    // `start_game_skip_mulligan` sets `active_player` to `PlayerId(0)`, so
    // WITHOUT this line the stale echo on main is byte-identical to the value
    // the fix produces and the post-call `player == active_player` assertion
    // passes on main — i.e. it would be vacuous. With this line it fails on
    // main, making that assertion a genuine reach-guard and a second
    // independent discriminator. Do not "clean this up".
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    let result = engine::start_game_skip_mulligan(runner.state_mut());

    let state = runner.state();
    // CR 501.1 + CR 502.4: the walk crossed Untap (no priority, no triggers
    // processed) and stopped at Upkeep.
    assert_eq!(
        state.phase,
        Phase::Upkeep,
        "the walk must reach the upkeep step"
    );
    // Genuine reach-guard, given the pre-seed above: on main the stale echo
    // returns `Priority { PlayerId(1) }` and this fails.
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { player } if player == state.active_player),
        "CR 503.1: expected Priority for the active player, got {:?}",
        result.waiting_for
    );
    // --- The two assertions carrying the discrimination: positive facts only
    // --- the drain can satisfy.
    assert!(
        state.deferred_triggers.is_empty(),
        "CR 603.3: the parked queue must be drained at the first upkeep, still holds {:?}",
        state.deferred_triggers
    );
    assert_eq!(
        sole_stack_trigger_source(state),
        altar,
        "CR 603.3 + CR 503.1a: the observer's ability must be on the stack"
    );
}

/// Row A11's multi-authority case (CR 603.3b).
///
/// Proves the pipeline-free consumer surfaces a **real** drain prompt rather
/// than a fallback: two parked contexts under one controller must produce an
/// `OrderTriggers` prompt out of `start_game_skip_mulligan` itself.
///
/// Two DISTINCT observers, for the reason given on
/// `two_parked_triggers_surface_cr_603_3b_ordering`.
#[test]
fn two_parked_triggers_from_start_game_surface_cr_603_3b_ordering() {
    let mut scenario = GameScenario::new();
    add_altar(&mut scenario, PlayerId(0));
    add_impact_tremors(&mut scenario, PlayerId(0));
    let mut runner = scenario.build();
    let state = runner.state_mut();
    seed_parked_trigger(state, PlayerId(0));

    assert_eq!(
        state.deferred_triggers.len(),
        2,
        "two observers must each park a context"
    );

    let result = engine::start_game_skip_mulligan(runner.state_mut());

    match &result.waiting_for {
        WaitingFor::OrderTriggers { player, triggers } => {
            assert_eq!(
                *player,
                PlayerId(0),
                "CR 603.3b: the controller of the triggers chooses their order"
            );
            assert_eq!(
                triggers.len(),
                2,
                "the pipeline-free consumer must offer both parked contexts"
            );
        }
        other => panic!(
            "CR 603.3b: the pipeline-free consumer must surface a real ordering prompt, got {other:?}"
        ),
    }
}

/// **Row A12 — Unit A2's discriminating test. The brief's second required
/// invariant.**
///
/// CR 514.3a: a `deferred_triggers` batch live at the cleanup step is put on the
/// stack **during cleanup**, the active player gets priority, and the turn does
/// **not** advance.
///
/// It reds on unmodified main (the queue crosses the boundary and the turn
/// advances) **and** reds with Units A and C applied but A2 reverted (the queue
/// crosses the boundary and drains at the *next* turn's `Upkeep`, so
/// `turn_number` increments and `phase == Upkeep`). It therefore discriminates
/// Unit A2 specifically.
///
/// WHICH ASSERTIONS CARRY THE DISCRIMINATION — read before attributing a red.
/// Only `turn_number == recorded` and `phase == Phase::Cleanup` discriminate.
/// The other three (`deferred_triggers.is_empty()`, the stack shape, and the
/// `Priority` pairing) are **green on main**, because on main the queue does
/// drain inside the same `apply` call — one step later, at the next turn's
/// upkeep. Those three are anti-band-aid guards (they fail a
/// `deferred_triggers.clear()` shortcut and a drain that loses the source
/// identity), not discriminators.
///
/// SEEDING ORDER IS LOAD-BEARING: the park is seeded **after** every earlier
/// priority pass, immediately before the final one. Seeding earlier is wrong —
/// an earlier pass returns `Priority` and the post-action pipeline's drain would
/// empty the queue before it could ever reach cleanup.
///
/// FIXTURE TRAP 1: the seeded permanent must be attack-incapable, or a walk
/// through combat parks at `Phase::DeclareAttackers` and the test never reaches
/// this seam. `ALTAR_OF_THE_BROOD` is built as a noncreature permanent for
/// exactly this reason; the `phase == Phase::End` reach-guard below catches a
/// regression loudly rather than vacuously.
///
/// FIXTURE TRAP 2: the fixture is placed **directly** at the end step with
/// `at_phase(Phase::End)` rather than walked there with `advance_to_end_step()`.
/// A `GameScenario` library is empty, so a walk that crosses the draw step kills
/// the active player (CR 704.5b) and the game ends — the observed symptom was
/// `InvalidAction("apply_as_current: no authorized submitter (game over?)")` on
/// the first pass. `at_phase` also sets `waiting_for`, `priority_player`,
/// `active_player`, and `turn_number` consistently, which is exactly the
/// pre-state this row's reach-guards assert.
#[test]
fn parked_queue_settles_during_cleanup_not_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let altar = add_altar(&mut scenario, PlayerId(0));
    let mut runner = scenario.build();

    // Pass with every player EXCEPT the last, so the next pass is the one that
    // ends the end step and enters cleanup.
    let active = runner.state().active_player;
    runner
        .act(GameAction::PassPriority)
        .expect("first end-step pass must succeed");

    // Seed the park only now — see the doc comment.
    let state = runner.state_mut();
    seed_parked_trigger(state, PlayerId(0));

    // --- Pre-action reach-guards.
    assert_eq!(
        state.phase,
        Phase::End,
        "fixture must be at the end step before the final pass"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "fixture must hold priority before the final pass, got {:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty(), "fixture must have an empty stack");
    assert_eq!(state.deferred_triggers.len(), 1);
    let turn_before = state.turn_number;
    let passer = match state.waiting_for {
        WaitingFor::Priority { player } => player,
        ref other => unreachable!("guarded above, got {other:?}"),
    };

    let result = engine::apply(runner.state_mut(), passer, GameAction::PassPriority);
    assert!(
        result.is_ok(),
        "the final end-step pass must succeed: {result:?}"
    );

    let state = runner.state();
    // --- The two discriminating assertions (CR 514.3a).
    assert_eq!(
        state.turn_number, turn_before,
        "CR 514.3a: the queue must settle DURING cleanup — the turn must not roll"
    );
    assert_eq!(
        state.phase,
        Phase::Cleanup,
        "CR 514.3a: the game must still be in the cleanup step, got {:?}",
        state.phase
    );
    // --- Anti-band-aid guards (green on main; they fail a `clear()` shortcut).
    assert!(
        state.deferred_triggers.is_empty(),
        "CR 603.3: the parked queue must be drained, still holds {:?}",
        state.deferred_triggers
    );
    assert_eq!(
        sole_stack_trigger_source(state),
        altar,
        "CR 514.3a: the observer's ability must be on the stack"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == state.active_player),
        "CR 514.3a: the active player gets priority during cleanup, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        active, state.active_player,
        "the active player must not change"
    );
}
