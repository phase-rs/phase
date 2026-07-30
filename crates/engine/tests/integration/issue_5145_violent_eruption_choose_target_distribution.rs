//! Issue #5145: Violent Eruption deals 4 damage divided as you choose among
//! any number of targets. The production client commits targets slot-by-slot via
//! `GameAction::ChooseTarget`; that completion path must surface
//! `WaitingFor::DistributeAmong` before payment, same as bulk `SelectTargets`.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const VIOLENT_ERUPTION_ORACLE: &str =
    "Violent Eruption deals 4 damage divided as you choose among any number of targets.";

fn add_red_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::PlayerId,
    count: usize,
) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(ManaType::Red, dummy, false, vec![]));
    }
}

#[test]
fn violent_eruption_choose_target_path_divides_damage_among_two_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Violent Eruption", true, VIOLENT_ERUPTION_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red, ManaCostShard::Red],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    add_red_mana(&mut runner, P0, 6);

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Violent Eruption should be accepted");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "expected target selection after cast announcement"
    );

    // Mirror the client: one ChooseTarget per slot (opponent player, then creature).
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("first ChooseTarget (player) should succeed");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("second ChooseTarget (creature) should succeed");
    // Skip remaining optional slots — mirrors the client "done" action.
    runner
        .act(GameAction::ChooseTarget { target: None })
        .expect("skipping optional tail should complete target selection");

    let WaitingFor::DistributeAmong { total, targets, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected DistributeAmong after slot-by-slot target selection, got {:?}",
            runner.state().waiting_for,
        );
    };
    assert_eq!(total, 4, "damage pool to divide must be 4");
    assert_eq!(
        targets.len(),
        2,
        "both chosen targets must participate in the distribution",
    );

    runner
        .act(GameAction::DistributeAmong {
            distribution: vec![(TargetRef::Player(P1), 1), (TargetRef::Object(bear), 3)],
        })
        .expect("1/3 distribution should be accepted");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        3,
        "creature must take only its assigned share",
    );
    let p1_life = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .map(|p| p.life)
        .expect("P1 must exist");
    assert_eq!(
        p1_life, 19,
        "opponent must lose only their assigned share (20 - 1)",
    );
}
