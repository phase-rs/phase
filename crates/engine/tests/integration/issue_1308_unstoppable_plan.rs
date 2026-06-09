//! Issue #1308 — Unstoppable Plan must untap all nonland permanents you control
//! at the beginning of your end step.
//!
//! Oracle: "At the beginning of your end step, untap all nonland permanents
//! you control."

use super::rules::{GameRunner, GameScenario, Phase, WaitingFor, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, StaticDefinition, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor as EngineWaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase as EnginePhase;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const UNSTOPPABLE_PLAN: &str =
    "At the beginning of your end step, untap all nonland permanents you control.";

fn advance_until_end_step_trigger_resolved(runner: &mut GameRunner) {
    for _ in 0..200 {
        let in_end_step = runner.state().phase == Phase::End;
        let stack_empty = runner.state().stack.is_empty();
        if in_end_step
            && stack_empty
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while advancing to end step");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            other => panic!(
                "unexpected waiting state advancing to end step: {other:?} \
                 (phase={:?}, stack_len={})",
                runner.state().phase,
                runner.state().stack.len()
            ),
        }
    }
    panic!(
        "engine did not resolve Unstoppable Plan's end-step trigger within 200 steps \
         (phase={:?}, stack_len={})",
        runner.state().phase,
        runner.state().stack.len()
    );
}

#[test]
fn unstoppable_plan_parses_end_step_untap_all() {
    let parsed = parse_oracle_text(
        UNSTOPPABLE_PLAN,
        "Unstoppable Plan",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Unstoppable Plan must have an end-step trigger");
    assert_eq!(trigger.mode, TriggerMode::Phase);
    assert_eq!(trigger.phase, Some(EnginePhase::End));
    let execute = trigger.execute.as_ref().expect("trigger must execute");
    assert!(
        matches!(*execute.effect, Effect::UntapAll { .. }),
        "expected UntapAll effect, got {:?}",
        execute.effect
    );
}

#[test]
fn unstoppable_plan_untaps_nonland_permanents_at_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Unstoppable Plan", 0, 0)
        .as_enchantment()
        .from_oracle_text(UNSTOPPABLE_PLAN);
    let creature = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let land = scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .tapped = true;
    runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;

    advance_until_end_step_trigger_resolved(&mut runner);

    assert!(
        !runner.state().objects[&creature].tapped,
        "creature must be untapped by Unstoppable Plan's end-step trigger"
    );
    assert!(
        runner.state().objects[&land].tapped,
        "lands must remain tapped — only nonland permanents untap"
    );
}

/// Cast Unstoppable Plan from hand, tap a creature, then reach end step — the
/// ETB trigger registration must survive the cast pipeline.
#[test]
fn unstoppable_plan_untaps_after_cast_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let plan = scenario
        .add_creature_to_hand_from_oracle(P0, "Unstoppable Plan", 0, 0, UNSTOPPABLE_PLAN)
        .as_enchantment()
        .id();
    let creature = scenario.add_creature(P0, "Soldier", 2, 2).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .tapped = true;

    let card_id = runner.state().objects[&plan].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: plan,
            card_id,
            targets: vec![],
        })
        .expect("Unstoppable Plan is 0-cost in scenario shell");
    runner.advance_until_stack_empty();

    advance_until_end_step_trigger_resolved(&mut runner);

    assert!(
        !runner.state().objects[&creature].tapped,
        "end-step untap must fire after casting Unstoppable Plan from hand"
    );
}

/// When entering the end step pauses on a CR 616.1 empty-mana-pool choice,
/// `auto_advance` returns before its `Phase::End` arm runs. After the choice
/// resolves, phase-begin triggers must still fire (CR 513.1 + CR 603.3b).
#[test]
fn unstoppable_plan_untaps_after_deferred_end_step_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);

    scenario
        .add_creature(P0, "Unstoppable Plan", 0, 0)
        .as_enchantment()
        .from_oracle_text(UNSTOPPABLE_PLAN);
    let creature = scenario.add_creature(P0, "Soldier", 2, 2).id();

    let mut runner = scenario.build();

    // Two retention handlers force a ReplacementChoice while entering End step.
    for (n, color) in [(1u64, ManaColor::Green), (2, ManaColor::Blue)] {
        let source = engine::game::zones::create_object(
            runner.state_mut(),
            engine::types::identifiers::CardId(100 + n),
            P0,
            format!("Retention {n}"),
            Zone::Battlefield,
        );
        runner
            .state_mut()
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::StepEndUnspentMana {
                    filter: Some(color),
                    action: engine::types::mana::StepEndManaAction::Retain,
                })
                .affected(TargetFilter::Controller),
            );
    }

    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .tapped = true;
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Green,
        ObjectId(900),
        false,
        vec![],
    ));
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Blue,
        ObjectId(901),
        false,
        vec![],
    ));

    // Leave postcombat main — entering End step pauses on mana retention choice.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 pass from postcombat main");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 pass from postcombat main");

    assert!(
        matches!(
            runner.state().waiting_for,
            EngineWaitingFor::ReplacementChoice { .. }
        ),
        "expected mana-retention choice entering end step, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "enter_phase sets End immediately; the mana drain may still be pending"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose first retention handler");

    assert_eq!(
        runner.state().phase,
        Phase::End,
        "phase entry must complete after the deferred mana choice"
    );

    // Resolve the end-step trigger if it reached the stack.
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects[&creature].tapped,
        "Unstoppable Plan must untap nonlands even when end-step entry was deferred \
         by a CR 616.1 mana-pool choice"
    );
}
