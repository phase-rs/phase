use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::restrictions;
use crate::game::zones;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

use super::engine::EngineError;

/// Outcome of a sacrifice attempt routed through the replacement pipeline.
pub(crate) enum SacrificeOutcome {
    /// Sacrifice completed (normally or via replacement redirect).
    Complete,
    /// A replacement effect requires player choice before sacrifice can proceed.
    /// Callers must handle this by surfacing the replacement choice to the player.
    NeedsReplacementChoice(PlayerId),
}

/// CR 701.21 + CR 118.3: Sacrifice a permanent — move to graveyard as a cost or effect.
/// Routes through replacement pipeline (e.g., Rest in Peace → exile).
///
/// Returns `SacrificeOutcome` so callers can handle the `NeedsChoice` case appropriately:
/// - Effect resolution: pause via `WaitingFor::ReplacementChoice`
/// - Cost payment: proceed with default sacrifice (extremely rare edge case)
pub(crate) fn sacrifice_permanent(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<SacrificeOutcome, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Sacrifice target not found".to_string()))?;
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::ActionNotAllowed(
            "Cannot sacrifice: permanent is not on the battlefield".to_string(),
        ));
    }

    // CR 701.21: "Can't be sacrificed" prevents this action. The effect/cost
    // invoking sacrifice resolves as if no permanent was sacrificed — no
    // graveyard move, no leaves-the-battlefield triggers, no events emitted.
    if crate::game::static_abilities::object_has_static_other(state, object_id, "CantBeSacrificed")
    {
        return Ok(SacrificeOutcome::Complete);
    }

    let proposed = ProposedEvent::Sacrifice {
        object_id,
        player_id: player,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            match apply_sacrifice_after_replacement(state, event, events) {
                SacrificeApply::Complete => {}
                SacrificeApply::NeedsChoice(player) => {
                    // `state.waiting_for` is already set by the inner pass.
                    return Ok(SacrificeOutcome::NeedsReplacementChoice(player));
                }
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(choice_player) => {
            return Ok(SacrificeOutcome::NeedsReplacementChoice(choice_player));
        }
    }

    Ok(SacrificeOutcome::Complete)
}

/// Outcome of delivering an accepted Sacrifice proposed event through the inner
/// graveyard-move replacement pass. `NeedsChoice` carries the player who must
/// order/choose among multiple applicable graveyard replacements (CR 616.1);
/// `state.waiting_for` is already set when it is returned.
pub(crate) enum SacrificeApply {
    Complete,
    NeedsChoice(PlayerId),
}

/// CR 701.21a + CR 614.1: Apply an accepted Sacrifice proposed event.
///
/// The sacrifice's move to the graveyard is itself a zone change subject to
/// replacement (CR 701.21a + CR 614.1), so this routes the inner graveyard
/// `ZoneChange` through the replacement pipeline — mirroring
/// `apply_destroy_after_replacement` — instead of moving straight to the
/// graveyard. A "would be put into a graveyard from anywhere → exile/redirect
/// instead" `Moved` replacement (Disturb back faces, Rest in Peace, Leyline of
/// the Void) therefore applies on sacrifice too. Records the sacrifice for
/// restriction tracking (CR 701.21) and emits `PermanentSacrificed` regardless
/// of the redirected zone (leaves-the-battlefield is still a sacrifice).
///
/// Shared by the cost/effect path (`sacrifice_permanent`) and the
/// post-replacement-choice delivery path (`handle_replacement_choice`).
pub(crate) fn apply_sacrifice_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> SacrificeApply {
    match event {
        ProposedEvent::Sacrifice {
            object_id: oid,
            player_id: pid,
            ..
        } => {
            // CR 701.21: record the sacrifice for restriction tracking before any
            // redirect — leaving the battlefield is still a sacrifice.
            restrictions::record_sacrifice(state, oid, pid);
            // CR 701.21a + CR 614.1: propose the inner graveyard move so a Moved
            // replacement can intercept it. The Sacrifice proposal carries no
            // source, so `cause` is None.
            let zone_proposed =
                ProposedEvent::zone_change(oid, Zone::Battlefield, Zone::Graveyard, None);
            match replacement::replace_event(state, zone_proposed, events) {
                ReplacementResult::Execute(ProposedEvent::ZoneChange { object_id, to, .. }) => {
                    zones::move_to_zone(state, object_id, to, events);
                    crate::game::layers::mark_layers_full(state);
                }
                // Defensive: the inner proposal is always a ZoneChange.
                ReplacementResult::Execute(_) => {}
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    // CR 616.1: an ordered/optional Moved replacement needs a
                    // choice — pause and let the caller resume.
                    state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                    return SacrificeApply::NeedsChoice(player);
                }
            }
            // CR 701.21: PermanentSacrificed fires regardless of the redirected
            // zone (leaves-the-battlefield is still a sacrifice).
            events.push(GameEvent::PermanentSacrificed {
                object_id: oid,
                player_id: pid,
            });
            SacrificeApply::Complete
        }
        ProposedEvent::ZoneChange {
            object_id: oid, to, ..
        } => {
            // Outer Sacrifice replacement redirected directly to a zone change.
            zones::move_to_zone(state, oid, to, events);
            crate::game::layers::mark_layers_full(state);
            SacrificeApply::Complete
        }
        _ => SacrificeApply::Complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{StaticDefinition, TargetFilter};
    use crate::types::identifiers::CardId;
    use crate::types::statics::StaticMode;

    #[test]
    fn sacrifice_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        let result = sacrifice_permanent(&mut state, obj_id, PlayerId(0), &mut events);
        assert!(matches!(result, Ok(SacrificeOutcome::Complete)));
        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    /// CR 701.21a + CR 614.1: a sacrificed permanent's inner graveyard move is
    /// subject to replacement. With a Rest-in-Peace-style "would be put into a
    /// graveyard → exile instead" Moved replacement in play, sacrifice must land
    /// the permanent in exile (not the graveyard), still emit
    /// `PermanentSacrificed`, and still record the sacrifice. This is the general
    /// fix for Disturb back faces and global graveyard hate on sacrifice.
    #[test]
    fn sacrifice_redirected_to_exile_by_moved_replacement() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let victim = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sacrifice Victim".to_string(),
            Zone::Battlefield,
        );

        // Rest in Peace: "If a card or token would be put into a graveyard from
        // anywhere, exile it instead." Modeled as a Moved (→Graveyard)
        // replacement that redirects to Exile.
        let rip_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut rip = GameObject::new(
            rip_id,
            CardId(888),
            PlayerId(1),
            "Rest in Peace".to_string(),
            Zone::Battlefield,
        );
        rip.replacement_definitions.push(
            crate::types::ability::ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Graveyard)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        destination: Zone::Exile,
                        origin: None,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        face_down_profile: None,
                    },
                ))
                .description("Rest in Peace".to_string()),
        );
        state.objects.insert(rip_id, rip);
        state.battlefield.push_back(rip_id);

        let mut events = Vec::new();
        let result = sacrifice_permanent(&mut state, victim, PlayerId(0), &mut events);

        assert!(matches!(result, Ok(SacrificeOutcome::Complete)));
        assert!(
            state.exile.contains(&victim),
            "the inner graveyard move must re-enter replacement; Moved→Exile sends the victim to exile"
        );
        assert!(
            !state.players[0].graveyard.contains(&victim),
            "redirected sacrifice must not land in the graveyard"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::PermanentSacrificed { object_id, .. } if *object_id == victim
            )),
            "PermanentSacrificed must fire regardless of the redirected zone"
        );
        assert_eq!(
            state.sacrificed_permanents_this_turn.len(),
            1,
            "the sacrifice must still be recorded for restriction tracking"
        );
    }

    #[test]
    fn sacrifice_emits_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        sacrifice_permanent(&mut state, obj_id, PlayerId(0), &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PermanentSacrificed { object_id, player_id }
                if *object_id == obj_id && *player_id == PlayerId(0)
        )));
    }

    #[test]
    fn sacrifice_artifact_records_restriction() {
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        let mut events = Vec::new();

        sacrifice_permanent(&mut state, obj_id, PlayerId(0), &mut events).unwrap();

        // record_sacrifice tracks artifact sacrifices for restriction checking
        assert!(state
            .players_who_sacrificed_artifact_this_turn
            .contains(&PlayerId(0)));
        assert_eq!(state.sacrificed_permanents_this_turn.len(), 1);
        let record = &state.sacrificed_permanents_this_turn[0];
        assert_eq!(record.object_id, obj_id);
        assert_eq!(record.controller, PlayerId(0));
        assert!(record.core_types.contains(&CoreType::Artifact));
    }

    #[test]
    fn cant_be_sacrificed_prevents_sacrifice() {
        // CR 701.21: A permanent with a `CantBeSacrificed` static cannot be sacrificed.
        let mut state = GameState::new_two_player(42);
        let victim = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sigarda".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&victim)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantBeSacrificed".to_string()))
                    .affected(TargetFilter::SelfRef),
            );

        let mut events = Vec::new();
        let result = sacrifice_permanent(&mut state, victim, PlayerId(0), &mut events);

        assert!(matches!(result, Ok(SacrificeOutcome::Complete)));
        // Permanent is still on the battlefield — sacrifice was a no-op.
        assert!(state.battlefield.contains(&victim));
        assert!(!state.players[0].graveyard.contains(&victim));
        // No PermanentSacrificed event was emitted.
        assert!(!events.iter().any(|e| matches!(
            e,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == victim
        )));
    }

    #[test]
    fn sacrifice_non_battlefield_errors() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();

        let result = sacrifice_permanent(&mut state, obj_id, PlayerId(0), &mut events);
        assert!(result.is_err());
    }
}
