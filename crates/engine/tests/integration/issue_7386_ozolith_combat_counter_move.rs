//! Issue #7386 coverage: The Ozolith's beginning-of-combat counter move.
//!
//! "At the beginning of combat on your turn, if The Ozolith has counters on it,
//! you may move all counters from The Ozolith onto target creature."
//!
//! SHAPE UNDER TEST. `MoveCounters { source: SelfRef, mode: Move, selection:
//! StackTarget, target: Typed{Creature} }`, driven by a `Phase` trigger, with
//! `counter_type: null` / `count: null` ("all counters, every kind").
//! `selection: StackTarget` is the discriminant routing `resolve_move` into the
//! single-destination branch rather than the two "any number" distribution
//! branches; before this file that branch had no end-to-end coverage at all
//! (the nearest test, issue_680_shalai_upkeep_move.rs, drives
//! `ResolutionDistributionAnyNumber`).
//!
//! DELIBERATELY NOT COVERED HERE. Other points on the `StackTarget` grid remain
//! uncovered: the `count: Some(n)` transfer-limit branch and the
//! `counter_type: Some(kind)` narrowing used by the activated siblings
//! (Weapon Rack, Afiya Grove, Costume Closet, Simic Fluxmage, Diamond City).
//! This file scopes to the trigger-driven "all counters" instance that #7386
//! reports; the limit/narrowing axes are follow-up work, not covered by a claim
//! made here.
//!
//! STATUS OF THE UNDERLYING REPORT. Issue #7386 reports that the trigger fired,
//! a creature was chosen, the trigger resolved, and no counters moved. That
//! narrative is the reporter's; it is NOT engine-confirmed. The verbatim
//! reported scenario passes on unmodified `origin/main`, and no save or replay
//! of the reporting game exists, so the root cause is UNPINNED. These tests
//! close the coverage gap above and pin the four distinct paths that all
//! produce the reporter's user-visible symptom (counters stay put):
//!
//!   1. the declared target is illegal at resolution      (CR 608.2b)
//!   2. the intervening-if is false at resolution          (CR 603.4)
//!   3. no legal target exists at put-on-stack time        (CR 603.3d)
//!   4. the "you may" window is declined or unanswered     (CR 608.2d)
//!
//! Paths 1-3 emit no prompt at all, matching the report's most diagnostic
//! detail (no "you may" prompt was shown).
//!
//! A successful move IS distinguishable in the event stream: it commits through
//! `apply_counter_move_commit` (game/effects/counters.rs), which emits
//! `GameEvent::CounterRemoved` then `GameEvent::CounterAdded`. Every one of the
//! four no-op paths returns before those. What the event stream does NOT
//! distinguish is WHICH of the four fired — all four emit only
//! `EffectResolved`, and three of them present no prompt, so they are
//! indistinguishable from one another at the table. That is why the reporting
//! game could not be diagnosed after the fact.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 122.5: If an effect says to move a counter, it's removed from the
//!     first object and put on the second object. If either action isn't
//!     possible, no counter is removed from or put onto anything.
//!   - CR 603.3d: A triggered ability that requires targets is put on the stack
//!     only if legal targets are available; otherwise it's removed.
//!   - CR 603.4: An intervening-if clause is checked on trigger and again on
//!     resolution; if false at resolution the ability is removed from the stack.
//!   - CR 608.2b: If all of a spell or ability's targets are illegal on
//!     resolution, it doesn't resolve and is removed from the stack.
//!   - CR 608.2d: Choices offered by a resolving effect are announced as the
//!     effect is applied; a player can't choose an impossible option.

use super::rules::{GameScenario, Phase, WaitingFor, Zone, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;

const OZOLITH_ORACLE: &str = "Whenever a creature you control leaves the battlefield, if it had counters on it, put those counters on The Ozolith.\nAt the beginning of combat on your turn, if The Ozolith has counters on it, you may move all counters from The Ozolith onto target creature.";

/// Same `MoveCounters` shape as The Ozolith's second ability but with NO
/// intervening-if, so the trigger still resolves when the source holds nothing.
/// Used to pin the counterfactual the negative tests depend on.
const NO_GATE_ORACLE: &str = "At the beginning of combat on your turn, you may move all counters from Counter Shuttle onto target creature.";

/// Removal used to take a creature off the battlefield through the production
/// pipeline. Casting this and letting it resolve drives the departure through
/// `ProposedEvent::ZoneChange` and the state-based-action pass, so replacement
/// effects apply and the engine itself queues any leaves-the-battlefield
/// trigger — none of which a direct `zones::move_to_zone` write would exercise.
const DESTROY_ORACLE: &str = "Destroy target creature.";

/// Count counters of a given type on an object.
fn counters(runner: &super::rules::GameRunner, id: ObjectId, ct: &CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(ct).copied())
        .unwrap_or(0)
}

/// Which decisions the engine actually presented while the trigger resolved.
///
/// These counts are the load-bearing discriminator for the negative tests. A
/// "removed from the stack" path (CR 603.3d / 603.4 / 608.2b) asks NOTHING,
/// whereas an ability that resolves and merely moves zero counters DOES present
/// its "you may" window first (CR 608.2d). Without observing `optional`, a
/// negative test that only checks counter totals passes either way.
///
/// `trigger_target` and `target` are tracked separately so an assertion can pin
/// which selection surface was used rather than conflating the two.
#[derive(Default, Debug, PartialEq, Eq)]
struct Prompts {
    trigger_target: usize,
    target: usize,
    optional: usize,
}

/// Drive the trigger to completion, answering the optional window with
/// `accept`, and report which decisions the engine presented.
fn drive_trigger(
    runner: &mut super::rules::GameRunner,
    receiver: ObjectId,
    accept: bool,
) -> Prompts {
    let mut prompts = Prompts::default();
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 40, "trigger resolution did not terminate");
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                prompts.trigger_target += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(receiver)),
                    })
                    .expect("ChooseTarget should succeed");
            }
            WaitingFor::TargetSelection { .. } => {
                prompts.target += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(receiver)),
                    })
                    .expect("ChooseTarget should succeed");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                prompts.optional += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer optional trigger");
            }
            _ => break,
        }
    }
    prompts
}

/// The reported scenario: The Ozolith holds 11 +1/+1 counters, the chosen
/// creature already holds 12, and all 11 must move (0 / 23).
///
/// Two axes are exercised deliberately beyond the bare report:
///
///   * A second creature is on the battlefield, so the destination is genuinely
///     ambiguous and the engine must raise a real `TriggerTargetSelection`
///     walk. With a single legal creature the target is auto-bound and the
///     selection path is never entered at all.
///   * The Ozolith also carries stun counters, because the effect is "move ALL
///     counters" (`counter_type: null`). A +1/+1-only fixture cannot tell
///     "moves every kind" apart from "moves +1/+1".
#[test]
fn issue_7386_ozolith_moves_all_counters_to_target_at_begin_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ozolith = scenario
        .add_creature_from_oracle(P0, "The Ozolith", 0, 0, OZOLITH_ORACLE)
        .as_artifact()
        .with_plus_counters(11)
        .id();
    scenario.with_counter(ozolith, CounterType::Stun, 3);

    let receiver = scenario
        .add_creature(P0, "Counter Receiver", 2, 2)
        .with_plus_counters(12)
        .id();
    // Second legal creature: forces a real target-selection walk.
    let decoy = scenario.add_creature(P0, "Decoy", 1, 1).id();

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "precondition: reached the beginning of combat on P0's turn"
    );

    let prompts = drive_trigger(&mut runner, receiver, true);

    assert_eq!(
        prompts,
        Prompts {
            trigger_target: 1,
            target: 0,
            optional: 1
        },
        "the engine must raise the trigger's own target walk (two legal \
         creatures) and the optional 'you may' window"
    );
    assert_eq!(
        counters(&runner, ozolith, &CounterType::Plus1Plus1),
        0,
        "CR 122.5: moving all counters must REMOVE them from The Ozolith"
    );
    assert_eq!(
        counters(&runner, receiver, &CounterType::Plus1Plus1),
        23,
        "CR 122.5: the target must receive all 11 counters on top of its own 12"
    );
    assert_eq!(
        counters(&runner, ozolith, &CounterType::Stun),
        0,
        "CR 122.5: 'all counters' is every kind, so the stun counters move too"
    );
    assert_eq!(
        counters(&runner, receiver, &CounterType::Stun),
        3,
        "CR 122.5: the target receives the stun counters as well as the +1/+1s"
    );
    assert_eq!(
        counters(&runner, decoy, &CounterType::Plus1Plus1),
        0,
        "control: only the chosen target receives counters"
    );
}

/// End-to-end sequence as the reporter actually reached it: the counters are
/// not placed by fiat, they are COLLECTED by The Ozolith's own first trigger
/// when a creature dies, and only then moved by the begin-combat trigger.
#[test]
fn issue_7386_ozolith_moves_counters_it_collected_from_a_dead_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ozolith = scenario
        .add_creature_from_oracle(P0, "The Ozolith", 0, 0, OZOLITH_ORACLE)
        .as_artifact()
        .id();

    let dying = scenario
        .add_creature(P0, "Counter Bearer", 2, 2)
        .with_plus_counters(11)
        .id();

    let receiver = scenario
        .add_creature(P0, "Counter Receiver", 2, 2)
        .with_plus_counters(12)
        .id();

    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Bearer Removal", true, DESTROY_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // The bearer dies through the real cast → destroy → state-based-action →
    // zone-change path, so replacements apply and the ENGINE queues The
    // Ozolith's leaves-the-battlefield trigger rather than the test hand-feeding
    // it (the #2358 path, covered in ozolith_leaves_battlefield_counters.rs).
    runner.cast(removal).target_object(dying).resolve();

    assert_eq!(
        runner.state().objects[&dying].zone,
        Zone::Graveyard,
        "precondition: the removal spell actually destroyed the bearer"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        counters(&runner, ozolith, &CounterType::Plus1Plus1),
        11,
        "precondition: The Ozolith collected the dead creature's 11 counters"
    );

    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "precondition: reached the beginning of combat on P0's turn"
    );

    let prompts = drive_trigger(&mut runner, receiver, true);

    assert_eq!(
        prompts.optional, 1,
        "the optional 'you may' window must be presented"
    );
    assert_eq!(
        counters(&runner, ozolith, &CounterType::Plus1Plus1),
        0,
        "CR 122.5: collected counters move off The Ozolith like placed ones"
    );
    assert_eq!(
        counters(&runner, receiver, &CounterType::Plus1Plus1),
        23,
        "CR 122.5: the target must receive all 11 collected counters"
    );
}

/// COUNTERFACTUAL CONTROL for the two "no prompt" tests below.
///
/// Those tests prove a rule fired by observing that NO "you may" window was
/// presented. That inference is only valid while a `MoveCounters` that resolves
/// with nothing to move DOES present one. `optional_effect_is_infeasible`
/// (effects/mod.rs) has CR 608.2d suppression arms for `PutChosenCounter`,
/// `RemoveCounter`, `Forage` and `CastFromZone` but none for `MoveCounters`;
/// adding the natural `MoveCounters` arm would silently turn both tests into
/// tautologies that pass even with their rule deleted.
///
/// This test pins that assumption directly, using the same effect shape without
/// an intervening-if so the ability actually resolves while empty. If someone
/// adds the suppression arm, THIS test fails loudly and points at the two tests
/// whose discriminator it just disarmed.
#[test]
fn issue_7386_resolved_but_empty_move_still_offers_its_optional_window() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // No counters at all, and no intervening-if to stop the trigger.
    let shuttle = scenario
        .add_creature_from_oracle(P0, "Counter Shuttle", 0, 0, NO_GATE_ORACLE)
        .as_artifact()
        .id();

    let receiver = scenario
        .add_creature(P0, "Counter Receiver", 2, 2)
        .with_plus_counters(12)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);

    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: an ungated begin-combat trigger reaches the stack even \
         with nothing to move"
    );

    let prompts = drive_trigger(&mut runner, receiver, true);

    assert_eq!(
        prompts.optional, 1,
        "CR 608.2d: a MoveCounters that resolves with nothing to move STILL \
         offers its 'you may' window. The CR 603.4 and CR 608.2b tests below \
         infer 'the ability left the stack' from the ABSENCE of this window — \
         if this assertion ever fails, those two tests are no longer valid."
    );
    assert_eq!(
        counters(&runner, shuttle, &CounterType::Plus1Plus1),
        0,
        "control: nothing was on the source to move"
    );
    assert_eq!(
        counters(&runner, receiver, &CounterType::Plus1Plus1),
        12,
        "control: accepting an empty move changes nothing"
    );
}

/// CR 608.2b: the declared target is illegal by resolution, so the ability is
/// removed from the stack and does nothing — the counters stay on The Ozolith
/// rather than being destroyed or half-moved.
///
/// The target is an OPPONENT's creature on purpose. "Target creature" is
/// unrestricted, and it keeps the fixture reachable in real play: if the
/// departing creature were one P0 controlled, The Ozolith's own "whenever a
/// creature YOU CONTROL leaves the battlefield" trigger would fire and collect
/// its 12 counters, so an 11-counter end state could never occur. That trigger
/// is run below and asserted NOT to fire.
#[test]
fn issue_7386_illegal_target_at_resolution_leaves_counters_untouched() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ozolith = scenario
        .add_creature_from_oracle(P0, "The Ozolith", 0, 0, OZOLITH_ORACLE)
        .as_artifact()
        .with_plus_counters(11)
        .id();

    let receiver = scenario
        .add_creature(P1, "Opposing Receiver", 2, 2)
        .with_plus_counters(12)
        .id();

    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Responsive Removal", true, DESTROY_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);

    // Reach-guards: prove the trigger really is on the stack with a bound
    // target before the target is removed. Without these the assertions below
    // would also hold for a run in which the trigger never fired at all.
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "reach-guard: the begin-combat step was actually entered"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: the begin-combat trigger is on the stack awaiting resolution"
    );

    // Kill the bound target IN RESPONSE, exactly as a real game would: an
    // instant cast while the trigger sits on the stack. `commit()` + a single
    // `resolve_top()` resolves only the removal — `SpellCast::resolve()` would
    // drain the whole stack, resolving the very trigger under test and taking
    // the prompt discriminator with it.
    runner.cast(removal).target_object(receiver).commit();
    runner.resolve_top();

    assert_eq!(
        runner.state().objects.get(&receiver).unwrap().zone,
        Zone::Graveyard,
        "reach-guard: the target really left the battlefield"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "control: the removal has left the stack and only the begin-combat \
         trigger remains — an OPPONENT's creature leaving pushed no second \
         trigger, so the 'creature you control' collection ability did not \
         fire, which is what keeps an 11-counter end state reachable"
    );

    let prompts = drive_trigger(&mut runner, receiver, true);

    assert_eq!(
        prompts,
        Prompts::default(),
        "CR 608.2b: the ability is removed from the stack, so NO decision is \
         presented — not the target walk and not the 'you may' window. A \
         resolved-but-empty move WOULD have asked (pinned by \
         issue_7386_resolved_but_empty_move_still_offers_its_optional_window)."
    );
    assert_eq!(
        runner.state().stack.len(),
        0,
        "CR 608.2b: the ability actually left the stack rather than stalling on it"
    );
    assert_eq!(
        counters(&runner, ozolith, &CounterType::Plus1Plus1),
        11,
        "CR 608.2b: with its only target illegal the ability does nothing — the \
         counters remain on The Ozolith rather than vanishing"
    );
}

/// CR 603.4: the intervening-if is checked again on resolution. With the
/// counters gone by then, the ability is removed from the stack and the target
/// receives nothing.
///
/// The discriminator is `prompts.optional == 0`. Were the recheck skipped, the
/// ability would resolve normally, present its "you may" window, and then move
/// zero counters — leaving counter totals byte-identical to this test's.
#[test]
fn issue_7386_intervening_if_rechecked_at_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ozolith = scenario
        .add_creature_from_oracle(P0, "The Ozolith", 0, 0, OZOLITH_ORACLE)
        .as_artifact()
        .with_plus_counters(11)
        .id();

    let receiver = scenario
        .add_creature(P0, "Counter Receiver", 2, 2)
        .with_plus_counters(12)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);

    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "reach-guard: the begin-combat step was actually entered"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: the begin-combat trigger is on the stack awaiting resolution"
    );

    // Stand in for an opponent's removal effect emptying The Ozolith while the
    // trigger is on the stack. Injected directly because no counter-removal
    // spell is part of this fixture; the ability under test is the resolving
    // trigger, not the removal.
    runner
        .state_mut()
        .objects
        .get_mut(&ozolith)
        .expect("The Ozolith is on the battlefield")
        .counters
        .clear();

    let prompts = drive_trigger(&mut runner, receiver, true);

    assert_eq!(
        prompts.optional, 0,
        "CR 603.4: the intervening-if is false on resolution, so the ability \
         leaves the stack WITHOUT presenting its 'you may' window. A \
         resolved-but-empty move WOULD have prompted (pinned by \
         issue_7386_resolved_but_empty_move_still_offers_its_optional_window)."
    );
    assert_eq!(
        runner.state().stack.len(),
        0,
        "CR 603.4: the ability actually left the stack rather than stalling on it"
    );
    assert_eq!(
        counters(&runner, receiver, &CounterType::Plus1Plus1),
        12,
        "CR 603.4: the target receives nothing and keeps exactly its own 12"
    );
}

/// CR 608.2d: declining the "you may" leaves every counter where it was. This
/// is the fourth silent path to the reported symptom, and the one a player hits
/// most often — the window IS presented, so `optional == 1` distinguishes it
/// from the removed-from-stack paths above.
#[test]
fn issue_7386_declining_the_optional_window_moves_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ozolith = scenario
        .add_creature_from_oracle(P0, "The Ozolith", 0, 0, OZOLITH_ORACLE)
        .as_artifact()
        .with_plus_counters(11)
        .id();

    let receiver = scenario
        .add_creature(P0, "Counter Receiver", 2, 2)
        .with_plus_counters(12)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);

    let prompts = drive_trigger(&mut runner, receiver, false);

    assert_eq!(
        prompts.optional, 1,
        "the 'you may' window IS presented — declining is a choice, not a \
         removed-from-stack path"
    );
    assert_eq!(
        counters(&runner, ozolith, &CounterType::Plus1Plus1),
        11,
        "CR 608.2d: declining leaves all 11 counters on The Ozolith"
    );
    assert_eq!(
        counters(&runner, receiver, &CounterType::Plus1Plus1),
        12,
        "CR 608.2d: the target receives nothing when the move is declined"
    );
}

/// CR 603.3d: with counters on The Ozolith but no creature anywhere on the
/// battlefield, the trigger has no legal target and is never put on the stack.
///
/// Carries its own positive control: the identical fixture WITH a creature does
/// put the trigger on the stack, so the negative half cannot be satisfied by a
/// blanket "the trigger never fires" regression.
#[test]
fn issue_7386_no_legal_target_keeps_trigger_off_the_stack() {
    // Positive control first: same fixture, one legal creature.
    let mut control = GameScenario::new();
    control.at_phase(Phase::PreCombatMain);
    control
        .add_creature_from_oracle(P0, "The Ozolith", 0, 0, OZOLITH_ORACLE)
        .as_artifact()
        .with_plus_counters(11);
    control.add_creature(P0, "Counter Receiver", 2, 2);
    let mut control_runner = control.build();
    control_runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(
        control_runner.state().stack.len(),
        1,
        "positive control: with a legal creature the trigger DOES reach the stack"
    );

    // Negative case: identical, minus the creature.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ozolith = scenario
        .add_creature_from_oracle(P0, "The Ozolith", 0, 0, OZOLITH_ORACLE)
        .as_artifact()
        .with_plus_counters(11)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);

    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "reach-guard: the begin-combat step was actually entered"
    );
    assert_eq!(
        runner.state().stack.len(),
        0,
        "CR 603.3d: with no legal creature target the trigger is never put on \
         the stack"
    );

    let prompts = drive_trigger(&mut runner, ozolith, true);

    assert_eq!(
        prompts,
        Prompts::default(),
        "CR 603.3d: no target walk and no 'you may' window are presented"
    );
    assert_eq!(
        counters(&runner, ozolith, &CounterType::Plus1Plus1),
        11,
        "the counters stay on The Ozolith"
    );
}
