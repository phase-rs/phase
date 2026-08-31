//! Regression for #8215: Hellkite Courser's forwarded commander result is not
//! a declared target. Its immediate haste rider reads that result, while the
//! delayed return remains sourced by Hellkite Courser (CR 608.2c / CR 603.7c).

use engine::game::layers::flush_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const HELLKITE_COURSER_ORACLE: &str = "Flying\nWhen this creature enters, you may put a commander you own from the command zone onto the battlefield. It gains haste. Return it to the command zone at the beginning of the next end step.";

#[test]
fn issue_8215_hellkite_courser_returns_the_forwarded_commander_at_end_step() {
    let db = shared_card_db().expect("integration card fixture must load");
    assert_eq!(
        db.get_face_by_name("Hellkite Courser")
            .expect("fixture must contain Hellkite Courser")
            .oracle_text,
        Some(HELLKITE_COURSER_ORACLE.to_string()),
        "the regression must use Hellkite Courser's exact Oracle text"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let commander = scenario.add_real_card(P0, "The Ur-Dragon", Zone::Command, db);
    scenario.with_commander(commander);
    let courser = scenario.add_real_card(P0, "Hellkite Courser", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| {
                ManaUnit::new(
                    ManaType::Red,
                    engine::types::identifiers::ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );

    let mut runner = scenario.build();
    runner.cast(courser).accept_optional().resolve();

    assert_eq!(runner.state().objects[&commander].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&courser].zone, Zone::Battlefield);
    flush_layers(runner.state_mut());
    assert!(
        runner.state().objects[&commander].has_keyword(&Keyword::Haste),
        "the forwarded commander must gain haste"
    );
    assert!(
        !runner.state().objects[&courser].has_keyword(&Keyword::Haste),
        "Hellkite Courser must not receive the commander's haste rider"
    );

    let delayed = runner
        .state()
        .delayed_triggers
        .first()
        .expect("the return-to-command-zone trigger must be installed");
    assert_eq!(
        delayed.source_id, courser,
        "the delayed trigger remains Courser's"
    );
    assert_eq!(
        delayed.ability.source_id, courser,
        "the delayed ability remains Courser's"
    );
    assert_eq!(
        delayed.ability.targets,
        vec![engine::types::ability::TargetRef::Object(commander)],
        "the delayed return must snapshot the forwarded commander"
    );

    // CR 508.1: `advance_to_end_step` stops at the declare-attackers
    // turn-based action when invoked from precombat main. Cross that action
    // through the normal game flow before asking the phase helper to reach End.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("an empty attack declaration must cross combat");
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&commander].zone, Zone::Command);
    assert_eq!(runner.state().objects[&courser].zone, Zone::Battlefield);
    assert!(
        runner.state().stack.is_empty(),
        "the delayed return must settle"
    );
}
