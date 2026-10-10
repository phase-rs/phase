use crate::types::ability::{
    DelayedTriggerCondition, DelayedTriggerKind, DelayedTriggerLifetime, Effect, EffectError,
    EffectKind, ResolvedAbility, TargetFilter, TargetRef, TriggerDefinition,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::player::PlayerId;

/// Digital-only Alchemy (no CR entry): "you get a one-time boon with
/// `<ability>`" installs the granted trigger for the recipient as a
/// Persistent one-shot `WhenNextEvent` delayed trigger. One-shot removal,
/// intervening-if gating, cross-turn persistence, and cleanup survival all
/// reuse the CR 603.7 machinery; `DelayedTriggerKind::Boon` distinguishes the
/// entry for "if you have a boon" (`TriggerCondition::HasBoon`) and for the
/// boon-only embedded-condition gate in delayed matching.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (recipient, inner) = match &ability.effect {
        Effect::CreateBoon { recipient, trigger } => (recipient.clone(), trigger.as_ref().clone()),
        _ => {
            return Err(EffectError::MissingParam("CreateBoon".to_string()));
        }
    };
    let holder = resolve_recipient(state, ability, &recipient)?;
    let execute = inner.execute.clone().ok_or_else(|| {
        EffectError::MissingParam("boon inner trigger has no execute".to_string())
    })?;

    // CR 603.7a: the delayed condition carries the event matcher; the effect
    // lives in the delayed ability, not in the embedded trigger.
    let mut embedded = inner;
    embedded.execute = None;
    reanchor_holder_refs(&mut embedded, holder);

    let condition = DelayedTriggerCondition::WhenNextEvent {
        trigger: Box::new(embedded),
        or_trigger: None,
        lifetime: DelayedTriggerLifetime::Persistent,
    };

    // CR 603.7c + CR 608.2c: same forwarded-result rebind as
    // `delayed_trigger::resolve` — a forward-result continuation can
    // temporarily rebind `source_id`, but the boon remains owned by the
    // trigger source, which the captured provenance preserves.
    let delayed_source_id = ability
        .context
        .forwarded_result_context
        .as_ref()
        .and(ability.trigger_source.as_ref())
        .map(|source| source.identity.reference.object_id)
        .unwrap_or(ability.source_id);
    // CR 608.2h: propagate the creating ability's chain-root target list (see
    // `build_resolved_from_def_with_chain_root`'s doc). The delayed ability
    // is controlled by the HOLDER, not the creator, so every "you" inside
    // the granted body reads the holder.
    let mut delayed_ability = crate::game::ability_utils::build_resolved_from_def_with_chain_root(
        &execute,
        delayed_source_id,
        holder,
        ability.context.chain_root_targets.clone(),
    );

    // Same source-context propagation as `delayed_trigger::resolve`: matchers
    // read the source snapshot for event attribution, and the boon's source
    // is the creating object.
    let source_context = ability.trigger_source.clone().or_else(|| {
        state
            .objects
            .get(&ability.source_id)
            .map(|source| super::super::triggers::trigger_source_context_for_latch(state, source))
    });
    if let Some(mut source_context) = source_context {
        if source_context.identity.reference.object_id == delayed_ability.source_id {
            if let Some(obj) = state.objects.get(&delayed_ability.source_id) {
                source_context.identity.expected_zone = obj.zone;
                source_context.identity.reference.incarnation = obj.incarnation;
            }
        }
        delayed_ability.set_trigger_source_recursive(source_context);
    }
    // Digital-only Alchemy (no CR entry): the source-context setter stamps
    // the CREATOR as controller (correct for ordinary delayed triggers).
    // A boon belongs to its HOLDER, so re-stamp after — otherwise "you" in
    // the granted body would read the creator when they differ (Loch
    // Larent, Valiant Batrider).
    delayed_ability.set_controller_recursive(holder);

    // Digital-only Alchemy (no CR entry): per-grant note capture. A boon
    // whose body reads "the noted number" sees what its OWN resolution
    // noted — not whatever a later (or cross-card) note overwrote the
    // live global with before the boon fired. Snapshot the noting
    // player's RESOLUTION-LOCAL entry into the granted ability now; the
    // stored ability rides the delayed-fire path untouched, so the
    // capture survives fire, stack, and resolution with no trigger-entry
    // lookup (and no sensitivity to one-shot consumption timing). The
    // reader is the same subject the `NoteNumber` leg writes for (its
    // resolving player), and the slot is cleared at every top-level
    // resolution — so a grant whose own resolution noted nothing (even
    // with a stale live note from an earlier resolution) captures `None`
    // and reads fall back to the live global.
    let noting_player = ability.original_controller.unwrap_or(ability.controller);
    delayed_ability.context.boon_captured_noted_number = state
        .noted_numbers_this_resolution
        .iter()
        .find(|(player, _)| *player == noting_player)
        .map(|(_, noted)| *noted);

    // CR 701.27f + CR 400.7: same creation-time generation/incarnation
    // capture as `delayed_trigger::resolve`.
    let source = state
        .objects
        .get(&ability.source_id)
        .filter(|object| object.back_face.is_some());
    let source_transformation_count = source.map(|object| object.transformation_count);
    delayed_ability.set_source_transformation_count_recursive(source_transformation_count);
    delayed_ability.set_source_incarnation_recursive(source.map(|object| object.incarnation));

    crate::game::triggers::install_delayed_trigger(
        state,
        DelayedTrigger {
            condition,
            ability: Box::new(delayed_ability),
            controller: holder,
            source_id: delayed_source_id,
            one_shot: true,
            kind: DelayedTriggerKind::Boon,
            provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
        },
        events,
    );

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CreateBoon,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// Lower the parser-emitted recipient to the concrete boon holder.
///
/// * `Controller` ("you get") is the resolving ability's controller.
/// * `TriggeringPlayer` ("that player gets", Valiant Batrider) is the player
///   of the event that fired the creating trigger, read live from
///   `current_trigger_event`.
/// * A declarative player filter (`Typed`, `Player`, `Opponent` — Loch
///   Larent's "target opponent") or `ParentTarget` reads the first chosen
///   player target, which chain propagation in
///   `effects::mod.rs::resolve_ability_chain` copied onto this sub-ability.
/// * `SpecificPlayer` is already concrete.
fn resolve_recipient(
    state: &GameState,
    ability: &ResolvedAbility,
    recipient: &TargetFilter,
) -> Result<PlayerId, EffectError> {
    match recipient {
        TargetFilter::Controller => Ok(ability.controller),
        TargetFilter::SpecificPlayer { id } => Ok(*id),
        TargetFilter::TriggeringPlayer => state
            .current_trigger_event
            .as_ref()
            .and_then(|event| crate::game::targeting::extract_player_from_event(event, state))
            .ok_or_else(|| {
                EffectError::MissingParam("boon recipient has no triggering player".to_string())
            }),
        TargetFilter::ParentTarget
        | TargetFilter::Typed(_)
        | TargetFilter::Player
        | TargetFilter::Opponent => ability
            .targets
            .iter()
            .find_map(|target| match target {
                TargetRef::Player(id) => Some(*id),
                _ => None,
            })
            .ok_or_else(|| {
                EffectError::InvalidParam("boon recipient has no chosen player".to_string())
            }),
        _ => Err(EffectError::InvalidParam(
            "unsupported boon recipient".to_string(),
        )),
    }
}

/// Re-anchor the embedded matcher's holder-relative references to the
/// concrete holder.
///
/// Every `TargetFilter`-bearing matcher field — the four subject slots
/// (`valid_card`, `valid_target`, `valid_subject_player`, `valid_source`)
/// and each disjunctive zone-change clause's `valid_card` — routes through
/// the shared [`crate::game::filter::reanchor_filter_to_holder`] traversal,
/// so nested descendants (`TrackedSetFiltered` filters, `DistinctFrom`
/// references, `Typed` properties) re-anchor too. Each field is
/// transactional on its own; an incomplete bounded walk leaves that field
/// creator-relative (today's behavior for exotic shapes) rather than
/// publishing a partial rewrite.
///
/// Only `You`-shaped references are re-anchored. `Opponent` and the other
/// relative `ControllerRef`s have no concrete singular form and never appear
/// in a printed boon inner; execute-time "you" needs no rewrite because the
/// delayed ability is already controlled by the holder. Intervening-if
/// CONDITIONS are intentionally not walked here: the delayed path evaluates
/// them with `delayed.controller` (the holder) as the controller parameter,
/// so player-valued leaves are already holder-correct, and no printed boon
/// inner carries a holder-relative condition filter.
fn reanchor_holder_refs(inner: &mut TriggerDefinition, holder: PlayerId) {
    for slot in [
        inner.valid_card.as_mut(),
        inner.valid_target.as_mut(),
        inner.valid_subject_player.as_mut(),
        inner.valid_source.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        crate::game::filter::reanchor_filter_to_holder(slot, holder);
    }
    for clause in inner.zone_change_clauses.iter_mut() {
        if let Some(valid_card) = clause.valid_card.as_mut() {
            crate::game::filter::reanchor_filter_to_holder(valid_card, holder);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ControllerRef, OriginConstraint, TypedFilter, ZoneChangeClause};
    use crate::types::triggers::TriggerMode;

    /// M10: re-anchoring covers every matcher field — the four subject slots
    /// AND each zone-change clause — so no nested `You` keeps the creator.
    #[test]
    fn reanchor_covers_subject_player_and_zone_clauses() {
        let holder = PlayerId(1);
        let you_creature =
            || TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));
        let bound_creature = TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::SpecificPlayer { id: holder }),
        );

        let mut inner = TriggerDefinition::new(TriggerMode::SpellCast);
        inner.valid_card = Some(you_creature());
        inner.valid_target = Some(TargetFilter::Controller);
        inner.valid_subject_player = Some(TargetFilter::Controller);
        inner.valid_source = Some(you_creature());
        inner.zone_change_clauses = vec![ZoneChangeClause {
            origin: OriginConstraint::Any,
            destination: None,
            destination_constraint: OriginConstraint::Any,
            valid_card: Some(you_creature()),
        }];

        reanchor_holder_refs(&mut inner, holder);

        assert_eq!(inner.valid_card.as_ref(), Some(&bound_creature));
        assert_eq!(
            inner.valid_target,
            Some(TargetFilter::SpecificPlayer { id: holder })
        );
        assert_eq!(
            inner.valid_subject_player,
            Some(TargetFilter::SpecificPlayer { id: holder }),
            "alternate subject must re-anchor"
        );
        assert_eq!(inner.valid_source.as_ref(), Some(&bound_creature));
        assert_eq!(
            inner.zone_change_clauses[0].valid_card.as_ref(),
            Some(&bound_creature),
            "zone-clause filters must re-anchor"
        );
    }
}
