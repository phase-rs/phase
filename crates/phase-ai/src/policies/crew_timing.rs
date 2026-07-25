//! Timing guard for the initial Crew choice.

use engine::ai_support::legal_actions_full;
use engine::game::combat::{get_valid_attacker_ids, get_valid_blocker_ids};
use engine::game::engine::apply_as_current_for_simulation;
use engine::game::{resolve_all_fast_forward, ResolveAllCallbackDecision};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

pub struct CrewTimingPolicy;

impl TacticalPolicy for CrewTimingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CrewTiming
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        Some(1.0) // activation-constant: exact initial-Crew action self-gates in verdict.
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let (
            WaitingFor::Priority { player },
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids,
            },
        ) = (&ctx.decision.waiting_for, &ctx.candidate.action)
        else {
            return PolicyVerdict::neutral(PolicyReason::new("crew_timing_na"));
        };
        if *player != ctx.ai_player || !creature_ids.is_empty() {
            return PolicyVerdict::neutral(PolicyReason::new("crew_timing_na"));
        }

        if crew_has_exact_combat_use(ctx.state, ctx.ai_player, *vehicle_id, &ctx.candidate.action) {
            PolicyVerdict::neutral(PolicyReason::new("crew_timing_combat_use"))
        } else {
            PolicyVerdict::strong(
                -ctx.penalties().crew_no_immediate_use_penalty,
                PolicyReason::new("crew_timing_no_combat_use"),
            )
        }
    }
}

/// Replays every engine-legal crew subset and asks the combat authority whether
/// the resulting Vehicle itself can attack or block. Phase labels are not a
/// sufficient proxy: summoning sickness, "can't attack/block" effects, and
/// the chosen crew subset all change the exact answer.
fn crew_has_exact_combat_use(
    state: &GameState,
    actor: PlayerId,
    vehicle_id: ObjectId,
    activation: &GameAction,
) -> bool {
    let mut selection_state = state.clone();
    if apply_as_current_for_simulation(&mut selection_state, activation.clone()).is_err() {
        return false;
    }

    legal_actions_full(&selection_state)
        .0
        .iter()
        .filter(|action| {
            matches!(
                action,
                GameAction::CrewVehicle {
                    vehicle_id: candidate_vehicle,
                    creature_ids,
                } if *candidate_vehicle == vehicle_id && !creature_ids.is_empty()
            )
        })
        .any(|subset| crew_subset_has_exact_combat_use(&selection_state, actor, vehicle_id, subset))
}

fn crew_subset_has_exact_combat_use(
    selection_state: &GameState,
    actor: PlayerId,
    vehicle_id: ObjectId,
    subset: &GameAction,
) -> bool {
    let mut replay = selection_state.clone();
    if apply_as_current_for_simulation(&mut replay, subset.clone()).is_err() {
        return false;
    }
    resolve_all_fast_forward(&mut replay, actor, 1, |_, _| {
        ResolveAllCallbackDecision::Action(GameAction::PassPriority)
    });

    // CR 508.1: A Vehicle can be declared as an attacker only before attackers
    // are declared, including priority in its controller's begin-combat step.
    if replay.active_player == actor
        && matches!(replay.phase, Phase::PreCombatMain | Phase::BeginCombat)
    {
        return get_valid_attacker_ids(&replay).contains(&vehicle_id);
    }
    // CR 509.1: A Vehicle can be declared as a blocker only after an opposing
    // attack has actually been declared and before the blocker declaration.
    if replay.active_player != actor
        && replay.phase == Phase::DeclareAttackers
        && replay.combat.as_ref().is_some_and(|combat| {
            combat
                .attackers
                .iter()
                .any(|attacker| attacker.defending_player == actor)
        })
    {
        return get_valid_blocker_ids(&replay).contains(&vehicle_id);
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::Keyword;
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);

    fn crew_fixture() -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: AI };

        let vehicle = create_object(
            &mut state,
            CardId(1),
            AI,
            "Test Vehicle".to_string(),
            Zone::Battlefield,
        );
        let vehicle_object = state.objects.get_mut(&vehicle).expect("vehicle exists");
        vehicle_object
            .card_types
            .core_types
            .push(CoreType::Artifact);
        vehicle_object
            .card_types
            .subtypes
            .push("Vehicle".to_string());
        vehicle_object.keywords.push(Keyword::Crew {
            power: 1,
            once_per_turn: None,
        });
        vehicle_object.base_power = Some(1);
        vehicle_object.base_toughness = Some(1);
        vehicle_object.power = Some(1);
        vehicle_object.toughness = Some(1);
        vehicle_object.summoning_sick = false;

        let crew_member = create_object(
            &mut state,
            CardId(2),
            AI,
            "Crew Member".to_string(),
            Zone::Battlefield,
        );
        let crew_object = state
            .objects
            .get_mut(&crew_member)
            .expect("crew member exists");
        crew_object.card_types.core_types.push(CoreType::Creature);
        crew_object.power = Some(1);
        crew_object.toughness = Some(1);
        (state, vehicle, crew_member)
    }

    fn verdict(state: &GameState, waiting_for: WaitingFor, action: GameAction) -> PolicyVerdict {
        let candidate = CandidateAction {
            action,
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Utility),
        };
        let decision = AiDecisionContext {
            waiting_for,
            candidates: vec![candidate.clone()],
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        CrewTimingPolicy.verdict(&PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: super::super::context::SearchDepth::Root,
        })
    }

    #[test]
    fn real_vehicle_postcombat_crew_is_not_treated_as_an_attack_use() {
        let (mut state, vehicle, _) = crew_fixture();
        state.phase = Phase::PostCombatMain;
        let activation = GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: Vec::new(),
        };
        assert!(!crew_has_exact_combat_use(&state, AI, vehicle, &activation));
        let result = verdict(&state, WaitingFor::Priority { player: AI }, activation);
        assert!(
            matches!(result, PolicyVerdict::Score { delta, reason } if delta < 0.0 && reason.kind == "crew_timing_no_combat_use")
        );
    }

    #[test]
    fn crew_selection_step_is_neutral() {
        let state = GameState::new_two_player(42);
        let result = verdict(
            &state,
            WaitingFor::CrewVehicle {
                player: AI,
                vehicle_id: ObjectId(1),
                crew_power: 1,
                eligible_creatures: Vec::new(),
                contributions: Vec::new(),
            },
            GameAction::CrewVehicle {
                vehicle_id: ObjectId(1),
                creature_ids: Vec::new(),
            },
        );
        assert!(
            matches!(result, PolicyVerdict::Score { delta: 0.0, reason } if reason.kind == "crew_timing_na")
        );
    }

    #[test]
    fn priority_crew_replays_a_legal_subset_into_an_exact_attack_use() {
        let (state, vehicle, crew_member) = crew_fixture();
        let activation = GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: Vec::new(),
        };
        assert!(crew_has_exact_combat_use(&state, AI, vehicle, &activation));

        let mut selection = state.clone();
        apply_as_current_for_simulation(&mut selection, activation)
            .expect("priority crew activation enters the engine subset prompt");
        let subset = legal_actions_full(&selection)
            .0
            .into_iter()
            .find(|action| {
                matches!(action, GameAction::CrewVehicle { vehicle_id: action_vehicle, creature_ids }
                    if *action_vehicle == vehicle && creature_ids == &vec![crew_member])
            })
            .expect("engine offers the exact sufficient crew subset");

        let mut replay = selection.clone();
        apply_as_current_for_simulation(&mut replay, subset)
            .expect("engine accepts its listed crew subset");
        resolve_all_fast_forward(&mut replay, AI, 1, |_, _| {
            ResolveAllCallbackDecision::Action(GameAction::PassPriority)
        });
        assert!(get_valid_attacker_ids(&replay).contains(&vehicle));
    }

    #[test]
    fn real_vehicle_begin_combat_crew_is_an_exact_attack_use() {
        let (mut state, vehicle, _) = crew_fixture();
        state.phase = Phase::BeginCombat;
        let activation = GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: Vec::new(),
        };

        assert!(crew_has_exact_combat_use(&state, AI, vehicle, &activation));
    }
}
