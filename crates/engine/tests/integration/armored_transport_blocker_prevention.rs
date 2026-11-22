//! Armored Transport — runtime witness for the source-scoped prevention clause
//! "Prevent all combat damage that would be dealt to this creature by creatures
//! blocking it."
//!
//! CR 509.1a + CR 509.1g: the defending player's chosen creature becomes a
//!   blocking creature and is blocking the attacker it was assigned to.
//! CR 510.1c + CR 510.1d: the attacker assigns to the creatures blocking it, and
//!   a blocking creature assigns to the creature it is blocking.
//! CR 615.1a + CR 615.2: "prevent" creates a prevention effect that applies to
//!   damage from a source (see CR 609.7).
//! CR 120.3a: damage dealt to a player causes that player to lose that much life.
//!
//! Verbatim printed Oracle text (MTGJSON AtomicCards):
//! "Prevent all combat damage that would be dealt to this creature by creatures
//! blocking it."

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
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

/// Completion boundary: combat damage was dealt and the game settled back into
/// a clean priority window with an empty stack.
fn assert_combat_settled(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "combat must settle into a priority window, got {:?}",
        state.waiting_for
    );
    assert!(
        state.stack.is_empty(),
        "nothing may remain on the stack after combat damage"
    );
    assert!(
        matches!(state.phase, Phase::EndCombat | Phase::PostCombatMain),
        "combat damage must have been dealt, phase is {:?}",
        state.phase
    );
}

/// P0 casts Lightning Bolt (printed text: "Lightning Bolt deals 3 damage to any
/// target.") at `target`, answering the target prompt if the engine raises it.
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
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "the Bolt must resolve, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 509.1a + CR 509.1g + CR 510.1c + CR 510.1d + CR 615.1a + CR 615.2:
/// blocked by a 3/3, the damage the blocker would deal to Armored Transport is
/// prevented (the blocker is a creature blocking it), while the Transport's own
/// 2 combat damage to that blocker is dealt.
#[test]
fn armored_transport_prevented_damage_from_its_blocker() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let transport = scenario.add_real_card(P0, "Armored Transport", Zone::Battlefield, db);
    let blocker = scenario.add_vanilla(P1, 3, 3);

    let mut runner = scenario.build();
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(transport, AttackTarget::Player(P1))])
        .expect("P0 attacks with Armored Transport");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, transport)])
        .expect("P1 blocks Armored Transport with the 3/3");
    to_priority(&mut runner);
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.state().objects[&transport].zone,
        Zone::Battlefield,
        "CR 615.1a: the blocker's combat damage to the Transport must be prevented, \
         so the 2/1 survives"
    );
    assert_eq!(
        runner.state().objects[&transport].damage_marked,
        0,
        "the blocker's 3 combat damage must not be marked on the Transport"
    );
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        2,
        "the Transport's own 2 combat damage to the blocking creature is dealt (CR 510.1c)"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "reach guard: the 3/3 blocker took the Transport's 2 damage and survived"
    );
    assert_eq!(
        runner.life(P1),
        20,
        "reach guard: the attack was blocked, so the defending player takes no damage"
    );
}

/// CR 510.1d + CR 615.1a: the shield keys on "creatures blocking it". When the
/// Transport is the BLOCKER, the attacker is not a creature blocking the
/// Transport, so its combat damage is dealt — and lethal to a 2/1.
#[test]
fn armored_transport_blocking_attacker_damage_is_not_prevented() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let transport = scenario.add_real_card(P0, "Armored Transport", Zone::Battlefield, db);
    let attacker = scenario.add_vanilla(P1, 2, 2);

    let mut runner = scenario.build();
    begin_p1_attack_step(&mut runner);
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 attacks with the 2/2");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(transport, attacker)])
        .expect("P0 blocks with Armored Transport");
    to_priority(&mut runner);
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.state().objects[&transport].zone,
        Zone::Graveyard,
        "the attacking creature is not blocking the Transport, so its 2 damage kills the 2/1"
    );
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Graveyard,
        "reach guard: the Transport's 2 damage killed the 2/2 attacker"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: the attack was blocked, so P0 takes no damage"
    );
}

/// CR 615.1a + CR 615.2: the clause is limited to COMBAT damage from creatures
/// blocking it. A noncombat Lightning Bolt deals its 3 damage and kills the 2/1.
#[test]
fn armored_transport_noncombat_damage_is_not_prevented() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let transport = scenario.add_real_card(P0, "Armored Transport", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P0, red_mana(1));

    let mut runner = scenario.build();
    cast_bolt_at(&mut runner, bolt, transport);

    assert_eq!(
        runner.state().objects[&transport].zone,
        Zone::Graveyard,
        "a Lightning Bolt is not a creature blocking the Transport, so its 3 damage is dealt"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the Bolt resolves and P0 regains priority, got {:?}",
        runner.state().waiting_for
    );
}
