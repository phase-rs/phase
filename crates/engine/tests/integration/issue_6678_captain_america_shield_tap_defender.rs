//! Regression for GitHub issue #6678 — Captain America's Shield's attack trigger
//! must tap the chosen creature the defending player controls.
//!
//! https://github.com/phase-rs/phase/issues/6678
//!
//! Oracle (the relevant clause):
//!   "Whenever equipped creature attacks, tap target creature defending player
//!    controls."
//!
//! The parse is faithful: an `Attacks` trigger watching the attached permanent
//! (`valid_card: AttachedTo`) whose effect is `SetTapState { scope: Single,
//! state: Tap }` targeting `Typed { Creature, controller: DefendingPlayer }`.
//! The bug is at runtime. The trigger SOURCE is the Equipment, which is not the
//! attacker, so `capture_combat_status` records `defending_player: None` on the
//! Shield's trigger-source snapshot. `source_defending_player` then read that
//! captured `None` through `.map(...).unwrap_or_else(...)`, collapsing it to
//! `Some(None)` and SUPPRESSING the fallback to `resolve_defending_player`
//! (which reads the defender of the *attacking creature* via the triggering
//! event, per CR 508.5a). With no defending player resolvable, the chosen target
//! failed the CR 608.2b resolution-time legality re-check and the ability
//! silently fizzled — the prompt appeared, nothing tapped.
//!
//! This drives the REAL declare-attackers / trigger pipeline (not a synthetic
//! trigger event): the equipped creature attacks, the Shield's trigger is placed
//! on the stack, its target (the defender's creature) is chosen, and the target
//! must be tapped when the trigger resolves.
//!
//! Revert-probe: restoring the `.map().unwrap_or_else()` form makes
//! `source_defending_player` return `None` for the Equipment source, the chosen
//! target is dropped as illegal at resolution, and the final `tapped` assertion
//! flips red.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

// The tap clause in isolation. Verbatim from the card's Oracle text so the
// parser takes the production branch (Attacks trigger + AttachedTo subject +
// DefendingPlayer-scoped tap target).
const SHIELD_TAP_TRIGGER: &str =
    "Whenever equipped creature attacks, tap target creature defending player controls.";

/// Attach `equipment` to `host` in the live state (mirrors how the equip action
/// wires `attached_to` + `attachments`; CR 301.5).
fn attach(runner: &mut GameRunner, equipment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&equipment).unwrap().attached_to = Some(AttachTarget::Object(host));
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(equipment);
}

/// Drain any attack-trigger target-selection / ordering prompt, choosing
/// `target` for the Shield's "tap target creature defending player controls"
/// trigger. When there is a single legal target the engine auto-selects it and
/// the trigger is already on the stack awaiting priority — that path returns
/// without a prompt, which is fine; the resolution assertion is the real check.
fn choose_attack_trigger_target(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering attack triggers should succeed");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choosing the attack-trigger target should succeed");
                return;
            }
            _ => return,
        }
    }
    panic!("expected the Shield attack trigger to request a target");
}

/// CR 508.5a + CR 701.26a: an Equipment's "Whenever equipped creature attacks,
/// tap target creature defending player controls" must resolve the defending
/// player from the ATTACKING creature (carried by the triggering event), not
/// from the Equipment's own combat status — and tap the chosen target.
#[test]
fn captain_america_shield_taps_defenders_creature_on_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The equipped attacker (P0) and the Shield attached to it.
    let attacker = scenario.add_creature(P0, "Equipped Attacker", 2, 2).id();
    let shield = scenario
        .add_creature(P0, "Captain America's Shield", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(SHIELD_TAP_TRIGGER)
        .id();

    // The defending player's creature — the intended tap target, untapped.
    let defender_creature = scenario.add_creature(P1, "Defender's Bear", 2, 2).id();

    let mut runner = scenario.build();
    attach(&mut runner, shield, attacker);

    assert!(
        !runner.state().objects[&defender_creature].tapped,
        "precondition: the defender's creature starts untapped"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("the equipped creature should be a legal attacker");
    choose_attack_trigger_target(&mut runner, defender_creature);
    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&defender_creature].tapped,
        "issue #6678: the Shield's attack trigger must tap the chosen creature \
         the defending player controls"
    );
}
