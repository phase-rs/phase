//! Unlicensed Hearse — "the number of cards exiled with it" after a blink.
//!
//! Oracle:
//!   "{T}: Exile up to two target cards from a single graveyard.
//!    Unlicensed Hearse's power and toughness are each equal to the number of
//!    cards exiled with it.
//!    Crew 2"
//!
//! Ruling: "If you activated the crew ability of Unlicensed Hearse before
//! exiling any cards with its first ability, its power and toughness will be
//! 0/0. Unless there is another effect increasing its toughness, it will be put
//! into its owner's graveyard as a state-based action."
//!
//! CR 400.7: a Hearse that is exiled and returned is a new object.
//! CR 607.2a: none of its abilities exiled anything, so no cards are exiled
//! with it.
//! CR 604.3: the characteristic-defining ability reads that count.
//! CR 702.122a + CR 704.5f: crewed, a 0/0 Hearse is put into the graveyard.

use engine::game::players::linked_exile_cards_for_source;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HEARSE_TEXT: &str = "{T}: Exile up to two target cards from a single graveyard.\n\
     Unlicensed Hearse's power and toughness are each equal to the number of cards exiled with \
     it.\nCrew 2";

const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, \
     then return those cards to the battlefield under your control.";

struct HearseBoard {
    runner: GameRunner,
    hearse: ObjectId,
    land: ObjectId,
    pilot: ObjectId,
    flicker: ObjectId,
    graveyard_cards: [ObjectId; 2],
}

/// Hearse, a blank land, a 2/2 pilot, two cards in the opponent's graveyard
/// and a zero-cost Ghostly Flicker in hand. Hearse's `{T}` has already exiled
/// both graveyard cards.
fn hearse_with_two_exiled() -> HearseBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hearse = scenario
        .add_artifact_from_oracle(P0, "Unlicensed Hearse", HEARSE_TEXT)
        .with_subtypes(vec!["Vehicle"])
        .id();
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    let pilot = scenario.add_creature(P0, "Pilot", 2, 2).id();
    let first = scenario.add_spell_to_graveyard(P1, "Grave A", true).id();
    let second = scenario.add_spell_to_graveyard(P1, "Grave B", true).id();
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner
        .activate(hearse, 0)
        .target_objects(&[first, second])
        .resolve();
    for card in [first, second] {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Exile,
            "Hearse's ability exiled the graveyard card"
        );
    }
    HearseBoard {
        runner,
        hearse,
        land,
        pilot,
        flicker,
        graveyard_cards: [first, second],
    }
}

fn linked(state: &engine::types::game_state::GameState, hearse: ObjectId) -> Vec<ObjectId> {
    linked_exile_cards_for_source(state, hearse)
        .iter()
        .map(|snapshot| snapshot.exiled_id)
        .collect()
}

/// CR 702.122a: crew Hearse with the pilot (two-step `CrewVehicle`) and let
/// the crew ability resolve.
fn crew(runner: &mut GameRunner, hearse: ObjectId, pilot: ObjectId) {
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: hearse,
            creature_ids: vec![],
        })
        .expect("enter crew mode");
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: hearse,
            creature_ids: vec![pilot],
        })
        .expect("crew with the 2/2 pilot");
    runner.advance_until_stack_empty();
}

/// CR 400.7 + CR 607.2a + CR 604.3 + CR 704.5f: after Ghostly Flicker, the
/// returned Hearse has no cards exiled with it; crewed, it is a 0/0 and dies.
#[test]
fn flickered_hearse_has_no_cards_exiled_with_it() {
    let HearseBoard {
        mut runner,
        hearse,
        land,
        pilot,
        flicker,
        graveyard_cards,
    } = hearse_with_two_exiled();
    let incarnation_before = runner.state().objects[&hearse].incarnation;
    let phase_before = runner.state().phase;

    runner
        .cast(flicker)
        .target_objects(&[hearse, land])
        .resolve();

    let state = runner.state();
    assert_eq!(state.objects[&hearse].zone, Zone::Battlefield);
    assert_ne!(
        state.objects[&hearse].incarnation, incarnation_before,
        "Hearse came back as a new object"
    );
    assert_eq!(state.phase, phase_before, "still the same step");
    for card in graveyard_cards {
        assert_eq!(state.objects[&card].zone, Zone::Exile);
    }
    assert!(
        linked(state, hearse).is_empty(),
        "no cards are exiled with the returned Hearse; linked = {:?}",
        linked(state, hearse)
    );

    crew(&mut runner, hearse, pilot);
    assert_eq!(
        runner.state().objects[&hearse].zone,
        Zone::Graveyard,
        "a crewed 0/0 Hearse is put into its owner's graveyard"
    );
}

/// CR 604.3 + CR 702.122a: without the blink, the crewed Hearse is a 2/2.
#[test]
fn crewed_hearse_counts_the_cards_exiled_with_it() {
    let HearseBoard {
        mut runner,
        hearse,
        pilot,
        graveyard_cards,
        ..
    } = hearse_with_two_exiled();
    assert_eq!(linked(runner.state(), hearse).len(), graveyard_cards.len());

    crew(&mut runner, hearse, pilot);
    let obj = &runner.state().objects[&hearse];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.card_types.core_types.contains(&CoreType::Creature));
    assert_eq!((obj.power, obj.toughness), (Some(2), Some(2)));
}
