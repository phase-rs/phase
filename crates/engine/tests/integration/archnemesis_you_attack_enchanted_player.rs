//! Archnemesis — "Whenever you attack enchanted player, that player loses 2
//! life. You draw a card and gain 2 life."
//!
//! Before the fix this clause parsed as a bare `YouAttack` trigger with
//! `LoseLife { target: TriggeringPlayer }` and no defender scope, so:
//!   1. the *controller* (you) lost 2 life instead of the enchanted player, and
//!   2. the trigger fired on *any* attack the controller declared, not only
//!      attacks against the enchanted player.
//!
//! The fix routes the clause through the shared "attacks enchanted player"
//! grammar (`Attacks` mode, `valid_source = Controller`,
//! `valid_target = AttachedTo`, `attack_target_filter = Player`) and binds the
//! "that player" anaphor to the enchanted (attached) player via
//! `TargetFilter::AttachedTo`.
//!
//! These tests drive the real combat pipeline (declare attackers → attack
//! trigger onto the stack → resolve). Combat damage is isolated out by clearing
//! the declared attackers after the trigger fires (mirrors
//! `attack_qualifier_stack_conditions.rs`), so the asserted life deltas come
//! solely from the resolved trigger.
//!
//! CR references:
//!   - CR 303.4b: the player an Aura is attached to is the enchanted player;
//!     resolved at runtime via `TargetFilter::AttachedTo`.
//!   - CR 508.1a: the active player chooses which creatures attack.
//!   - CR 608.2k: "that player" refers back to the enchanted player named by the
//!     trigger condition.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

const ARCHNEMESIS_ORACLE: &str = "Enchant opponent\n\
     Whenever you attack enchanted player, that player loses 2 life. You draw a card and gain 2 life.\n\
     Whenever a player attacks you, you may attach this Aura to that player.";

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

/// Set the active player and pass priority until the declare-attackers step.
fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance toward declare attackers");
    }
    panic!("expected DeclareAttackers");
}

/// Attack triggers fire at declaration; drop the declared attackers so the
/// combat-damage step contributes nothing to the asserted life deltas.
fn resolve_trigger_only(runner: &mut GameRunner) {
    if let Some(combat) = &mut runner.state_mut().combat {
        combat.attackers.clear();
    }
    runner.advance_until_stack_empty();
}

/// Build a 3-player game where P0 controls Archnemesis enchanting P1. Returns
/// the runner and the attacking creature's object id.
fn setup(attacker_controller: PlayerId) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Archnemesis", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura"]);
        builder.from_oracle_text(ARCHNEMESIS_ORACLE);
        builder.id()
    };
    let attacker = scenario
        .add_creature(attacker_controller, "Grizzly Bears", 2, 2)
        .id();
    // Give P0 a library so the controller's mandatory "draw a card" never
    // decks-out (also matters for the pre-fix comparison, which drew for P0).
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);
    (runner, attacker)
}

/// NAMED FIX: P0 (controller) attacks P1 (the enchanted player). The enchanted
/// player loses 2 life, the controller draws a card and gains 2 life. Before the
/// fix the controller lost 2 life (net 0 after the gain) and the enchanted
/// player was untouched — so both life assertions flip on revert.
#[test]
fn archnemesis_drains_enchanted_player_when_controller_attacks_it() {
    let (mut runner, attacker) = setup(P0);

    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);
    let p0_hand = hand_size(&runner, P0);

    hand_turn_to(&mut runner, P0);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declaring an attack on the enchanted player should succeed");
    resolve_trigger_only(&mut runner);

    assert_eq!(
        life(&runner, P1),
        p1_life - 2,
        "the enchanted player (P1) must lose 2 life, not the controller"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life + 2,
        "the controller (P0) only gains 2 life; it must NOT also lose 2"
    );
    assert_eq!(
        hand_size(&runner, P0),
        p0_hand + 1,
        "the controller draws a card"
    );
}

/// DEFENDER SCOPE: P0 attacks P2, a player who is NOT enchanted. The trigger
/// must not fire — no draw, no life change. Before the fix (bare `YouAttack`,
/// no defender scope) the trigger fired on any attack by P0, so the controller
/// drew a card; the unchanged hand size flips on revert. The positive test above
/// proves the trigger *can* fire, so this negative is non-vacuous.
#[test]
fn archnemesis_no_effect_when_controller_attacks_non_enchanted_player() {
    let (mut runner, attacker) = setup(P0);

    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);
    let p2_life = life(&runner, P2);
    let p0_hand = hand_size(&runner, P0);

    hand_turn_to(&mut runner, P0);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P2))])
        .expect("declaring an attack on a non-enchanted player should succeed");
    resolve_trigger_only(&mut runner);

    assert_eq!(
        hand_size(&runner, P0),
        p0_hand,
        "attacking a non-enchanted player must not fire the trigger (no draw)"
    );
    assert_eq!(life(&runner, P0), p0_life, "controller life unchanged");
    assert_eq!(
        life(&runner, P1),
        p1_life,
        "enchanted player life unchanged"
    );
    assert_eq!(life(&runner, P2), p2_life, "attacked player life unchanged");
}

/// ATTACKER SCOPE: P2 (not the Aura's controller) attacks P1 (the enchanted
/// player). The trigger belongs to P0 and fires only when P0 attacks, so it must
/// stay silent here. Regression guard for `valid_source = Controller`; the
/// positive test above is the paired reach-guard.
#[test]
fn archnemesis_does_not_fire_when_non_controller_attacks_enchanted_player() {
    let (mut runner, attacker) = setup(P2);

    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);
    let p0_hand = hand_size(&runner, P0);

    hand_turn_to(&mut runner, P2);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("P2 attacking the enchanted player should be a legal declaration");
    resolve_trigger_only(&mut runner);

    assert_eq!(
        life(&runner, P1),
        p1_life,
        "enchanted player unaffected when a non-controller attacks it"
    );
    assert_eq!(life(&runner, P0), p0_life, "controller life unchanged");
    assert_eq!(
        hand_size(&runner, P0),
        p0_hand,
        "controller does not draw when it is not the attacker"
    );
}
