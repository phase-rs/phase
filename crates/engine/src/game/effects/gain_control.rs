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

    // CR 613.3 + CR 102.1: "the player to your left/right" resolves to a single
    // living seating-neighbor (CR 101.4 / CR 103.1). `game::players::neighbor`
    // already skips eliminated players, so this returns one player and bypasses
    // the generic ambiguity loop below.
    if let TargetFilter::Neighbor { direction } = filter {
        return Ok(crate::game::players::neighbor(
            state,
            source_controller,
            *direction,
        ));
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

    /// CR 102.1 + CR 103.1 + CR 613.3: "The player to your right gains control
    /// of this artifact" (Bucknard's Everfull Purse). Drives the real recipient
    /// path: `resolve_give` → `unique_recipient_from_filter` →
    /// `players::neighbor(Right)` = `previous_player`. In a 3-player game with
    /// seat_order [P0,P1,P2] and controller P0, RIGHT = P2 (previous seat),
    /// distinct from LEFT = P1 (next seat) — so this discriminates the seat
    /// direction AND proves the single-recipient (no-ambiguity) resolution.
    #[test]
    fn give_control_to_player_to_the_right_targets_previous_seat() {
        use crate::types::ability::SeatDirection;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        assert_eq!(
            state.seat_order,
            vec![PlayerId(0), PlayerId(1), PlayerId(2)]
        );

        // The artifact (Bucknard's) controlled by the activator P0.
        let artifact = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bucknard's Everfull Purse".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::Neighbor {
                    direction: SeatDirection::Right,
                },
            },
            vec![TargetRef::Object(artifact)],
            artifact,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&artifact).unwrap().controller,
            PlayerId(2),
            "player to your right = previous seat (P2), not next seat (P1)"
        );
    }

    /// CR 706.2 + CR 102.1 + CR 103.1 + CR 613.3: End-to-end resolution of
    /// Bucknard's Everfull Purse's activated ability
    /// (`{1}, {T}: Roll a d4 and create a number of Treasure tokens equal to
    /// the result. The player to your right gains control of this artifact.`)
    /// through the REAL chain pipeline. This is the combined-ability test the
    /// two unit tests (token-count parse + 3/4-player Neighbor{Right} recipient)
    /// don't cover individually: it drives `RollDie → Token{count:
    /// EventContextAmount} → GiveControl{recipient: Neighbor{Right}}` through
    /// `resolve_ability_chain` and asserts both effects on the post-resolution
    /// state.
    ///
    /// (a) CR 706.2: exactly N Treasures are created where N is the d4 result
    ///     READ FROM the emitted `GameEvent::DieRolled` (not hard-coded), so the
    ///     `EventContextAmount` count snapshot is proven to flow from the roll.
    /// (b) CR 102.1 + CR 103.1 + CR 613.3: control of the Purse transfers to the
    ///     controller's RIGHT neighbor = previous seat. In [P0,P1,P2] with
    ///     controller P0, RIGHT = P2 (previous seat), distinct from LEFT = P1.
    #[test]
    fn bucknards_everfull_purse_full_chain_rolls_treasures_and_passes_right() {
        use crate::game::players::previous_player;
        use crate::types::ability::{PtValue, QuantityExpr, QuantityRef, SeatDirection};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        assert_eq!(
            state.seat_order,
            vec![PlayerId(0), PlayerId(1), PlayerId(2)]
        );

        // Bucknard's Everfull Purse, controlled by the activator P0.
        let purse = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bucknard's Everfull Purse".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&purse).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
        }

        // Build the resolved ability EXACTLY as the parser produces it:
        //   RollDie{sides:4}
        //     └─ Token{name:"Treasure", count: EventContextAmount, owner: Controller}
        //          └─ GiveControl{target: SelfRef, recipient: Neighbor{Right}}
        let give_control = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::Neighbor {
                    direction: SeatDirection::Right,
                },
            },
            vec![TargetRef::Object(purse)],
            purse,
            PlayerId(0),
        );
        let create_treasures = ResolvedAbility::new(
            Effect::Token {
                name: "Treasure".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Treasure".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                // CR 706.2: "a number of Treasure tokens equal to the result"
                // parses to an EventContextAmount count, snapshotted from the
                // preceding RollDie's DieRolled event.
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            purse,
            PlayerId(0),
        )
        .sub_ability(give_control);
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 4,
                results: vec![],
                modifier: None,
            },
            vec![],
            purse,
            PlayerId(0),
        )
        .sub_ability(create_treasures);

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        // (a) CR 706.2: read N from the emitted DieRolled event — never hard-coded.
        let roll = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => Some(*result as usize),
                _ => None,
            })
            .expect("RollDie must emit a DieRolled event");
        assert!((1..=4).contains(&roll), "d4 result out of range: {roll}");

        // Count Treasure tokens controlled by the activator (owner = Controller).
        // After the GiveControl sub-effect the Purse moves to P2, so filtering on
        // the Treasure subtype (not all P0-controlled artifacts) isolates the
        // tokens from the artifact itself.
        let treasure_count = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| {
                o.card_types.subtypes.contains(&"Treasure".to_string())
                    && o.controller == PlayerId(0)
            })
            .count();
        assert_eq!(
            treasure_count, roll,
            "must create exactly N={roll} Treasures (the d4 result), not a hard-coded count",
        );
        // Treasure tokens are colorless artifacts — sanity-check the type line so
        // a future Token-shape regression can't silently pass the count check.
        assert!(
            state
                .battlefield
                .iter()
                .filter_map(|id| state.objects.get(id))
                .filter(|o| o.card_types.subtypes.contains(&"Treasure".to_string()))
                .all(
                    |o| o.card_types.core_types.contains(&CoreType::Artifact) && o.color.is_empty()
                ),
            "Treasure tokens must be colorless artifacts",
        );

        // (b) CR 102.1 + CR 103.1 + CR 613.3: control passed to the RIGHT
        // neighbor = previous seat = P2 (distinct from LEFT = P1).
        assert_eq!(
            previous_player(&state, PlayerId(0)),
            PlayerId(2),
            "right neighbor of P0 in [P0,P1,P2] is the previous seat P2",
        );
        assert_eq!(
            state.objects.get(&purse).unwrap().controller,
            PlayerId(2),
            "the Purse must transfer to the player on the controller's right (P2), not the left (P1)",
        );
        assert_ne!(
            state.objects.get(&purse).unwrap().controller,
            PlayerId(1),
            "control must NOT go to the LEFT neighbor (next seat P1)",
        );
    }

    /// CR 102.1 + CR 800.4b: When the immediate right-neighbor has left the
    /// game, "the player to your right" skips to the next living seat
    /// counter-clockwise. In a 4-player game [P0,P1,P2,P3] with controller P0,
    /// the immediate right is P3; eliminating P3 routes control to P2.
    #[test]
    fn give_control_to_the_right_skips_eliminated_neighbor() {
        use crate::types::ability::SeatDirection;

        let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
        assert_eq!(
            state.seat_order,
            vec![PlayerId(0), PlayerId(1), PlayerId(2), PlayerId(3)]
        );
        // Eliminate the immediate right neighbor (P3).
        state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(3))
            .unwrap()
            .is_eliminated = true;

        let artifact = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bucknard's Everfull Purse".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::Neighbor {
                    direction: SeatDirection::Right,
                },
            },
            vec![TargetRef::Object(artifact)],
            artifact,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&artifact).unwrap().controller,
            PlayerId(2),
            "eliminated right neighbor (P3) is skipped; control passes to P2"
        );
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
