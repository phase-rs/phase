//! Accursed Witch: the dying source returns transformed and attached to the
//! selected opponent, through the production zone-change and trigger pipeline.
//! CR 400.7e + CR 603.6: the trigger finds its source in the destination zone.
//! CR 712.14a: returning a double-faced card transformed uses its back face.
//!
//! The same self-return class also covers a chain that moves the source first
//! (Lamplight Phoenix, CR 400.7j) and an otherwise branch (Bogardan Phoenix).

use std::sync::Arc;

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::process_triggers;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::parse_counter_type;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const DIES_TRIGGER: &str = "When this creature dies, return it to the battlefield transformed under your control attached to target opponent.";

#[test]
fn accursed_witch_dies_return_it_binds_self() {
    let db = shared_card_db().expect("the real DFC regression requires generated card data");
    // Two players leave exactly one legal opponent and the engine selects it
    // automatically. Three players exercise the target-selection pause.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let witch = scenario.add_real_card(P0, "Accursed Witch", Zone::Battlefield, db);
    let mut runner = scenario.build();
    // Hydrate the real two-faced card, but parse the trigger with this source
    // tree so stale generated parser output cannot hide an origin-stamp revert.
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let triggers = parse_oracle_text(
        DIES_TRIGGER,
        "Accursed Witch",
        &[],
        &["Creature".to_string()],
        &[],
    )
    .triggers;
    assert_eq!(triggers.len(), 1, "expected Accursed Witch's dies trigger");
    let obj = runner.state_mut().objects.get_mut(&witch).unwrap();
    assert!(!obj.transformed);
    assert!(
        obj.back_face.is_some(),
        "the return must exercise a real DFC"
    );
    // Keep the overlay in the base set: `process_triggers` flushes layers before
    // collecting LKI, which materializes live triggers from this authority.
    obj.install_trigger_base_definitions(Arc::new(triggers))
        .expect("test trigger base set must materialize");
    reindex_object_triggers(runner.state_mut(), witch);

    let mut events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(witch, Zone::Graveyard, witch),
        &mut events,
    ));
    process_triggers(runner.state_mut(), &events);
    assert_eq!(runner.state().objects[&witch].zone, Zone::Graveyard);
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
        ),
        "the dying Witch must ask for the opponent attachment target; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose the opponent to enchant");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "the trigger must resolve completely"
    );
    let returned = &runner.state().objects[&witch];
    assert_eq!(returned.zone, Zone::Battlefield);
    assert!(returned.transformed);
    assert_eq!(returned.name, "Infectious Curse");
    assert_eq!(returned.attached_to, Some(AttachTarget::Player(P1)));
}

/// Lamplight Phoenix's dies trigger, parsed with this source tree.
const LAMPLIGHT_TRIGGER: &str = "When this creature dies, you may exile it and collect evidence 4. If you do, return this card to the battlefield tapped. (To collect evidence 4, exile cards with total mana value 4 or greater from your graveyard.)";

/// CR 400.7j + CR 603.6: Lamplight Phoenix exiles itself from the graveyard as
/// part of its own effect, so the later "return this card" finds it in exile,
/// not in the graveyard the dies event put it in.
#[test]
fn lamplight_phoenix_returns_from_exile_after_exiling_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let phoenix = scenario
        .add_creature_from_oracle(P0, "Lamplight Phoenix", 2, 2, LAMPLIGHT_TRIGGER)
        .id();
    let evidence = scenario
        .add_creature_to_graveyard(P0, "Evidence", 4, 4)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(phoenix, Zone::Graveyard, phoenix),
        &mut events,
    ));
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the dies trigger offers its optional exile; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("exile the Phoenix and collect evidence");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CollectEvidenceChoice { .. }
        ),
        "collect evidence 4 asks for graveyard cards; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![evidence],
        })
        .expect("exile mana value 4 as evidence");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&evidence].zone, Zone::Exile);
    let returned = &runner.state().objects[&phoenix];
    assert_eq!(
        returned.zone,
        Zone::Battlefield,
        "the return must find the Phoenix in exile, where its own effect moved it"
    );
    assert!(returned.tapped, "the Phoenix returns tapped");
}

/// Bogardan Phoenix's dies trigger, parsed with this source tree.
const BOGARDAN_TRIGGER: &str = "When this creature dies, exile it if it had a death counter on it. Otherwise, return it to the battlefield under your control and put a death counter on it.";

/// CR 608.2c + CR 603.6: the "otherwise" return in Bogardan Phoenix's dies
/// trigger names the dying Phoenix, found in the graveyard the dies event put
/// it in. The second death, with the counter, exiles it instead.
#[test]
fn bogardan_phoenix_otherwise_branch_returns_the_dying_phoenix() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let phoenix = scenario
        .add_creature_from_oracle(P0, "Bogardan Phoenix", 3, 3, BOGARDAN_TRIGGER)
        .id();
    let mut runner = scenario.build();

    let die = |runner: &mut engine::game::scenario::GameRunner| {
        let mut events = Vec::new();
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(phoenix, Zone::Graveyard, phoenix),
            &mut events,
        ));
        process_triggers(runner.state_mut(), &events);
        runner.advance_until_stack_empty();
    };

    die(&mut runner);
    let returned = &runner.state().objects[&phoenix];
    assert_eq!(
        returned.zone,
        Zone::Battlefield,
        "without a death counter the otherwise branch returns the Phoenix"
    );
    assert_eq!(
        returned.counters.get(&parse_counter_type("death")),
        Some(&1),
        "the returned Phoenix gets a death counter"
    );

    die(&mut runner);
    assert_eq!(
        runner.state().objects[&phoenix].zone,
        Zone::Exile,
        "with a death counter the Phoenix is exiled"
    );
}
