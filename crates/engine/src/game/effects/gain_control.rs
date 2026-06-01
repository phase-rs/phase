use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;

/// CR 613.3: GainControl creates a transient continuous effect that changes the
/// target permanent's controller through the layer system (Layer 2).
///
/// The duration comes from the resolved ability: "until end of turn" → UntilEndOfTurn,
/// permanent control change → Permanent (indefinite). The layer system handles
/// reverting control when the effect expires.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 613.1b: Layer 2 — control-changing effects are applied.
    let duration = ability.duration.clone().unwrap_or(Duration::Permanent);

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            // Verify target exists
            if !state.objects.contains_key(obj_id) {
                return Err(EffectError::ObjectNotFound(*obj_id));
            }

            // CR 613.3: Create a transient continuous effect at Layer 2 (Control).
            // The affected filter targets this specific object by ID.
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificObject { id: *obj_id },
                vec![ContinuousModification::ChangeController],
                None,
            );
            mark_echo_due_for_new_controller(state, *obj_id);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 110.2: Give control of target permanent to a specified recipient player.
/// Unlike `resolve` (controller takes), this transfers to a different player
/// specified by the recipient target.
pub fn resolve_give(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let duration = ability.duration.clone().unwrap_or(Duration::Permanent);

    // CR 110.2 + CR 613.3: The recipient is the player target when one is
    // explicitly in ability.targets (normal targeting path). When no player
    // target is present — e.g. a post-replacement continuation whose target
    // list only carries the damaged object — resolve the effect's `recipient`
    // filter only if it identifies exactly one legal player. CR 608.2d choices
    // are made while applying the effect; arbitrary first-match selection would
    // be wrong in multiplayer when several opponents are legal.
    let recipient_id = if let Some(pid) = ability.targets.iter().find_map(|t| {
        if let TargetRef::Player(pid) = t {
            Some(*pid)
        } else {
            None
        }
    }) {
        pid
    } else if let Effect::GiveControl { recipient, .. } = &ability.effect {
        unique_recipient_from_filter(state, recipient, ability.controller)?
    } else {
        ability.controller
    };

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            if !state.objects.contains_key(obj_id) {
                return Err(EffectError::ObjectNotFound(*obj_id));
            }

            // CR 613.3: Create a transient continuous effect at Layer 2 (Control)
            // with the recipient as the new controller.
            state.add_transient_continuous_effect(
                ability.source_id,
                recipient_id,
                duration.clone(),
                TargetFilter::SpecificObject { id: *obj_id },
                vec![ContinuousModification::ChangeController],
                None,
            );
            mark_echo_due_for_new_controller(state, *obj_id);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GiveControl,
        source_id: ability.source_id,
    });

    Ok(())
}

fn unique_recipient_from_filter(
    state: &GameState,
    filter: &TargetFilter,
    source_controller: PlayerId,
) -> Result<PlayerId, EffectError> {
    if let TargetFilter::SpecificPlayer { id } = filter {
        return state
            .players
            .iter()
            .find(|p| p.id == *id && !p.is_eliminated)
            .map(|p| p.id)
            .ok_or_else(|| EffectError::MissingParam("GiveControl recipient".to_string()));
    }

    let mut matching = state
        .players
        .iter()
        .filter(|p| {
            !p.is_eliminated
                && crate::game::filter::player_matches_target_filter(
                    filter,
                    p.id,
                    Some(source_controller),
                )
        })
        .map(|p| p.id);

    let Some(recipient) = matching.next() else {
        return Err(EffectError::MissingParam(
            "GiveControl recipient".to_string(),
        ));
    };

    if matching.next().is_some() {
        return Err(EffectError::MissingParam(
            "ambiguous GiveControl recipient".to_string(),
        ));
    }
    Ok(recipient)
}

fn mark_echo_due_for_new_controller(state: &mut GameState, obj_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        if obj.keywords.iter().any(|kw| matches!(kw, Keyword::Echo(_))) {
            obj.echo_due = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, Effect, TargetFilter, TargetRef, TypedFilter};
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::zones::Zone;

    fn make_gain_control_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn gain_control_creates_transient_effect() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_gain_control_ability(target_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Verify a transient continuous effect was created
        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.controller, PlayerId(0));
        assert_eq!(tce.affected, TargetFilter::SpecificObject { id: target_id });
        assert_eq!(
            tce.modifications,
            vec![ContinuousModification::ChangeController]
        );
        assert!(state.layers_dirty.is_dirty());
    }

    /// CR 613.1b: Non-regression for Bug B (layer fix). After switching the
    /// ChangeController layer arm to trust `effect.controller` instead of
    /// `source.controller`, the standard gain-control flow (where caster is
    /// also source.controller) must still transfer control correctly through
    /// the full layer pipeline.
    #[test]
    fn gain_control_layer_pipeline_transfers_control() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        // Source (the Control Magic aura) is controlled by PlayerId(0) (the caster),
        // matching the real gain-control shape where source.controller == new controller.
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Control Magic".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            PlayerId(0),
            "target should now be controlled by the caster after gain_control"
        );
    }

    /// CR 110.2 + CR 613.1b: End-to-end layer pipeline test for
    /// `resolve_give` (Donate-style "give target permanent to target player").
    /// The recipient differs from both the caster and the source's controller,
    /// so this specifically exercises the post-Bug-B invariant that
    /// `effect.controller` is the single authority. Pre-fix, the layer read
    /// `source.controller` and ignored the resolver's recipient choice,
    /// silently giving the permanent to the caster instead of the recipient.
    #[test]
    fn give_control_layer_pipeline_transfers_to_recipient() {
        let mut state = GameState::new_two_player(42);
        // Target: the permanent to be donated. Initially controlled by the caster.
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gift".to_string(),
            Zone::Battlefield,
        );
        // Source (e.g. Donate on the stack) — controlled by the caster.
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Donate".to_string(),
            Zone::Stack,
        );
        // Recipient is the OPPONENT (PlayerId(1)), distinct from both caster
        // and source.controller. Pre-fix, layer pipeline would read
        // source.controller (= caster) and leave target with caster.
        let recipient = PlayerId(1);
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::Any,
                recipient: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id), TargetRef::Player(recipient)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_give(&mut state, &ability, &mut events).unwrap();

        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            recipient,
            "target should now be controlled by the recipient, not the caster or source.controller"
        );
    }

    /// CR 608.2d + CR 613.1b: When an untargeted "opponent gains control"
    /// effect has exactly one legal recipient, the resolver may derive that
    /// recipient from game state. This covers two-player Khârn continuations,
    /// whose inherited target list carries only the damaged object.
    #[test]
    fn give_control_derives_single_opponent_recipient() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kharn the Betrayer".to_string(),
            Zone::Battlefield,
        );
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Kharn Trigger".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![TargetRef::Object(target_id)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            PlayerId(1)
        );
    }

    /// CR 608.2d: If several opponents are legal for an untargeted recipient
    /// choice, resolving by iteration order would make a choice the player never
    /// made. The resolver fails closed until a proper resolution-time choice is
    /// available.
    #[test]
    fn give_control_rejects_ambiguous_opponent_recipient() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kharn the Betrayer".to_string(),
            Zone::Battlefield,
        );
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Kharn Trigger".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![TargetRef::Object(target_id)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve_give(&mut state, &ability, &mut events);

        assert!(matches!(
            result,
            Err(EffectError::MissingParam(message)) if message == "ambiguous GiveControl recipient"
        ));
        assert!(events.is_empty());
    }

    /// CR 611.2b + CR 110.5d + CR 613.1b: Callous Oppressor regression (issue
    /// #498). A `ForAsLongAs { SourceIsTapped }` gain-control effect must end
    /// when the tapped source leaves the battlefield — an off-battlefield card
    /// is neither tapped nor untapped, so the duration condition becomes false
    /// and the Layer 2 base-controller reset reverts control to the owner.
    ///
    /// Reverted-fix-discriminating: pre-fix the graveyard Oppressor still has
    /// `tapped == true`, `SourceIsTapped` returns `true`, the `ChangeController`
    /// TCE keeps applying, and the final assertion fails.
    #[test]
    fn gain_control_for_as_long_as_tapped_ends_when_source_leaves_battlefield() {
        use crate::types::ability::{Duration, StaticCondition};

        let mut state = GameState::new_two_player(42);

        // The Oppressor: controlled by PlayerId(0), on the battlefield, tapped.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Callous Oppressor".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().tapped = true;

        // The stolen creature: owned/controlled by PlayerId(1).
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(
            state.objects.get(&target_id).unwrap().base_controller,
            Some(PlayerId(1)),
            "target's base controller should be its owner",
        );

        let mut ability = ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id)],
            source,
            PlayerId(0),
        );
        ability.duration = Some(Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsTapped,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            PlayerId(0),
            "control should be gained while the tapped Oppressor is on the battlefield",
        );

        // The Oppressor dies (or is otherwise removed) while still tapped.
        crate::game::zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut events);

        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            PlayerId(1),
            "control must revert to the owner once the tapped source leaves the battlefield",
        );
    }

    #[test]
    fn gain_control_nonexistent_target_returns_error() {
        let mut state = GameState::new_two_player(42);
        let ability = make_gain_control_ability(ObjectId(999));
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_err());
    }
}
