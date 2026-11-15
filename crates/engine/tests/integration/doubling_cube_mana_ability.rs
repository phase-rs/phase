//! CR 605.1a + CR 701.10f + CR 605.3b: Doubling Cube's ability is a mana
//! ability — it has no target, it adds mana ("that player adds an amount of mana
//! of that type equal to the amount they already have"), it is not a loyalty
//! ability and moves no card to or from a library. So it resolves without the
//! stack (CR 605.3b) and can be activated while a cost is being paid
//! (CR 605.3a). It doubles each of the six mana types (CR 106.1b; colorless is
//! a type), and the copies carry none of the originals' restrictions (Doubling
//! Cube ruling). Its `{T}` makes it tapped for mana (CR 106.12).

use crate::support::shared_card_db;
use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DOUBLING_CUBE: &str = "{3}, {T}: Double the amount of each type of unspent mana you have.";

struct Board {
    runner: GameRunner,
    cube: ObjectId,
    battlefield: Vec<ObjectId>,
    hand: Vec<ObjectId>,
}

/// A main-phase board for P0: Doubling Cube (verbatim Oracle text), the named
/// fixture cards on the battlefield and in hand, in order.
fn board(battlefield: &[&str], hand: &[&str]) -> Board {
    let db = shared_card_db().expect("the checked-in fixture contains the cards");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cube = scenario
        .add_artifact_from_oracle(P0, "Doubling Cube", DOUBLING_CUBE)
        .id();
    let mut add = |names: &[&str], zone: Zone| -> Vec<ObjectId> {
        names
            .iter()
            .map(|name| scenario.add_real_card(P0, name, zone, db))
            .collect()
    };
    let battlefield = add(battlefield, Zone::Battlefield);
    let hand = add(hand, Zone::Hand);
    Board {
        runner: scenario.build(),
        cube,
        battlefield,
        hand,
    }
}

/// Submit the engine-offered `TapLandForMana` for `land`'s ability
/// `ability_index`.
fn tap_land_for_mana(runner: &mut GameRunner, land: ObjectId, ability_index: usize) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection }
                if selection.ability_index == Some(ability_index) =>
            {
                Some(selection.clone())
            }
            _ => None,
        })
        .expect("the engine offers the land's mana ability");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tap the land");
}

fn cube_activation(runner: &GameRunner, cube: ObjectId) -> GameAction {
    let ability_index = runner.state().objects[&cube]
        .abilities
        .iter()
        .position(|a| a.kind == AbilityKind::Activated)
        .expect("Doubling Cube's activated ability");
    GameAction::ActivateAbility {
        source_id: cube,
        ability_index,
        payment_mode: CastPaymentMode::Auto,
    }
}

/// The Doubling Cube activation the engine offers now, from the per-object
/// legal-action bucket the client renders Doubling Cube's click from.
fn offered_cube_activation(runner: &GameRunner, cube: ObjectId) -> GameAction {
    let (_, _, grouped) = legal_actions_full(runner.state());
    grouped
        .get(&cube)
        .into_iter()
        .flatten()
        .find(|action| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == cube))
        .cloned()
        .expect("the engine offers Doubling Cube's mana ability")
}

fn pool_units(runner: &GameRunner) -> Vec<ManaUnit> {
    runner.state().players[0].mana_pool.units().collect()
}

fn count(runner: &GameRunner, mana_type: ManaType) -> usize {
    runner.state().players[0].mana_pool.count_color(mana_type)
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

fn cast(runner: &mut GameRunner, spell: ObjectId, payment_mode: CastPaymentMode) -> WaitingFor {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode,
        })
        .expect("the cast begins")
        .waiting_for
}

fn pass_until_stack_empty(runner: &mut GameRunner) {
    while !runner.state().stack.is_empty() {
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
}

/// C1 — CR 605.3b: Doubling Cube resolves without the stack. CR 701.10f: the
/// pool is doubled after the `{3}` is paid: BBBB − 3 = B, doubled → BB.
#[test]
fn doubling_cube_resolves_inline_and_doubles_the_pool_left_after_its_cost() {
    let Board {
        mut runner,
        cube,
        battlefield: swamps,
        ..
    } = board(&["Swamp", "Swamp", "Swamp", "Swamp"], &[]);
    for &swamp in &swamps {
        tap_land_for_mana(&mut runner, swamp, 0);
    }
    assert_eq!(count(&runner, ManaType::Black), 4, "reach: pool is BBBB");

    let activation = offered_cube_activation(&runner, cube);
    let result = runner.act(activation).expect("Doubling Cube activates");

    assert!(
        runner.state().stack.is_empty(),
        "CR 605.3b: a mana ability does not use the stack: {:?}",
        runner.state().stack
    );
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        result.waiting_for
    );
    assert!(tapped(&runner, cube), "Doubling Cube's {{T}} was paid");
    assert_eq!(count(&runner, ManaType::Black), 2, "B doubled to BB");
    assert_eq!(pool_units(&runner).len(), 2, "nothing else is in the pool");
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::ManaAbilityProduced { source_id, produced, .. }
                if *source_id == cube && produced == &[ManaType::Black]
        )),
        "the mana-ability production event names Doubling Cube: {:?}",
        result.events
    );
    // CR 106.12: a `{T}` mana ability taps its source for mana.
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::TappedForMana { source_id, .. } if *source_id == cube
        )),
        "Doubling Cube is tapped for mana"
    );
}

/// C3 — CR 106.1b + Doubling Cube rulings: colorless is a type of mana and is
/// doubled; the copies do not inherit the Eldrazi Temple restriction. The
/// restricted {C}{C} cannot pay Doubling Cube's {3} (it is not an Eldrazi), so
/// the Swamps' BBB does.
#[test]
fn doubling_cube_doubles_restricted_colorless_into_unrestricted_copies() {
    let Board {
        mut runner,
        cube,
        battlefield,
        ..
    } = board(&["Eldrazi Temple", "Swamp", "Swamp", "Swamp"], &[]);
    let (temple, swamps) = (battlefield[0], &battlefield[1..]);
    // Eldrazi Temple's second ability: "{T}: Add {C}{C}. Spend this mana only to
    // cast colorless Eldrazi spells or activate abilities of colorless Eldrazi."
    tap_land_for_mana(&mut runner, temple, 1);
    for &swamp in swamps {
        tap_land_for_mana(&mut runner, swamp, 0);
    }
    assert_eq!(count(&runner, ManaType::Colorless), 2, "reach: Temple's CC");
    assert_eq!(count(&runner, ManaType::Black), 3, "reach: Swamps' BBB");

    let activation = cube_activation(&runner, cube);
    runner.act(activation).expect("Doubling Cube activates");

    let colorless: Vec<ManaUnit> = pool_units(&runner)
        .into_iter()
        .filter(|unit| unit.color == ManaType::Colorless)
        .collect();
    assert_eq!(
        colorless.len(),
        4,
        "CR 106.1b: colorless is doubled: {colorless:?}"
    );
    assert_eq!(
        colorless
            .iter()
            .filter(|unit| !unit.restrictions.is_empty())
            .count(),
        2,
        "the Temple's two units keep their restriction"
    );
    assert_eq!(
        colorless
            .iter()
            .filter(|unit| unit.restrictions.is_empty())
            .count(),
        2,
        "the copies carry no restriction"
    );
    assert_eq!(
        count(&runner, ManaType::Black),
        0,
        "reach: the BBB paid the {{3}}"
    );
    assert_eq!(pool_units(&runner).len(), 4, "nothing else is in the pool");
    assert!(runner.state().stack.is_empty(), "CR 605.3b: no stack");
}

/// C3 sibling — four Eldrazi Temples tapped for their unrestricted `{T}: Add {C}`:
/// CCCC − {3} = C, doubled → two unrestricted C.
#[test]
fn doubling_cube_doubles_unrestricted_colorless() {
    let Board {
        mut runner,
        cube,
        battlefield: temples,
        ..
    } = board(
        &[
            "Eldrazi Temple",
            "Eldrazi Temple",
            "Eldrazi Temple",
            "Eldrazi Temple",
        ],
        &[],
    );
    for &temple in &temples {
        tap_land_for_mana(&mut runner, temple, 0);
    }
    assert_eq!(count(&runner, ManaType::Colorless), 4, "reach: CCCC");

    let activation = cube_activation(&runner, cube);
    runner.act(activation).expect("Doubling Cube activates");

    let units = pool_units(&runner);
    assert_eq!(
        count(&runner, ManaType::Colorless),
        2,
        "C doubled: {units:?}"
    );
    assert_eq!(units.len(), 2, "{units:?}");
    assert!(units.iter().all(|unit| unit.restrictions.is_empty()));
}

/// C4 — CR 605.3a: Doubling Cube can be activated while paying for a spell.
/// RRRR − {3} = R, doubled → RR; Lightning Bolt then takes one R.
#[test]
fn doubling_cube_activates_inside_a_spells_mana_payment() {
    let Board {
        mut runner,
        cube,
        battlefield: mountains,
        hand,
    } = board(
        &["Mountain", "Mountain", "Mountain", "Mountain"],
        &["Lightning Bolt"],
    );
    let bolt = hand[0];
    for &mountain in &mountains {
        tap_land_for_mana(&mut runner, mountain, 0);
    }
    assert_eq!(count(&runner, ManaType::Red), 4, "reach: pool is RRRR");

    let waiting_for = cast(&mut runner, bolt, CastPaymentMode::Manual);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "{waiting_for:?}"
    );
    let waiting_for = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("P1 is a legal target")
        .waiting_for;
    assert!(
        matches!(waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: Lightning Bolt's payment window: {waiting_for:?}"
    );

    let activation = offered_cube_activation(&runner, cube);
    let waiting_for = runner
        .act(activation)
        .expect("CR 605.3a: a mana ability may be activated while paying")
        .waiting_for;

    assert!(
        matches!(waiting_for, WaitingFor::ManaPayment { .. }),
        "Doubling Cube resolves back into the payment window: {waiting_for:?}"
    );
    assert_eq!(count(&runner, ManaType::Red), 2, "R doubled to RR");
    assert_eq!(pool_units(&runner).len(), 2);
    assert!(tapped(&runner, cube));
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only Lightning Bolt is on the stack"
    );

    runner
        .act(GameAction::PassPriority)
        .expect("pay Lightning Bolt's cost");
    pass_until_stack_empty(&mut runner);
    assert_eq!(runner.life(P1), 17, "Lightning Bolt dealt 3");
    assert_eq!(
        count(&runner, ManaType::Red),
        1,
        "Lightning Bolt took one R"
    );
    assert_eq!(pool_units(&runner).len(), 1);
}

/// C5 — auto-pay never activates Doubling Cube on its own: its yield is the
/// pool already present, not a static production.
#[test]
fn auto_pay_never_activates_doubling_cube() {
    let Board {
        mut runner,
        cube,
        battlefield,
        hand,
    } = board(&["Forest", "Swamp", "Swamp", "Swamp"], &["Grizzly Bears"]);
    let bears = hand[0];
    for &swamp in &battlefield[1..] {
        tap_land_for_mana(&mut runner, swamp, 0);
    }
    assert_eq!(count(&runner, ManaType::Black), 3, "reach: pool is BBB");

    cast(&mut runner, bears, CastPaymentMode::Auto);
    pass_until_stack_empty(&mut runner);

    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "Grizzly Bears were cast and resolved"
    );
    assert!(
        !tapped(&runner, cube),
        "auto-pay left Doubling Cube untapped"
    );
    assert_eq!(count(&runner, ManaType::Black), 2, "one B paid the {{1}}");
    assert_eq!(pool_units(&runner).len(), 2);
}

/// C6 — CR 605.3b + CR 701.10f: the doubling read every unit in the pool, so no
/// land tapped before it can be refunded afterwards; a land tapped after it can.
#[test]
fn doubling_cube_closes_the_land_mana_undo_window() {
    let Board {
        mut runner,
        cube,
        battlefield,
        ..
    } = board(&["Swamp", "Swamp", "Swamp", "Swamp", "Swamp"], &[]);
    let (swamps, fifth) = (&battlefield[..4], battlefield[4]);
    for &swamp in swamps {
        tap_land_for_mana(&mut runner, swamp, 0);
    }
    assert_eq!(
        runner.state().lands_tapped_for_mana.get(&P0),
        Some(&swamps.to_vec()),
        "reach: the undo window is open over all four Swamps"
    );

    let activation = cube_activation(&runner, cube);
    runner.act(activation).expect("Doubling Cube activates");
    assert_eq!(count(&runner, ManaType::Black), 2, "reach: B doubled to BB");

    for &swamp in swamps {
        assert!(
            runner
                .act(GameAction::UntapLandForMana { object_id: swamp })
                .is_err(),
            "a Swamp whose mana the doubling read cannot be refunded"
        );
    }
    assert_eq!(count(&runner, ManaType::Black), 2, "the pool is unchanged");
    assert_eq!(pool_units(&runner).len(), 2);
    assert!(swamps.iter().all(|&swamp| tapped(&runner, swamp)));
    assert!(!runner.state().lands_tapped_for_mana.contains_key(&P0));

    // The window closes at the doubling, not permanently.
    tap_land_for_mana(&mut runner, fifth, 0);
    assert_eq!(count(&runner, ManaType::Black), 3);
    runner
        .act(GameAction::UntapLandForMana { object_id: fifth })
        .expect("a land tapped after the doubling is refundable");
    assert_eq!(count(&runner, ManaType::Black), 2);
    assert!(!tapped(&runner, fifth));
}
