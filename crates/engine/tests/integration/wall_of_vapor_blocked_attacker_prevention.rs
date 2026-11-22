//! Wall of Vapor — runtime witness for the source-scoped prevention clause
//! "Prevent all damage that would be dealt to this creature by creatures it's
//! blocking."
//!
//! CR 509.1a + CR 509.1g: the defending player's chosen creature becomes a
//!   blocking creature blocking the attacker assigned to it.
//! CR 510.1d: a blocking creature assigns combat damage to the creature it is
//!   blocking — the relation the shield keys on.
//! CR 615.1a + CR 615.2 + CR 609.7c: "prevent" creates a prevention effect that
//!   applies to damage from a source with the stated properties, rechecked when
//!   the damage would be dealt.
//! CR 120.3a: damage dealt to a player causes that player to lose that much life.
//!
//! Verbatim printed Oracle text (MTGJSON AtomicCards):
//! "Defender (This creature can't attack.)
//! Prevent all damage that would be dealt to this creature by creatures it's
//! blocking."

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn db() -> &'static CardDatabase {
    load_db().expect("the committed integration card fixture is present")
}

fn red_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, Vec::new()))
        .collect()
}

/// Pass priority (or drain a trigger-ordering prompt) until `stop` holds.
fn drive_to(runner: &mut GameRunner, stop: impl Fn(&GameRunner) -> bool) {
    for _ in 0..64 {
        if stop(runner) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers");
            }
            other => panic!("unexpected prompt while advancing: {other:?}"),
        }
    }
    panic!("the game never reached the expected step");
}

fn advance_to_declare_attackers(runner: &mut GameRunner) {
    drive_to(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
}

fn advance_to_declare_blockers(runner: &mut GameRunner) {
    drive_to(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
    });
}

fn to_priority(runner: &mut GameRunner) {
    drive_to(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
}

/// Make P1 the active player at its precombat main, so its creatures can be
/// declared as attackers (CR 508.1).
fn begin_p1_attack_step(runner: &mut GameRunner) {
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
}

/// Completion boundary: an empty stack in a clean priority window.
fn assert_settled(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the game must settle into a priority window, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing may remain on the stack"
    );
}

/// Completion boundary for a combat witness: combat damage was dealt (the phase
/// left the combat-damage step for `EndCombat` or `PostCombatMain`) and the game
/// settled into a clean priority window with an empty stack. `combat_damage()`
/// stops silently when a priority pass is rejected, so the phase is the proof.
fn assert_combat_settled(runner: &GameRunner) {
    assert_settled(runner);
    assert!(
        matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat damage must have been dealt, phase is {:?}",
        runner.state().phase
    );
}

/// P0 casts Lightning Bolt (printed text: "Lightning Bolt deals 3 damage to any
/// target.") at `target`.
fn cast_bolt_at(runner: &mut GameRunner, bolt: ObjectId, target: ObjectId) {
    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Lightning Bolt is castable");
    // Lightning Bolt has several legal targets, so the engine always asks: assert the
    // chooser and that the intended target is offered before answering.
    let WaitingFor::TargetSelection {
        player, selection, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Lightning Bolt must ask for its target, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player,
        runner.state().objects[&bolt].owner,
        "the caster chooses the target"
    );
    assert!(
        selection
            .current_legal_targets
            .contains(&engine::types::TargetRef::Object(target)),
        "the intended target is offered"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::TargetRef::Object(target)),
        })
        .expect("the target is legal");
    runner.advance_until_stack_empty();
    assert_settled(runner);
}

/// The ability index of the printed "{T}: This creature deals 1 damage to any
/// target." (Prodigal Sorcerer).
fn damage_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| matches!(ability.effect.as_ref(), Effect::DealDamage { .. }))
        .expect("the printed damage ability")
}

/// CR 509.1a + CR 510.1d + CR 615.1a + CR 615.2: when Wall of Vapor blocks a
/// 3/3, the attacker is a creature the Wall is blocking, so the attacker's 3
/// combat damage to the 0/1 Wall is prevented.
#[test]
fn wall_of_vapor_prevents_damage_from_the_creature_it_blocks() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall = scenario.add_real_card(P0, "Wall of Vapor", Zone::Battlefield, db);
    let attacker = scenario.add_vanilla(P1, 3, 3);

    let mut runner = scenario.build();
    begin_p1_attack_step(&mut runner);
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 attacks with the 3/3");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, attacker)])
        .expect("P0 blocks with Wall of Vapor");
    to_priority(&mut runner);
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.state().objects[&wall].zone,
        Zone::Battlefield,
        "CR 615.1a: the blocked creature's combat damage to the Wall must be prevented"
    );
    assert_eq!(
        runner.state().objects[&wall].damage_marked,
        0,
        "the attacker's 3 combat damage must not be marked on the 0/1 Wall"
    );
    assert_eq!(
        runner.state().objects[&attacker].damage_marked,
        0,
        "reach guard: the Wall's 0 power assigns no combat damage (CR 510.1a)"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: the attack was blocked, so P0 takes no damage"
    );
}

/// CR 615.1a + CR 615.2: the clause is not a general shield — a noncombat
/// Lightning Bolt is not damage dealt by a creature the Wall is blocking, so its
/// 3 damage kills the 0/1.
#[test]
fn wall_of_vapor_burn_damage_is_not_prevented() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall = scenario.add_real_card(P0, "Wall of Vapor", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P0, red_mana(1));

    let mut runner = scenario.build();
    cast_bolt_at(&mut runner, bolt, wall);

    assert_eq!(
        runner.state().objects[&wall].zone,
        Zone::Graveyard,
        "a Lightning Bolt is not a creature the Wall is blocking, so its 3 damage is dealt"
    );
}

/// CR 509.1a + CR 615.1a + CR 609.7c: the shield's source must be a creature the
/// Wall is blocking. While the Wall blocks attacker A, another P1 creature (a
/// Prodigal Sorcerer's "{T}: This creature deals 1 damage to any target.") is
/// not blocked by the Wall, so its 1 damage is dealt and kills the 0/1.
#[test]
fn wall_of_vapor_does_not_prevent_an_unrelated_creature_ping() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall = scenario.add_real_card(P0, "Wall of Vapor", Zone::Battlefield, db);
    let attacker = scenario.add_vanilla(P1, 3, 3);
    let pinger = scenario.add_real_card(P1, "Prodigal Sorcerer", Zone::Battlefield, db);

    let mut runner = scenario.build();
    begin_p1_attack_step(&mut runner);
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 attacks with the 3/3");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, attacker)])
        .expect("P0 blocks with Wall of Vapor");
    to_priority(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "the attacking player must hold priority before combat damage, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .expect("still in combat")
            .blocker_assignments
            .get(&attacker)
            .is_some_and(|blockers| blockers.contains(&wall)),
        "reach guard: the Wall is blocking the 3/3"
    );

    let index = damage_ability_index(&runner, pinger);
    runner.activate(pinger, index).target_object(wall).resolve();
    assert!(
        runner.state().objects[&pinger].tapped,
        "reach guard: the ping ability's {{T}} cost was paid"
    );

    assert_eq!(
        runner.state().objects[&wall].zone,
        Zone::Graveyard,
        "the pinger is not a creature the Wall is blocking, so its 1 damage is dealt"
    );
    assert_settled(&runner);
}
