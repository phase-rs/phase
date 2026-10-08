//! Regression for GitHub issue #6011 — Kibo, Uktabi Prince's attack trigger.
//!
//! Oracle (attack trigger):
//!   "Whenever Kibo attacks, defending player sacrifices an artifact of their
//!    choice."
//!
//! Bug: the "defending player" subject was dropped by the parser, so the
//! `Sacrifice` effect's target filter had `controller: None` and defaulted to
//! the ability's controller — the ATTACKING player. The attacker was forced to
//! sacrifice their own artifact instead of the defending player.
//!
//! Root cause: `player_filter_as_controller_ref` (parser/oracle_effect/mod.rs)
//! mapped `TargetFilter::Player` → `TargetPlayer` (Diabolic Edict) but had no
//! arm for `TargetFilter::DefendingPlayer`, so the sacrifice injection arm never
//! stamped the filter's controller. The runtime `resolve_sacrifice_scope`
//! already routes `ControllerRef::DefendingPlayer` to the combat defender
//! (CR 508.5) — only the parser dropped the scope.
//!
//! This test drives the full pipeline through `apply`: Kibo attacks P1, the
//! attack trigger resolves, and the DEFENDING player (P1), not the attacker
//! (P0), sacrifices their artifact.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

// Verbatim attack-trigger line from Kibo, Uktabi Prince. Isolated from the
// card's other abilities (the graveyard +1/+1 trigger and the Banana mana
// ability) so the sacrifice resolves without a follow-on trigger cascade;
// this exact sentence is what the real card's parser sees for this ability.
const KIBO_ATTACK_TRIGGER: &str =
    "Whenever Kibo attacks, defending player sacrifices an artifact of their choice.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).expect("object exists").zone
}

/// Drive the attack trigger to resolution, answering the defending player's
/// sacrifice choice by picking their artifact.
fn resolve_trigger(runner: &mut GameRunner, defender_artifact: ObjectId) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![] })
                    .expect("order triggers");
            }
            other => panic!("unexpected waiting state during Kibo trigger: {other:?}"),
        }
    }
    let _ = defender_artifact;
    panic!("Kibo attack trigger did not resolve");
}

/// CR 508.5 + CR 701.21a: Kibo's attack trigger makes the DEFENDING player
/// sacrifice one of their artifacts. Reverting the parser fix routes the
/// sacrifice to the attacking controller instead.
#[test]
fn kibo_attack_trigger_defending_player_sacrifices_their_artifact() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let kibo = scenario
        .add_creature_from_oracle(P0, "Kibo, Uktabi Prince", 2, 2, KIBO_ATTACK_TRIGGER)
        .id();

    // Attacker (P0) and defender (P1) each control exactly one artifact.
    let attacker_artifact = {
        let mut b = scenario.add_creature(P0, "Attacker Artifact", 0, 1);
        b.as_artifact();
        b.id()
    };
    let defender_artifact = {
        let mut b = scenario.add_creature(P1, "Defender Artifact", 0, 1);
        b.as_artifact();
        b.id()
    };

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kibo, AttackTarget::Player(P1))])
        .expect("declare Kibo attacking P1");

    resolve_trigger(&mut runner, defender_artifact);

    // The DEFENDING player's artifact is sacrificed; the attacker's remains.
    assert_eq!(
        zone_of(&runner, defender_artifact),
        Zone::Graveyard,
        "issue #6011: the DEFENDING player must sacrifice their artifact"
    );
    assert_eq!(
        zone_of(&runner, attacker_artifact),
        Zone::Battlefield,
        "issue #6011: the ATTACKING player must NOT sacrifice their artifact"
    );
}
