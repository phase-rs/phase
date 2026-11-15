//! Runtime regression for Ichneumon Druid's non-first instant-spell trigger.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::PlayerId;

const ICHNEUMON_DRUID: &str = "Whenever an opponent casts an instant spell other than the first instant spell that player casts each turn, this creature deals 4 damage to that player.";
const P2: PlayerId = PlayerId(2);

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

    let controller_life = runner.life(P0);
    let initial_life = runner.life(P1);
    // Source/controller and caster deliberately diverge: the source's own
    // instant is not an opponent event and must not damage its controller.
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
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
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
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

/// In Two-Headed Giant, P0/P1 are teammates and P2/P3 are the
/// opposing team. The same spell-history threshold is met for P1 and P2, so
/// team membership is the only axis that may determine whether this opponent
/// trigger fires.
#[test]
fn ichneumon_druid_excludes_teammates_in_two_headed_giant() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Ichneumon Druid", 1, 1, ICHNEUMON_DRUID);

    let teammate_first = scenario.add_bolt_to_hand(P1);
    let teammate_second = scenario.add_bolt_to_hand(P1);
    let opponent_first = scenario.add_bolt_to_hand(P2);
    let opponent_second = scenario.add_bolt_to_hand(P2);
    let target_a = scenario.add_creature(P0, "Target A", 0, 8).id();
    let target_b = scenario.add_creature(P0, "Target B", 0, 8).id();
    let target_c = scenario.add_creature(P0, "Target C", 0, 8).id();
    let target_d = scenario.add_creature(P0, "Target D", 0, 8).id();
    let mana = ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]);
    scenario.with_mana_pool(P1, vec![mana.clone(), mana.clone()]);
    scenario.with_mana_pool(P2, vec![mana.clone(), mana]);

    let mut runner = scenario.build();
    let teammate_life = runner.life(P1);
    let opponent_life = runner.life(P2);

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .cast(teammate_first)
        .target_object(target_a)
        .resolve();
    runner
        .cast(teammate_second)
        .target_object(target_b)
        .resolve();
    assert_eq!(
        runner.life(P1),
        teammate_life,
        "P1 is P0's teammate in Two-Headed Giant, so neither instant may trigger Ichneumon Druid"
    );

    runner.state_mut().active_player = P2;
    runner.state_mut().priority_player = P2;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P2 };
    runner
        .cast(opponent_first)
        .target_object(target_c)
        .resolve();
    assert_eq!(
        runner.life(P2),
        opponent_life,
        "an opposing team's first instant must not trigger Ichneumon Druid"
    );
    runner
        .cast(opponent_second)
        .target_object(target_d)
        .resolve();
    assert_eq!(
        runner.life(P2),
        opponent_life - 4,
        "an opposing team's second instant must trigger Ichneumon Druid exactly once"
    );
}
