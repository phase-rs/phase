//! Issue #5268 — The Invincible Iron Man: begin-combat optional put from hand
//! must attach Equipment to Iron Man (not self-attach or skip attachment).
//!
//! CR 608.2c + CR 301.5b + CR 701.3a: the moved card is the attachment and the
//! ability source is the host when the follow-up reads "attach it to ~".

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCondition, Effect, TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const IRON_MAN_ORACLE: &str = "Flying, haste\nAt the beginning of combat on your turn, you may put an artifact card from your hand onto the battlefield. If it's an Equipment, attach it to The Invincible Iron Man.";

const TEST_EQUIPMENT_ORACLE: &str = "Equipped creature gets +2/+0.\nEquip {1}";

fn advance_to_begin_combat_optional(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..240 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).ok();
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .ok();
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .ok();
            }
            _ => return,
        }
    }
}

#[test]
fn invincible_iron_man_trigger_parses_attach_to_source_not_self() {
    let parsed = parse_oracle_text(
        IRON_MAN_ORACLE,
        "The Invincible Iron Man",
        &["Flying".to_string(), "Haste".to_string()],
        &[
            "Legendary".to_string(),
            "Artifact".to_string(),
            "Creature".to_string(),
        ],
        &["Human".to_string(), "Hero".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::BeginCombat))
        .expect("begin-combat trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(execute.forward_result, "put must forward moved card");
    let attach = execute
        .sub_ability
        .as_ref()
        .expect("Equipment attach follow-up");
    match attach.effect.as_ref() {
        Effect::Attach { attachment, target } => {
            assert_eq!(*attachment, TargetFilter::SelfRef);
            assert_eq!(*target, TargetFilter::ParentTarget);
        }
        other => panic!("expected Attach sub, got {other:?}"),
    }
    match attach.condition.as_ref() {
        Some(AbilityCondition::ZoneChangedThisWay { filter }) => match filter {
            TargetFilter::Typed(t) => assert!(t.type_filters.iter().any(
                |f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Equipment"))
            )),
            other => panic!("expected Typed Equipment filter, got {other:?}"),
        },
        other => panic!("expected ZoneChangedThisWay, got {other:?}"),
    }
}

#[test]
fn invincible_iron_man_puts_equipment_from_hand_and_attaches_to_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let iron_man = scenario
        .add_creature_from_oracle(P0, "The Invincible Iron Man", 3, 3, IRON_MAN_ORACLE)
        .id();

    let equipment = scenario
        .add_creature_to_hand(P0, "Test Sword", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(TEST_EQUIPMENT_ORACLE)
        .id();

    let mut runner = scenario.build();
    advance_to_begin_combat_optional(&mut runner);

    assert_eq!(runner.state().phase, Phase::BeginCombat);
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(*player, P0);
        }
        other => panic!("expected OptionalEffectChoice at begin combat, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept Iron Man optional put");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&equipment].zone,
        Zone::Battlefield,
        "Equipment must enter the battlefield (waiting={:?}, stack={:?})",
        runner.state().waiting_for,
        runner.state().stack
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(iron_man)),
        "Equipment must attach to The Invincible Iron Man, not self-attach"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&iron_man)
            .expect("Iron Man")
            .attachments
            .contains(&equipment),
        "Iron Man must list the Equipment as attached"
    );
}
