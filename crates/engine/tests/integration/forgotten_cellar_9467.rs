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
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FORGOTTEN_CELLAR: &str = "When you unlock this door, you may cast spells from your graveyard this turn, and if a card would be put into your graveyard from anywhere this turn, exile it instead.\n(You may cast either half. That door unlocks on the battlefield. As a sorcery, you may pay the mana cost of a locked door to unlock it.)";

const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

const MIZZIXS_MASTERY: &str = "Exile target card that's an instant or sorcery from your graveyard. For each card exiled this way, copy it, and you may cast the copy without paying its mana cost. Exile Mizzix's Mastery.\nOverload {5}{R}{R}{R} (You may cast this spell for its overload cost. If you do, change \"target\" in its text to \"each.\")";

const CHAPLAINS_BLESSING: &str = "You gain 5 life.";

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
    // The red mana makes the Bolt's {R} payable before the unlock, so only the
    // missing graveyard permission can keep it uncastable.
    let mut pool = mana(ManaType::Green, 5);
    pool.extend(mana(ManaType::Red, 1));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    assert!(
        !can_cast_object_now(runner.state(), P0, bolt),
        "nothing is castable from the graveyard before the door is unlocked"
    );
    // The unlock pays exactly the door's cost; set the red mana aside.
    assert!(runner.state_mut().players[0]
        .mana_pool
        .spend(ManaType::Red)
        .is_some());

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

/// Pass priority until a non-priority prompt opens or the stack is empty,
/// collecting every engine event on the way.
fn pass_until_prompt(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                events.extend(result.events);
            }
            _ => return,
        }
    }
    panic!("the stack did not settle: {:?}", runner.state().waiting_for);
}

/// CR 108.2 + CR 707.12 + CR 704.5e: "if a card would be put into your
/// graveyard" does not cover a cast copy of a card. A copy of Chaplain's Blessing cast
/// through Mizzix's Mastery is put into P0's graveyard as it resolves
/// (CR 608.2n) and then ceases to exist; a Lightning Bolt card headed to the
/// same graveyard is exiled instead.
#[test]
fn unlocked_cellar_lets_a_cast_spell_copy_reach_the_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cellar = add_cellar(&mut scenario);
    let blessing = scenario
        .add_spell_to_graveyard(P0, "Chaplain's Blessing", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .from_oracle_text(CHAPLAINS_BLESSING)
        .id();
    let mizzix = scenario
        .add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        })
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .id();
    let bolt = add_bolt(&mut scenario, Zone::Hand);
    scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
    let mut runner = scenario.build();

    unlock(&mut runner, cellar);
    add_mana(&mut runner, ManaType::Red, 4);
    let life_before = runner.life(P0);

    let mut events = Vec::new();
    let card_id = runner.state().objects[&mizzix].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: mizzix,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Mizzix's Mastery");
    events.extend(result.events);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        let result = runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(blessing)),
            })
            .expect("target Chaplain's Blessing");
        events.extend(result.events);
    }
    pass_until_prompt(&mut runner, &mut events);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseFromZoneChoice { .. }
        ),
        "Mizzix's Mastery offers the copy: {:?}",
        runner.state().waiting_for
    );
    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![blessing],
        })
        .expect("cast the copy");
    events.extend(result.events);
    pass_until_prompt(&mut runner, &mut events);

    assert_eq!(runner.life(P0), life_before + 5, "the cast copy resolved");
    let copy_moves: Vec<Zone> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Stack),
                to,
                ..
            } if *object_id != mizzix => Some(*to),
            _ => None,
        })
        .collect();
    assert_eq!(
        copy_moves,
        vec![Zone::Graveyard],
        "the spell copy leaves the stack for P0's graveyard, not exile"
    );
    assert!(
        !runner.state().players[0].graveyard.contains(&blessing),
        "the Blessing card itself was exiled by Mizzix's Mastery"
    );
    assert!(
        runner.state().players[0].graveyard.is_empty(),
        "CR 704.5e: the copy ceased to exist: {:?}",
        runner.state().players[0].graveyard
    );

    add_mana(&mut runner, ManaType::Red, 1);
    runner.cast(bolt).target_player(P1).resolve();
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Exile,
        "a card headed to the same graveyard is still exiled instead"
    );
}
