//! Runtime coverage for GitHub issue #1058: Pact of Negation's deferred
//! "At the beginning of your next upkeep, pay {3}{U}{U}. If you don't, you
//! lose the game." must actually end the game when the controller can't or
//! doesn't pay, and must NOT end the game when they can and do.
//!
//! The whole card is cast through the normal priority and stack pipeline:
//! P1 casts a counterable spell, passes priority to P0, then P0 casts Pact
//! targeting that exact spell. This proves both the counter and its delayed
//! upkeep payment are connected through production card resolution.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verified Scryfall Oracle text (2026-07-29). The newline is significant to
/// exercise the same card parser path as the production card data pipeline.
const PACT_ORACLE: &str =
    "Counter target spell.\nAt the beginning of your next upkeep, pay {3}{U}{U}. If you don't, you lose the game.";

/// Drive the engine through the REAL phase machinery until P0 is in its own
/// upkeep step, exactly as the live driver does: drain trigger ordering,
/// pass priority otherwise. Bounded to guard stalls.
fn advance_to_p0_upkeep(runner: &mut GameRunner) {
    for _ in 0..400 {
        if runner.state().phase == Phase::Upkeep && runner.state().active_player == P0 {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("the production OrderTriggers action must settle");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("P1 may decline to attack with the Dash control");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no attackers means no blocking assignments");
            }
            other => panic!("unexpected WaitingFor while advancing to P0 upkeep: {other:?}"),
        }
    }
    panic!(
        "failed to reach P0's upkeep (stuck at phase {:?}, active {:?})",
        runner.state().phase,
        runner.state().active_player
    );
}

fn pact_runner(with_payment_lands: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Pact of Negation", true, PACT_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterable Test Spell", true, "Draw a card.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let dash = scenario
        .add_creature_to_hand(P1, "Dash Control", 2, 1)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(Keyword::Dash(ManaCost::zero()))
        .id();
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    if with_payment_lands {
        for _ in 0..5 {
            scenario.add_basic_land(P0, ManaColor::Blue);
        }
    }
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        state.priority_passes.clear();
    }

    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().priority_player, P1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    assert!(runner.state().priority_passes.is_empty());
    assert!(runner.state().stack.is_empty());

    let p1_commit = runner.cast(p1_spell).commit();
    assert!(
        p1_commit
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == p1_spell),
        "P1's spell must be on the stack after the real cast pipeline"
    );
    drop(p1_commit);
    assert_eq!(runner.state().priority_player, P1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    assert!(runner.state().priority_passes.is_empty());

    runner
        .act(GameAction::PassPriority)
        .expect("P1's real priority pass must succeed");
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().priority_player, P0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().priority_passes.len(), 1);
    assert!(runner.state().priority_passes.contains(&P1));

    let outcome = runner.cast(pact).target_object(p1_spell).resolve();
    assert_eq!(outcome.zone_of(p1_spell), Zone::Graveyard);
    assert!(
        runner
            .state()
            .players
            .iter()
            .find(|player| player.id == P1)
            .expect("P1 exists")
            .hand
            .contains(&dash),
        "P1's Dash control remains available after its other spell is countered"
    );
    assert!(
        !runner
            .state()
            .players
            .iter()
            .find(|player| player.id == P1)
            .expect("P1 exists")
            .hand
            .contains(&p1_spell),
        "P1's Draw a card spell must be countered, not resolve before reaching the graveyard"
    );
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert_eq!(runner.state().delayed_triggers[0].source_id, pact);
    assert_eq!(runner.state().objects[&pact].zone, Zone::Graveyard);

    // CR 702.109a: this is a real Dash cast and resolution, not a hand-installed
    // delayed record. It gives the Pact fixture a non-Pact sibling whose terminal
    // path must not remove or satisfy the Pact upkeep obligation.
    let dash_outcome = runner
        .cast(dash)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    assert_eq!(dash_outcome.zone_of(dash), Zone::Battlefield);
    assert_eq!(runner.state().delayed_triggers.len(), 2);
    assert!(
        runner
            .state()
            .delayed_triggers
            .iter()
            .any(|trigger| trigger.source_id == pact),
        "the departed Pact source must retain its next-upkeep delayed trigger"
    );
    assert!(
        runner
            .state()
            .delayed_triggers
            .iter()
            .any(|trigger| trigger.source_id == dash),
        "the Dash cast must install its own end-step delayed trigger"
    );
    (runner, pact, dash)
}

/// At P0's next upkeep, the one-shot delayed trigger has been consumed and
/// placed on the stack. The stack entry, rather than `delayed_triggers`, is
/// therefore the live carrier of Pact's payment obligation.
fn assert_pact_upkeep_trigger_is_stacked(runner: &GameRunner, pact: ObjectId, dash: ObjectId) {
    assert_eq!(runner.state().objects[&dash].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&pact].zone, Zone::Graveyard);
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "Dash must resolve at P1's end step and Pact's one-shot trigger must be collected at P0's upkeep"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));

    let pact_entries: Vec<_> = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == pact)
        .collect();
    assert_eq!(
        pact_entries.len(),
        1,
        "the pending Pact payment must have exactly one stack carrier"
    );
    let pact_entry = pact_entries[0];
    assert_ne!(
        pact_entry.id, pact,
        "the triggered-ability carrier must not alias Pact's graveyard object"
    );
    assert!(
        matches!(
            &pact_entry.kind,
            StackEntryKind::TriggeredAbility {
                source_id,
                ..
            } if *source_id == pact
        ),
        "the pending Pact payment must be a triggered ability sourced by Pact, got {:?}",
        pact_entry.kind
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .all(|entry| entry.source_id != dash),
        "Dash's end-step trigger must have terminalized before Pact's upkeep trigger is stacked"
    );
}

#[test]
fn pact_of_negation_loses_the_game_when_upkeep_cost_goes_unpaid() {
    let (mut runner, pact, dash) = pact_runner(false);
    advance_to_p0_upkeep(&mut runner);

    assert_pact_upkeep_trigger_is_stacked(&runner, pact, dash);
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "P0 must lose the game when the deferred {{3}}{{U}}{{U}} cost goes unpaid, got {:?}",
        runner.state().waiting_for
    );
    // CR 800.4a: losing P0 leaves the game, so every card P0 owns — including
    // the already-resolved Pact in its graveyard — leaves the game as well.
    assert_eq!(runner.state().objects[&pact].zone, Zone::Exile);
    assert!(runner.state().delayed_triggers.is_empty());
}

#[test]
fn pact_of_negation_does_not_lose_the_game_when_upkeep_cost_is_paid() {
    let (mut runner, pact, dash) = pact_runner(true);
    advance_to_p0_upkeep(&mut runner);

    assert_pact_upkeep_trigger_is_stacked(&runner, pact, dash);
    runner.advance_until_stack_empty();

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "P0 must NOT lose the game when the deferred cost is paid, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&pact].zone, Zone::Graveyard);
    assert!(runner.state().delayed_triggers.is_empty());
    assert!(
        runner.state().stack.is_empty(),
        "paid Pact trigger must finish resolving rather than leave an entry on the stack"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "paid Pact must return the game to P0 priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|object| object.controller == P0 && object.tapped)
            .count(),
        5,
        "the five payment lands must be tapped as evidence that {{3}}{{U}}{{U}} was paid"
    );
}
