//! Light of Sanction — runtime witness for the two-axis prevention clause
//! "Prevent all damage that would be dealt to creatures you control by sources
//! you control."
//!
//! CR 615.1 + CR 615.1a: a continuous effect that says "prevent" is a
//!   prevention effect ("shield") that watches for a damage event and prevents
//!   the damage.
//! CR 615.2 + CR 609.7c: the shield applies to damage from a source with the
//!   stated properties and rechecks those properties when that source would
//!   deal damage.
//! CR 609.7c: a static ability's shield applies to sources that are permanents
//!   with that property (the clause is a static ability of an enchantment).
//! CR 120.3a + CR 120.3e: damage dealt to a player makes them lose that much
//!   life; damage dealt to a creature is marked on it.
//! CR 509.1a + CR 509.1g + CR 510.1c + CR 510.1d: a declared blocker is
//!   blocking its attacker, the attacker assigns to the creatures blocking it,
//!   and a blocker assigns to the creature it is blocking.
//!
//! Verbatim printed Oracle text (MTGJSON AtomicCards):
//! "Prevent all damage that would be dealt to creatures you control by sources
//! you control."

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::TargetRef;

use crate::support::shared_card_db as load_db;

fn db() -> &'static CardDatabase {
    load_db().expect("the committed integration card fixture is present")
}

fn mana(kind: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, Vec::new()))
        .collect()
}

/// Pass priority once, from the active player to P1 (two-player game).
fn pass_to_p1(runner: &mut GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "P0 must hold priority before passing, got {:?}",
        runner.state().waiting_for
    );
    runner.act(GameAction::PassPriority).expect("pass priority");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 must hold priority after the pass, got {:?}",
        runner.state().waiting_for
    );
}

/// Completion boundary: an empty stack in a clean priority window for `player`.
fn assert_settled(runner: &GameRunner, player: PlayerId) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player),
        "the game must settle with priority for {player:?}, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing may remain on the stack"
    );
}

/// P0 casts Lightning Bolt (printed text: "Lightning Bolt deals 3 damage to any
/// target.") at `target`, answering the target prompt if the engine raises it.
fn cast_bolt_at(runner: &mut GameRunner, bolt: ObjectId, target: TargetRef) {
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
        selection.current_legal_targets.contains(&target),
        "the intended target is offered"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(target),
        })
        .expect("the target is legal");
    runner.advance_until_stack_empty();
    assert_settled(runner, P0);
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

/// CR 615.1a + CR 615.2 + CR 609.7c: the shield is scoped on both
/// axes — recipient "creatures you control" and source "sources you control" —
/// so P0's own Lightning Bolt aimed at P0's own creature deals no damage.
#[test]
fn light_of_sanction_prevents_its_controllers_bolt_on_its_controllers_creature() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sanction = scenario.add_real_card(P0, "Light of Sanction", Zone::Battlefield, db);
    let victim = scenario.add_vanilla(P0, 4, 4);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts Lightning Bolt");
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Lightning Bolt must open a target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "P0 is the spell's controller and its announcer");
    assert!(
        target_slots
            .iter()
            .flat_map(|slot| slot.legal_targets.iter())
            .any(|target| *target == TargetRef::Object(victim)),
        "P0's own creature must be in the Bolt's legal target set"
    );
    assert!(
        selection
            .current_legal_targets
            .contains(&TargetRef::Object(victim)),
        "the announced target must be P0's creature"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .expect("targeting P0's own creature is legal");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&sanction].zone,
        Zone::Battlefield,
        "reach guard: Light of Sanction is on the battlefield under P0"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "reach: the 4/4 survives"
    );
    assert_eq!(
        runner.state().objects[&victim].damage_marked,
        0,
        "the Bolt's source is controlled by the shield's controller and its recipient \
         is a creature that controller controls, so all 3 damage is prevented"
    );
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "the Bolt still resolves and is put into its owner's graveyard"
    );
    assert_settled(&runner, P0);
}

/// CR 615.2 + CR 609.7c: the source axis is "sources you control". P1's Bolt is
/// controlled by P1, so the shield does not apply and the 3 damage is marked.
#[test]
fn light_of_sanction_does_not_prevent_its_opponents_bolt_on_its_controllers_creature() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Light of Sanction", Zone::Battlefield, db);
    let victim = scenario.add_vanilla(P0, 4, 4);
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P1, mana(ManaType::Red, 1));

    let mut runner = scenario.build();
    pass_to_p1(&mut runner);
    cast_bolt_at(&mut runner, bolt, TargetRef::Object(victim));

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "reach: the 4/4 survives 3 damage"
    );
    assert_eq!(
        runner.state().objects[&victim].damage_marked,
        3,
        "the damage source is controlled by P1, not by the shield's controller, \
         so the 3 damage is dealt"
    );
    assert_settled(&runner, P0);
}

/// CR 615.1a + CR 615.2: the recipient axis is "creatures you control". P0's
/// Bolt at P1's creature is not covered, so its 3 damage is dealt.
#[test]
fn light_of_sanction_does_not_prevent_its_controllers_bolt_on_an_opponents_creature() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Light of Sanction", Zone::Battlefield, db);
    let opponent_creature = scenario.add_vanilla(P1, 4, 4);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));

    let mut runner = scenario.build();
    cast_bolt_at(&mut runner, bolt, TargetRef::Object(opponent_creature));

    assert_eq!(
        runner.state().objects[&opponent_creature].zone,
        Zone::Battlefield,
        "reach: the 4/4 survives 3 damage"
    );
    assert_eq!(
        runner.state().objects[&opponent_creature].damage_marked,
        3,
        "P1's creature is not a creature P0 controls, so the damage is dealt"
    );
    assert_settled(&runner, P0);
}

/// CR 615.1a + CR 120.3a: a player is not "a creature you control", so P0's own
/// Bolt at P0 still makes P0 lose 3 life.
#[test]
fn light_of_sanction_does_not_prevent_damage_to_its_controller() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Light of Sanction", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));

    let mut runner = scenario.build();
    cast_bolt_at(&mut runner, bolt, TargetRef::Player(P0));

    assert_eq!(
        runner.life(P0),
        17,
        "the recipient is a player, not a creature P0 controls, so the 3 damage is dealt"
    );
    assert_eq!(runner.life(P1), 20, "reach guard: the Bolt was aimed at P0");
    assert_settled(&runner, P0);
}

/// CR 509.1a + CR 509.1g + CR 510.1c + CR 510.1d + CR 615.2: with P0 attacking
/// and P1 blocking, neither shield axis holds — the blocker's damage comes from
/// a source P1 controls, and the attacker's damage goes to a creature P1
/// controls — so both creatures take their 2 damage.
#[test]
fn light_of_sanction_does_not_prevent_either_direction_of_a_blocked_attack() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Light of Sanction", Zone::Battlefield, db);
    let attacker = scenario.add_vanilla(P0, 2, 3);
    let blocker = scenario.add_vanilla(P1, 2, 3);

    let mut runner = scenario.build();
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("P0 attacks with the 2/3");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("P1 blocks with the 2/3");
    to_priority(&mut runner);
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Battlefield,
        "reach guard: the 2/3 attacker survives 2 damage"
    );
    assert_eq!(
        runner.state().objects[&attacker].damage_marked,
        2,
        "the blocking creature's damage comes from a source P1 controls, so the \
         shield does not apply to it"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "reach guard: the 2/3 blocker survives 2 damage"
    );
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        2,
        "the attacker's damage is dealt to a creature P1 controls, not to a \
         creature P0 controls, so it is not prevented"
    );
    assert_eq!(
        runner.life(P1),
        20,
        "reach guard: the attack was blocked, so the defending player takes no damage"
    );
}

/// CR 510.1d + CR 615.2: when P1 attacks and P0 blocks, the attacker's damage to
/// P0's blocker still comes from a source P1 controls, so it is not prevented and
/// the 2 damage is marked on the blocker.
#[test]
fn light_of_sanction_does_not_prevent_an_opponents_attacker_damage_to_its_controllers_blocker() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Light of Sanction", Zone::Battlefield, db);
    let blocker = scenario.add_vanilla(P0, 2, 3);
    let attacker = scenario.add_vanilla(P1, 2, 3);

    let mut runner = scenario.build();
    begin_p1_attack_step(&mut runner);
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 attacks with the 2/3");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("P0 blocks with the 2/3");
    to_priority(&mut runner);
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "reach guard: the 2/3 blocker survives 2 damage"
    );
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        2,
        "the damage source is controlled by P1, so the shield does not apply \
         even though the recipient is a creature P0 controls"
    );
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Battlefield,
        "reach guard: the 2/3 attacker survives the blocker's 2 damage"
    );
    assert_eq!(
        runner.state().objects[&attacker].damage_marked,
        2,
        "the blocker's damage is dealt to a creature P1 controls, so it is not prevented"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: the attack was blocked, so P0 takes no damage"
    );
}
