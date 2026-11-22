//! New Master of Arms — runtime witness for both printed lines:
//! "First strike
//! Prevent all combat damage that would be dealt by blocking creatures that are
//! tapped.
//! {1}{W}: Tap target creature blocking New Master of Arms."
//!
//! CR 509.1a + CR 509.1g: the defending player's chosen creature becomes a
//!   blocking creature.
//! CR 506.4b: tapping a creature that has already been declared as a blocker
//!   does not remove it from combat and does not prevent its combat damage.
//! CR 510.1 + CR 702.7b: first strike deals damage in the first combat damage
//!   step; the non-first-strike blocker deals its damage in the second.
//! CR 615.1a + CR 615.2: "prevent" creates a prevention effect that applies to
//!   damage from a source with the stated properties.
//! CR 508.1f: attacking causes the attacking creature to become tapped.
//!
//! Verbatim printed Oracle text (MTGJSON AtomicCards), as above.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, TapStateChange};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::TargetRef;

use crate::support::shared_card_db as load_db;

fn db() -> &'static CardDatabase {
    load_db().expect("the committed integration card fixture is present")
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

/// The ability index of the printed "{1}{W}: Tap target creature blocking New
/// Master of Arms." (the `SetTapState` tap instruction).
fn tap_blocker_ability_index(runner: &GameRunner, master: ObjectId) -> usize {
    runner.state().objects[&master]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                ability.effect.as_ref(),
                Effect::SetTapState {
                    state: TapStateChange::Tap,
                    ..
                }
            )
        })
        .expect("the printed tap ability")
}

/// CR 509.1a + CR 506.4b + CR 510.1 + CR 615.1a: the second line taps the
/// blocker before combat damage. The tapped blocked creature still assigns and
/// deals its combat damage (CR 506.4b), but the first line prevents it, so New
/// Master of Arms survives with no damage marked.
#[test]
fn new_master_of_arms_tapped_blocker_damage_is_prevented() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let master = scenario.add_real_card(P0, "New Master of Arms", Zone::Battlefield, db);
    let blocker = scenario.add_vanilla(P1, 3, 3);
    // The pool empties at every step; the {1}{W} cost is paid from lands at the
    // moment of activation.
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::White);

    let mut runner = scenario.build();
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(master, AttackTarget::Player(P1))])
        .expect("P0 attacks with New Master of Arms");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, master)])
        .expect("P1 blocks with the 3/3");
    to_priority(&mut runner);

    let index = tap_blocker_ability_index(&runner, master);
    runner
        .act(GameAction::ActivateAbility {
            source_id: master,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the {1}{W} tap ability is activated");
    // A single legal target is chosen by the engine without a prompt: the
    // activation lands on the stack with the creature blocking New Master of
    // Arms already bound as its only target.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "P0 keeps priority with the ability on the stack, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "exactly the tap ability is on the stack"
    );
    let entry = runner
        .state()
        .stack
        .back()
        .expect("the tap ability is on the stack");
    let StackEntryKind::ActivatedAbility { source_id, ability } = &entry.kind else {
        panic!(
            "the stack entry is an activated ability, got {:?}",
            entry.kind
        );
    };
    assert_eq!(source_id, &master);
    assert_eq!(
        ability.targets,
        vec![TargetRef::Object(blocker)],
        "the creature blocking New Master of Arms is the ability's only target"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&blocker].tapped,
        "reach guard: the ability's effect tapped the blocker"
    );

    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.state().objects[&master].zone,
        Zone::Battlefield,
        "CR 615.1a: the tapped blocker's combat damage must be prevented, so the 2/2 survives"
    );
    assert_eq!(
        runner.state().objects[&master].damage_marked,
        0,
        "the blocker's 3 combat damage must not be marked on New Master of Arms"
    );
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        2,
        "CR 702.7b: New Master of Arms' first-strike damage to the blocker is dealt"
    );
    assert_eq!(
        runner.life(P1),
        20,
        "reach guard: the attack was blocked, so the defending player takes no damage"
    );
}

/// Control for the first line: without the tap ability the blocker is untapped,
/// so nothing is prevented. First strike kills no 3/3 — the blocker's regular
/// 3 combat damage then destroys the 2/2.
#[test]
fn new_master_of_arms_untapped_blocker_damage_is_dealt() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let master = scenario.add_real_card(P0, "New Master of Arms", Zone::Battlefield, db);
    let blocker = scenario.add_vanilla(P1, 3, 3);

    let mut runner = scenario.build();
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(master, AttackTarget::Player(P1))])
        .expect("P0 attacks with New Master of Arms");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, master)])
        .expect("P1 blocks with the 3/3");
    to_priority(&mut runner);
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.state().objects[&master].zone,
        Zone::Graveyard,
        "the untapped blocker's 3 combat damage kills the 2/2"
    );
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        2,
        "reach guard: New Master of Arms' first-strike damage was dealt"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "reach guard: 2 damage does not destroy a 3/3"
    );
}

/// Control for "blocking creatures": a tapped creature that is not blocking is
/// outside the shield (CR 508.1f — attacking taps the attacker). The attacking
/// 3/3, tapped from attacking and blocking nothing, deals its damage in full.
#[test]
fn new_master_of_arms_tapped_nonblocker_damage_is_dealt() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let master = scenario.add_real_card(P0, "New Master of Arms", Zone::Battlefield, db);
    let attacker = scenario.add_vanilla(P1, 3, 3);

    let mut runner = scenario.build();
    begin_p1_attack_step(&mut runner);
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 attacks with the 3/3");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(master, attacker)])
        .expect("P0 blocks with New Master of Arms");
    to_priority(&mut runner);
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert!(
        runner.state().objects[&attacker].tapped,
        "reach guard: CR 508.1f — attacking tapped the creature"
    );
    assert_eq!(
        runner.state().objects[&master].zone,
        Zone::Graveyard,
        "the tapped creature is not a blocking creature, so its 3 damage is dealt"
    );
    assert_eq!(
        runner.state().objects[&attacker].damage_marked,
        2,
        "reach guard: New Master of Arms' first-strike damage was dealt"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: the attack was blocked, so P0 takes no damage"
    );
}
