//! Runtime regression for GitHub issue #4599 — She-Hulk, Wallbreaker's
//! "becomes blocked" trigger.
//!
//! Oracle (relevant clause): "Whenever a Hero you control becomes blocked, put
//! a +1/+1 counter on that Hero for each creature blocking it."
//!
//! Reported bug: the trigger does not fire / no counter is applied when a Hero
//! the controller owns becomes blocked. The parse is correct (a `BecomesBlocked`
//! trigger with `valid_card: Hero you control`, child `PutCounter` on the
//! blocked Hero, count = creatures blocking it), so the defect is in the runtime
//! block-event → trigger → counter path.
//!
//! This drives a real combat: She-Hulk (a Hero) attacks, the opponent blocks
//! her, and the test asserts she gains exactly one +1/+1 counter (one blocker).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const SHE_HULK: &str =
    "Whenever a Hero you control becomes blocked, put a +1/+1 counter on that Hero for each creature blocking it.";

#[test]
fn she_hulk_becomes_blocked_puts_counter_on_blocked_hero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // She-Hulk is a Hero; her own "a Hero you control becomes blocked" trigger
    // must fire when she herself is blocked.
    let she_hulk = scenario
        .add_creature_from_oracle(P0, "She-Hulk, Wallbreaker", 4, 4, SHE_HULK)
        .with_subtypes(vec!["Hero"])
        .id();
    // Opponent blocker.
    let blocker = scenario.add_creature(P1, "Wall", 0, 4).id();

    let mut runner = scenario.build();

    let mut declared = false;
    let mut blocked = false;
    for _ in 0..400 {
        // Stop once She-Hulk has the counter (trigger resolved) — before combat
        // damage muddies the picture.
        if runner.state().objects.get(&she_hulk).is_some_and(|o| {
            o.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
                > 0
        }) {
            break;
        }
        let acted = match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { player, .. } if player == P0 && !declared => {
                declared = true;
                runner.act(GameAction::DeclareAttackers {
                    attacks: vec![(she_hulk, AttackTarget::Player(P1))],
                    bands: vec![],
                })
            }
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers { .. } if !blocked => {
                blocked = true;
                runner.act(GameAction::DeclareBlockers {
                    assignments: vec![(blocker, she_hulk)],
                })
            }
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            _ => break,
        };
        if acted.is_err() {
            break;
        }
    }

    assert!(declared, "She-Hulk must have been declared as an attacker");
    assert!(blocked, "the opponent must have blocked She-Hulk");
    let counters = runner
        .state()
        .objects
        .get(&she_hulk)
        .expect("She-Hulk still exists")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        counters, 1,
        "She-Hulk (a Hero you control) becoming blocked by one creature must put \
         one +1/+1 counter on her",
    );
}
