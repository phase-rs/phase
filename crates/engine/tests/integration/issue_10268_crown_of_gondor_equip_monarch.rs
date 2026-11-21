//! Issue #10268 — Crown of Gondor's equip discount ("if you're the monarch").
//!
//! Oracle (verbatim from Scryfall, https://scryfall.com/card/ltc/75/crown-of-gondor):
//!   Equipped creature gets +1/+1 for each creature you control.
//!   When a legendary creature you control enters, if there is no monarch, you become the monarch.
//!   Equip {4}. This ability costs {3} less to activate if you're the monarch.
//!
//! CR 725.1: the monarch is a player designation. CR 601.2f + CR 602.2b: a
//! self cost reduction is folded into an activated ability's total cost at
//! cost-determination time, so the equip must cost {1} while its ACTIVATOR is
//! the monarch and {4} otherwise.
//!
//! DISCRIMINATING: `can_activate_ability_now` (the production affordability
//! gate) accepts the equip with exactly {1} only while the activator holds the
//! designation, and the full announce/pay/resolve pipeline actually pays that
//! reduced cost. Reverting the parser or the condition evaluator leaves the
//! equip at a flat {4}, so every assertion below fails.

use engine::game::casting::{activated_ability_definitions, can_activate_ability_now};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::AbilityTag;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P1: PlayerId = PlayerId(1);

const CROWN_OF_GONDOR: &str = "Equipped creature gets +1/+1 for each creature you control.\n\
When a legendary creature you control enters, if there is no monarch, you become the monarch.\n\
Equip {4}. This ability costs {3} less to activate if you're the monarch.";

/// Crown of Gondor on P0's battlefield as an artifact, one creature to equip,
/// and exactly `mana` colorless mana in P0's pool (the driver does not model
/// auto-tapping sources).
fn crown_board(mana: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crown = {
        let mut builder = scenario.add_creature(P0, "Crown of Gondor", 0, 0);
        builder.from_oracle_text(CROWN_OF_GONDOR).as_artifact();
        builder.id()
    };
    let host = scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();
    // CR 301.5: the card is an Equipment (the scenario builders do not read a
    // card's type line from Oracle text), and the CR 701.3b attach gate requires
    // the attachment to carry the subtype.
    {
        let obj = runner.state_mut().objects.get_mut(&crown).unwrap();
        obj.card_types.subtypes.push("Equipment".to_string());
        obj.base_card_types.subtypes.push("Equipment".to_string());
    }
    for _ in 0..mana {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
    }
    (runner, crown, host)
}

fn mana_left(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].mana_pool.total()
}

/// The runtime index of the card's equip ability — the only `AbilityTag::Equip`
/// entry `GameAction::ActivateAbility` may select.
fn equip_ability_index(runner: &GameRunner, crown: ObjectId) -> usize {
    let abilities = activated_ability_definitions(runner.state(), crown);
    let indices: Vec<usize> = abilities
        .iter()
        .filter(|(_, ability)| ability.ability_tag == Some(AbilityTag::Equip))
        .map(|(index, _)| *index)
        .collect();
    assert_eq!(
        indices.len(),
        1,
        "Crown of Gondor prints exactly one equip ability, got {abilities:#?}"
    );
    indices[0]
}

/// Activate the equip through the production `ActivateAbility` path and drive
/// it to resolution: announce, choose the host, pay from the pool, resolve.
fn activate_and_resolve_equip(runner: &mut GameRunner, crown: ObjectId, host: ObjectId) {
    let index = equip_ability_index(runner, crown);
    let outcome = runner.activate(crown, index).target_object(host).resolve();
    assert_eq!(
        outcome.state().objects[&crown].attached_to,
        Some(host.into()),
        "CR 702.6a: the equip ability attaches the Equipment to the host"
    );
    assert!(
        outcome.state().objects[&host].attachments.contains(&crown),
        "the equipped creature carries the Equipment"
    );
}

/// CR 725.1 + CR 601.2f + CR 602.2b: {4} minus {3} is {1}, so exactly one mana
/// must afford the equip while its activator is the monarch — and that one mana
/// is what the activation actually pays.
#[test]
fn crown_of_gondor_equip_costs_one_while_the_activator_is_the_monarch() {
    let (mut runner, crown, host) = crown_board(1);
    runner.state_mut().monarch = Some(P0);
    let index = equip_ability_index(&runner, crown);
    assert!(
        can_activate_ability_now(runner.state(), P0, crown, index),
        "{{1}} must afford the {{4}} equip discounted by {{3}}"
    );

    activate_and_resolve_equip(&mut runner, crown, host);
    assert_eq!(
        mana_left(&runner),
        0,
        "the discounted {{1}} equip cost was paid"
    );
}

/// CR 601.2f: without the designation the equip keeps its printed {4} cost:
/// {3} is unaffordable, exactly {4} is affordable, and all {4} is paid.
#[test]
fn crown_of_gondor_equip_costs_full_price_without_the_designation() {
    let (runner, crown, _host) = crown_board(3);
    assert_eq!(runner.state().monarch, None, "precondition: no monarch");
    let index = equip_ability_index(&runner, crown);
    assert!(
        !can_activate_ability_now(runner.state(), P0, crown, index),
        "{{3}} must not afford the undiscounted {{4}} equip"
    );

    let (mut runner, crown, host) = crown_board(4);
    let index = equip_ability_index(&runner, crown);
    assert!(
        can_activate_ability_now(runner.state(), P0, crown, index),
        "{{4}} must afford the undiscounted equip"
    );
    activate_and_resolve_equip(&mut runner, crown, host);
    assert_eq!(mana_left(&runner), 0, "the full {{4}} equip cost was paid");
}

/// CR 109.5 + CR 725.1: the leaf reads the ACTIVATING player's designation, so
/// an opponent holding the monarch must not discount P0's equip.
#[test]
fn crown_of_gondor_discount_requires_the_activating_player_to_be_the_monarch() {
    let (mut runner, crown, _host) = crown_board(1);
    runner.state_mut().monarch = Some(P1);
    let index = equip_ability_index(&runner, crown);
    assert!(
        !can_activate_ability_now(runner.state(), P0, crown, index),
        "another player's designation must not discount P0's equip"
    );
}
