//! Regression for GitHub issue #6427 — Ajani, Nacatl Pariah's transform trigger
//! when Ajani co-departs with the dying Cat.
//!
//! Oracle (Ajani): "Whenever one or more other Cats you control die, you may
//! exile Ajani, then return him to the battlefield transformed under his
//! owner's control."
//!
//! When Ajani and another Cat die in the same simultaneous event (mass removal),
//! accepting the optional trigger must NOT exile/return/transform Ajani — he
//! stays in the graveyard, untransformed. The co-departure mis-latch of
//! `expected_zone` to Graveyard must not let `SelfRef` early-bind.
//!
//! CR 400.7e: co-departure must not let SelfRef bind Ajani in the graveyard when
//! the trigger event is the Cat's departure — only his own departure successor may bind.
//! CR 603.10a: Ajani's trigger source retains battlefield last-known information.
//!
//! Harness note: use `cast(...).commit()` (not `.resolve()`) so the cast driver
//! does not auto-answer `OptionalEffectChoice` before the test can observe it.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const WRATH: &str = "Destroy all creatures.";

/// Ajani + Savannah Lions die together via Wrath of God (simultaneous batch).
/// Accepting the optional trigger must leave Ajani in the graveyard, untransformed.
#[test]
fn ajani_stays_in_graveyard_when_co_departing_with_dying_cat() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ajani = scenario.add_real_card(P0, "Ajani, Nacatl Pariah", Zone::Battlefield, db);
    let _lion = scenario.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    {
        let obj = runner.state().objects.get(&ajani).expect("Ajani exists");
        assert!(
            !obj.transformed,
            "precondition: Ajani starts front-face on the battlefield"
        );
    }

    // Commit Wrath to the stack without auto-declining Ajani's optional.
    runner.cast(wrath).commit();

    let mut saw_optional = false;
    loop {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                saw_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Ajani's optional exile/return trigger");
            }
            _ => break,
        }
    }
    assert!(
        saw_optional,
        "reach guard: Ajani's optional co-departure trigger must be offered"
    );

    let ajani_obj = runner
        .state()
        .objects
        .get(&ajani)
        .expect("Ajani object still exists");
    assert_eq!(
        ajani_obj.zone,
        Zone::Graveyard,
        "co-departure: Ajani must stay in the graveyard when the SelfRef mis-latch gate fails"
    );
    assert!(
        !ajani_obj.transformed,
        "co-departure: Ajani must remain untransformed"
    );
}

/// Declining the optional trigger leaves Ajani in the graveyard. Reach-guards that
/// the trigger still fires as optional (acceptance #3); effect resolution is not
/// entered, so this does not discriminate the SelfRef gate.
#[test]
fn ajani_stays_in_graveyard_when_co_departure_trigger_declined() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ajani = scenario.add_real_card(P0, "Ajani, Nacatl Pariah", Zone::Battlefield, db);
    let _lion = scenario.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.cast(wrath).commit();

    let mut saw_optional = false;
    loop {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                saw_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline Ajani's optional exile/return trigger");
            }
            _ => break,
        }
    }
    assert!(
        saw_optional,
        "reach guard: optional must still be offered on co-departure (acceptance #3)"
    );

    let ajani_obj = runner
        .state()
        .objects
        .get(&ajani)
        .expect("Ajani object still exists");
    assert_eq!(
        ajani_obj.zone,
        Zone::Graveyard,
        "declined co-departure trigger: Ajani stays in the graveyard"
    );
    assert!(
        !ajani_obj.transformed,
        "declined co-departure trigger: Ajani stays untransformed"
    );
}
