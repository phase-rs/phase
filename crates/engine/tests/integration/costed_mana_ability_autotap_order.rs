//! Auto-tap must make an already-selected free mana source available before
//! resolving a selected mana ability that has a mana sub-cost.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, ManaChoice, WaitingFor};
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
/// The cost is `{B}{B}` so only Mire's filter ability pays it; for `{1}{B}`
/// auto-pay uses Mire's plain `{T}: Add {C}` instead
/// (`mana_autotap_drawback_free_rows`).
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
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
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

const MIRARI_ECHO_ORACLE: &str = "Creatures you control get +1/+1.\nWhenever you tap a land for mana, add one mana of any type that land produced.";
const SIMIC_CHAMBER_ORACLE: &str =
    "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {G}{U}.";

fn pool_count(runner: &GameRunner, color: ManaType) -> usize {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .count_color(color)
}

/// CR 605.4a + CR 608.2d: a nested mana sub-cost auto-tap that taps a
/// multi-type land pauses for the echo choice through the ultimate mana-ability
/// root settlement (the nested path carries `resume=None` by construction, so
/// the inline pre-spend gate skips it by design). Mirari's Wake plus Simic
/// Growth Chamber ({G}{U}) plus Darkwater Catacombs ("{1}, {T}: Add {U}{B}"),
/// empty pool: activating Catacombs auto-taps the chamber for its {1}
/// sub-cost, the echo pauses, answering funds the activation, and Catacombs'
/// own {U}{B} production triggers a second (production) echo that the
/// activation settles through.
#[test]
fn nested_sub_cost_echo_choice_precedes_catacombs_production() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ECHO_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", SIMIC_CHAMBER_ORACLE);
    let catacombs = scenario
        .add_land_from_oracle(P0, "Darkwater Catacombs", DARKWATER_CATACOMBS_ORACLE)
        .id();
    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&catacombs]
        .abilities
        .iter()
        .position(|def| matches!(*def.effect, Effect::Mana { .. }))
        .expect("Catacombs must have its mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: catacombs,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("nested sub-cost tap must pause for echo choice");
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor { player, .. } => {
            assert_eq!(*player, P0, "P0 activates, so P0 chooses");
        }
        other => panic!("expected a SingleColor echo choice, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering Green must continue the activation");
    // Catacombs is itself a land: its own {U}{B} production triggers Mirari's
    // Wake again, so the activation only settles after that second echo is
    // chosen.
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor { player, .. } => {
            assert_eq!(*player, P0, "P0 activates, so P0 chooses");
        }
        other => panic!("expected the production echo choice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("answering the production echo must complete the activation");
    // Base {G}{U} + first bonus {G}, minus {1} for the sub-cost, plus
    // Catacombs' {U}{B} production, plus the production echo's bonus: five mana
    // with the produced colors present.
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        5,
        "base plus two bonuses minus sub-cost plus production"
    );
    assert!(
        pool_count(&runner, ManaType::Blue) >= 1,
        "Catacombs must have produced Blue"
    );
    assert!(
        pool_count(&runner, ManaType::Black) >= 1,
        "Catacombs must have produced Black"
    );
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion returns to Priority, got {:?}",
        runner.state().waiting_for
    );
}
