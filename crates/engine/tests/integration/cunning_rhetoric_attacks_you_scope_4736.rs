//! Issue #4736 — Cunning Rhetoric: "Whenever an opponent attacks you and/or one
//! or more planeswalkers you control, exile the top card of that player's
//! library. ..."
//!
//! The trigger is scoped to attacks against **you** (the controller). It parses
//! to `TriggerMode::AttackersDeclared` with `valid_source = Opponent` and
//! `valid_target = Controller`, but the runtime matcher ignored those scopes and
//! fired on every attack declaration by anyone against anyone. In a 3+ player
//! game an opponent attacking a *different* player wrongly triggered it.
//!
//! This test discriminates in a 4-player game:
//!   1. Opponent P1 attacks the controller P0     → trigger fires.
//!   2. Opponent P1 attacks a third player P2      → trigger does NOT fire.
//!
//! CR references:
//!   - CR 508.3d: "Whenever [a player] attacks [a player]" fires once per attack
//!     declaration against the scoped defending player, not once per attacker.
//!   - CR 508.5a: the defending player is the one the attack is declared against.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

const CUNNING_RHETORIC_ORACLE: &str = "Whenever an opponent attacks you and/or one or more planeswalkers you control, \
     exile the top card of that player's library. You may play that card for as long as it remains exiled, \
     and you may spend mana as though it were mana of any color to cast it.";

/// Count triggered abilities on the stack sourced from `source`.
fn stack_triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .count()
}

/// Set up a 4-player game: P0 controls Cunning Rhetoric; P1 (an opponent) has a
/// creature to attack with. Returns (runner, rhetoric_id, p1_attacker_id).
fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let rhetoric = {
        let mut builder = scenario.add_creature(P0, "Cunning Rhetoric", 0, 0);
        builder.as_enchantment();
        builder.from_oracle_text(CUNNING_RHETORIC_ORACLE);
        builder.id()
    };
    let attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    // Library padding so exile-top has cards and nobody decks.
    for p in [P0, P1, P2] {
        for _ in 0..10 {
            scenario.add_card_to_library_top(p, "Plains");
        }
    }

    let runner = scenario.build();
    (runner, rhetoric, attacker)
}

/// Opponent P1 attacks the controller P0 → Cunning Rhetoric fires.
#[test]
fn cunning_rhetoric_fires_when_opponent_attacks_you() {
    let (mut runner, rhetoric, attacker) = setup();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 declares an attacker against the controller P0");

    assert!(
        stack_triggers_from(&runner, rhetoric) >= 1,
        "Cunning Rhetoric must fire when an opponent attacks its controller"
    );
}

/// Opponent P1 attacks a THIRD player P2 (not the controller) → Cunning Rhetoric
/// must NOT fire. This is the discriminating case: before the fix the trigger
/// fired on any attack declaration regardless of the defending player.
#[test]
fn cunning_rhetoric_does_not_fire_when_opponent_attacks_third_player() {
    let (mut runner, rhetoric, attacker) = setup();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P2))])
        .expect("P1 declares an attacker against a third player P2");

    assert_eq!(
        stack_triggers_from(&runner, rhetoric),
        0,
        "Cunning Rhetoric must NOT fire when an opponent attacks a player other than its controller"
    );
}
