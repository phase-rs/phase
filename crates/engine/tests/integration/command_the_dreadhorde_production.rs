//! F10 (PR 9920): production proof for the newly admitted
//! damage-and-delivery class — Command the Dreadhorde casts through
//! GameScenario/GameRunner with a creature and a planeswalker graveyard
//! target, dealing summed-mana-value damage to the caster BEFORE delivering
//! both cards to the battlefield (CR 608.2c), plus empty- and illegal-target
//! controls. The AST-only shape test stays in
//! `song_of_inspiration_target_set_aggregate.rs`; these tests execute the
//! parsed spell.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COMMAND: &str = "Choose any number of target creature and/or planeswalker cards in graveyards. Command the Dreadhorde deals damage to you equal to the total mana value of those cards. Put them onto the battlefield under your control.";

fn mv_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic,
    }
}

/// Stage Beast (creature, MV 3) in P0's graveyard, Walker (planeswalker,
/// MV 4) buried through the real zone pipeline — no direct
/// planeswalker-to-graveyard builder exists — and Command in hand.
fn command_runner() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let beast = scenario
        .add_creature_to_graveyard(P0, "Beast", 3, 3)
        .with_mana_cost(mv_cost(3))
        .id();
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 4, "+1: You gain 1 life.")
        .with_mana_cost(mv_cost(4))
        .id();
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Command the Dreadhorde", false, COMMAND)
        .id();
    let mut runner = scenario.build();
    let mut burial = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), walker, Zone::Graveyard, &mut burial);
    assert!(
        burial.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Graveyard,
                ..
            } if *object_id == walker
        )),
        "NON-VACUITY: the walker must be buried battlefield→graveyard, got {burial:?}"
    );
    (runner, beast, walker, command)
}

fn caster_life(outcome: &engine::game::scenario::Outcome) -> i32 {
    outcome
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("P0 exists")
        .life
}

/// The damage-and-delivery path end to end: 3 + 4 = 7 damage drops P0 to
/// 13, then both targets enter the battlefield under P0's control — and
/// the damage event precedes both arrivals (CR 608.2c: instructions in
/// the order written).
#[test]
fn command_dreadhorde_damages_then_delivers() {
    let (mut runner, beast, walker, command) = command_runner();
    let outcome = runner
        .cast(command)
        .target_objects(&[beast, walker])
        .commit()
        .resolve();
    assert_eq!(
        caster_life(&outcome),
        13,
        "Beast (MV 3) + Walker (MV 4) must deal 7 to the 20-life caster"
    );
    outcome.assert_zone(&[beast, walker], Zone::Battlefield);
    for id in [beast, walker] {
        assert_eq!(
            outcome.state().objects[&id].controller,
            P0,
            "delivery is under the caster's control"
        );
    }
    let events = outcome.events();
    let damage_at = events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::DamageDealt {
                    amount: 7,
                    is_combat: false,
                    ..
                }
            )
        })
        .expect("Command must deal exactly 7 noncombat damage");
    for id in [beast, walker] {
        let arrival_at = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    GameEvent::ZoneChanged {
                        object_id,
                        to: Zone::Battlefield,
                        ..
                    } if *object_id == id
                )
            })
            .expect("each target must arrive on the battlefield");
        assert!(
            damage_at < arrival_at,
            "CR 608.2c: the damage event must precede the battlefield delivery"
        );
    }
}

/// Empty control ("any number" includes zero): no damage, nothing moves.
#[test]
fn command_dreadhorde_empty_cast_deals_nothing_and_moves_nothing() {
    let (mut runner, beast, walker, command) = command_runner();
    let outcome = runner.cast(command).target_objects(&[]).commit().resolve();
    assert_eq!(
        caster_life(&outcome),
        20,
        "a targetless Command must deal no damage"
    );
    outcome.assert_zone(&[beast, walker], Zone::Graveyard);
}

/// Illegal-target control (CR 608.2b): Beast is exiled mid-stack through
/// the real pipeline, so only the Walker's MV 4 is dealt and only the
/// Walker is delivered.
#[test]
fn command_dreadhorde_illegal_target_drops_from_damage_and_delivery() {
    let (mut runner, beast, walker, command) = command_runner();
    let mut committed = runner
        .cast(command)
        .target_objects(&[beast, walker])
        .commit();
    let mut exile_events = Vec::new();
    engine::game::zones::move_to_zone(committed.state_mut(), beast, Zone::Exile, &mut exile_events);
    assert!(
        exile_events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Graveyard),
                to: Zone::Exile,
                ..
            } if *object_id == beast
        )),
        "NON-VACUITY: Beast must leave the graveyard mid-stack, got {exile_events:?}"
    );
    let outcome = committed.resolve();
    assert_eq!(
        caster_life(&outcome),
        16,
        "only the surviving Walker (MV 4) must count toward damage"
    );
    outcome.assert_zone(&[walker], Zone::Battlefield);
    outcome.assert_zone(&[beast], Zone::Exile);
}
