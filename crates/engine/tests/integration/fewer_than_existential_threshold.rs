//! "there are fewer than N ..." strict-inequality threshold — end-to-end.
//!
//! Two production paths consume the strict-comparator-prefix axis added to
//! `parse_there_are_conditions`:
//!
//! - CR 603.4 (intervening-"if" triggered ability): Shadowborn Demon —
//!   "At the beginning of your upkeep, if there are fewer than six creature
//!   cards in your graveyard, sacrifice a creature." The trigger fires only
//!   while the controller's graveyard holds fewer than six creature cards.
//! - CR 611.3a + CR 613.1d (Layer-4 type-removal static "as long as" gate):
//!   The Warring Triad — "As long as there are fewer than eight cards in your
//!   graveyard, ~ isn't a creature." The Creature type is removed only while
//!   the gate is true.
//!
//! Pre-fix the "fewer than" prefix was unrecognized, so both conditions
//! silently swallowed: the demon sacrificed every upkeep regardless of
//! graveyard size, and the type-removal applied unconditionally. Each path has
//! a boundary-exact revert-failing negative (graveyard size == N) paired with a
//! positive reach-guard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SHADOWBORN_DEMON: &str = "At the beginning of your upkeep, if there are fewer than six \
creature cards in your graveyard, sacrifice a creature.";
const WARRING_TRIAD: &str =
    "As long as there are fewer than eight cards in your graveyard, ~ isn't a creature.";

/// Stage `count` distinct creature cards in `player`'s graveyard.
fn fill_graveyard_with_creatures(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    for i in 0..count {
        scenario.add_creature_to_graveyard(player, &format!("Graveyard Creature {i}"), 1, 1);
    }
}

/// Drive to P0's upkeep and pass priority until an `EffectZoneChoice` surfaces
/// or the upkeep stack empties (no trigger). Returns the runner positioned there.
fn advance_p0_upkeep(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 1;
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::Untap;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_upkeep();
    for _ in 0..64 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ) {
            return runner;
        }
        assert_eq!(
            runner.state().phase,
            Phase::Upkeep,
            "upkeep trigger processing must not advance into a later phase"
        );
        if runner.state().stack.is_empty() {
            return runner;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "expected priority or the sacrifice choice while advancing upkeep, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass while resolving the upkeep trigger");
    }
    panic!("upkeep trigger processing did not settle within 64 priority passes")
}

/// CR 603.4 + CR 107.1: the gate is true (5 < 6), so the upkeep sacrifice
/// fires. P1's graveyard holds six creature cards — multi-authority noise that
/// proves the count binds to the demon's controller (P0), not the union of
/// graveyards (which would total 11 and suppress the trigger). Positive reach:
/// the sacrifice choice arrives and resolves.
#[test]
fn shadowborn_demon_fewer_than_six_fires_and_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Shadowborn Demon", 5, 5, SHADOWBORN_DEMON);
    let fodder = scenario.add_creature(P0, "Sacrifice Fodder", 1, 1).id();
    // P0 controller graveyard: five creature cards (< 6 → gate true).
    fill_graveyard_with_creatures(&mut scenario, P0, 5);
    // P1 graveyard noise: six creature cards. If the count read all graveyards
    // the total (11) would NOT be fewer than six and the trigger would not fire.
    fill_graveyard_with_creatures(&mut scenario, P1, 6);

    let mut runner = advance_p0_upkeep(scenario);

    let choices = match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the demon's controller sacrifices");
            assert!(
                cards.contains(&fodder),
                "the fodder creature must be an eligible sacrifice, got {cards:?}"
            );
            cards.clone()
        }
        other => panic!("expected a sacrifice EffectZoneChoice at P0 upkeep, got {other:?}"),
    };
    assert!(
        choices.len() >= 2,
        "both the demon and the fodder are eligible, got {choices:?}"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("sacrifice choice must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "the chosen fodder must be sacrificed to the graveyard"
    );
}

/// CR 603.4 + CR 107.1: boundary-exact negative. Six creature cards is NOT
/// fewer than six, so the intervening-"if" is false and no sacrifice occurs.
/// This kills unconditional / GE / LE misparses; the paired positive above
/// proves the trigger is reachable. Revert-failing: pre-fix the swallowed
/// condition made the demon sacrifice here too.
#[test]
fn shadowborn_demon_exactly_six_does_not_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let demon = scenario
        .add_creature_from_oracle(P0, "Shadowborn Demon", 5, 5, SHADOWBORN_DEMON)
        .id();
    let fodder = scenario.add_creature(P0, "Sacrifice Fodder", 1, 1).id();
    // P0 controller graveyard: exactly six creature cards (== 6 → gate false).
    fill_graveyard_with_creatures(&mut scenario, P0, 6);

    let runner = advance_p0_upkeep(scenario);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "no sacrifice choice may surface at exactly six creature cards, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&demon].zone,
        Zone::Battlefield,
        "the demon must remain (no sacrifice fired)"
    );
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Battlefield,
        "the fodder must remain (no sacrifice fired)"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].graveyard.len(),
        6,
        "graveyard is untouched at the boundary"
    );
}

/// CR 611.3a + CR 613.1d: the Layer-4 type-removal gate is active (0 < 8), so
/// the permanent is not a creature. Drives the real layer-recompute pipeline
/// (PassPriority → SBAs → evaluate_layers) and asserts post-layer core_types.
#[test]
fn warring_triad_fewer_than_eight_removes_creature_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let triad = scenario
        .add_creature_from_oracle(P0, "The Warring Triad", 3, 3, WARRING_TRIAD)
        .id();
    // P0 graveyard empty (0 < 8 → gate true).

    let mut runner = scenario.build();
    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to apply the static type-removal gate");

    let core_types = &runner.state().objects[&triad].card_types.core_types;
    assert!(
        !core_types.contains(&CoreType::Creature),
        "gate active (<8) must strip the Creature type, got {core_types:?}"
    );
}

/// CR 611.3a + CR 613.1d: boundary-exact negative. Eight cards is NOT fewer
/// than eight, so the gate is inactive and the Creature type is retained.
/// Revert-failing: pre-fix the swallowed condition read as always-true and the
/// type was stripped unconditionally.
#[test]
fn warring_triad_exactly_eight_retains_creature_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let triad = scenario
        .add_creature_from_oracle(P0, "The Warring Triad", 3, 3, WARRING_TRIAD)
        .id();
    // P0 graveyard: exactly eight cards (== 8 → gate false).
    fill_graveyard_with_creatures(&mut scenario, P0, 8);

    let mut runner = scenario.build();
    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to evaluate the boundary static gate");

    let core_types = &runner.state().objects[&triad].card_types.core_types;
    assert!(
        core_types.contains(&CoreType::Creature),
        "gate inactive (==8) must retain the Creature type, got {core_types:?}"
    );
}
