//! Issue #9467 — Forgotten Cellar: "When you unlock this door, you may cast
//! spells from your graveyard this turn, and if a card would be put into your
//! graveyard from anywhere this turn, exile it instead."
//!
//! Cards are built from their verbatim Oracle text
//! (`client/public/card-data.json`); CI has no card database. The scenario
//! puts the Cellar straight onto the battlefield as a one-door Room, skipping
//! the battlefield entry that stamps a Room's doors
//! (`GameObject::install_room_door_text`), so its trigger names no door and
//! matches any unlock. The parser this pins sets no door either way.

use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::game_object::RoomDoor;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FORGOTTEN_CELLAR: &str = "When you unlock this door, you may cast spells from your graveyard this turn, and if a card would be put into your graveyard from anywhere this turn, exile it instead.\n(You may cast either half. That door unlocks on the battlefield. As a sorcery, you may pay the mana cost of a locked door to unlock it.)";

const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

fn mana(kind: ManaType, count: usize) -> Vec<ManaUnit> {
    let dummy = ObjectId(0);
    (0..count)
        .map(|_| ManaUnit::new(kind, dummy, false, vec![]))
        .collect()
}

fn add_mana(runner: &mut GameRunner, kind: ManaType, count: usize) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for unit in mana(kind, count) {
        pool.add(unit);
    }
}

fn red() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    }
}

/// Forgotten Cellar, locked, on P0's battlefield; its door costs {3}{G}{G}.
fn add_cellar(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Forgotten Cellar", FORGOTTEN_CELLAR)
        .with_subtypes(vec!["Room"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 3,
        })
        .id()
}

fn add_bolt(scenario: &mut GameScenario, zone: Zone) -> ObjectId {
    let mut builder = match zone {
        Zone::Hand => scenario.add_spell_to_hand(P0, "Lightning Bolt", true),
        _ => scenario.add_spell_to_graveyard(P0, "Lightning Bolt", true),
    };
    builder
        .with_mana_cost(red())
        .from_oracle_text(LIGHTNING_BOLT);
    builder.id()
}

/// CR 709.5e + CR 709.5h: pay the door's cost as the special action and let
/// its "when you unlock this door" trigger resolve. The pool must hold exactly
/// the door's cost.
fn unlock(runner: &mut GameRunner, cellar: ObjectId) {
    runner
        .act(GameAction::UnlockRoomDoor {
            object_id: cellar,
            door: RoomDoor::Left,
        })
        .expect("unlock Forgotten Cellar");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "CR 709.5e: the door's {{3}}{{G}}{{G}} is paid"
    );
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            // CR 601.3: the "may" is the later cast; the trigger asks nothing.
            WaitingFor::OptionalEffectChoice { .. } => {
                panic!("the unlock trigger must not ask whether to apply it")
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            _ => runner.resolve_top(),
        }
    }
    panic!(
        "the unlock trigger did not resolve: {:?}",
        runner.state().waiting_for
    );
}

/// CR 601.3 + CR 611.2c: after the trigger resolves, every spell card in P0's
/// graveyard is castable this turn, any number of them; nothing is castable
/// from it before.
#[test]
fn unlocking_the_cellar_lets_you_cast_spells_from_your_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cellar = add_cellar(&mut scenario);
    let bolt = add_bolt(&mut scenario, Zone::Graveyard);
    let bear = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
    let mut runner = scenario.build();

    assert!(
        !can_cast_object_now(runner.state(), P0, bolt),
        "nothing is castable from the graveyard before the door is unlocked"
    );

    unlock(&mut runner, cellar);
    add_mana(&mut runner, ManaType::Green, 2);
    add_mana(&mut runner, ManaType::Red, 1);

    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&bolt) && castable.contains(&bear),
        "both spell cards in P0's graveyard are castable: {castable:?}"
    );

    runner.cast(bolt).target_player(P1).resolve();
    assert_eq!(runner.life(P1), 17, "the graveyard Lightning Bolt resolved");
    assert!(
        can_cast_object_now(runner.state(), P0, bear),
        "\"spells\" is any number of casts: the bear is still castable"
    );
}

/// CR 614.1a: "if a card would be put into your graveyard from anywhere this
/// turn, exile it instead" replaces every card going to P0's graveyard this
/// turn: the spell cast from hand and the creature it kills. A card going to
/// P1's graveyard is not replaced.
#[test]
fn unlocking_the_cellar_exiles_cards_put_into_your_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cellar = add_cellar(&mut scenario);
    let bolt = add_bolt(&mut scenario, Zone::Hand);
    let second_bolt = scenario
        .add_spell_to_hand(P0, "Second Bolt", true)
        .with_mana_cost(red())
        .from_oracle_text(LIGHTNING_BOLT)
        .id();
    let own_bear = scenario.add_creature(P0, "Own Bear", 2, 2).id();
    let opponents_bear = scenario.add_creature(P1, "Opponent's Bear", 2, 2).id();
    scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
    let mut runner = scenario.build();

    unlock(&mut runner, cellar);
    add_mana(&mut runner, ManaType::Red, 2);

    runner.cast(bolt).target_object(own_bear).resolve();
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Exile,
        "the spell from hand is exiled instead of going to P0's graveyard"
    );
    assert_eq!(
        runner.state().objects[&own_bear].zone,
        Zone::Exile,
        "P0's creature that dies is exiled (\"from anywhere\")"
    );

    runner
        .cast(second_bolt)
        .target_object(opponents_bear)
        .resolve();
    assert_eq!(
        runner.state().objects[&opponents_bear].zone,
        Zone::Graveyard,
        "\"your graveyard\" is P0's: P1's creature goes to P1's graveyard"
    );
}
