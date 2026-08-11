//! Runtime regression for Ichneumon Druid's non-first instant-spell trigger.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const ICHNEUMON_DRUID: &str = "Whenever an opponent casts an instant spell other than the first instant spell that player casts each turn, this creature deals 4 damage to that player.";

/// CR 603.2: this is a fire-time event qualifier, not a CR 603.4
/// intervening-if. A noninstant between the first and second instant must not
/// increment the instant-only history.
#[test]
fn ichneumon_druid_damages_only_after_opponents_first_instant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Ichneumon Druid", 1, 1, ICHNEUMON_DRUID);
    let own_instant = scenario.add_bolt_to_hand(P0);
    let first_instant = scenario.add_bolt_to_hand(P1);
    let noninstant = scenario
        .add_creature_to_hand_from_oracle(P1, "Ordinary Bear", 1, 1, "")
        .with_mana_cost(engine::types::mana::ManaCost::generic(0))
        .id();
    let second_instant = scenario.add_bolt_to_hand(P1);
    let third_instant = scenario.add_bolt_to_hand(P1);
    let target_a = scenario.add_creature(P0, "Target A", 0, 8).id();
    let target_b = scenario.add_creature(P0, "Target B", 0, 8).id();
    let target_c = scenario.add_creature(P0, "Target C", 0, 8).id();
    let own_target = scenario.add_creature(P1, "Own-Cast Target", 0, 8).id();
    let mana = ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]);
    scenario.with_mana_pool(P1, vec![mana.clone(), mana.clone(), mana]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let initial_life = runner.life(P1);
    let controller_life = runner.life(P0);
    // Source/controller and caster deliberately diverge: the source's own
    // instant is not an opponent event and must not damage its controller.
    runner.cast(own_instant).target_object(own_target).resolve();
    assert_eq!(
        runner.life(P0),
        controller_life,
        "controller's own instant must not trigger"
    );
    assert_eq!(
        runner.life(P1),
        initial_life,
        "own instant must not damage opponent either"
    );
    runner.cast(first_instant).target_object(target_a).resolve();
    assert_eq!(
        runner.life(P1),
        initial_life,
        "first instant must not trigger"
    );
    runner.cast(noninstant).resolve();
    assert_eq!(
        runner.life(P1),
        initial_life,
        "noninstant must not increment instant history"
    );
    runner
        .cast(second_instant)
        .target_object(target_b)
        .resolve();
    assert_eq!(
        runner.life(P1),
        initial_life - 4,
        "second instant must trigger once"
    );
    runner.cast(third_instant).target_object(target_c).resolve();
    assert_eq!(
        runner.life(P1),
        initial_life - 8,
        "every later instant must trigger"
    );
}
