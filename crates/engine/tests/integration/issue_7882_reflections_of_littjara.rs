//! Issue #7882 — Reflections of Littjara must copy a spell whose creature
//! subtype matches its own persisted as-enters choice.
//!
//! The two Reflections deliberately choose different values. This proves the
//! SpellCast filter reads the trigger source's choice, rather than a global or
//! most-recent choice, and the Zombie/Bear pair proves both the positive and
//! negative cast-trigger paths through the normal cast pipeline.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn mana(kind: ManaType, count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); count]
}

fn battlefield_named<'a>(
    runner: &'a GameRunner,
    name: &str,
) -> Vec<&'a engine::game::game_object::GameObject> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.name == name)
        .collect()
}

fn cast_reflections_and_choose(runner: &mut GameRunner, reflections: ObjectId, choice: &str) {
    let outcome = runner.cast(reflections).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::NamedChoice { .. }),
        "Reflections must reach its real as-enters choice, got {:?}",
        outcome.final_waiting_for()
    );
    let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
        panic!("Reflections must present a NamedChoice");
    };
    assert!(
        options.iter().any(|option| option == choice),
        "Reflections creature-type choice must offer {choice}, got {options:?}"
    );
    runner
        .act(GameAction::ChooseOption {
            choice: choice.to_string(),
        })
        .expect("answering Reflections' as-enters choice must succeed");
    runner.advance_until_stack_empty();
}

/// CR 607.2d + CR 205.3 + CR 707.10: each Reflections instance reads its own
/// chosen creature type; a matching creature spell is copied, while a
/// nonmatching creature spell resolves only once.
#[test]
fn reflections_of_littjara_copies_only_its_instances_chosen_creature_type() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zombie_reflections = scenario.add_real_card(P0, "Reflections of Littjara", Zone::Hand, db);
    let elf_reflections = scenario.add_real_card(P0, "Reflections of Littjara", Zone::Hand, db);
    let zombie_outlander = scenario.add_real_card(P0, "Zombie Outlander", Zone::Hand, db);
    let grizzly_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        [
            mana(ManaType::Colorless, 20),
            mana(ManaType::Blue, 5),
            mana(ManaType::White, 5),
            mana(ManaType::Black, 5),
            mana(ManaType::Green, 5),
        ]
        .concat(),
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    cast_reflections_and_choose(&mut runner, zombie_reflections, "Zombie");
    cast_reflections_and_choose(&mut runner, elf_reflections, "Elf");

    assert_eq!(
        runner.state().objects[&zombie_reflections].chosen_creature_type(),
        Some("Zombie"),
        "the first Reflections instance must retain its own choice"
    );
    assert_eq!(
        runner.state().objects[&elf_reflections].chosen_creature_type(),
        Some("Elf"),
        "the second Reflections instance must retain its distinct choice"
    );

    runner.cast(zombie_outlander).resolve();
    let zombies = battlefield_named(&runner, "Zombie Outlander");
    assert_eq!(
        zombies.len(),
        2,
        "a Zombie spell must resolve once and create exactly one copied permanent spell"
    );
    assert_eq!(
        zombies.iter().filter(|object| !object.is_token).count(),
        1,
        "the original Zombie Outlander must resolve as a nontoken"
    );
    assert_eq!(
        zombies.iter().filter(|object| object.is_token).count(),
        1,
        "exactly one Reflections copy of Zombie Outlander must become a token"
    );

    runner.cast(grizzly_bears).resolve();
    let bears = battlefield_named(&runner, "Grizzly Bears");
    assert_eq!(
        bears.len(),
        1,
        "a Bear must resolve once when neither Reflections chose Bear"
    );
    assert!(
        !bears[0].is_token,
        "the original Grizzly Bears must resolve before the no-copy assertion"
    );
}
