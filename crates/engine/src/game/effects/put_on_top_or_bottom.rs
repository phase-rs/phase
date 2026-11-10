use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
/// Resolves a top-or-bottom choice and sends the object to its owner's library.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 608.2c + CR 603.10: Delegate target resolution to the unified 3-tier
    // dispatch (`resolved_targets`) so this resolver picks up the same self-ref
    // handling `Effect::Bounce` and the other zone-change resolvers use.
    // `resolved_targets` short-circuits `SelfRef` to `ability.source_id`
    // regardless of `ability.targets`, so a self-tuck (Arashin Sovereign: "When
    // ~ dies, you may put it on the top or bottom of its owner's library")
    // resolves to the source; a chosen/parent target is unchanged (tier 3
    // returns the pre-selected targets satisfying the filter).
    let Effect::PutOnTopOrBottom {
        target,
        chooser,
        top_position,
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "PutOnTopOrBottom requires its matching effect".to_string(),
        ));
    };
    let object_id = crate::game::targeting::resolved_targets(ability, target, state)
        .iter()
        .find_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .ok_or(EffectError::InvalidParam(
            "PutOnTopOrBottom requires a target".to_string(),
        ))?;

    if !state.objects.contains_key(&object_id) {
        return Err(EffectError::ObjectNotFound(object_id));
    }

    // CR 608.2d: the Oracle text supplies the player who makes this choice.
    let Some(choice_player) = super::resolve_player_for_context_ref(state, ability, chooser) else {
        return Ok(());
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    // CR 730.3 + CR 730.3c: record the physical component cards NOW. A merged
    // permanent that already left the battlefield has been split (CR 730.3), so
    // the object id parked below cannot recover its siblings from live state at
    // answer time. An ordinary single-card target records nothing and keeps the
    // single-card delivery path unchanged.
    let recorded = recorded_components(state, ability, target, object_id);
    let components = if recorded.len() > 1 {
        recorded
    } else {
        Vec::new()
    };

    state.waiting_for = WaitingFor::TopOrBottomChoice {
        player: choice_player,
        object_id,
        top_position: top_position.clone(),
        components,
    };

    Ok(())
}

/// CR 730.3 + CR 730.3c: the physical card ids the resolved object brings to the
/// library, captured while the prompt is created.
///
/// A still-merged battlefield permanent publishes its component stack
/// ([`crate::game::merge::physical_arrivals`]). Once it has already left the
/// battlefield — Arashin Sovereign dying as a merged permanent — the live merge
/// state is gone and only the recorded split successors remain: the departure's
/// co-split components ([`crate::game::merge::expand_returned_merge_components`])
/// and, for a self-reference, the components pinned to the survivor's current
/// incarnation ([`crate::game::merge::co_split_components`]). CR 400.7: a
/// component that has since changed zones is a new object this effect can no
/// longer find, so neither authority records it.
fn recorded_components(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    object_id: crate::types::identifiers::ObjectId,
) -> Vec<crate::types::identifiers::ObjectId> {
    let mut components = crate::game::merge::physical_arrivals(state, &[object_id]);
    if components.len() > 1 {
        return components;
    }
    components =
        crate::game::merge::expand_returned_merge_components(state, ability, components, target);
    if matches!(target, TargetFilter::SelfRef) {
        // The prompt's own self-reference is "the object the merged permanent
        // became as it left" (CR 730.3c): the split pins are the recorded fact
        // that survives the split, and they are read before the answer parks on
        // this prompt.
        for id in crate::game::merge::co_split_components(state, object_id, &components) {
            if !components.contains(&id) {
                components.push(id);
            }
        }
    }
    components
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn test_resolve_uses_chooser_not_destination_owner() {
        let mut state = GameState::new_two_player(42);
        // Create a creature owned by player 1 but controlled by player 0
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        // Override controller to player 0 (simulating gain control)
        state.objects.get_mut(&obj_id).unwrap().controller = PlayerId(0);

        let ability = ResolvedAbility::new(
            Effect::PutOnTopOrBottom {
                target: TargetFilter::Any,
                chooser: TargetFilter::Controller,
                top_position: Default::default(),
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // P0 controls the effect and must choose; P1 still owns the destination library.
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::TopOrBottomChoice {
                    player: PlayerId(0),
                    object_id: oid,
                    ..
                } if oid == obj_id
            ),
            "Expected TopOrBottomChoice for chooser P0, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn test_resolve_stack_spell_prompts_owner() {
        let mut state = GameState::new_two_player(42);
        // The owner-framed form selects the stack spell's owner as the chooser.
        let obj_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Stack Spell".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&obj_id).unwrap().controller = PlayerId(0);

        let ability = ResolvedAbility::new(
            Effect::PutOnTopOrBottom {
                target: TargetFilter::Any,
                chooser: TargetFilter::ParentTargetOwner,
                top_position: Default::default(),
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(101),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::TopOrBottomChoice {
                    player: PlayerId(1),
                    object_id: oid,
                    ..
                } if oid == obj_id
            ),
            "Expected TopOrBottomChoice for stack spell owner (P1), got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn chooser_serde_defaults_and_omits_owner_form() {
        let legacy: Effect =
            serde_json::from_str(r#"{"type":"PutOnTopOrBottom","target":{"type":"Any"}}"#)
                .expect("legacy PutOnTopOrBottom without chooser must deserialize");
        assert!(matches!(
            legacy,
            Effect::PutOnTopOrBottom {
                chooser: TargetFilter::ParentTargetOwner,
                ..
            }
        ));

        let owner_chooser = Effect::PutOnTopOrBottom {
            target: TargetFilter::Any,
            chooser: TargetFilter::ParentTargetOwner,
            top_position: Default::default(),
        };
        let json = serde_json::to_string(&owner_chooser).unwrap();
        assert!(
            !json.contains("chooser"),
            "default owner chooser must be omitted from JSON, got {json}"
        );

        let controller_chooser = Effect::PutOnTopOrBottom {
            target: TargetFilter::SelfRef,
            chooser: TargetFilter::Controller,
            top_position: Default::default(),
        };
        let json = serde_json::to_string(&controller_chooser).unwrap();
        assert!(json.contains("chooser"));
        let round_trip: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip, controller_chooser);
    }

    // Arashin Sovereign's real Oracle text (verified against card-data.json).
    const ARASHIN_SOVEREIGN: &str =
        "Flying\nWhen this creature dies, you may put it on the top or bottom of its owner's library.";

    /// CR 730.3 + CR 730.3c: drive the REAL dies-trigger pipeline for a MERGED
    /// Arashin Sovereign — a second card is merged onto it, so its death splits
    /// the pile before the trigger resolves. Returns the runner parked on
    /// `TopOrBottomChoice` plus the survivor and absorbed component ids; both
    /// belong to player 1, so the same owner arranges them.
    fn merged_arashin_at_choice() -> (crate::game::scenario::GameRunner, ObjectId, ObjectId) {
        use crate::game::merge::{merge_object_onto, MergeSide};
        use crate::game::scenario::{GameScenario, P1};
        use crate::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P1, &["Filler A", "Filler B"]);
        let arashin = scenario
            .add_creature_from_oracle(P1, "Arashin Sovereign", 5, 7, ARASHIN_SOVEREIGN)
            .id();
        let rider = scenario.add_creature(P1, "Mutating Rider", 4, 4).id();
        let mut runner = scenario.build();

        // Runtime invariant: the mutating spell resolved off the stack and is
        // never an independent member of the battlefield list.
        runner.state_mut().battlefield.retain(|&id| id != rider);
        let mut events = Vec::new();
        merge_object_onto(
            runner.state_mut(),
            rider,
            arashin,
            MergeSide::Top,
            &mut events,
        );

        // Kill the merged permanent: the leave split (CR 730.3) separates the
        // components, so only their recorded successors remain by answer time.
        events.clear();
        crate::game::zones::move_to_zone(runner.state_mut(), arashin, Zone::Graveyard, &mut events);
        crate::game::triggers::process_triggers(runner.state_mut(), &events);
        assert_eq!(
            runner.state().stack.len(),
            1,
            "the merged permanent's dies trigger must be on the stack"
        );

        // Accept the optional "you may" and pass priority until the tuck choice.
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(
                guard < 64,
                "resolution exceeded safety bound; waiting_for = {} stack = {}",
                runner.waiting_for_kind(),
                runner.state().stack.len()
            );
            match runner.waiting_for_kind() {
                "OptionalEffectChoice" => {
                    runner
                        .act(crate::types::actions::GameAction::DecideOptionalEffect {
                            accept: true,
                        })
                        .expect("accept the optional dies trigger");
                }
                "TopOrBottomChoice" => break,
                "Priority" => {
                    runner
                        .act(crate::types::actions::GameAction::PassPriority)
                        .expect("pass priority toward the tuck choice");
                }
                other => panic!("unexpected prompt {other} before the tuck choice"),
            }
        }

        (runner, arashin, rider)
    }

    fn library_of(runner: &crate::game::scenario::GameRunner, player: PlayerId) -> Vec<ObjectId> {
        runner
            .state()
            .players
            .iter()
            .find(|seat| seat.id == player)
            .expect("player seat exists")
            .library
            .iter()
            .copied()
            .collect()
    }

    /// CR 730.3c: answer the parked choice and let the owner's arrangement
    /// deliver the whole pile. Asserts the arrangement offers every physical
    /// component and that the submitted order lands as one block at `position`.
    fn answer_split_choice(
        runner: crate::game::scenario::GameRunner,
        arashin: ObjectId,
        rider: ObjectId,
        top: bool,
        submitted: [ObjectId; 2],
        expected_library: [ObjectId; 4],
    ) {
        use crate::game::scenario::P1;
        use crate::types::actions::GameAction;

        let mut runner = runner;

        // The prompt itself must carry the recorded split successors: live
        // expansion cannot recover them once the pile has left the battlefield.
        let recorded = match &runner.state().waiting_for {
            WaitingFor::TopOrBottomChoice { components, .. } => components.clone(),
            other => panic!("expected TopOrBottomChoice, got {other:?}"),
        };
        assert!(
            recorded.contains(&arashin) && recorded.contains(&rider),
            "the prompt must record every physical component of the merged \
             permanent that split on departure (CR 730.3c); got {recorded:?}"
        );

        runner
            .act(GameAction::ChooseTopOrBottom { top })
            .expect("answer the top-or-bottom choice");

        // CR 401.4 + CR 730.3c: more than one card arrives at once, so their
        // owner must arrange all of them — every eligible component is offered.
        assert_eq!(
            runner.waiting_for_kind(),
            "EffectZoneChoice",
            "a multi-card pile must open the owner's arrangement, got {}",
            runner.waiting_for_kind()
        );
        let offered = match &runner.state().waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => cards.clone(),
            other => panic!("expected the owner arrangement prompt, got {other:?}"),
        };
        assert_eq!(
            offered.len(),
            2,
            "the arrangement must offer both physical components, got {offered:?}"
        );
        assert!(offered.contains(&arashin) && offered.contains(&rider));

        runner
            .act(GameAction::SelectCards {
                cards: submitted.to_vec(),
            })
            .expect("submit the owner's order");

        // Drain any priority passes the resolved trigger still wants.
        let mut guard = 0;
        while !runner.state().stack.is_empty() {
            guard += 1;
            assert!(guard < 64, "stack never emptied after the arrangement");
            assert_eq!(runner.waiting_for_kind(), "Priority");
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority after the arrangement");
        }

        for id in [arashin, rider] {
            assert_eq!(
                runner.state().objects[&id].zone,
                Zone::Library,
                "every component must move to the library (CR 730.3c)"
            );
        }
        let library = library_of(&runner, P1);
        assert_eq!(
            library, expected_library,
            "the submitted block must land, in order, as one block (CR 730.3c)"
        );
    }

    #[test]
    fn merged_arashin_dies_and_every_component_goes_on_top() {
        use crate::game::scenario::P1;
        let (runner, arashin, rider) = merged_arashin_at_choice();
        let fillers: Vec<ObjectId> = library_of(&runner, P1);
        assert_eq!(fillers.len(), 2, "premise: two filler cards in the library");
        answer_split_choice(
            runner,
            arashin,
            rider,
            true,
            [rider, arashin],
            [rider, arashin, fillers[0], fillers[1]],
        );
    }

    #[test]
    fn merged_arashin_dies_and_every_component_goes_on_bottom() {
        use crate::game::scenario::P1;
        let (runner, arashin, rider) = merged_arashin_at_choice();
        let fillers: Vec<ObjectId> = library_of(&runner, P1);
        answer_split_choice(
            runner,
            arashin,
            rider,
            false,
            [arashin, rider],
            [fillers[0], fillers[1], arashin, rider],
        );
    }
}
