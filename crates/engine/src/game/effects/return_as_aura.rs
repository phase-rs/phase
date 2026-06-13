//! CR 614.1 + CR 614.12 + CR 303.4 + CR 303.4a + CR 303.4g + CR 613.1d +
//! CR 613.1f + CR 113.10 + CR 702.5a + CR 604.1 + CR 611.2a + CR 400.7:
//! Return-as-Aura resolver.
//!
//! Resolution sequence (after the preceding `Effect::ChangeZone` returned the
//! host object to the battlefield):
//!
//! 1. Locate the just-returned object via `state.last_zone_changed_ids`.
//! 2. Build the candidate list of legal attach targets by iterating
//!    battlefield objects and calling `filter::matches_target_filter` against
//!    `enchant_filter`. **NOT** `find_legal_targets` — CR 115.1 + CR 303.4f
//!    treat the Aura's attach choice as a CHOICE, not a target, so hexproof /
//!    shroud / protection (CR 702.16b) must **not** filter the candidate list.
//! 3. If no legal target exists, route a Battlefield → Graveyard
//!    `ProposedEvent::ZoneChange` through the replacement pipeline per
//!    CR 303.4g + CR 614.6 + CR 616.1 (so Rest in Peace, regen shields,
//!    etc. can intercept the LTB exactly as they would for any other
//!    leaves-the-battlefield event).
//! 4. If exactly one legal target exists, attach immediately via
//!    `finalize_attach`.
//! 5. If multiple legal targets exist, install
//!    `WaitingFor::ReturnAsAuraTarget` for a controller pick and return; the
//!    `engine.rs` apply arm for `(WaitingFor::ReturnAsAuraTarget,
//!    GameAction::ChooseTarget)` invokes `finalize_attach` after the pick.
//!
//! `finalize_attach` registers a single `TransientContinuousEffect` keyed to
//! the returned object with `Duration::UntilHostLeavesPlay` and the full
//! layer-appropriate modification list (Layer 4 type/subtype set, Layer 6
//! enchant keyword + granted abilities). The layer system dependency-orders
//! `RemoveAllAbilities` (when present at `grants[0]`) before any
//! `Grant*` per CR 613.1f + CR 613.8 (Layer-6 dependency ordering: within a
//! layer, an effect that would change the existence of another effect is
//! applied first; `RemoveAllAbilities` would remove the abilities `Grant*`
//! adds, so the grants depend on the removal and apply after it).

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::card_type::{CoreType, SubtypeSet};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 614.1 + CR 614.12: Resolve a `Effect::ReturnAsAura` sub-effect.
///
/// Pre-condition: the preceding `Effect::ChangeZone { destination: Battlefield,
/// target: TriggeringSource }` (or equivalent) has populated
/// `state.last_zone_changed_ids` with exactly the returned object.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (enchant_filter, grants) = match &ability.effect {
        Effect::ReturnAsAura {
            enchant_filter,
            grants,
        } => (enchant_filter.clone(), grants.clone()),
        _ => {
            return Err(EffectError::InvalidParam(
                "return_as_aura::resolve called with non-ReturnAsAura effect".to_string(),
            ));
        }
    };

    // CR 614.1c: Locate the just-returned object. If empty (the preceding
    // ChangeZone was intercepted by a replacement effect or SBA moved the
    // object), emit EffectResolved and return Ok — nothing to attach.
    //
    // The lookup is keyed on `ability.source_id`: only the host whose
    // own trigger/spell resolved this effect is a valid return target.
    // Scanning `last_zone_changed_ids` blindly would collide with other
    // objects that changed zone in the same resolution step (e.g., tokens
    // created mid-chain, sibling triggers' bounces).
    let returned_id = match find_returned_object(state, ability.source_id) {
        Some(id) => id,
        None => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ReturnAsAura,
                source_id: ability.source_id,
            });
            return Ok(());
        }
    };

    // CR 115.1 + CR 303.4f + CR 303.4g: Build candidate list via
    // matches_target_filter (NOT find_legal_targets) — Aura attach is a
    // choice, not a target.
    let ctx = FilterContext::from_ability(ability);
    let candidates: Vec<ObjectId> = state
        .objects
        .iter()
        .filter_map(|(id, obj)| {
            if obj.zone != Zone::Battlefield {
                return None;
            }
            if *id == returned_id {
                // CR 303.4d: An Aura can't enchant itself.
                return None;
            }
            if matches_target_filter(state, *id, &enchant_filter, &ctx) {
                Some(*id)
            } else {
                None
            }
        })
        .collect();

    if candidates.is_empty() {
        // CR 614.12 + CR 113.10 + CR 303.4g: The returned host becomes an Aura
        // and loses its other abilities (including its own dies trigger) before
        // the no-target graveyard move. Without stripping first, the subsequent
        // LTB event re-queues the same self-return trigger forever (issue #1332).
        install_aura_continuous_effect(state, ability, returned_id, &enchant_filter, grants);
        crate::game::layers::evaluate_layers(state);
        // CR 614.12 + CR 303.4g: Ensure the imminent Battlefield → Graveyard
        // snapshot omits stripped triggers. `evaluate_layers` applies
        // `RemoveAllAbilities` to the live list, but the zone-change record
        // clones `trigger_definitions.iter_all()` at move time — an explicit
        // clear guarantees LTB look-back (issue #1332) cannot re-queue the
        // self-return trigger from a stale live list.
        if let Some(obj) = state.objects.get_mut(&returned_id) {
            obj.trigger_definitions.clear();
        }

        // CR 303.4g + CR 614.6 + CR 616.1: No legal object to enchant → owner's
        // graveyard. Route the Battlefield → Graveyard move through the
        // replacement pipeline so leaves-the-battlefield replacements
        // (Rest in Peace → exile, Leyline of the Void → exile, regen shields,
        // etc.) can intercept the zone change exactly as they would for any
        // other LTB event.
        let proposed = ProposedEvent::zone_change(
            returned_id,
            Zone::Battlefield,
            Zone::Graveyard,
            Some(ability.source_id),
        );
        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(ProposedEvent::ZoneChange {
                object_id,
                to: dest,
                ..
            }) => {
                crate::game::zones::move_to_zone(state, object_id, dest, events);
                crate::game::layers::mark_layers_full(state);
            }
            ReplacementResult::Execute(_) => {
                // Pipeline preserves the event variant; other variants are
                // unreachable for a `zone_change`-seeded pipeline.
            }
            ReplacementResult::Prevented => {
                // Move was prevented by a replacement (e.g., regen shield);
                // CR 704.5n SBA will sweep any residual unattached Aura.
            }
            ReplacementResult::NeedsChoice(player) => {
                // CR 616.1: Multi-replacement player choice — install the
                // picker and defer EffectResolved. After the player picks,
                // CR 704.5n SBA sweeps the still-orphaned Aura on the next
                // pass, yielding the same end state for the rare contested
                // case (no post-replacement continuation is stashed here).
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
        }
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ReturnAsAura,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    if candidates.len() == 1 {
        let target_id = candidates[0];
        finalize_attach(
            state,
            ability,
            returned_id,
            target_id,
            &enchant_filter,
            grants,
            events,
        )?;
        return Ok(());
    }

    // CR 303.4g + CR 115.1: Multiple legal candidates — install the picker.
    state.waiting_for = WaitingFor::ReturnAsAuraTarget {
        player: ability.controller,
        source_id: ability.source_id,
        returned_id,
        legal_targets: candidates.into_iter().map(TargetRef::Object).collect(),
        pending_effect: Box::new(ability.clone()),
    };
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ReturnAsAura,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 614.1d + CR 113.10 + CR 303.4a + CR 702.5a: Install the Aura's
/// continuous effect on the returned object and attach it to `target_id`.
///
/// Builds a single `TransientContinuousEffect` carrying:
/// - Layer 4 (CR 613.1d): `SetCardTypes { core_types: [Enchantment] }`,
///   `RemoveAllSubtypes { set: SubtypeSet::Creature }`,
///   `AddSubtype { subtype: "Aura" }`.
/// - Layer 6 (CR 613.1f): `AddKeyword { keyword: Keyword::Enchant(filter) }`
///   followed by every `ContinuousModification` from `grants` (which may
///   start with `RemoveAllAbilities` — Layer 6 dependency rule CR 613.8
///   ensures it applies before the `Grant*` modifications, because a
///   `Grant*` effect depends on whether removal occurred per CR 613.8a).
///
/// Duration is hard-coded to `Duration::UntilHostLeavesPlay` per CR 611.2a +
/// CR 400.7: a new object on re-entry is not the same object, so the prior
/// continuous effect implicitly ends.
fn install_aura_continuous_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    returned_id: ObjectId,
    enchant_filter: &TargetFilter,
    grants: Vec<ContinuousModification>,
) {
    let mut modifications: Vec<ContinuousModification> = Vec::with_capacity(grants.len() + 4);
    modifications.push(ContinuousModification::SetCardTypes {
        core_types: vec![CoreType::Enchantment],
    });
    modifications.push(ContinuousModification::RemoveAllSubtypes {
        set: SubtypeSet::Creature,
    });
    modifications.push(ContinuousModification::AddSubtype {
        subtype: "Aura".to_string(),
    });
    modifications.push(ContinuousModification::AddKeyword {
        keyword: Keyword::Enchant(enchant_filter.clone()),
    });
    modifications.extend(grants);
    state.add_transient_continuous_effect(
        ability.source_id,
        ability.controller,
        Duration::UntilHostLeavesPlay,
        TargetFilter::SpecificObject { id: returned_id },
        modifications,
        None,
    );
}

pub(crate) fn finalize_attach(
    state: &mut GameState,
    ability: &ResolvedAbility,
    returned_id: ObjectId,
    target_id: ObjectId,
    enchant_filter: &TargetFilter,
    grants: Vec<ContinuousModification>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !state.objects.contains_key(&returned_id) {
        return Err(EffectError::ObjectNotFound(returned_id));
    }

    install_aura_continuous_effect(state, ability, returned_id, enchant_filter, grants);

    // CR 701.3 + CR 303.4: attach the Aura to the chosen permanent. This is
    // a silent no-op if the target carries `CantBeEnchanted` / `CantBeAttached`
    // (CR 701.3 / CR 702.5 / CR 702.6) — the next SBA pass will then move the
    // newly-orphaned Aura to its owner's graveyard per CR 704.5n.
    let _ = crate::game::effects::attach::attach_to(state, returned_id, target_id);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ReturnAsAura,
        source_id: ability.source_id,
    });
    Ok(())
}

/// Verify that `source_id` is the host that was just returned to the
/// battlefield by the preceding `ChangeZone` step.
///
/// Returns `Some(source_id)` only if `source_id` appears in
/// `state.last_zone_changed_ids` AND currently sits in `Zone::Battlefield`.
/// Returns `None` otherwise — either the preceding `ChangeZone` was
/// intercepted by a replacement effect (Rest in Peace, etc.), or the host has
/// already left play (SBA, secondary replacement, blink). Looking up by
/// `source_id` (rather than scanning the list in reverse) prevents collisions
/// with other objects that changed zone in the same resolution step.
fn find_returned_object(state: &GameState, source_id: ObjectId) -> Option<ObjectId> {
    if !state.last_zone_changed_ids.contains(&source_id) {
        return None;
    }
    state
        .objects
        .get(&source_id)
        .filter(|obj| obj.zone == Zone::Battlefield)
        .map(|_| source_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, TypeFilter, TypedFilter};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    fn forest_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Subtype("Forest".to_string())],
            controller: Some(ControllerRef::You),
            ..TypedFilter::default()
        })
    }

    fn creature_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: Some(ControllerRef::You),
            ..TypedFilter::default()
        })
    }

    /// Build a state where a creature `host` was just returned to the
    /// battlefield (populating `last_zone_changed_ids`) and call `resolve`
    /// with a `ReturnAsAura` effect.
    fn setup_return(state: &mut GameState, owner: PlayerId, host_subtype: &str) -> ObjectId {
        let host = create_object(
            state,
            CardId(99),
            owner,
            "Creature".to_string(),
            Zone::Battlefield,
        );
        if let Some(obj) = state.objects.get_mut(&host) {
            obj.card_types.subtypes.push(host_subtype.to_string());
        }
        state.last_zone_changed_ids.push(host);
        host
    }

    #[test]
    fn single_legal_target_attaches_immediately() {
        let mut state = GameState::new_two_player(7);
        let host = setup_return(&mut state, PlayerId(0), "Cat");

        // One Forest controlled by P0.
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );
        if let Some(obj) = state.objects.get_mut(&forest) {
            obj.card_types.subtypes.push("Forest".to_string());
        }

        let ability = ResolvedAbility::new(
            Effect::ReturnAsAura {
                enchant_filter: forest_filter(),
                grants: vec![],
            },
            vec![],
            host,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Exactly one TransientContinuousEffect installed on host.
        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.affected, TargetFilter::SpecificObject { id: host });
        assert_eq!(tce.duration, Duration::UntilHostLeavesPlay);
        // Layer 4 + Layer 6 mods present in the install order.
        assert!(tce
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetCardTypes { core_types } if core_types == &vec![CoreType::Enchantment])));
        assert!(tce.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddSubtype { subtype } if subtype == "Aura"
        )));
        assert!(tce.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Enchant(_)
            }
        )));
        // CR 701.3a + CR 303.4: attach_to MUST have wired both sides of the
        // attachment, not silently no-op'd. (Silent no-ops would happen if the
        // target had CantBeEnchanted or CantBeAttached — neither is set here.)
        let host_attached_to = state.objects.get(&host).and_then(|o| o.attached_to);
        assert_eq!(
            host_attached_to,
            Some(forest.into()),
            "host.attached_to should point at the Forest after attach"
        );
        let forest_attachments = state
            .objects
            .get(&forest)
            .map(|o| o.attachments.clone())
            .unwrap_or_default();
        assert!(
            forest_attachments.contains(&host),
            "Forest.attachments should contain the host (id={host:?}), got {forest_attachments:?}"
        );
    }

    #[test]
    fn multi_target_installs_waiting_for_picker() {
        let mut state = GameState::new_two_player(7);
        let host = setup_return(&mut state, PlayerId(0), "Cat");

        // Two Forests controlled by P0.
        for i in 0..2 {
            let forest = create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                "Land".to_string(),
                Zone::Battlefield,
            );
            if let Some(obj) = state.objects.get_mut(&forest) {
                obj.card_types.subtypes.push("Forest".to_string());
            }
        }

        let ability = ResolvedAbility::new(
            Effect::ReturnAsAura {
                enchant_filter: forest_filter(),
                grants: vec![],
            },
            vec![],
            host,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ReturnAsAuraTarget {
                player,
                returned_id,
                legal_targets,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*returned_id, host);
                assert_eq!(legal_targets.len(), 2);
            }
            other => panic!("expected ReturnAsAuraTarget, got {other:?}"),
        }
        // No transient continuous effect should be installed yet — that happens
        // after the player picks.
        assert!(state.transient_continuous_effects.is_empty());
    }

    #[test]
    fn no_legal_target_strips_live_trigger_without_mutating_baseline() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, QuantityExpr, TriggerDefinition,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(7);
        let host = setup_return(&mut state, PlayerId(0), "Cat");
        let mut dies_trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        dies_trigger.origin = Some(Zone::Battlefield);
        dies_trigger.destination = Some(Zone::Graveyard);
        dies_trigger.valid_card = Some(TargetFilter::SelfRef);
        dies_trigger.trigger_zones = vec![Zone::Graveyard];
        dies_trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Database,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )));
        {
            let obj = state.objects.get_mut(&host).unwrap();
            std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(dies_trigger.clone());
            obj.trigger_definitions.push(dies_trigger);
        }

        let ability = ResolvedAbility::new(
            Effect::ReturnAsAura {
                enchant_filter: creature_filter(),
                grants: vec![ContinuousModification::RemoveAllAbilities],
            },
            vec![],
            host,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.objects[&host].zone, Zone::Graveyard);
        assert!(!state.objects[&host].base_trigger_definitions.is_empty());
        let GameEvent::ZoneChanged { record, .. } = events
            .iter()
            .find(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == host))
            .expect("return-as-aura no-target path should emit a ZoneChanged event")
        else {
            panic!("expected ZoneChanged event");
        };
        assert!(record.trigger_definitions.is_empty());
    }

    #[test]
    fn no_legal_target_goes_to_graveyard() {
        let mut state = GameState::new_two_player(7);
        let host = setup_return(&mut state, PlayerId(0), "Cat");
        // No Forests on the battlefield.

        let ability = ResolvedAbility::new(
            Effect::ReturnAsAura {
                enchant_filter: forest_filter(),
                grants: vec![],
            },
            vec![],
            host,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 303.4g: host moved to graveyard.
        let host_zone = state.objects.get(&host).map(|o| o.zone);
        assert_eq!(host_zone, Some(Zone::Graveyard));
        // No transient effect installed.
        assert!(state.transient_continuous_effects.is_empty());
    }

    #[test]
    fn self_attach_forbidden_when_no_other_candidate() {
        let mut state = GameState::new_two_player(7);
        // Host is a creature, not a creature-you-control filter member in any
        // useful way after type changes, but ReturnAsAura specifically excludes
        // `returned_id` from candidates anyway.
        let host = setup_return(&mut state, PlayerId(0), "Cat");

        let ability = ResolvedAbility::new(
            Effect::ReturnAsAura {
                enchant_filter: creature_filter(),
                grants: vec![],
            },
            vec![],
            host,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // With no OTHER creature on the battlefield, the host should move to
        // graveyard per CR 303.4g — the self-attach exclusion in `resolve`
        // takes the only candidate off the list.
        let host_zone = state.objects.get(&host).map(|o| o.zone);
        assert_eq!(host_zone, Some(Zone::Graveyard));
    }
}
