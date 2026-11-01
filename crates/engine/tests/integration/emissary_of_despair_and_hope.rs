//! CR 120.3 + CR 119.3 + CR 510.2: Emissary of Despair and Emissary of Hope
//! combat-damage triggered abilities with artifact counts.
//!
//! Emissary of Despair:
//! "Flying\nWhenever this creature deals combat damage to a player, that player loses 1 life for each artifact they control."
//!
//! Emissary of Hope:
//! "Flying\nWhenever this creature deals combat damage to a player, you gain 1 life for each artifact that player controls."

use super::rules::{GameScenario, Phase, P0, P1};
use engine::game::combat::AttackTarget;
use engine::types::ability::{
    ControllerRef, Effect, QuantityExpr, QuantityRef, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::triggers::TriggerMode;

const EMISSARY_OF_DESPAIR: &str = "Flying\nWhenever this creature deals combat damage to a player, that player loses 1 life for each artifact they control.";
const EMISSARY_OF_HOPE: &str = "Flying\nWhenever this creature deals combat damage to a player, you gain 1 life for each artifact that player controls.";

#[test]
fn emissary_of_despair_and_hope_parse_ast() {
    let despair_abilities = engine::parser::oracle::parse_oracle_text(
        EMISSARY_OF_DESPAIR,
        "Emissary of Despair",
        &[],
        &[],
        &[],
    );
    let despair_trigger = despair_abilities
        .triggers
        .first()
        .expect("Emissary of Despair has a trigger");
    assert_eq!(despair_trigger.mode, TriggerMode::DamageDone);
    let despair_exec = despair_trigger
        .execute
        .as_deref()
        .expect("trigger has body");
    match &*despair_exec.effect {
        Effect::LoseLife { amount, target } => {
            assert_eq!(
                target.as_ref(),
                Some(&TargetFilter::TriggeringPlayer),
                "damaged player must be the life-loss target"
            );
            assert_eq!(
                amount,
                &QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Artifact],
                            controller: Some(ControllerRef::TriggeringPlayer),
                            properties: Vec::new(),
                        })
                    }
                },
                "count must be artifacts controlled by TriggeringPlayer"
            );
        }
        other => panic!("expected LoseLife, got {other:?}"),
    }

    let hope_abilities = engine::parser::oracle::parse_oracle_text(
        EMISSARY_OF_HOPE,
        "Emissary of Hope",
        &[],
        &[],
        &[],
    );
    let hope_trigger = hope_abilities
        .triggers
        .first()
        .expect("Emissary of Hope has a trigger");
    assert_eq!(hope_trigger.mode, TriggerMode::DamageDone);
    let hope_exec = hope_trigger.execute.as_deref().expect("trigger has body");
    match &*hope_exec.effect {
        Effect::GainLife { amount, player } => {
            assert_eq!(
                player,
                &TargetFilter::Controller,
                "ability controller must gain the life"
            );
            assert_eq!(
                amount,
                &QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Artifact],
                            controller: Some(ControllerRef::TriggeringPlayer),
                            properties: Vec::new(),
                        })
                    }
                },
                "count must be artifacts controlled by TriggeringPlayer"
            );
        }
        other => panic!("expected GainLife, got {other:?}"),
    }
}

/// CR 120.3 + CR 119.3 + CR 510.2: When Emissary of Despair deals combat damage
/// to a player, that player loses 1 life for each artifact they control.
#[test]
fn emissary_of_despair_causes_life_loss_for_defenders_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emissary = {
        let mut b = scenario.add_creature(P0, "Emissary of Despair", 2, 1);
        b.from_oracle_text(EMISSARY_OF_DESPAIR);
        b.id()
    };

    // P0 controls 1 artifact.
    scenario.add_artifact_from_oracle(P0, "Sol Ring", "{T}: Add {C}{C}.");

    // P1 controls 3 artifacts.
    for i in 0..3 {
        scenario.add_artifact_from_oracle(P1, &format!("Mox {i}"), "{T}: Add {W}.");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(emissary, AttackTarget::Player(P1))])
        .expect("declare attacker");

    let outcome = runner.combat_damage();

    // 20 - 2 combat damage = 18.
    // Trigger resolves: P1 controls 3 artifacts -> loses 3 life -> 18 - 3 = 15 life.
    let p1_life = outcome.state().players[P1.0 as usize].life;
    let p0_life = outcome.state().players[P0.0 as usize].life;
    assert_eq!(
        p1_life, 15,
        "P1 must take 2 combat damage and lose 3 life for their 3 artifacts"
    );
    assert_eq!(p0_life, 20, "P0's life total must remain 20");
}

/// CR 120.3 + CR 119.3 + CR 510.2: When Emissary of Hope deals combat damage
/// to a player, you gain 1 life for each artifact that player controls.
#[test]
fn emissary_of_hope_gains_life_for_defenders_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emissary = {
        let mut b = scenario.add_creature(P0, "Emissary of Hope", 2, 1);
        b.from_oracle_text(EMISSARY_OF_HOPE);
        b.id()
    };

    // P0 controls 1 artifact.
    scenario.add_artifact_from_oracle(P0, "Sol Ring", "{T}: Add {C}{C}.");

    // P1 controls 3 artifacts.
    for i in 0..3 {
        scenario.add_artifact_from_oracle(P1, &format!("Mox {i}"), "{T}: Add {W}.");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(emissary, AttackTarget::Player(P1))])
        .expect("declare attacker");

    let outcome = runner.combat_damage();

    // P1 takes 2 combat damage (20 -> 18).
    // Trigger resolves: P1 controls 3 artifacts -> P0 gains 3 life (20 -> 23).
    let p1_life = outcome.state().players[P1.0 as usize].life;
    let p0_life = outcome.state().players[P0.0 as usize].life;
    assert_eq!(p1_life, 18, "P1 must take 2 combat damage");
    assert_eq!(p0_life, 23, "P0 must gain 3 life for P1's 3 artifacts");
}
