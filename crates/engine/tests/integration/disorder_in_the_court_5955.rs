//! Regression for issue #5955 — Disorder in the Court.
//!
//! "Exile X target creatures, then investigate X times. Return the exiled cards
//! to the battlefield tapped under their owners' control at the beginning of the
//! next end step."
//!
//! The return sentence is a separate printed instruction (CR 608.2c: "follow the
//! instructions in the order written"), so it is a `SequentialSibling` of the
//! `repeat_for: X` investigate clause — the delayed-return trigger is created
//! exactly ONCE after the repeats, not once per investigation. Before the fix the
//! `CreateDelayedTrigger` wrapper defaulted to `ContinuationStep`, scoping it into
//! the investigate `repeat_for` loop and creating X copies of the return trigger.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DISORDER: &str = "Exile X target creatures, then investigate X times. Return the exiled cards to the battlefield tapped under their owners' control at the beginning of the next end step.";

fn clue_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.name == "Clue")
        .count()
}

/// X=2: exile two targeted creatures, investigate twice (two Clues), and create a
/// SINGLE delayed return that puts the two exiled cards back onto the battlefield
/// tapped at the next end step. The Clues are untouched by the return.
#[test]
fn disorder_in_the_court_returns_exiled_cards_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Disorder in the Court", true, DISORDER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::White, ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let c1 = scenario.add_vanilla(P1, 2, 2);
    let c2 = scenario.add_vanilla(P1, 3, 3);
    let dummy = ObjectId(0);
    // {X=2}{W}{U} → W, U, and two generic.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, dummy, false, vec![]),
            ManaUnit::new(ManaType::Blue, dummy, false, vec![]),
            ManaUnit::new(ManaType::Colorless, dummy, false, vec![]),
            ManaUnit::new(ManaType::Colorless, dummy, false, vec![]),
        ],
    );
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast begins");

    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: 2 })
                    .expect("choose X=2");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(c1)),
                    })
                    .or_else(|_| {
                        runner.act(GameAction::ChooseTarget {
                            target: Some(engine::types::ability::TargetRef::Object(c2)),
                        })
                    })
                    .expect("target select");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
                if runner.state().stack.is_empty() {
                    break;
                }
            }
            other => panic!("unexpected waiting_for during cast: {other:?}"),
        }
    }

    // On resolution: both creatures are exiled, X=2 Clues were created, and the
    // return is scheduled exactly ONCE (regression: was created X=2 times).
    assert_eq!(runner.state().objects[&c1].zone, Zone::Exile, "c1 exiled");
    assert_eq!(runner.state().objects[&c2].zone, Zone::Exile, "c2 exiled");
    assert_eq!(clue_count(&runner), 2, "investigate X=2 → two Clues");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the SequentialSibling return is a single delayed trigger, not one per investigation",
    );

    // Advance until the delayed trigger fires and its return resolves.
    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard <= 400, "did not reach/clear the end step return");
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // The exiled cards return to the battlefield tapped under their owner's control.
    assert_eq!(
        runner.state().objects[&c1].zone,
        Zone::Battlefield,
        "c1 returned",
    );
    assert_eq!(
        runner.state().objects[&c2].zone,
        Zone::Battlefield,
        "c2 returned",
    );
    assert!(runner.state().objects[&c1].tapped, "c1 returns tapped");
    assert!(runner.state().objects[&c2].tapped, "c2 returns tapped");
    assert_eq!(
        runner.state().objects[&c1].controller,
        P1,
        "c1 owner control"
    );
    assert_eq!(
        runner.state().objects[&c2].controller,
        P1,
        "c2 owner control"
    );
    // The Clues are NOT part of the returned tracked set — they stay put.
    assert_eq!(clue_count(&runner), 2, "Clues untouched by the return");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "the single return trigger resolved and cleared",
    );
}
