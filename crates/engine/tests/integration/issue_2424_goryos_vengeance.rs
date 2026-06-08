//! Issue #2424 — Goryo's Vengeance must grant haste and exile at next end step.
//!
//! Oracle:
//!   "Return target legendary creature card from your graveyard to the
//!   battlefield. That creature gains haste. Exile it at the beginning of the
//!   next end step."

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ContinuousModification, DelayedTriggerCondition, Effect, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GORYOS_VENGEANCE_ORACLE: &str = "Return target legendary creature card from your graveyard to the battlefield. That creature gains haste. Exile it at the beginning of the next end step.";

fn goryos_chain(
    source_id: ObjectId,
    controller: PlayerId,
    graveyard_creature: TargetRef,
) -> engine::types::ability::ResolvedAbility {
    let def = parse_effect_chain(GORYOS_VENGEANCE_ORACLE, AbilityKind::Spell);
    let mut ability = build_resolved_from_def(&def, source_id, controller);
    ability.targets = vec![graveyard_creature];
    ability
}

fn creature_has_haste_from_transient_effects(
    state: &engine::types::game_state::GameState,
    creature: ObjectId,
) -> bool {
    state.transient_continuous_effects.iter().any(|tce| {
        matches!(tce.affected, engine::types::ability::TargetFilter::SpecificObject { id } if id == creature)
            && tce.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddKeyword { keyword }
                        if matches!(keyword, Keyword::Haste)
                )
            })
    })
}

#[test]
fn goryos_vengeance_grants_haste_and_schedules_delayed_exile() {
    let controller = P0;
    let source_id = ObjectId(100);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let legendary = scenario
        .add_creature_to_graveyard(controller, "Legendary Creature", 4, 4)
        .as_legendary()
        .id();

    let mut runner = scenario.build();

    let chain = goryos_chain(source_id, controller, TargetRef::Object(legendary));
    assert!(
        chain.forward_result,
        "parsed Goryo's chain must mark forward_result on the reanimation parent"
    );

    let mut events = Vec::new();
    {
        let state = runner.state_mut();
        resolve_ability_chain(state, &chain, &mut events, 0).unwrap();

        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } if *object_id == legendary
            )),
            "legendary creature must return to the battlefield"
        );
        assert_eq!(
            state.objects[&legendary].zone,
            Zone::Battlefield,
            "creature should be on the battlefield after resolution"
        );
        assert!(
            creature_has_haste_from_transient_effects(state, legendary),
            "returned creature must gain haste"
        );

        assert_eq!(
            state.delayed_triggers.len(),
            1,
            "resolution must install exactly one delayed exile trigger"
        );
        assert!(matches!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
        ));
        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(legendary)],
            "delayed trigger must snapshot the returned creature"
        );
        assert!(matches!(
            state.delayed_triggers[0].ability.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ));
    }

    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 256,
            "delayed exile trigger never fired; phase = {:?}, waiting_for = {:?}, dt = {}, stack = {}",
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers while advancing to end step");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers while advancing to end step");
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while waiting for end-step exile");
            }
        }
    }

    assert_eq!(
        runner.state().objects[&legendary].zone,
        Zone::Exile,
        "creature must be exiled at the beginning of the next end step"
    );
}
