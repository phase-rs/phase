//! Production-path regression for Sproutwatch Dryad's shared-predicate keyword
//! grants. The real card database is intentional: this exercises card parsing,
//! trigger indexing, resolution-time condition evaluation, and the cleanup
//! duration boundary together.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
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
        scenario.add_real_card(P0, "Birds of Paradise", Zone::Battlefield, db);
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
    assert!(object.has_keyword(&Keyword::Flying));
    assert!(!object.has_keyword(&Keyword::FirstStrike));
}

/// CR 109.2 + CR 611.3a: the creature branch is a battlefield permanent
/// subject, while the card branch explicitly remains in its owner's hand.
/// A same-controller creature in another zone must not satisfy the trigger.
#[test]
fn sproutwatch_dryad_ignores_creatures_outside_battlefield() {
    let db = load_db().expect("card database is required for real-card integration coverage");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dryad = scenario.add_real_card(P0, "Sproutwatch Dryad", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Abattoir Ghoul", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Abattoir Ghoul", Zone::Library, db);
    for _ in 0..8 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
        scenario.add_real_card(P1, "Island", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert!(runner.state().objects[&dryad]
        .trigger_definitions
        .iter_all()
        .any(|entry| entry.definition.mode == TriggerMode::Phase));
    resolve_begin_combat(&mut runner);

    let object = &runner.state().objects[&dryad];
    assert!(!object.has_keyword(&Keyword::Flying));
    assert!(!object.has_keyword(&Keyword::FirstStrike));
}

/// CR 109.5 + CR 603.3a: a triggered ability keeps its controller after its
/// source changes control. P1's Act of Aggression resolves above the Dryad's
/// trigger, but the trigger still evaluates "you control" and "your hand" as
/// P0's subjects.
#[test]
fn sproutwatch_dryad_trigger_keeps_controller_after_act_of_aggression() {
    const ACT_OF_AGGRESSION: &str = "({R/P} can be paid with either {R} or 2 life.)\nGain control of target creature an opponent controls until end of turn. Untap that creature. It gains haste until end of turn.";

    let db = load_db().expect("card database is required for real-card integration coverage");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dryad = scenario.add_real_card(P0, "Sproutwatch Dryad", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Birds of Paradise", Zone::Battlefield, db);
    let act = scenario
        .add_spell_to_hand_from_oracle(P1, "Act of Aggression", true, ACT_OF_AGGRESSION)
        .with_mana_cost(ManaCost::zero())
        .id();
    for _ in 0..8 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
        scenario.add_real_card(P1, "Island", Zone::Library, db);
    }
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::BeginCombat);
    assert!(
        !runner.state().stack.is_empty(),
        "Dryad trigger must be on stack"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes to give P1 response priority");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );

    let card_id = runner.state().objects[&act].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: act,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 announces Act of Aggression");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(dryad)),
            })
            .expect("P1 targets the Dryad");
    }
    assert_eq!(
        runner.state().objects[&dryad].controller,
        P0,
        "the Dryad remains under P0 control until Act resolves"
    );

    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes Act of Aggression");
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes so Act of Aggression resolves");
    assert_eq!(runner.state().objects[&dryad].controller, P1);

    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&dryad].has_keyword(&Keyword::Flying));
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
