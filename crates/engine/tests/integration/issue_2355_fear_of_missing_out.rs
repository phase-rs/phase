//! Regression for issue #2355: Fear of Missing Out's delirium attack trigger must
//! prompt for and untap a target creature, then grant an additional combat phase.
//!
//! https://github.com/phase-rs/phase/issues/2355

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const FOMO_ORACLE: &str = "When this creature enters, discard a card, then draw a card.\n\
Delirium — Whenever this creature attacks for the first time each turn, if there are four or \
more card types among cards in your graveyard, untap target creature. After this phase, there \
is an additional combat phase.";

fn seed_delirium_graveyard(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_creature_to_graveyard(P0, "Delirium Creature", 1, 1);
    scenario.add_spell_to_graveyard(P0, "Delirium Instant", true);
    scenario.add_spell_to_graveyard(P0, "Delirium Sorcery", false);
    scenario.add_land_to_hand(P0, "Delirium Forest").id()
}

fn resolve_fomo_attack_trigger(runner: &mut engine::game::scenario::GameRunner, target: ObjectId) {
    let mut saw_target_prompt = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                saw_target_prompt = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose FOMO untap target");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.pass_both_players();
            }
            _ => break,
        }
    }
    assert!(
        saw_target_prompt,
        "FOMO delirium attack trigger must prompt for untap target, got waiting_for = {:?}, stack = {}",
        runner.state().waiting_for,
        runner.state().stack.len()
    );
    runner.advance_until_stack_empty();
}

#[test]
fn fear_of_missing_out_parses_delirium_attack_untap_trigger() {
    let parsed = parse_oracle_text(
        FOMO_ORACLE,
        "Fear of Missing Out",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Warrior".to_string()],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "FOMO must parse ETB + delirium attack triggers, got {:#?}",
        parsed.triggers
    );
    let attack = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("FOMO must have delirium attack trigger");
    assert!(matches!(
        attack.execute.as_ref().map(|e| &*e.effect),
        Some(engine::types::ability::Effect::SetTapState { .. })
    ));
}

#[test]
fn fear_of_missing_out_untaps_target_creature_on_delirium_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = seed_delirium_graveyard(&mut scenario);

    let fomo = scenario
        .add_creature(P0, "Fear of Missing Out", 2, 2)
        .from_oracle_text(FOMO_ORACLE)
        .id();
    let untap_target = scenario.add_creature(P0, "Tapped Ally", 1, 1).id();

    let mut runner = scenario.build();
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), land, Zone::Graveyard, &mut events);
    {
        let obj = runner.state_mut().objects.get_mut(&untap_target).unwrap();
        obj.tapped = true;
    }

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(fomo, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("DeclareBlockers should succeed");
        runner.pass_both_players();
    }

    resolve_fomo_attack_trigger(&mut runner, untap_target);

    assert!(
        !runner.state().objects[&untap_target].tapped,
        "FOMO must untap the chosen creature (issue #2355)"
    );
    assert!(
        !runner.state().extra_phases.is_empty(),
        "FOMO must schedule an additional combat phase after the untap"
    );
}
