//! CR 732.2a + CR 608.1: an offer's confirmed period performs its board's base period, compared on
//! the printed identities of the abilities and spells one replayed cycle resolves.

use engine::game::period_confirm::performed_for_tests;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, LoopDetectionMode, WaitingFor};

use crate::support::shared_card_db;

/// Whether `performed` is `base` begun at some resolution of it.
pub(crate) fn same_up_to_rotation(performed: &[String], base: &[&str]) -> bool {
    performed.len() == base.len()
        && (0..base.len().max(1)).any(|start| {
            performed.iter().map(String::as_str).eq(base
                .iter()
                .cycle()
                .skip(start)
                .take(base.len())
                .copied())
        })
}

/// Board A's base period, in the order the cards resolve it from Abdel Adrian's enters trigger.
pub(crate) const BOARD_A_PERIOD: [&str; 5] = [
    "Abdel Adrian, Gorion's Ward",
    "Animate Dead",
    "Altar of the Brood",
    "Animate Dead",
    "Altar of the Brood",
];

#[test]
fn same_up_to_rotation_accepts_a_rotation_and_rejects_a_swap() {
    let rotated: Vec<String> = [
        "Animate Dead",
        "Altar of the Brood",
        "Abdel Adrian, Gorion's Ward",
        "Animate Dead",
        "Altar of the Brood",
    ]
    .map(String::from)
    .to_vec();
    assert!(same_up_to_rotation(&rotated, &BOARD_A_PERIOD));
    let mut swapped = rotated.clone();
    swapped.swap(0, 1);
    assert!(!same_up_to_rotation(&swapped, &BOARD_A_PERIOD));
    assert!(!same_up_to_rotation(&rotated[1..], &BOARD_A_PERIOD));
}

/// The printed name of each play's source in the offered period, in order.
fn offered_plays(state: &GameState) -> Vec<String> {
    let wire = serde_json::to_value(&state.waiting_for).expect("the offer serializes");
    wire["data"]["period"]["items"]
        .as_array()
        .expect("the offer carries its period")
        .iter()
        .filter(|item| !item["play"].is_null())
        .map(|item| {
            let action: GameAction =
                serde_json::from_value(item["action"].clone()).expect("an action");
            let source = match action {
                GameAction::CastSpell { object_id, .. } => object_id,
                GameAction::ActivateAbility { source_id, .. } => source_id,
                other => panic!("a play the period holds: {other:?}"),
            };
            state.objects[&source].name.clone()
        })
        .collect()
}

/// The offered period's replay resolves the triggered abilities of `base`, up to rotation.
fn assert_performs(state: &GameState, base: &[&str]) {
    let performed = performed_for_tests(state)
        .expect("reach: an offer")
        .expect("the offered period replays");
    assert!(!performed.is_empty(), "reach: the replay resolves triggers");
    assert!(same_up_to_rotation(&performed, base), "{performed:?}");
}

fn assert_plays_named(state: &GameState, base: &[&str]) {
    let plays = offered_plays(state);
    assert!(same_up_to_rotation(&plays, base), "{plays:?}");
}

/// A period whose replay resolves no triggered ability is compared on its plays.
fn assert_plays(state: &GameState, base: &[&str]) {
    assert_eq!(performed_for_tests(state), Some(Ok(Vec::new())));
    assert_plays_named(state, base);
}

/// Kilo, Apogee Mind ("Whenever Kilo becomes tapped, proliferate.") tapped by Relic of Legends
/// and untapped by Freed from the Real.
#[test]
fn kilo_performs_its_base_period() {
    use crate::kilo_live_offer_from_real_dump::{
        drive_one_live_cycle, load_migrated_dump, FIXTURE_IDS,
    };
    let mut state = load_migrated_dump();
    drive_one_live_cycle(&mut state, &FIXTURE_IDS);
    assert_performs(&state, &["Kilo, Apogee Mind"]);
    assert_plays_named(&state, &["Relic of Legends", "Freed from the Real"]);
}

/// Basalt Monolith ("{T}: Add {C}{C}{C}. {3}: Untap this artifact.") under Power Artifact.
#[test]
fn basalt_performs_its_base_period() {
    use crate::loop_shortcut_mana_engine::{
        drive_one_period, mana_ability_index, setup, untap_ability_index,
    };
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana = mana_ability_index(rig.runner.state(), rig.basalt).expect("mana ability");
    let untap = untap_ability_index(rig.runner.state(), rig.basalt).expect("untap ability");
    for _ in 0..3 {
        if matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ) {
            break;
        }
        drive_one_period(&mut rig, mana, untap);
    }
    assert_plays(rig.runner.state(), &["Basalt Monolith", "Basalt Monolith"]);
}

/// Presence of Gond's granted "{T}: Create a 1/1 green Elf Warrior creature token." untapped by
/// Intruder Alarm ("Whenever a creature enters, untap all creatures.").
#[test]
fn gond_performs_its_base_period() {
    use crate::loop_shortcut_activation::{activate_and_drive, setup, token_ability_index};
    let Some(db) = shared_card_db() else { return };
    let mut canary = setup(true, true, LoopDetectionMode::Interactive, db);
    let index = token_ability_index(canary.runner.state(), canary.host).expect("Gond's grant");
    activate_and_drive(&mut canary.runner, canary.host, index);
    assert_performs(canary.runner.state(), &["Intruder Alarm"]);
}

/// Sprout Swarm ("Convoke", "Buyback {3}", "Create a 1/1 green Saproling creature token.") with
/// Witherbloom's affinity.
#[test]
fn sprout_swarm_performs_its_base_period() {
    let (mut runner, sprout, fodder) = crate::loop_shortcut::sprout_swarm_scenario(4);
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    assert_plays(outcome.state(), &["Sprout Swarm"]);
}

/// The Sprout Swarm board beside Altar of the Brood ("Whenever another permanent you control
/// enters, each opponent mills a card.").
#[test]
fn sprout_swarm_altar_of_the_brood_performs_its_base_period() {
    use crate::wba_loop_firewall_interposition::{mill_base, offer_state};
    let state = offer_state(mill_base());
    assert_performs(&state, &["Altar of the Brood"]);
    assert_plays_named(&state, &["Sprout Swarm"]);
}

/// Food Chain ("Exile a creature you control: Add X mana of any one color, … Spend this mana only
/// to cast creature spells.") recasting each Board C creature from exile.
#[test]
fn food_chain_performs_its_base_period() {
    use crate::food_chain_board::BoardCMember;
    for (member, creature) in [
        (BoardCMember::C1EternalScourge, "Eternal Scourge"),
        (BoardCMember::C2SqueeTheImmortal, "Squee, the Immortal"),
    ] {
        let Some(board) = crate::period_confirm_rows::board_c_offered(member) else {
            return;
        };
        assert_plays(board.runner.state(), &["Food Chain", creature]);
    }
}

/// Grand Architect tapping the recolored Pili-Pala for {C}{C}, and Pili-Pala's "{2}, {Q}".
#[test]
fn grand_architect_pili_pala_performs_its_period() {
    let Some(db) = shared_card_db() else { return };
    let (runner, _, offered) = crate::period_confirm_rows::grand_architect_pili_pala(db, true);
    assert!(offered, "reach: the recolored board offers");
    assert_plays(runner.state(), &["Grand Architect", "Pili-Pala"]);
}
