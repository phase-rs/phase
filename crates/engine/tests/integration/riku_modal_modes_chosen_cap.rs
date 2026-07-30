//! DynQty subgroup C — "choose up to X, where X is the number of times you chose
//! a mode for that spell" (Riku of Many Paths) and the shared modal dynamic-cap
//! path (Bumi/Riku). Covers the three seams this change introduces:
//!
//!   1. STORAGE — `handle_select_modes` latches the chosen mode indices onto
//!      `SpellContext.chosen_modes`, and cast-finalize stamps them onto the
//!      spell-on-stack `GameObject.chosen_modes` (CR 700.2a + CR 700.2d).
//!   2. RESOLVER — `QuantityRef::EventContextSourceModesChosen` reads
//!      `chosen_modes.len()` off the `current_trigger_event` spell object
//!      (CR 700.2d + CR 601.2b).
//!   3. END-TO-END — Riku's triggered modal cap (`dynamic_max_choices`) resolves
//!      live to the number of modes chosen for the triggering spell, clamped to
//!      Riku's own mode_count (CR 700.2d), and surfaces as
//!      `AbilityModeChoice.modal.max_choices`.
//!
//! All Oracle text is verbatim engine-authoritative text (matches
//! `riku_modal_spell_trigger.rs` / `atarkas_command_*`).

use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{QuantityExpr, QuantityRef, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const RIKU_ORACLE: &str = "Whenever you cast a modal spell, choose up to X, where X is the number of times you chose a mode for that spell —\n\u{2022} Exile the top card of your library. Until the end of your next turn, you may play it.\n\u{2022} Put a +1/+1 counter on Riku. It gains trample until end of turn.\n\u{2022} Create a 1/1 blue Bird creature token with flying.";

const ABRADE_ORACLE: &str =
    "Choose one \u{2014}\n\u{2022} Abrade deals 3 damage to target creature.\n\u{2022} Destroy target artifact.";

const ATARKAS_COMMAND_ORACLE: &str = "Choose two \u{2014}\n\u{2022} Your opponents can't gain life this turn.\n\u{2022} Atarka's Command deals 3 damage to each opponent.\n\u{2022} You may put a land card from your hand onto the battlefield.\n\u{2022} Creatures you control get +1/+1 and gain reach until end of turn.";

/// Drive a cast through the pipeline: answer the spell's own `ModeChoice`
/// (`modes`) and `TargetSelection` (`targets`), then stop at Riku's triggered
/// `AbilityModeChoice` and return that modal's resolved `max_choices` — the live
/// value produced by `modal_choice_for_player` from Riku's
/// `dynamic_max_choices` (`EventContextSourceModesChosen`). Panics if the
/// pipeline halts at `Priority` (Riku's trigger never fired) or any other window.
fn cast_and_capture_riku_cap(
    runner: &mut GameRunner,
    spell: ObjectId,
    modes: &[usize],
    targets: &[ObjectId],
) -> usize {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell must be accepted");

    let mut remaining_targets = targets.to_vec();
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::ModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes {
                        indices: modes.to_vec(),
                    })
                    .expect("SelectModes must be accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                let target = remaining_targets.remove(0);
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("ChooseTarget must be accepted");
            }
            // Riku's triggered modal surfaces here (before the Priority window).
            // Its `max_choices` is the resolved dynamic cap under test.
            WaitingFor::AbilityModeChoice { modal, .. } => {
                return modal.max_choices;
            }
            WaitingFor::Priority { .. } => {
                panic!("Riku's modal trigger did not fire (halted at Priority)")
            }
            other => panic!(
                "unexpected WaitingFor while driving the cast: {}",
                other.variant_name()
            ),
        }
    }
    panic!("cast pipeline did not reach Riku's AbilityModeChoice within the step budget");
}

/// END-TO-END: casting a modal spell that chose ONE mode (Abrade "Choose one")
/// resolves Riku's dynamic cap to 1 (`min(1, mode_count=3)`, CR 700.2d).
///
/// Revert probes: (a) resolver arm `=> 0` → cap 0; (b) drop the finalize stamp /
/// population line → `chosen_modes` empty → resolver 0 → cap 0; (c) drop the
/// parser where-X arm → Riku's header falls to `Fixed{1,1}` → cap 1 (NOT
/// discriminated by this single-mode case — the two-mode case below discriminates
/// the parser revert).
#[test]
fn riku_cap_resolves_to_one_mode_chosen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _riku = scenario
        .add_creature_from_oracle(P0, "Riku, of Many Paths", 2, 4, RIKU_ORACLE)
        .id();
    let dummy = scenario.add_creature(P1, "Target Dummy", 3, 3).id();
    let abrade = scenario
        .add_spell_to_hand_from_oracle(P0, "Abrade", true, ABRADE_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let cap = cast_and_capture_riku_cap(&mut runner, abrade, &[0], &[dummy]);
    assert_eq!(
        cap, 1,
        "Riku's dynamic cap must equal the 1 mode chosen for Abrade (CR 700.2d)"
    );
}

/// END-TO-END: casting a modal spell that chose TWO modes (Atarka's Command
/// "Choose two") resolves Riku's dynamic cap to 2 (`min(2, 3)`). The contrast
/// with the one-mode case above proves the cap tracks the ACTUAL mode count, not
/// a constant or Riku's own mode_count.
///
/// Revert probes: (a) resolver arm `=> 0` → cap 0 ≠ 2; (b) drop stamp/population
/// → 0 ≠ 2; (c) drop the parser where-X arm → Riku header `Fixed{1,1}` → cap 1
/// ≠ 2.
#[test]
fn riku_cap_resolves_to_two_modes_chosen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _riku = scenario
        .add_creature_from_oracle(P0, "Riku, of Many Paths", 2, 4, RIKU_ORACLE)
        .id();
    let atarka = scenario
        .add_spell_to_hand_from_oracle(P0, "Atarka's Command", true, ATARKAS_COMMAND_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    // Modes 0 (opponents can't gain life) and 3 (creatures +1/+1) are targetless.
    let cap = cast_and_capture_riku_cap(&mut runner, atarka, &[0, 3], &[]);
    assert_eq!(
        cap, 2,
        "Riku's dynamic cap must equal the 2 modes chosen for Atarka's Command (CR 700.2d)"
    );
}

/// STORAGE SEAM (real modal SPELL, subset with a gap): casting Atarka's Command
/// choosing modes {3, 0} stamps the ascending indices `[0, 3]` onto the
/// spell-on-stack object's `chosen_modes`. Uses `.commit()` to inspect the live
/// stack object before resolution. No Riku on the battlefield, so the commit
/// driver reaches the `Priority` window cleanly.
///
/// Revert probe: remove the population line (`handle_select_modes`) OR the
/// finalize stamp → `chosen_modes` empty → this assertion fails LOUDLY. Guards
/// the `handle_select_modes` → `SpellContext.chosen_modes` → finalize seam.
#[test]
fn modal_spell_stamps_chosen_modes_on_stack_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let atarka = scenario
        .add_spell_to_hand_from_oracle(P0, "Atarka's Command", true, ATARKAS_COMMAND_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    // Declare modes out of order to also prove the ascending-sort (CR 601.2c).
    let commit = runner.cast(atarka).modes(&[3, 0]).commit();
    let obj = commit
        .state()
        .objects
        .get(&atarka)
        .expect("committed Atarka's Command must be on the stack");
    assert_eq!(
        obj.chosen_modes,
        vec![0, 3],
        "the two chosen modes must be latched (ascending) onto the stack object"
    );

    // Reach-guard (non-vacuous): the spell actually committed to the stack, so the
    // stamp seam was exercised — not skipped by an upstream cast failure.
    assert!(
        matches!(commit.state().waiting_for, WaitingFor::Priority { .. }),
        "cast must have committed to the stack (Priority window)"
    );
}

/// END-TO-END STORAGE→RESOLVER: the same real committed modal spell's stamped
/// `chosen_modes` is read back through the production `resolve_quantity` for
/// `EventContextSourceModesChosen`, mirroring how a cast-trigger resolves it.
///
/// Revert probes: (a) resolver arm `=> 0` → resolves 0 ≠ 2; (b) stamp/population
/// removed → `chosen_modes` empty → 0 ≠ 2.
#[test]
fn modes_chosen_ref_resolves_off_committed_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let atarka = scenario
        .add_spell_to_hand_from_oracle(P0, "Atarka's Command", true, ATARKAS_COMMAND_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let mut commit = runner.cast(atarka).modes(&[0, 1]).commit();
    let card_id = commit.state().objects[&atarka].card_id;
    // Point the trigger event at the committed spell, as trigger dispatch does.
    commit.state_mut().current_trigger_event = Some(GameEvent::SpellCast {
        card_id,
        controller: P0,
        object_id: atarka,
    });

    let expr = QuantityExpr::Ref {
        qty: QuantityRef::EventContextSourceModesChosen,
    };
    // source_id is deliberately NOT the event object — the ref must read the
    // event's spell object, not its source.
    let resolved = resolve_quantity(commit.state(), &expr, P0, atarka);
    assert_eq!(
        resolved, 2,
        "EventContextSourceModesChosen must read the 2 stamped modes off the SpellCast event object"
    );
}

/// MULTI-AUTHORITY HOSTILE FIXTURE (b2) + EMPTY SIBLING (b3): with TWO objects
/// carrying different `chosen_modes` and a distinct source, the ref reads the
/// `current_trigger_event` object's count — not the first object's, not the
/// source's. And an object with empty `chosen_modes` (or an absent event)
/// resolves to 0 without panicking.
///
/// Revert probe: if the resolver read `source_id` instead of the event object,
/// it would return the source's count (1) — the assertion (3) fails.
#[test]
fn modes_chosen_ref_reads_event_object_not_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Three distinct objects standing in for: first modal spell, second modal
    // spell (the one Riku triggers off), and Riku (the trigger source).
    let first = scenario.add_creature(P0, "First Spell", 1, 1).id();
    let second = scenario.add_creature(P0, "Second Spell", 1, 1).id();
    let riku = scenario.add_creature(P0, "Riku Source", 2, 4).id();
    let mut runner = scenario.build();
    let state: &mut GameState = runner.state_mut();
    // First spell chose 1 mode; second chose 3; Riku (source) carries its own 1.
    state.objects.get_mut(&first).unwrap().chosen_modes = vec![0];
    state.objects.get_mut(&second).unwrap().chosen_modes = vec![0, 1, 2];
    state.objects.get_mut(&riku).unwrap().chosen_modes = vec![2];

    let expr = QuantityExpr::Ref {
        qty: QuantityRef::EventContextSourceModesChosen,
    };

    // Trigger event points at the SECOND spell.
    let card_id = runner.state().objects[&second].card_id;
    runner.state_mut().current_trigger_event = Some(GameEvent::SpellCast {
        card_id,
        controller: P0,
        object_id: second,
    });
    // source_id = riku (the trigger source, chosen_modes=[2]).
    assert_eq!(
        resolve_quantity(runner.state(), &expr, P0, riku),
        3,
        "must read the SECOND spell's 3 modes (the event object), not the source's or first's"
    );

    // b3: empty chosen_modes on the event object → 0.
    let empty_card = runner.state().objects[&first].card_id;
    runner
        .state_mut()
        .objects
        .get_mut(&first)
        .unwrap()
        .chosen_modes = Vec::new();
    runner.state_mut().current_trigger_event = Some(GameEvent::SpellCast {
        card_id: empty_card,
        controller: P0,
        object_id: first,
    });
    assert_eq!(
        resolve_quantity(runner.state(), &expr, P0, riku),
        0,
        "empty chosen_modes must resolve to 0"
    );

    // b3: no trigger event → 0 (no panic).
    runner.state_mut().current_trigger_event = None;
    assert_eq!(
        resolve_quantity(runner.state(), &expr, P0, riku),
        0,
        "absent trigger event must resolve to 0"
    );
}

// ---------------------------------------------------------------------------
// Bumi, King of Three Trials — ETB modal cap = ZoneCardCount(Lesson, graveyard)
// ---------------------------------------------------------------------------
//
// END-TO-END (production parse→resolve WIRE, per PR #6108 review): a parser-shape
// assertion proves only that Bumi's header parses to a dynamic cap; it cannot
// prove the Lesson-graveyard `ZoneCardCount` reaches `modal_choice_for_player`
// and sets the live `AbilityModeChoice.max_choices`. These tests cast Bumi,
// resolve its ETB, and read the cap off the real modal window — including the
// clamp to `mode_count` (CR 107.3m + CR 700.2d). Bumi's cap source is the
// graveyard, not the trigger event, so it is independent of the trigger-event
// window fix that Riku's cap needs.

const BUMI_ORACLE: &str = "When Bumi enters, choose up to X, where X is the number of Lesson cards in your graveyard \u{2014}\n\u{2022} Put three +1/+1 counters on Bumi.\n\u{2022} Target player scries 3.\n\u{2022} Earthbend 3.";

/// Seed `n` Lesson-subtyped sorceries into P0's graveyard so
/// `ZoneCardCount { zone: Graveyard, card_types: [Lesson], scope: Controller }`
/// resolves to `n`.
fn seed_lessons(scenario: &mut GameScenario, n: usize) {
    for i in 0..n {
        scenario
            .add_spell_to_graveyard(P0, &format!("Lesson {i}"), false)
            .with_subtypes(vec!["Lesson"]);
    }
}

/// Cast Bumi from hand and drive the pipeline until its ETB triggered modal
/// (`AbilityModeChoice`) surfaces; return `(max_choices, mode_count)`. The
/// `max_choices` is the live value produced by `modal_choice_for_player` from
/// Bumi's `dynamic_max_choices` (`ZoneCardCount` of Lesson cards), clamped to
/// `mode_count` (CR 107.3m + CR 700.2d). Panics if the modal window is never
/// reached (e.g. the ETB never fired).
fn cast_bumi_and_capture_cap(runner: &mut GameRunner, bumi: ObjectId) -> (usize, usize) {
    let card_id = runner.state().objects[&bumi].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bumi,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Bumi must be accepted");

    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            // Bumi's ETB modal surfaces here (modes chosen as the trigger is put
            // on the stack, CR 603.3c). Its resolved cap is under test.
            WaitingFor::AbilityModeChoice { modal, .. } => {
                return (modal.max_choices, modal.mode_count)
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            // Pass priority so Bumi resolves and its ETB reaches the stack.
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority to resolve Bumi must succeed");
            }
            other => panic!(
                "unexpected WaitingFor while driving Bumi's cast: {}",
                other.variant_name()
            ),
        }
    }
    panic!("cast pipeline did not reach Bumi's AbilityModeChoice within the step budget");
}

/// END-TO-END: with 2 Lesson cards in the graveyard, Bumi's ETB modal cap
/// resolves to 2 (`min(2, mode_count=3)`). This is the parse→resolve WIRE a
/// parser-shape test cannot prove: the Lesson-graveyard `ZoneCardCount` reaches
/// `modal_choice_for_player` and sets the live `max_choices`.
///
/// Revert probes (measured, see PR): (a) drop the parser where-X arm → Bumi's
/// header falls to `Fixed{1,1}` → `dynamic_max_choices=None` → cap 1 ≠ 2; (b)
/// stub the `ZoneCardCount` resolver → 0 ≠ 2.
#[test]
fn bumi_etb_cap_resolves_to_lesson_graveyard_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_lessons(&mut scenario, 2);
    let bumi = scenario
        .add_creature_to_hand_from_oracle(P0, "Bumi, King of Three Trials", 4, 4, BUMI_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let (cap, modes) = cast_bumi_and_capture_cap(&mut runner, bumi);
    assert_eq!(modes, 3, "Bumi's header must parse three modes");
    assert_eq!(
        cap, 2,
        "Bumi's ETB modal cap must equal the 2 Lesson cards in the graveyard (min(2, 3), CR 700.2d)"
    );
}

/// END-TO-END CLAMP: with 5 Lesson cards (exceeding Bumi's 3 modes), the cap
/// clamps to `mode_count = 3` (CR 700.2d: a player can't choose more modes than
/// exist). The contrast with the 2-Lesson case proves the cap TRACKS the Lesson
/// count until the clamp bites — neither a constant nor unclamped.
///
/// Revert probe (measured, see PR): remove the `.min(modal.mode_count)` clamp in
/// `modal_choice_for_player` → cap 5 ≠ 3.
#[test]
fn bumi_etb_cap_clamps_to_mode_count_when_lessons_exceed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_lessons(&mut scenario, 5);
    let bumi = scenario
        .add_creature_to_hand_from_oracle(P0, "Bumi, King of Three Trials", 4, 4, BUMI_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let (cap, modes) = cast_bumi_and_capture_cap(&mut runner, bumi);
    assert_eq!(modes, 3, "Bumi's header must parse three modes");
    assert_eq!(
        cap, 3,
        "5 Lessons exceed Bumi's 3 modes, so the cap must clamp to mode_count=3 (CR 700.2d)"
    );
}
