//! Issue #5249 — The Spear of Bashenga: "Whenever equipped creature attacks
//! the monarch, destroy target tapped nonland permanent that player controls."
//!
//! Before the fix, the parser had no " the monarch" arm, so the trigger
//! degraded to a bare `Attacks` with no attack-target scope — it fired on EVERY
//! attack and prompted for a destroy target regardless of who was the monarch.
//! The fix adds `AttackTargetFilter::Monarch`, a Player-type attack whose
//! defending player must currently hold the monarch designation (CR 725.1),
//! checked statefully in `attack_target_matches` against `state.monarch`.
//!
//! These integration tests drive the real combat pipeline and discriminate all
//! three directions:
//!   1. Attack the monarch (P1) → the trigger fires and reaches the stack.
//!   2. Attack a non-monarch player → the trigger does NOT fire (the reported
//!      bug; this is the revert canary).
//!   3. No monarch in the game → the trigger does NOT fire (CR 725.1).
//!
//! CR references:
//!   - CR 508.1a: The active player chooses which creatures will attack.
//!   - CR 725.1: The monarch is a designation a player can have; there is no
//!     monarch until an effect creates one.

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

const SPEAR_OF_BASHENGA_ORACLE: &str =
    "When The Spear of Bashenga enters, if there is no monarch, you become the monarch.\n\
     Equipped creature gets +2/+2 and has vigilance.\n\
     Whenever equipped creature attacks the monarch, destroy target tapped nonland \
     permanent that player controls.\n\
     Equip {2}";

/// Count stack entries sourced from `source`.
fn stack_triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .count()
}

/// Build a 3-player scenario: P0 controls a creature equipped with The Spear of
/// Bashenga; `monarch` (if any) holds the monarch designation; the defender of
/// the attack (`defender`) controls a tapped creature (a tapped nonland
/// permanent) that the destroy effect can target. Returns the runner, the Spear
/// object id, the attacker, and the tapped target on the defender.
fn setup(monarch: Option<PlayerId>, defender: PlayerId) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spear = scenario
        .add_creature(P0, "The Spear of Bashenga", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(SPEAR_OF_BASHENGA_ORACLE)
        .id();

    // The defender controls a tapped creature — a legal destroy target.
    let victim = scenario.add_creature(defender, "Tapped Bear", 2, 2).id();

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }

    let mut runner = scenario.build();
    runner.state_mut().monarch = monarch;
    runner.state_mut().objects.get_mut(&victim).unwrap().tapped = true;

    attach_to(runner.state_mut(), spear, attacker);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), spear);

    (runner, spear, attacker)
}

/// P0's equipped creature attacks the monarch (P1) → the Spear's trigger fires
/// and reaches the stack.
#[test]
fn spear_of_bashenga_fires_when_attacking_the_monarch() {
    let (mut runner, spear, attacker) = setup(Some(P1), P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");

    assert!(
        stack_triggers_from(&runner, spear) >= 1,
        "attacking the monarch (P1) must fire The Spear of Bashenga's destroy trigger, \
         got stack {:?}",
        runner.stack_names()
    );
}

/// Revert canary: P1 is the monarch, but P0's equipped creature attacks P2 (a
/// NON-monarch player). The trigger must NOT fire. On the unfixed code (no
/// " the monarch" arm) the trigger degrades to a scope-less `Attacks` and fires
/// here — so this assertion fails when the fix is reverted.
#[test]
fn spear_of_bashenga_does_not_fire_when_attacking_non_monarch() {
    let (mut runner, spear, attacker) = setup(Some(P1), P2);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P2))])
        .expect("DeclareAttackers must succeed");

    assert_eq!(
        stack_triggers_from(&runner, spear),
        0,
        "attacking a NON-monarch player (P2 while P1 is monarch) must NOT fire the trigger, \
         got stack {:?}",
        runner.stack_names()
    );
}

/// With no monarch in the game (CR 725.1), the trigger must not fire even though
/// the equipped creature attacks a player.
#[test]
fn spear_of_bashenga_does_not_fire_with_no_monarch() {
    let (mut runner, spear, attacker) = setup(None, P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");

    assert_eq!(
        stack_triggers_from(&runner, spear),
        0,
        "with no monarch (CR 725.1) the trigger must not fire, got stack {:?}",
        runner.stack_names()
    );
}
