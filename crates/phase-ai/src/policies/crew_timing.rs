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

/// Floor under the vehicles-commitment scale. A crew activation the AI is
/// already looking at is worth judging even in a deck the axis rates low — and
/// even at commitment 0.0, because the deck-time bench detection is deliberately
/// conservative and cannot see tokens or variable-power creatures. The weight is
/// damped rather than zeroed, unlike the payoff policies which opt out entirely
/// below their floor.
const MIN_CREW_ACTIVATION: f32 = 0.25;

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
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // CR 702.122: previously `activation-constant Some(1.0)` — the exact
        // initial-Crew action self-gates in `verdict`, so a constant was safe,
        // but it applied identical weight in a dedicated Vehicles shell and in a
        // deck running one incidental Copter. `features::vehicles` now supplies
        // that deck signal.
        //
        // This NEVER opts out, including at commitment 0.0, and that is
        // deliberate rather than an oversight. `features::vehicles` is
        // conservative by design: `crew_capable_power` excludes non-fixed
        // printed power (`PtValue::Variable`), and a decklist cannot see tokens
        // at all. So a zero commitment means "no bench I could prove at
        // deck-build time", NOT "cannot crew". A Vehicles deck whose bench is
        // tokens, or `*`-power creatures, scores 0.0 and can still reach a legal
        // `CrewVehicle` action — and dropping the timing safeguard exactly there
        // would be worst in the case that needs it most.
        //
        // The weight is therefore DAMPED, never zeroed: `MIN_CREW_ACTIVATION`
        // floors it so the policy keeps judging a crew action the AI is already
        // looking at, while a dedicated shell still outweighs an incidental one.
        Some(features.vehicles.commitment.max(MIN_CREW_ACTIVATION))
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

        // CR 702.122a: Crew N's only effect is "This permanent becomes an artifact
        // creature until end of turn." If that payoff is ALREADY in force — a
        // `KeywordAction::Crew` for this Vehicle pending on the stack, or the
        // resolved-crew marker recorded when the pending entry RESOLVED and
        // installed its UEOT `AddType(Creature)` effect — then re-activating
        // Crew is pure waste: the payoff is already owed (pending) or already
        // applied (resolved), and the only remaining consequence is tapping a
        // fresh untapped body for nothing.
        //
        // The engine authorities (`crew_pending_on_stack` /
        // `crew_resolved_this_turn_contains`) are consulted at PAYOFF-IN-FORCE,
        // deliberately NOT at the announcement-cadence set
        // (`crew_activated_this_turn`): that set is recorded at crew
        // announcement and cleared only at turn start, so it persists even when
        // the crew is countered (CR 701.6a — Stifle/Tale's End-class effects
        // counter the pending keyword action before it resolves). The countered
        // case has neither a pending entry nor a resolution marker, so keying
        // the veto on the cadence set would wrongly forfeit an engine-legal
        // re-crew for the rest of the turn and leave the Vehicle uncrewed — yet
        // the unrestrained Vehicles this guard targets are NOT blocked by the
        // engine's CR 602.5b once-each-turn gate.
        //
        // The resolution marker is the 'live-payoff' authority — recorded
        // (stack.rs, `record_crew_resolution`) exactly when the
        // `KeywordAction::Crew` entry resolves and installs the UEOT animation —
        // deliberately NOT a transient-effect shape match: a generic SelfRef
        // self-animation (Kylox, Voltstrider-class) installs the SAME transient
        // shape (source==Vehicle, UEOT, SpecificObject{Vehicle},
        // AddType(Creature)) with no Crew resolution behind it, and a shape
        // match would misreport it as a Crew payoff and suppress the still-legal
        // re-crew (and any real VehicleCrewed triggers). The marker is never set
        // by it, so the legal re-crew is preserved. Keyed by incarnation: a
        // Vehicle that leaves and returns is a new object (CR 400.7) and is
        // re-crewable.
        //
        // The redundant-crew veto MUST come before the combat-use gate: after a
        // successful crew the Vehicle is a legal attacker, so
        // `crew_has_exact_combat_use` would return true and shield the redundant
        // re-crew — letting the AI tap every body it controls (the crew-repeat
        // pathology).
        //
        // Advisory scope boundary: EXTERNAL non-crew animation (Ensoul Artifact,
        // Tezzeret-class "becomes an artifact creature") can produce the same
        // tap-every-body pathology — the Vehicle is already a creature, so the
        // combat-use gate shields re-crews — yet no Crew resolution ever happens
        // for it, so the marker cannot see it either. Covering that sibling is a
        // deliberate scope limit of this fix (the Kylox-class OWN-source
        // animation, by contrast, demonstrably preserves the re-crew — see
        // `generic_selfref_self_animation_is_not_treated_as_a_resolved_crew`).
        if engine::game::engine::crew_pending_on_stack(ctx.state, *vehicle_id)
            || engine::game::engine::crew_resolved_this_turn_contains(ctx.state, *vehicle_id)
        {
            return PolicyVerdict::reject(PolicyReason::new(
                "crew_timing_redundant_already_creature",
            ));
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
    use engine::game::effects::counter;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, Duration, Effect, ResolvedAbility,
        StaticDefinition, TargetFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::Keyword;
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;

    use crate::features::vehicles::VehiclesFeature;
    use crate::features::DeckFeatures;
    use crate::policies::registry::{PolicyId, PolicyRegistry};
    use crate::session::AiSession;
    use std::sync::Arc;

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

    #[test]
    fn already_crewed_vehicle_recrew_is_penalized_before_combat_use() {
        // Once a Vehicle's Crew payoff is in force, re-activating Crew only taps a
        // fresh body for nothing. The payoff-in-force state is established HERE
        // through the REAL engine mechanism (not raw cadence-set insertion): the
        // first crew is announced by driving priority activation → subset
        // selection → cost payment + stack push through `apply`, then left
        // pending on the stack or resolved (installing the transient UEOT
        // `AddType(Creature)` effect). Both in-force states must be vetoed. And
        // because the crewed Vehicle is a legal attacker,
        // `crew_has_exact_combat_use` would shield the re-crew — so the
        // redundant-crew veto must come before the combat-use gate. This is the
        // crew-repeat pathology regression (CR 702.122a).
        let (mut state, vehicle, crew_member) = crew_fixture();
        let activation = GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: Vec::new(),
        };

        // ── pending-on-stack: the first crew is announced but unresolved. ──
        apply_as_current_for_simulation(&mut state, activation.clone())
            .expect("priority crew activation enters the subset prompt");
        apply_as_current_for_simulation(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id: vehicle,
                creature_ids: vec![crew_member],
            },
        )
        .expect("engine accepts the announced crew and pushes the stack entry");

        assert!(
            state.crew_activated_this_turn.contains(
                &engine::types::identifiers::ObjectIncarnationRef::from_object(
                    state.objects.get(&vehicle).unwrap(),
                ),
            ),
            "reach-guard: the announcement recorded the cadence set"
        );
        assert!(
            engine::game::engine::crew_pending_on_stack(&state, vehicle),
            "reach-guard: the crew entry is pending on the stack"
        );
        assert!(
            !engine::game::engine::crew_resolved_this_turn_contains(&state, vehicle),
            "reach-guard: the pending crew has not resolved, so the resolution marker is absent"
        );
        let result = verdict(
            &state,
            WaitingFor::Priority { player: AI },
            activation.clone(),
        );
        assert!(
            matches!(&result, PolicyVerdict::Reject { reason } if reason.kind == "crew_timing_redundant_already_creature"),
            "a re-crew while the first crew is pending on the stack must be rejected; got {result:?}"
        );

        // ── live-payoff: resolve the pending crew through the engine's fast
        // forward — the resolved-crew marker is now recorded (alongside the
        // transient UEOT AddType(Creature) effect). ──
        resolve_all_fast_forward(&mut state, AI, 1, |_, _| {
            ResolveAllCallbackDecision::Action(GameAction::PassPriority)
        });
        assert!(
            engine::game::engine::crew_resolved_this_turn_contains(&state, vehicle),
            "reach-guard: resolving the crew recorded the resolved-crew marker"
        );
        assert!(
            get_valid_attacker_ids(&state).contains(&vehicle),
            "the crewed Vehicle is a legal attacker, so the exact-combat-use gate alone would shield the re-crew"
        );
        let result = verdict(&state, WaitingFor::Priority { player: AI }, activation);
        assert!(
            matches!(&result, PolicyVerdict::Reject { reason } if reason.kind == "crew_timing_redundant_already_creature"),
            "redundant re-crew of a live-crewed Vehicle must be rejected even though the Vehicle could attack; got {result:?}"
        );
    }

    #[test]
    fn countered_crew_does_not_block_an_engine_legal_recrew() {
        // MED (CR 702.122a + CR 701.6a): the cadence set is recorded at crew
        // ANNOUNCEMENT and cleared only at turn start. If the pending
        // `KeywordAction::Crew` is countered (a mass-counter path — `Effect::CounterAll`
        // with a StackAbility target scores `StackEntryKind::KeywordAction`; the
        // single-target `Effect::Counter` resolver flavor is not exercised here),
        // the Vehicle never becomes a creature
        // (the payoff applies at stack resolution), yet a cadence-set-keyed veto
        // — round-1's `crew_activated_this_turn_contains` — would keep rejecting
        // the re-crew all turn, leaving unrestrained Vehicles (not blocked by the
        // engine's CR 602.5b once-each-turn gate) uncrewed. The guard must key on
        // PAYOFF-IN-FORCE: with no pending entry and no live animation effect the
        // re-crew is engine-legal and must reach the combat-use gate, not be
        // vetoed.
        let (mut state, vehicle, crew_member) = crew_fixture();

        // Announce the first crew through the real engine path (cadence recorded,
        // cost paid, `KeywordAction::Crew` pushed on the stack)…
        apply_as_current_for_simulation(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id: vehicle,
                creature_ids: Vec::new(),
            },
        )
        .expect("priority crew activation enters the subset prompt");
        apply_as_current_for_simulation(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id: vehicle,
                creature_ids: vec![crew_member],
            },
        )
        .expect("engine accepts the announced crew and pushes the stack entry");

        // …then counter the pending keyword action through the engine's production
        // mass-counter path (`counter::resolve_all` with `Effect::CounterAll`),
        // which matches and removes the entry without moving any card (abilities
        // aren't cards, CR 701.6a). The single-target `Effect::Counter` resolver
        // flavor is intentionally not covered here — CounterAll is sufficient to
        // prove the fix (any production counter must clear the pending entry).
        let mut events = Vec::new();
        counter::resolve_all(
            &mut state,
            &ResolvedAbility::new(
                Effect::CounterAll {
                    target: TargetFilter::StackAbility {
                        controller: None,
                        tag: None,
                        kind: None,
                    },
                },
                Vec::new(),
                ObjectId(999),
                AI,
            ),
            &mut events,
        )
        .expect("counter resolves");

        // Discriminator setup: the cadence set STILL records the announcement
        // (it is cleared only at turn start), so this is exactly the state that
        // fooled the round-1 cadence-keyed veto — yet the payoff is not in force.
        assert!(
            state.crew_activated_this_turn.contains(
                &engine::types::identifiers::ObjectIncarnationRef::from_object(
                    state.objects.get(&vehicle).unwrap(),
                ),
            ),
            "discriminator: the stale cadence record persists after the counter"
        );
        assert!(
            !engine::game::engine::crew_pending_on_stack(&state, vehicle),
            "reach-guard: the counter removed the pending crew entry"
        );
        assert!(
            !engine::game::engine::crew_resolved_this_turn_contains(&state, vehicle),
            "reach-guard: the countered crew never resolved — the marker is never recorded"
        );

        let activation = GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: Vec::new(),
        };
        let result = verdict(
            &state,
            WaitingFor::Priority { player: AI },
            activation.clone(),
        );
        assert!(
            !matches!(result, PolicyVerdict::Reject { .. }),
            "a re-crew after the first crew was countered MUST NOT be vetoed (payoff not in force); got {result:?}"
        );

        // With a fresh untapped body the re-crew is also an exact combat use, so
        // it is judged neutrally by the combat-use gate rather than penalized —
        // the fullest production shape of "the AI re-crews with a fresh body".
        let body2 = create_object(
            &mut state,
            CardId(99),
            AI,
            "Second Crew Member".to_string(),
            Zone::Battlefield,
        );
        let body2_obj = state.objects.get_mut(&body2).expect("body2 exists");
        body2_obj.card_types.core_types.push(CoreType::Creature);
        body2_obj.power = Some(1);
        body2_obj.toughness = Some(1);
        assert!(
            crew_has_exact_combat_use(&state, AI, vehicle, &activation),
            "reach-guard: with a fresh body the re-crew crews the Vehicle into an exact attack use"
        );
        let result = verdict(&state, WaitingFor::Priority { player: AI }, activation);
        assert!(
            matches!(&result, PolicyVerdict::Score { delta: 0.0, reason } if reason.kind == "crew_timing_combat_use"),
            "re-crewing after a countered crew reaches the combat-use gate and is judged on its merits, not vetoed; got {result:?}"
        );
    }

    #[test]
    fn generic_selfref_self_animation_is_not_treated_as_a_resolved_crew() {
        // MED discriminator (round-3): the round-2 crew-repeat guard shape-matched
        // the resolved Crew payoff off `transient_continuous_effects`
        // (source==Vehicle, UEOT, SpecificObject{Vehicle}, AddType(Creature)).
        // The ENGINE's production generic-effect SelfRef path
        // (`effect.rs::register_transient_effect`'s SelfRef branch →
        // `install_transient`) installs EXACTLY that transient shape for a
        // generic self-animation — Kylox, Voltstrider-class: a Vehicle whose own
        // activated ability makes it an artifact creature until end of turn —
        // with NO `KeywordAction::Crew` resolution behind it. The shape-matcher
        // misreported that animation as a Crew payoff and vetoed the still-legal
        // Crew action. The fix keys the veto on an explicit resolved-Crew marker
        // (`crew_resolved_this_turn`), which ONLY the `KeywordAction::Crew`
        // stack-resolution arm records — so this animation must NOT be rejected,
        // and an actual re-crew (tapping a fresh body into an exact attack use)
        // reaches the combat-use gate.
        //
        // REVERT-PROBE: restore the round-2 shape-match veto (`crew_payoff_live`)
        // and assertion (c) flips — the generic animation is vetoed as a
        // "redundant crew". Assertions (a)/(b) pin WHY: the shape exists (a) yet
        // no Crew resolution ever happened (b).
        let (mut state, vehicle, _) = crew_fixture();
        // Mirror the fixture's own activation gates (the engine's
        // `ActivateAbility` arm checks `priority_player`; the CrewVehicle arm
        // does not).
        state.priority_player = AI;

        // Drive the REAL generic-effect install path: a genuine activated
        // ability whose effect is a SelfRef GenericEffect self-animate (the
        // Kylox shape), announced through `apply_as_current_for_simulation` and
        // resolved through the engine's fast forward. No Crew keyword action is
        // ever announced.
        let animate = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    }])],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
                end_cost: None,
            },
        );
        Arc::make_mut(&mut state.objects.get_mut(&vehicle).unwrap().abilities).push(animate);
        let ability_index = state.objects[&vehicle].abilities.len() - 1;

        apply_as_current_for_simulation(
            &mut state,
            GameAction::ActivateAbility {
                source_id: vehicle,
                ability_index,
            },
        )
        .expect("the self-animate activated ability must be announced and pushed to the stack");
        resolve_all_fast_forward(&mut state, AI, 1, |_, _| {
            ResolveAllCallbackDecision::Action(GameAction::PassPriority)
        });

        // (a) reach-guard: the generic install produced EXACTLY the transient
        // shape the round-2 shape-matcher keyed on — had `crew_payoff_live`
        // still existed it would have returned true here (the old veto fires).
        assert!(
            state.transient_continuous_effects.iter().any(|tce| {
                tce.source_id == vehicle
                    && tce.duration == Duration::UntilEndOfTurn
                    && matches!(
                        &tce.affected,
                        TargetFilter::SpecificObject { id } if *id == vehicle
                    )
                    && tce.modifications.iter().any(|m| {
                        matches!(
                            m,
                            ContinuousModification::AddType {
                                core_type: CoreType::Creature,
                            }
                        )
                    })
            }),
            "reach-guard: the GenericEffect SelfRef install produced the old crew_payoff_live shape"
        );
        // (b) the marker is NOT set: no `KeywordAction::Crew` ever resolved.
        assert!(
            !engine::game::engine::crew_resolved_this_turn_contains(&state, vehicle),
            "discriminator: no Crew resolution occurred, so the resolved-crew marker must be absent"
        );
        // (c) the crew candidate is NOT vetoed — it reaches the combat-use gate,
        // which judges the re-crew neutrally as an exact attack use.
        let activation = GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: Vec::new(),
        };
        let result = verdict(&state, WaitingFor::Priority { player: AI }, activation);
        assert!(
            matches!(&result, PolicyVerdict::Score { delta: 0.0, reason } if reason.kind == "crew_timing_combat_use"),
            "a Kylox-class generic self-animation must NOT be vetoed as a Crew payoff; \
             the re-crew must reach the combat-use gate, got {result:?}"
        );
    }

    // ─── review #6790: zero cached commitment must NOT silence the safeguard ──

    #[test]
    fn activation_never_opts_out_even_at_zero_commitment() {
        // `features::vehicles` is conservative: it excludes variable printed
        // power and cannot see tokens, so 0.0 means "no bench provable at
        // deck-build time", not "cannot crew".
        let zero = DeckFeatures {
            vehicles: VehiclesFeature::default(),
            ..Default::default()
        };
        assert_eq!(zero.vehicles.commitment, 0.0);
        assert_eq!(
            CrewTimingPolicy.activation(&zero, &GameState::new_two_player(42), AI),
            Some(MIN_CREW_ACTIVATION),
            "a zero-commitment deck can still reach a legal crew action"
        );
    }

    #[test]
    fn activation_scales_above_the_floor() {
        let committed = DeckFeatures {
            vehicles: VehiclesFeature {
                commitment: 0.8,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            CrewTimingPolicy.activation(&committed, &GameState::new_two_player(42), AI),
            Some(0.8),
            "a dedicated shell must outweigh an incidental one"
        );
    }

    #[test]
    fn registry_emits_a_crew_verdict_at_zero_commitment() {
        // The production seam: a live `CrewVehicle` candidate must still reach
        // `CrewTimingPolicy` through the registry when cached commitment is 0.0.
        // This is the regression the opt-out would have introduced — the deck
        // whose bench is tokens scores 0.0 and needs the safeguard most.
        let (state, vehicle, _) = crew_fixture();
        let candidate = CandidateAction {
            action: GameAction::CrewVehicle {
                vehicle_id: vehicle,
                creature_ids: Vec::new(),
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Utility),
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: vec![candidate.clone()],
        };
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        session.features.insert(
            AI,
            DeckFeatures {
                vehicles: VehiclesFeature::default(),
                ..Default::default()
            },
        );
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;

        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let verdicts = PolicyRegistry::default().verdicts(&ctx);
        assert!(
            verdicts.iter().any(|(id, _)| *id == PolicyId::CrewTiming),
            "CrewTimingPolicy must still be routed at commitment 0.0"
        );
    }
}
