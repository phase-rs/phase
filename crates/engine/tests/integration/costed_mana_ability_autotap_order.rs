//! Auto-tap must make an already-selected free mana source available before
//! resolving a selected mana ability that has a mana sub-cost.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;

const PLUNGE_INTO_DARKNESS_ORACLE: &str = "Choose one —\n• Sacrifice any number of creatures. You gain 3 life for each creature sacrificed this way.\n• Pay any amount of life, then look at that many cards from the top of your library. Put one of those cards into your hand and exile the rest.\nEntwine {B} (Choose both if you pay the entwine cost.)";
const TWILIGHT_MIRE_ORACLE: &str = "{T}: Add {C}.\n{B/G}, {T}: Add {B}{B}, {B}{G}, or {G}{G}.";
const DARKWATER_CATACOMBS_ORACLE: &str = "{1}, {T}: Add {U}{B}.";

fn begin_plunge_cast(runner: &mut GameRunner, plunge: ObjectId) {
    let card_id = runner.state().objects[&plunge].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: plunge,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Plunge announcement must reach its mode choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { .. }
    ));
}

/// CR 601.2g + CR 605.3b + CR 605.3c: the Swamp's selected mana ability resolves before
/// Twilight Mire's selected `{B/G}`-costed ability, making the Swamp's {B}
/// available for that activation and allowing the single selected mode to cast.
#[test]
fn plunge_into_darkness_one_mode_autotaps_swamp_before_twilight_mire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let twilight_mire = scenario
        .add_land_from_oracle(P0, "Twilight Mire", TWILIGHT_MIRE_ORACLE)
        .id();
    let plunge = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Plunge into Darkness",
            true,
            PLUNGE_INTO_DARKNESS_ORACLE,
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();

    begin_plunge_cast(&mut runner, plunge);
    let selected = runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("one-mode Plunge must cast with Swamp and Twilight Mire");

    let mana_added: Vec<(ObjectId, ManaType)> = selected
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ManaAdded {
                source_id,
                mana_type,
                ..
            } => Some((*source_id, *mana_type)),
            _ => None,
        })
        .collect();
    assert_eq!(
        mana_added,
        vec![
            (swamp, ManaType::Black),
            (twilight_mire, ManaType::Black),
            (twilight_mire, ManaType::Black),
        ],
        "the free Swamp must resolve before Twilight Mire's mana sub-cost"
    );

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(!runner.state().stack.is_empty());
    assert!(runner.state().objects[&swamp].tapped);
    assert!(runner.state().objects[&twilight_mire].tapped);
}

/// CR 601.2g + CR 605.3b + CR 605.3c: the same scheduling class also covers an Island
/// funding Darkwater Catacombs' selected `{1}`-costed mana ability for a
/// `{1}{B}` spell.
#[test]
fn darkwater_catacombs_and_island_autotap_for_one_black() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let island = scenario.add_basic_land(P0, ManaColor::Blue);
    let darkwater = scenario
        .add_land_from_oracle(P0, "Darkwater Catacombs", DARKWATER_CATACOMBS_ORACLE)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Autotap Order Witness", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Darkwater Catacombs plus Island must cast {1}{B}");

    let mana_added: Vec<(ObjectId, ManaType)> = cast
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ManaAdded {
                source_id,
                mana_type,
                ..
            } => Some((*source_id, *mana_type)),
            _ => None,
        })
        .collect();
    assert_eq!(
        mana_added,
        vec![
            (island, ManaType::Blue),
            (darkwater, ManaType::Blue),
            (darkwater, ManaType::Black),
        ],
        "the Island must resolve before Darkwater Catacombs' mana sub-cost"
    );

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(!runner.state().stack.is_empty());
    assert!(runner.state().objects[&island].tapped);
    assert!(runner.state().objects[&darkwater].tapped);
}
