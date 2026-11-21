//! Whole-card witnesses for "face a dilemma" (CR 700.17, edition effective
//! 2026-11-06) with printed cards from the curated card database
//! (`add_real_card`): a random modal trigger's dilemma observers are placed above the
//! whole original trigger batch (CR 603.3b).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::events::PlayerActionKind;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn dilemmas(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(actor, kind)| *actor == player && *kind == PlayerActionKind::FaceDilemma)
        .count()
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

/// The stack, bottom to top, as the object that owns each ability.
fn stack_sources(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .stack
        .iter()
        .map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { source_id, .. } => *source_id,
            other => panic!("only triggered abilities are expected on this stack: {other:?}"),
        })
        .collect()
}

/// Cult of Skaro ("choose one at random") with Isshin, Two Heavens as One
/// (each attack trigger fires an additional time), Adeline, Resplendent Cathar
/// ("whenever you attack") and Seven of Nine: attacking with Cult triggers four
/// original abilities (Cult twice, Adeline twice). Each Cult trigger faces a
/// dilemma, so Seven triggers twice more. CR 603.3b: abilities that triggered
/// while the original batch is being placed go on the stack after the whole batch,
/// so the two Seven triggers sit above every original sibling, even when the
/// original placement order is Cult, Cult, Adeline, Adeline.
#[test]
fn random_modal_dilemma_observers_sit_above_the_whole_original_batch() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for index in 0..10 {
            scenario.add_spell_to_library_top(
                player,
                &format!("Library {}-{index}", player.0),
                false,
            );
        }
    }
    let cult = scenario.add_real_card(P0, "Cult of Skaro", Zone::Battlefield, db);
    let isshin = scenario.add_real_card(P0, "Isshin, Two Heavens as One", Zone::Battlefield, db);
    let adeline = scenario.add_real_card(P0, "Adeline, Resplendent Cathar", Zone::Battlefield, db);
    let seven = scenario.add_real_card(P0, "Seven of Nine", Zone::Battlefield, db);
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(cult, AttackTarget::Player(P1))])
        .expect("Cult of Skaro attacks");

    let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone() else {
        panic!(
            "Cult's and Adeline's doubled triggers need ordering, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(
        triggers.len(),
        4,
        "Cult twice and Adeline twice (Isshin doubles each): {triggers:?}"
    );
    assert!(
        triggers.iter().all(|t| t.source_id == cult || t.source_id == adeline),
        "only the attack triggers are being ordered: {triggers:?}"
    );
    // Placement order, bottom first: both Cult triggers, then both Adeline
    // triggers, so a Cult finisher runs while an original sibling is still unplaced.
    let mut order: Vec<usize> = (0..triggers.len())
        .filter(|&i| triggers[i].source_id == cult)
        .collect();
    order.extend((0..triggers.len()).filter(|&i| triggers[i].source_id == adeline));
    assert_eq!(order.len(), 4);
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("ordering is accepted");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no prompt is owed after the order: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        stack_sources(&runner),
        vec![cult, cult, adeline, adeline, seven, seven],
        "both dilemma observers go above all four original triggers"
    );
    assert_eq!(dilemmas(&runner, P0), 2, "each Cult trigger faced one dilemma");
    let _ = isshin;

    // Settle: everything resolves, Seven draws once per dilemma.
    for _ in 0..64 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority while the stack resolves");
    }
    assert!(runner.state().stack.is_empty(), "the stack settled");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let drawn = hand_len(&runner, P0) - hand_before;
    assert!(
        drawn >= 2 && drawn % 2 == 0,
        "Seven drew twice, plus two per resolved Caan mode: drew {drawn}"
    );
}
