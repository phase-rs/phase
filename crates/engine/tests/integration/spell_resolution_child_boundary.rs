//! CR 616.1 / CR 616.1f + CR 608.3a: a permanent spell whose Stack→Battlefield
//! delivery pauses on an enters-with-counters ordering choice must park its
//! `PendingSpellResolution` BENEATH the `CounterAdditions` child that pause
//! raised — never on top of it.
//!
//! `active_counter_additions()` is top-only by design, so a parent pushed above
//! its own live child makes every resume drain in `engine_replacement`'s Execute
//! and Prevented arms read `None`. Both frames then survive the turn and
//! `start_next_turn` trips its CR 514.3a + CR 500.1 wrap assert.
//!
//! Goes RED if the site-A boundary insert in `game/stack.rs` is reverted to the
//! plain `push_spell_resolution` (Test 1, Test 4), or if the completion helper's
//! call sites in `game/engine_replacement.rs` are removed (Test 2).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall `cards/named?exact=`). Paraphrases can take a
// different parser branch and go green while the real card stays broken.
const REYHAN: &str = "Reyhan enters with three +1/+1 counters on it.\nWhenever a creature you control dies or is put into the command zone, if it had one or more +1/+1 counters on it, you may put that many +1/+1 counters on target creature.\nPartner";
const OZOLITH: &str = "If one or more +1/+1 counters would be put on an artifact or creature you control, that many plus one +1/+1 counters are put on it instead.";
const CORPSEJACK: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on it instead.";
const HARDENED: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const VELOCIPEDE: &str = "Trample\nEach other Vehicle and creature you control enters with an additional +1/+1 counter on it if its mana value is 4 or less. Otherwise, it enters with three additional +1/+1 counters on it.\nCrew 3";
const THRINAX: &str = "Devour 1 (As this creature enters, you may sacrifice any number of creatures. It enters with that many +1/+1 counters on it.)\nEach other creature you control enters with an additional X +1/+1 counters on it, where X is the number of +1/+1 counters on this creature.";

/// Six units, enough for Reyhan's `{1}{B}{G}` and Bloodspore Thrinax's
/// `{2}{G}{G}` without staging a mana-payment sub-prompt.
fn stage_mana(scenario: &mut GameScenario) {
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_991), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(9_992), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(9_993), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(9_994), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(9_995), false, vec![]),
        ],
    );
}

/// The depth-1 fixture: Reyhan (CR 614.1c, enters with three +1/+1 counters)
/// cast into an ADDITIVE (Ozolith) x MULTIPLICATIVE (Corpsejack Menace) pair.
/// The two classes do not commute, so CR 616.1 raises a real ordering choice
/// rather than auto-resolving a degenerate one.
fn depth_one_fixture() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Ozolith, the Shattered Spire", OZOLITH);
    scenario.add_creature_from_oracle(P0, "Corpsejack Menace", 4, 4, CORPSEJACK);
    let reyhan = scenario
        .add_creature_to_hand_from_oracle(P0, "Reyhan, Last of the Abzan", 0, 0, REYHAN)
        .id();
    stage_mana(&mut scenario);
    (scenario.build(), reyhan)
}

fn plus_counters(state: &GameState, object: ObjectId) -> u32 {
    state
        .objects
        .get(&object)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn frame_kinds(state: &GameState) -> Vec<String> {
    state
        .resolution_stack
        .iter()
        .map(|frame| format!("{:?}", frame.kind()))
        .collect()
}

#[test]
fn parked_spell_resolution_sits_beneath_its_etb_counter_child() {
    let (mut runner, reyhan) = depth_one_fixture();
    let outcome = runner.cast(reyhan).resolve();
    let state = outcome.state();

    // Paired positive reach-guards, asserted BEFORE the discriminating claim: a
    // fixture that never raised the prompt would make the claim vacuously false
    // on both sides of the fix.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "CR 616.1 ordering prompt must be open — an Additive+Multiplicative pair is \
         order-material; got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        state.resolution_stack.len() >= 2,
        "both the parked parent and its ETB-counter child must be resident; frames={:?}",
        frame_kinds(state)
    );

    // The discriminating assertions: RED with the site-A boundary insert
    // reverted to the plain push, GREEN with it.
    assert!(
        state.active_counter_additions().is_some(),
        "the ETB-counter child must own the resolution-stack top while the CR 616.1 \
         ordering choice is open; a SpellResolution parent above it makes every resume \
         drain read None (engine_replacement.rs Execute/Prevented arms); frames={:?}",
        frame_kinds(state)
    );
    // `active_spell_resolution` is TOP-ONLY: `None` means "does not own the top",
    // never "has been completed". That is exactly the claim here, and it is
    // paired with the positive read above on the same instant.
    assert!(
        state.active_spell_resolution().is_none(),
        "the parked parent must NOT own the top; frames={:?}",
        frame_kinds(state)
    );
}

/// Answers the CR 616.1 prompt with `index`, then reads the state that action
/// returned. Returns `(resolution_stack_len, counters_on_reyhan)`.
///
/// The read must be IMMEDIATE: a `PassPriority` first would let
/// `sweep_and_recover_priority_boundary_rest`'s live funnel consume a `len == 1`
/// residual and silently drop the CR 608.3a epilogue, turning this test green
/// against the very bug it guards.
fn answer_ordering_choice(index: usize) -> (usize, u32, Vec<String>, WaitingFor) {
    let (mut runner, reyhan) = depth_one_fixture();
    let outcome = runner.cast(reyhan).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: the CR 616.1 ordering prompt must be open before it can be answered"
    );

    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answering the CR 616.1 ordering choice must be a legal action");

    let state = runner.state();
    (
        state.resolution_stack.len(),
        plus_counters(state, reyhan),
        frame_kinds(state),
        state.waiting_for.clone(),
    )
}

#[test]
fn answering_the_etb_counter_ordering_choice_drains_the_resolution_stack() {
    let (len_first, counters_first, frames_first, waiting_first) = answer_ordering_choice(0);
    let (len_second, counters_second, frames_second, waiting_second) = answer_ordering_choice(1);

    // The revert-failing assertion: without the completion helper's call sites a
    // lone `SpellResolution` frame is left resident (`len == 1`).
    assert_eq!(
        len_first, 0,
        "both frames must retire once the choice is answered; frames={frames_first:?}"
    );
    assert_eq!(
        len_second, 0,
        "both frames must retire once the choice is answered; frames={frames_second:?}"
    );
    assert!(matches!(waiting_first, WaitingFor::Priority { .. }));
    assert!(matches!(waiting_second, WaitingFor::Priority { .. }));

    // CR 616.1f order-materiality — this doubles as the multi-authority hostile
    // fixture: two competing replacement authorities on one event whose answer
    // is order-dependent. Equal totals would mean the fixture is NOT
    // order-material and the prompt above was reached for the wrong reason.
    assert_ne!(
        counters_first, counters_second,
        "the two orderings must produce different totals, or the fixture is not \
         order-material and Test 1's prompt was reached for the wrong reason"
    );
    assert_eq!(
        counters_first, 8,
        "Ozolith applied first: (3 + 1) * 2 under the CR 616.1f repeat"
    );
    assert_eq!(
        counters_second, 7,
        "Corpsejack Menace applied first: 3 * 2 + 1 under the CR 616.1f repeat"
    );
}

/// Drives the real phase pipeline across the turn boundary. `advance_to_phase`
/// cannot be used: it breaks the moment `waiting_for` is not `Priority`, which a
/// `DeclareAttackers` window is, producing a silent no-advance that reads as
/// "no panic".
fn wrap_the_turn(runner: &mut GameRunner) {
    let start_turn = runner.state().turn_number;
    for step in 0..200 {
        if runner.state().turn_number > start_turn {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            other => panic!("turn wrap stuck at {other:?} (step {step})"),
        };
        runner
            .act(action)
            .unwrap_or_else(|err| panic!("turn wrap action refused at step {step}: {err:?}"));
    }
    panic!("turn wrap loop exhausted without advancing the turn");
}

#[test]
fn turn_boundary_after_a_paused_etb_counter_entry_does_not_strand_the_resolution_stack() {
    let (mut runner, reyhan) = depth_one_fixture();
    let outcome = runner.cast(reyhan).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: the CR 616.1 ordering prompt must be open"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answering the CR 616.1 ordering choice must be a legal action");

    let start_turn = runner.state().turn_number;
    // CR 514.3a + CR 500.1: on unpatched `main` this reaches `start_next_turn`'s
    // assert with a non-empty resolution stack.
    wrap_the_turn(&mut runner);
    assert!(
        runner.state().turn_number > start_turn,
        "reach-guard: the turn must actually have advanced before the verdict is read"
    );
    assert!(
        runner.state().resolution_stack.is_empty(),
        "the resolution stack must not survive the turn wrap; frames={:?}",
        frame_kinds(runner.state())
    );
}

#[test]
fn devour_entrant_parks_its_resolution_frame_beneath_its_whole_child_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Thunderous Velocipede", VELOCIPEDE);
    scenario.add_creature_from_oracle(P0, "Corpsejack Menace", 4, 4, CORPSEJACK);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED);
    // The intended Devour victim — not the only legal one, and nothing here
    // depends on the pool's membership because the choice is never answered.
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    let thrinax = {
        let mut builder = scenario.add_creature_to_hand(P0, "Bloodspore Thrinax", 2, 2);
        // The keyword hint MUST be the colon form: `Keyword`'s `FromStr` splits
        // on `:`, and the space form yields no synthesized `Moved` + `Sacrifice`
        // replacement — the fixture would silently degrade to the depth-1 shape
        // and every assertion below would pass for the wrong reason.
        builder.from_oracle_text_with_keywords(&["devour:1"], THRINAX);
        builder.with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        });
        builder.id()
    };
    stage_mana(&mut scenario);
    let mut runner = scenario.build();

    // Positive reach-guards, asserted BEFORE the claim. Keyed on all three of
    // event / valid_card / a Sacrifice execute, so the `valid_card` sibling the
    // degraded fixture keeps cannot satisfy it. `iter_all` is `pub(crate)`;
    // `iter_unchecked` is the public equivalent.
    assert!(
        runner.state().objects[&thrinax]
            .replacement_definitions
            .iter_unchecked()
            .any(|def| def.event == ReplacementEvent::Moved
                && def.valid_card == Some(TargetFilter::SelfRef)
                && def
                    .execute
                    .as_ref()
                    .is_some_and(|ability| matches!(*ability.effect, Effect::Sacrifice { .. }))),
        "synthesize_devour must have produced the CR 702.82a as-enters sacrifice \
         replacement — a fixture without it degrades to the ordinary depth-1 shape"
    );

    let outcome = runner.cast(thrinax).resolve();
    let state = outcome.state();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: the CR 616.1 ordering prompt must be open; got {:?}",
        outcome.final_waiting_for()
    );

    // The discriminating claim at depth 3: the boundary insert is
    // depth-agnostic — the parent parks beneath its WHOLE child stack, not just
    // beneath a single child.
    assert!(
        state.resolution_stack.len() >= 3,
        "the Devour snapshot frame and the counter child must BOTH be resident above \
         the parent; frames={:?}",
        frame_kinds(state)
    );
    assert!(
        state.active_counter_additions().is_some(),
        "the counter child must own the top at depth 3, exactly as at depth 1 — the \
         boundary insert is depth-agnostic; frames={:?}",
        frame_kinds(state)
    );
    assert!(
        state.active_spell_resolution().is_none(),
        "the parked parent must NOT own the top; frames={:?}",
        frame_kinds(state)
    );

    // This test deliberately STOPS before answering the choice. Answering it
    // reaches a deferred residual: the counter child retires, the parent is left
    // buried beneath the still-live CR 614.13a Devour eligibility snapshot, and
    // the turn wrap panics — the same panic `main` produces, with one fewer
    // stranded frame and the counter work completed. Do not extend this test
    // past this point without re-measuring.
}

#[test]
fn two_additive_counter_replacements_commute_and_raise_no_ordering_choice() {
    // Negative sibling: Ozolith (+1) and Hardened Scales (+1) are both ADDITIVE,
    // so `CommuteClass::commutes_with` returns true, CR 616.1 is degenerate, and
    // the choice auto-resolves. Proves the pause in Test 1 is genuinely
    // CR 616.1-gated rather than raised by any two applicable replacements.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Ozolith, the Shattered Spire", OZOLITH);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED);
    let reyhan = scenario
        .add_creature_to_hand_from_oracle(P0, "Reyhan, Last of the Abzan", 0, 0, REYHAN)
        .id();
    stage_mana(&mut scenario);
    let mut runner = scenario.build();
    let outcome = runner.cast(reyhan).resolve();
    let state = outcome.state();

    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "two commuting Additive replacements must not raise a CR 616.1 prompt; got {:?}",
        state.waiting_for
    );
    assert!(
        state.resolution_stack.is_empty(),
        "no pause means no parked frame; frames={:?}",
        frame_kinds(state)
    );
    assert_eq!(
        plus_counters(state, reyhan),
        5,
        "CR 614.1c three counters, then +1 and +1 in either order"
    );
}

#[test]
fn a_single_counter_replacement_authority_raises_no_ordering_choice() {
    // Single authority: `candidates.len() == 1` and non-optional, so there is
    // nothing to order and no pause.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Ozolith, the Shattered Spire", OZOLITH);
    let reyhan = scenario
        .add_creature_to_hand_from_oracle(P0, "Reyhan, Last of the Abzan", 0, 0, REYHAN)
        .id();
    stage_mana(&mut scenario);
    let mut runner = scenario.build();
    let outcome = runner.cast(reyhan).resolve();
    let state = outcome.state();

    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(
        state.resolution_stack.is_empty(),
        "frames={:?}",
        frame_kinds(state)
    );
    assert_eq!(plus_counters(state, reyhan), 4, "3 + 1");
}

#[test]
fn a_permanent_spell_that_raises_no_child_still_resolves_and_drains() {
    // No child raised at all: a vanilla creature has no enters-with-counters
    // replacement, so the delivery tail never pauses, site A is never reached,
    // and the resolution stack must be empty on both sides of the fix. The
    // direct guard that the site-A change is inert on entries that do not pause.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Ozolith, the Shattered Spire", OZOLITH);
    scenario.add_creature_from_oracle(P0, "Corpsejack Menace", 4, 4, CORPSEJACK);
    let bears = {
        let mut builder = scenario.add_creature_to_hand(P0, "Grizzly Bears", 2, 2);
        builder.with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        });
        builder.id()
    };
    stage_mana(&mut scenario);
    let mut runner = scenario.build();
    let outcome = runner.cast(bears).resolve();
    let state = outcome.state();

    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    outcome.assert_zone(&[bears], Zone::Battlefield);
    assert!(
        state.resolution_stack.is_empty(),
        "an entry that raises no child must leave nothing parked; frames={:?}",
        frame_kinds(state)
    );
    assert_eq!(
        plus_counters(state, bears),
        0,
        "no enters-with-counters replacement means the doublers never fire"
    );
}

#[test]
fn an_out_of_range_ordering_answer_is_refused_and_leaves_the_frames_nested() {
    // Empty / decline path: the answer is rejected before any drain runs, so the
    // prompt stays open and both frames stay resident in the CORRECTED order —
    // the parent still beneath its child, nothing stranded.
    let (mut runner, reyhan) = depth_one_fixture();
    let outcome = runner.cast(reyhan).resolve();
    let candidate_count = match outcome.final_waiting_for() {
        WaitingFor::ReplacementChoice {
            candidate_count, ..
        } => *candidate_count,
        other => panic!("reach-guard: the CR 616.1 ordering prompt must be open; got {other:?}"),
    };
    assert!(
        candidate_count >= 2,
        "an ordering choice needs two candidates"
    );

    let refused = runner.act(GameAction::ChooseReplacement {
        index: candidate_count,
    });
    assert!(
        refused.is_err(),
        "an out-of-range replacement index must be refused"
    );

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the prompt must still be open after a refused answer; got {:?}",
        state.waiting_for
    );
    assert!(
        state.active_counter_additions().is_some(),
        "the child must still own the top; frames={:?}",
        frame_kinds(state)
    );
    assert!(
        state.active_spell_resolution().is_none(),
        "the parent must still be parked beneath it; frames={:?}",
        frame_kinds(state)
    );
}
