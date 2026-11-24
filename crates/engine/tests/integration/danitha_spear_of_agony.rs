//! CR 115.9b: Danitha, Spear of Agony checks the current targets of each spell
//! its controller casts against the two target classes in its Oracle text.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const DANITHA_ORACLE: &str = "First strike\nWhenever you cast a spell that targets an opponent or a creature an opponent controls, put a +1/+1 counter on Danitha.";

fn extra_mana(red: usize, blue: usize) -> Vec<ManaUnit> {
    (0..red)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .chain((0..blue).map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])))
        .collect()
}

fn settle(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the stack should be empty");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the cast and its triggers should finish at P0 priority, got {:?}",
        runner.state().waiting_for
    );
}

fn plus_one_plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn danitha_triggers_only_for_opponents_and_their_creatures() {
    let db = shared_card_db().expect("integration fixture contains the real test cards");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Danitha is intentionally parsed from verbatim Oracle text so this witness
    // exercises the current parser, independent of any exported card snapshot.
    let danitha = scenario
        .add_creature_from_oracle(P0, "Danitha, Spear of Agony", 2, 2, DANITHA_ORACLE)
        .id();
    let own_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let opponent_creature = scenario.add_real_card(P1, "Hill Giant", Zone::Battlefield, db);
    let opponent_artifact = scenario.add_real_card(P1, "Bonesplitter", Zone::Battlefield, db);
    let p1_shock = scenario.add_real_card(P1, "Shock", Zone::Hand, db);
    let spectral_sailor = scenario.add_real_card(P1, "Spectral Sailor", Zone::Hand, db);

    let opponent_player_shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    let opponent_creature_shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    let own_player_shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    let own_creature_shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    let arc_trail = scenario.add_real_card(P0, "Arc Trail", Zone::Hand, db);
    let shatter = scenario.add_real_card(P0, "Shatter", Zone::Hand, db);
    let divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    let counterspell = scenario.add_real_card(P0, "Counterspell", Zone::Hand, db);

    scenario.with_mana_pool(P0, extra_mana(12, 12));
    scenario.with_mana_pool(P1, extra_mana(1, 1));
    scenario.with_library_top(P0, &["P0 Library A", "P0 Library B"]);

    let mut runner = scenario.build();
    assert_eq!(plus_one_plus_one_counters(&runner, danitha), 0);

    // Positive reach guards: the parsed trigger increments for each printed
    // target class before any negative assertion relies on its filter.
    let before = plus_one_plus_one_counters(&runner, danitha);
    runner
        .cast(opponent_player_shock)
        .target_player(P1)
        .resolve();
    settle(&mut runner);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        before + 1,
        "Shock targeting an opponent adds exactly one counter"
    );

    // CR 603.2c: one cast event triggers Danitha once even when two targets
    // satisfy its target condition (CR 115.9b). Hill Giant survives either hit.
    let before = plus_one_plus_one_counters(&runner, danitha);
    runner
        .cast(arc_trail)
        .target_player(P1)
        .target_object(opponent_creature)
        .resolve();
    settle(&mut runner);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        before + 1,
        "Arc Trail targeting an opponent and their creature adds exactly one counter"
    );

    let before = plus_one_plus_one_counters(&runner, danitha);
    runner
        .cast(opponent_creature_shock)
        .target_object(opponent_creature)
        .resolve();
    settle(&mut runner);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        before + 1,
        "Shock targeting an opponent's creature adds exactly one counter"
    );

    let before = plus_one_plus_one_counters(&runner, danitha);
    runner.cast(own_player_shock).target_player(P0).resolve();
    settle(&mut runner);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        before,
        "Shock targeting its caster does not add a counter"
    );

    let before = plus_one_plus_one_counters(&runner, danitha);
    runner
        .cast(own_creature_shock)
        .target_object(own_creature)
        .resolve();
    settle(&mut runner);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        before,
        "Shock targeting its controller's creature does not add a counter"
    );

    let before = plus_one_plus_one_counters(&runner, danitha);
    runner
        .cast(shatter)
        .target_object(opponent_artifact)
        .resolve();
    settle(&mut runner);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        before,
        "Shatter targeting an opponent's noncreature artifact does not add a counter"
    );

    let before = plus_one_plus_one_counters(&runner, danitha);
    runner.cast(divination).resolve();
    settle(&mut runner);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        before,
        "a non-targeting spell does not add a counter"
    );

    // CR 109.5: Danitha's "you cast" refers to its controller. P1 casts a
    // spell with a target that would qualify if P0 had cast it.
    let before = plus_one_plus_one_counters(&runner, danitha);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    let p1_commit = runner.cast(p1_shock).target_player(P1).commit();
    assert_eq!(
        p1_commit.state().stack.len(),
        1,
        "an opponent's qualifying target does not put Danitha's trigger on the stack"
    );
    p1_commit.resolve().assert_life_delta(P1, -2);
    settle(&mut runner);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        before,
        "an opponent's cast adds no counters"
    );

    // CR 109.2 + CR 115.9b: A creature description refers to a permanent.
    // Spectral Sailor is a creature spell when P0 targets it with Counterspell.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    let mut sailor_commit = runner.cast(spectral_sailor).commit();
    assert_eq!(
        sailor_commit.state().objects[&spectral_sailor].zone,
        Zone::Stack
    );
    sailor_commit
        .act(GameAction::PassPriority)
        .expect("P1 passes priority to P0");
    let countered = sailor_commit
        .cast(counterspell)
        .target_object(spectral_sailor)
        .resolve();
    // CR 701.6a: The countered spell goes to its owner's graveyard.
    countered.assert_zone(&[spectral_sailor], Zone::Graveyard);
    settle(&mut runner);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        before,
        "Counterspell targeting an opponent's creature spell adds no counters"
    );
}

#[test]
fn danitha_distinguishes_opponents_and_teammates_in_two_headed_giant() {
    let db = shared_card_db().expect("integration fixture contains the real test cards");
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let danitha = scenario
        .add_creature_from_oracle(P0, "Danitha, Spear of Agony", 2, 2, DANITHA_ORACLE)
        .id();
    // CR 102.3: P1 shares P0's team and P2 belongs to the opposing team.
    let teammate_creature = scenario.add_real_card(P1, "Hill Giant", Zone::Battlefield, db);
    let opponent_creature =
        scenario.add_real_card(PlayerId(2), "Hill Giant", Zone::Battlefield, db);
    let opponent_shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    let teammate_shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    scenario.with_mana_pool(P0, extra_mana(2, 0));

    let mut runner = scenario.build();
    assert_eq!(plus_one_plus_one_counters(&runner, danitha), 0);

    runner
        .cast(opponent_shock)
        .target_object(opponent_creature)
        .resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&opponent_creature].damage_marked, 2);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        1,
        "Shock targeting an opposing team's creature adds one counter"
    );

    runner
        .cast(teammate_shock)
        .target_object(teammate_creature)
        .resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&teammate_creature].damage_marked, 2);
    assert_eq!(
        plus_one_plus_one_counters(&runner, danitha),
        1,
        "Shock targeting a teammate's creature adds no counters"
    );
}
