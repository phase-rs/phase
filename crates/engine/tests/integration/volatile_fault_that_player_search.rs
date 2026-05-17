//! Regression (issue #483): Volatile Fault — "that player" search anaphor.
//!
//! Volatile Fault: "{1}, {T}, Sacrifice this land: Destroy target nonbasic land
//! an opponent controls. That player may search their library for a basic land
//! card, put it onto the battlefield, then shuffle. You create a Treasure token."
//!
//! The non-trigger "that player" subject previously parsed to a generic
//! `TargetFilter::Player`, which resolves to the *ability's controller* (the
//! activator). It must resolve to the controller of the destroyed nonbasic land
//! — CR 608.2c anaphor to the parent target — i.e. `ParentTargetController`.
//!
//! This is a full `apply()`-driven pipeline test: it proves the destroyed land's
//! `TargetRef::Object` propagates from the `Destroy` parent ability into the
//! `SearchLibrary` sub-ability so the search prompt routes to the land's
//! controller, NOT a hand-constructed `ResolvedAbility` shape test.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("export should load")))
}

#[test]
fn volatile_fault_that_player_search_routes_to_destroyed_lands_controller() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1 (the activator) controls Volatile Fault on the battlefield.
    let fault = scenario.add_real_card(P1, "Volatile Fault", Zone::Battlefield, db);
    // P0 controls a nonbasic land — the destroy target.
    let victim_land = scenario.add_real_card(P0, "Mishra's Factory", Zone::Battlefield, db);
    // P0's library holds a basic land to be found, plus another card to make a
    // shuffle observable.
    let p0_forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Make it P1's turn so the activator holds priority for the instant-speed
    // activated ability; P0 remains the opponent who controls the victim land.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        // {1} floating mana for P1 to pay the activation cost.
        state.players[1].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    // Precondition: victim land controlled by P0.
    assert_eq!(runner.state().objects[&victim_land].controller, P0);

    // P1 activates Volatile Fault's destroy ability (ability index 1 — index 0
    // is the {T}: Add {C} mana ability). The only legal target is P0's nonbasic
    // land, so the engine auto-selects it and puts the ability on the stack.
    runner
        .act(GameAction::ActivateAbility {
            source_id: fault,
            ability_index: 1,
        })
        .expect("activating Volatile Fault's destroy ability must succeed");

    // The ability is on the stack carrying P0's land as the chosen target —
    // this is what `ParentTargetController` resolves against at runtime.
    let stacked_targets: Vec<TargetRef> = runner
        .state()
        .stack
        .iter()
        .find_map(|item| item.ability())
        .expect("Volatile Fault's ability must be on the stack")
        .targets
        .clone();
    assert!(
        stacked_targets
            .iter()
            .any(|t| matches!(t, TargetRef::Object(id) if *id == victim_land)),
        "the stacked ability must carry the victim land as TargetRef::Object, \
         got {stacked_targets:?}",
    );

    // Resolve the ability off the stack.
    runner.advance_until_stack_empty();

    // The victim land is destroyed (moved to P0's graveyard).
    assert_eq!(
        runner.state().objects[&victim_land].zone,
        Zone::Graveyard,
        "the targeted nonbasic land must be destroyed",
    );

    // "That player MAY search" — the optional-effect prompt routes to P0, the
    // destroyed land's controller, NOT P1 the activator. This is the core fix.
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => assert_eq!(
            *player, P0,
            "the optional 'may search' prompt must route to the destroyed land's \
             controller (P0), not the activator (P1)",
        ),
        other => {
            panic!("expected OptionalEffectChoice after Volatile Fault resolves, got {other:?}")
        }
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P0 accepting the optional search must succeed");

    // The search-choice prompt also routes to P0.
    let search_cards = match &runner.state().waiting_for {
        WaitingFor::SearchChoice { player, cards, .. } => {
            assert_eq!(
                *player, P0,
                "SearchChoice must prompt the destroyed land's controller (P0), \
                 not the activator (P1)",
            );
            cards.clone()
        }
        other => panic!("expected SearchChoice after Volatile Fault resolves, got {other:?}"),
    };
    assert!(
        search_cards.contains(&p0_forest),
        "P0's Forest must be a legal basic-land search choice",
    );

    // P0 selects the basic land.
    runner
        .act(GameAction::SelectCards {
            cards: vec![p0_forest],
        })
        .expect("P0 selecting the Forest must resolve the search continuation");

    runner.advance_until_stack_empty();

    // The found basic land enters the battlefield under P0's control (its
    // owner — CR 110.2a / CR 400.3 default), not the activator's.
    assert_eq!(
        runner.state().objects[&p0_forest].zone,
        Zone::Battlefield,
        "the found basic land must be put onto the battlefield",
    );
    assert_eq!(
        runner.state().objects[&p0_forest].controller,
        P0,
        "the found basic land must enter under P0's control (the searching player)",
    );

    // P1 (the activator) creates the Treasure token.
    let p1_treasures = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == P1 && o.zone == Zone::Battlefield && o.name.contains("Treasure")
        })
        .count();
    assert_eq!(
        p1_treasures, 1,
        "the activator (P1) must create exactly one Treasure token",
    );
    let p0_treasures = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == P0 && o.zone == Zone::Battlefield && o.name.contains("Treasure")
        })
        .count();
    assert_eq!(p0_treasures, 0, "P0 must not receive a Treasure token");
}
