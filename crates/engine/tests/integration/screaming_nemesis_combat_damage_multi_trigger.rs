//! Runtime regression for issue #6014: Screaming Nemesis's "Whenever this
//! creature is dealt damage" trigger silently failed to fire when Nemesis took
//! COMBAT damage from two blockers in the same combat-damage step.
//!
//! Root cause: `resolve_combat_damage`'s regular sub-step processes
//! `DamageReceived` triggers internally via `process_combat_damage_triggers`,
//! which can set `state.waiting_for` to `OrderTriggers` (two simultaneous
//! triggers controlled by the same player, per CR 603.3b) WITHOUT returning
//! `Some(..)` from `resolve_combat_damage` — it still returns `None` because
//! the damage step itself completed. `handle_assign_combat_damage` and
//! `handle_assign_blocker_damage` (engine_combat.rs) — the interactive re-entry
//! points used whenever an attacker or blocker must divide its damage among
//! 2+ recipients — treated `None` as "nothing pending" and unconditionally
//! called `priority::reset_priority` + returned `WaitingFor::Priority`,
//! clobbering the just-set `OrderTriggers` prompt. The two pending triggers
//! were stranded in `state.pending_trigger_order`, never reaching the stack:
//! Nemesis's ability appeared not to trigger at all. The declare-attackers and
//! declare-blockers trigger call sites in the same file already guarded against
//! this (`if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. })`);
//! the two damage-assignment re-entry points were missing the same guard.
//!
//! CR references:
//!   * CR 603.3b - two or more abilities have triggered since the last time a
//!     player received priority; their controller puts them on the stack in
//!     any order they choose.
//!   * CR 510.1c - a blocked creature (Nemesis, the attacker here) with 2+
//!     blockers divides its combat damage among them as its controller
//!     chooses — the interactive re-entry point (`handle_assign_combat_damage`)
//!     this regression lives in.
//!   * CR 510.2 - all assigned combat damage is dealt simultaneously; each
//!     blocker's damage to Nemesis is dealt in the same turn-based action.
//!   * CR 603.2 - each qualifying event triggers a matching ability
//!     independently, so the two simultaneous `DamageDealt` events (one per
//!     blocker) each trigger "whenever this creature is dealt damage" on
//!     their own, not once for the combined batch.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::static_abilities::player_has_cant_gain_life;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

use super::rules::run_combat;

const NEMESIS_TEXT: &str = "Haste\n\
     Whenever this creature is dealt damage, it deals that much damage to any \
     other target. If a player is dealt damage this way, they can't gain life \
     for the rest of the game.";

/// Nemesis attacks and is blocked by two creatures, each dealing it a
/// different, non-lethal amount of combat damage in the same combat-damage
/// step. Both `DamageDealt` events must independently trigger Nemesis's
/// ability, requiring an `OrderTriggers` choice (CR 603.3b) before either
/// redirect can be targeted. Revert guard: before the fix, `waiting_for` lands
/// on plain `Priority` with an empty stack right after combat damage — the
/// ordering prompt (and both triggers) is silently dropped.
#[test]
fn nemesis_fires_once_per_blocker_and_survives_the_ordering_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let nemesis = scenario
        .add_creature_from_oracle(P0, "Screaming Nemesis", 3, 8, NEMESIS_TEXT)
        .id();
    // Two blockers, each surviving Nemesis's split damage, each dealing a
    // distinct non-lethal amount back to Nemesis (2 and 1) so the two
    // DamageDealt events - and the two resulting redirects - stay
    // distinguishable in the assertions below.
    let blocker_a = scenario.add_creature(P1, "Charging Bear", 2, 5).id();
    let blocker_b = scenario.add_creature(P1, "Grizzly Bears", 1, 5).id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    run_combat(
        &mut runner,
        vec![nemesis],
        vec![(blocker_a, nemesis), (blocker_b, nemesis)],
    );

    // Positive reach-guard: both DamageDealt events (from blocker_a and
    // blocker_b) must independently trigger Nemesis's ability, surfacing the
    // CR 603.3b ordering choice for its controller (P0) instead of silently
    // vanishing under Priority.
    let WaitingFor::OrderTriggers { triggers, player } = runner.state().waiting_for.clone() else {
        panic!(
            "expected an OrderTriggers prompt for Nemesis's two DamageDealt triggers, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "Nemesis's controller orders its own triggers");
    assert_eq!(
        triggers.len(),
        2,
        "both blockers' combat damage must each trigger Nemesis independently"
    );
    assert!(
        triggers.iter().all(|t| t.source_id == nemesis),
        "both pending triggers must be Nemesis's DamageReceived ability"
    );

    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("ordering the two triggers must succeed");

    // Redirect the first trigger's damage to P1 (locks them against life
    // gain) and the second to the surviving blocker (marks damage on it) -
    // proving each trigger resolves independently with its own amount rather
    // than one silently swallowing the other.
    for redirect_to in [TargetRef::Player(P1), TargetRef::Object(blocker_b)] {
        let WaitingFor::TriggerTargetSelection { target_slots, .. } =
            runner.state().waiting_for.clone()
        else {
            panic!(
                "expected a TriggerTargetSelection prompt, got {:?}",
                runner.state().waiting_for
            );
        };
        assert!(
            target_slots[0].legal_targets.contains(&redirect_to),
            "the intended redirect target must be legal for 'any other target'"
        );
        assert!(
            !target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(nemesis)),
            "Nemesis itself must never be a legal redirect target"
        );
        runner
            .act(GameAction::SelectTargets {
                targets: vec![redirect_to],
            })
            .expect("selecting the redirect target must succeed");
    }

    runner.advance_until_stack_empty();

    // Positive reach-guard: the first trigger's redirect (2 damage, from
    // blocker_a) actually resolved against P1.
    assert_eq!(
        runner.life(P1),
        p1_life_before - 2,
        "P1 must take the 2 damage redirected from blocker_a's DamageDealt event"
    );
    assert!(
        player_has_cant_gain_life(runner.state(), P1),
        "CR 119.7: a player dealt damage by the redirect can't gain life for the rest of the game"
    );

    // Positive reach-guard: the second trigger's redirect (1 damage, from
    // blocker_b) actually resolved against blocker_b - a second, independent
    // instance of the ability, not a duplicate of the first.
    assert_eq!(
        runner.state().objects[&blocker_b].damage_marked,
        1,
        "blocker_b must take the 1 damage redirected from its own DamageDealt event"
    );
}
