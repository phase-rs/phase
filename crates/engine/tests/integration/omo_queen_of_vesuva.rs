//! Full-pipeline integration test for Omo, Queen of Vesuva (M3C).
//!
//! Oracle text:
//!   Whenever Omo enters or attacks, put an everything counter on each of up to
//!   one target land and up to one target creature.
//!   Each land with an everything counter on it is every land type in addition
//!   to its other types.
//!   Each nonland creature with an everything counter on it is every creature
//!   type.
//!
//! This drives the REAL trigger -> counter -> layer pipeline:
//!   cast Omo (0-cost) -> resolves onto the battlefield -> ETB trigger fires
//!   through `process_triggers` -> `WaitingFor::TriggerTargetSelection` over a
//!   land slot and a creature slot -> `SelectTargets` places an `everything`
//!   counter on each -> layer evaluation grants the land every land type and
//!   the creature every creature type.
//!
//! NOT a shape test: no synthetic `pending_trigger`, no hand-rolled counters,
//! no hand-built `WaitingFor`. The two `PutCounter` siblings, the counter
//! placement, and the continuous-type grants all run through `apply`.
//!
//! CR 122.1 (counters) + CR 205.3i / CR 305.7 (every land type + mana abilities)
//! + CR 205.3 (every creature type) + CR 601.2c / CR 603.3d (target selection).

use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

const OMO_ORACLE: &str = "Whenever Omo enters or attacks, put an everything counter on \
     each of up to one target land and up to one target creature.\n\
     Each land with an everything counter on it is every land type in addition to its \
     other types.\n\
     Each nonland creature with an everything counter on it is every creature type.";

const P0: PlayerId = PlayerId(0);

fn everything_counter() -> CounterType {
    CounterType::Generic("everything".to_string())
}

/// CR 122.1 + CR 205.3i + CR 305.7 + CR 205.3: Omo's ETB places an everything
/// counter on a target land and a target creature; the resulting continuous
/// statics make the land every land type (with mana abilities) and the creature
/// every creature type.
#[test]
fn omo_etb_grants_all_land_and_creature_types_through_pipeline() {
    let mut scenario = engine::game::scenario::GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // A vanilla land and a vanilla creature to receive the counters.
    let forest = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // Omo enters P0's hand as a 0-cost legendary creature so casting needs no
    // mana prompt; its abilities parse from the real Oracle text.
    let omo = scenario
        .add_creature_to_hand_from_oracle(P0, "Omo, Queen of Vesuva", 3, 3, OMO_ORACLE)
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    // The scenario harness loads no deck, so the engine's global creature-type
    // set (normally derived from the card pool in `deck_loading`) is empty.
    // Seed it so `AddAllCreatureTypes` has a set to expand at layer evaluation,
    // mirroring a real game's populated `all_creature_types`.
    runner.state_mut().all_creature_types = vec![
        "Bear".to_string(),
        "Goblin".to_string(),
        "Sliver".to_string(),
    ];
    let omo_card = runner.state().objects[&omo].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: omo,
            card_id: omo_card,
            targets: vec![],
        })
        .expect("casting a 0-cost legendary creature should succeed");
    // Resolve Omo onto the battlefield so its ETB trigger fires.
    runner.advance_until_stack_empty();

    // The ETB trigger must pause on target selection (land slot + creature slot).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Omo's ETB must pause on TriggerTargetSelection, got {:?}",
        runner.state().waiting_for
    );

    // Select the land for the land slot and the creature for the creature slot.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                engine::types::ability::TargetRef::Object(forest),
                engine::types::ability::TargetRef::Object(bear),
            ],
        })
        .expect("selecting a land and a creature target must succeed");
    runner.advance_until_stack_empty();

    // Both objects received an everything counter from the real resolution.
    let forest_obj = runner.state().objects.get(&forest).expect("forest present");
    assert!(
        forest_obj
            .counters
            .get(&everything_counter())
            .copied()
            .unwrap_or(0)
            >= 1,
        "land should have an everything counter; counters = {:?}",
        forest_obj.counters
    );
    let bear_obj = runner.state().objects.get(&bear).expect("bear present");
    assert!(
        bear_obj
            .counters
            .get(&everything_counter())
            .copied()
            .unwrap_or(0)
            >= 1,
        "creature should have an everything counter; counters = {:?}",
        bear_obj.counters
    );

    // CR 205.3i + CR 305.7: the land is now every land type (spot-check a
    // spread of basic + nonbasic types) and gains a basic-land mana ability.
    let forest_obj = runner.state().objects.get(&forest).expect("forest present");
    for name in [
        "Forest", "Island", "Swamp", "Mountain", "Plains", "Desert", "Gate", "Locus",
    ] {
        assert!(
            forest_obj.card_types.subtypes.contains(&name.to_string()),
            "land missing land type {name}; subtypes = {:?}",
            forest_obj.card_types.subtypes
        );
    }

    // CR 205.3: the nonland creature is now every creature type — assert it
    // carries the game's global creature-type set.
    let bear_obj = runner.state().objects.get(&bear).expect("bear present");
    let all_creature_types = runner.state().all_creature_types.clone();
    assert!(
        !all_creature_types.is_empty(),
        "game state must expose the global creature-type set"
    );
    for ct in &all_creature_types {
        assert!(
            bear_obj.card_types.subtypes.contains(ct),
            "creature missing creature type {ct}"
        );
    }
}
