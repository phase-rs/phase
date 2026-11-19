//! Production-path regression for Sproutwatch Dryad's shared-predicate keyword
//! grants. The real card database is intentional: this exercises card parsing,
//! trigger indexing, resolution-time condition evaluation, and the cleanup
//! duration boundary together.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn setup_board(
    with_qualifiers: bool,
    opponent_has_first_strike: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    let db = load_db().expect("card database is required for real-card integration coverage");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dryad = scenario.add_real_card(P0, "Sproutwatch Dryad", Zone::Battlefield, db);
    if with_qualifiers {
        scenario.add_real_card(P0, "Birds of Paradise", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Abattoir Ghoul", Zone::Hand, db);
    } else if opponent_has_first_strike {
        scenario.add_real_card(P1, "Abattoir Ghoul", Zone::Hand, db);
    }
    for _ in 0..8 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
        scenario.add_real_card(P1, "Island", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    (runner, dryad)
}

fn resolve_begin_combat(runner: &mut engine::game::scenario::GameRunner) {
    runner.advance_to_combat();
    runner.advance_until_stack_empty();
}

/// CR 608.2c + CR 608.2h + CR 611.2a: a controlled creature with flying and a
/// card in the controller's hand with first strike satisfy different grants.
/// The independent negative assertion proves the first-strike condition was
/// rewritten instead of copying the flying predicate to every clone.
#[test]
fn sproutwatch_dryad_applies_each_keyword_condition_independently() {
    let (mut runner, dryad) = setup_board(true, false);
    resolve_begin_combat(&mut runner);

    let object = &runner.state().objects[&dryad];
    assert!(object.has_keyword(&Keyword::Flying));
    assert!(object.has_keyword(&Keyword::FirstStrike));
    assert!(!object.has_keyword(&Keyword::DoubleStrike));
    assert!(!object.has_keyword(&Keyword::Deathtouch));
}

/// CR 109.5 + CR 608.2h: "your hand" is scoped to the ability controller;
/// an opponent's first-strike card cannot satisfy Sproutwatch Dryad.
#[test]
fn sproutwatch_dryad_does_not_read_opponents_hand() {
    let (mut runner, dryad) = setup_board(false, true);
    resolve_begin_combat(&mut runner);

    let object = &runner.state().objects[&dryad];
    assert!(!object.has_keyword(&Keyword::Flying));
    assert!(!object.has_keyword(&Keyword::FirstStrike));
}

/// CR 611.2a + CR 514.2: the grants last through the current turn and expire
/// as the turn crosses cleanup.
#[test]
fn sproutwatch_dryad_grants_expire_at_cleanup() {
    let (mut runner, dryad) = setup_board(true, false);
    resolve_begin_combat(&mut runner);
    assert!(runner.state().objects[&dryad].has_keyword(&Keyword::Flying));

    runner.declare_attackers(&[]).expect("declare no attackers");
    runner.advance_to_end_step();
    runner.advance_to_upkeep();

    let object = &runner.state().objects[&dryad];
    assert!(!object.has_keyword(&Keyword::Flying));
    assert!(!object.has_keyword(&Keyword::FirstStrike));
}
