//! CR 608.2c: a trailing "<instruction> if <condition>" guards that instruction
//! alone; a leading "If <condition>, A, then B" still governs every instruction.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const SUFFIX_GUARDED: &str =
    "Draw two cards if you control three or more creatures, then gain 1 life.";

const LEADING_GUARDED: &str =
    "If you control three or more creatures, draw two cards, then gain 1 life.";

/// Two consecutive instructions, each with its own trailing guard.
const TWO_SUFFIX_GUARDS: &str =
    "Draw a card if you control an artifact, then draw a card if you control an enchantment.";

const BANISH_INTO_FABLE: &str = "When you cast this spell from your hand, copy it if you control \
an artifact, then copy it if you control an enchantment. You may choose new targets for the \
copies.\nReturn target nonland permanent to its owner's hand. You create a 2/2 white Knight \
creature token with vigilance.";

/// P0 casts `oracle` controlling `creatures` creatures (and an enchantment when
/// `enchantment`); returns (cards drawn, life gained) once the stack is empty.
fn cast(oracle: &str, creatures: usize, enchantment: bool) -> (i64, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Guarded", false)
        .from_oracle_text(oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    for index in 0..creatures {
        scenario.add_creature(P0, &format!("Bear {index}"), 2, 2);
    }
    if enchantment {
        scenario.add_enchantment_from_oracle(P0, "Enchantment", "");
    }
    for index in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {index}"));
    }
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert!(outcome.state().stack.is_empty(), "the spell resolved");
    (outcome.hand_drawn(P0), outcome.life_delta(P0))
}

#[test]
fn suffix_guard_false_skips_only_its_instruction() {
    assert_eq!(cast(SUFFIX_GUARDED, 0, false), (0, 1));
}

#[test]
fn suffix_guard_true_runs_both_instructions() {
    assert_eq!(cast(SUFFIX_GUARDED, 3, false), (2, 1));
}

#[test]
fn leading_guard_false_skips_every_instruction() {
    assert_eq!(cast(LEADING_GUARDED, 0, false), (0, 0));
}

#[test]
fn leading_guard_true_runs_every_instruction() {
    assert_eq!(cast(LEADING_GUARDED, 3, false), (2, 1));
}

/// The second instruction is checked on its own guard after the first's fails.
#[test]
fn second_suffix_guard_is_evaluated_after_first_fails() {
    assert_eq!(cast(TWO_SUFFIX_GUARDS, 0, true), (1, 0));
}

#[test]
fn both_suffix_guards_false_draw_nothing() {
    assert_eq!(cast(TWO_SUFFIX_GUARDS, 0, false), (0, 0));
}

/// P0 casts Banish into Fable from hand at a P1 permanent; returns how many copies it made.
fn banish_into_fable_copies(artifact: bool, enchantment: bool) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Banish into Fable", true)
        .from_oracle_text(BANISH_INTO_FABLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    if artifact {
        scenario.add_artifact_from_oracle(P0, "Artifact", "");
    }
    if enchantment {
        scenario.add_enchantment_from_oracle(P0, "Enchantment", "");
    }
    let mut runner = scenario.build();
    let mut cast = runner.cast(spell).target_object(victim).commit();
    let mut copies = 0;
    for _ in 0..32 {
        let action = match &cast.state().waiting_for {
            WaitingFor::CopyRetarget { .. } => GameAction::KeepAllCopyTargets,
            WaitingFor::Priority { .. } if cast.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        let result = cast.act(action).expect("action accepted");
        copies += result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::SpellCopied { .. }))
            .count();
    }
    assert!(
        cast.state().stack.is_empty(),
        "the spell and its copies resolved"
    );
    assert!(
        !cast.state().battlefield.contains(&victim),
        "the target was returned"
    );
    copies
}

/// Banish into Fable: the enchantment copy does not wait on the artifact guard.
#[test]
fn banish_into_fable_copies_with_enchantment_and_no_artifact() {
    assert_eq!(banish_into_fable_copies(false, true), 1);
}

#[test]
fn banish_into_fable_copies_once_per_guard_met() {
    assert_eq!(banish_into_fable_copies(true, false), 1);
    assert_eq!(banish_into_fable_copies(true, true), 2);
    assert_eq!(banish_into_fable_copies(false, false), 0);
}

/// The second guarded instruction is a delayed trigger.
const DELAYED_SECOND_GUARD: &str = "Draw a card if you control an artifact, then draw a card at \
the beginning of the next end step if you control an enchantment, then gain 1 life.";

/// P0 casts `DELAYED_SECOND_GUARD` with no artifact; returns (cards drawn on
/// resolution, life gained on resolution, cards drawn at the next end step).
fn cast_delayed_second_guard(enchantment: bool) -> (i64, i32, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Guarded", false)
        .from_oracle_text(DELAYED_SECOND_GUARD)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    if enchantment {
        scenario.add_enchantment_from_oracle(P0, "Enchantment", "");
    }
    for index in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {index}"));
    }
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert!(outcome.state().stack.is_empty(), "the spell resolved");
    let resolved = (outcome.hand_drawn(P0), outcome.life_delta(P0));
    let hand = |runner: &GameRunner| runner.state().players[0].hand.len();
    let hand_after_resolution = hand(&runner);
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reached the next end step"
    );
    (
        resolved.0,
        resolved.1,
        hand(&runner) - hand_after_resolution,
    )
}

#[test]
fn delayed_suffix_guard_is_evaluated_after_first_fails() {
    assert_eq!(cast_delayed_second_guard(true), (0, 1, 1));
}

#[test]
fn delayed_suffix_guard_false_skips_only_the_delayed_draw() {
    assert_eq!(cast_delayed_second_guard(false), (0, 1, 0));
}
