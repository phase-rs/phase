//! CR 702.171b: effect-level "becomes saddled" resolver.
//!
//! Distinct from the Saddle keyword's activated ability (CR 702.171a,
//! resolved in `stack.rs` as `KeywordAction::Saddle`, which is paid by tapping
//! creatures with total power N or greater): this handler grants the saddled
//! designation directly, without paying the saddle cost. It backs "becomes
//! saddled" instructions — Guidelight Matrix's `{2}, {T}: Target Mount you
//! control becomes saddled`, Kolodin's `Whenever a Mount you control enters, it
//! becomes saddled`, and Alacrian Armory's combat trigger.
//!
//! CR 702.171c records the creatures that *saddle* a permanent (tapped to pay
//! the cost). An effect-granted saddle has no such creatures, so `saddled_by`
//! is left empty and the `Saddled` event carries an empty creature list.

use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 702.171b: single authority for granting the saddled designation. Sets
/// `is_saddled = true` on a battlefield permanent, records the saddling
/// creatures (CR 702.171c) without duplicates, and emits `GameEvent::Saddled`
/// so any "becomes saddled" trigger fires. The event is emitted only on the
/// false→true transition: a permanent that is already saddled stays saddled
/// until end of turn (CR 702.171b), so re-saddling it never re-fires the
/// trigger. Used both by the Saddle keyword resolution (with the paid creatures)
/// and the `BecomeSaddled` effect (with an empty creature list — no cost was
/// paid).
///
/// No-op (and no event) when the target is no longer on the battlefield, so the
/// designation never lands on an object that has left play (CR 702.171b: only
/// permanents can be or become saddled).
pub fn mark_saddled(
    state: &mut GameState,
    mount_id: ObjectId,
    saddling_creatures: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) {
    let Some(mount) = state.objects.get_mut(&mount_id) else {
        return;
    };
    if mount.zone != Zone::Battlefield {
        return;
    }
    // CR 702.171b: "becomes saddled" is a false→true transition. A permanent
    // that is already saddled stays saddled until end of turn, so re-saddling it
    // must not re-fire "whenever ~ becomes saddled" triggers.
    let already_saddled = mount.is_saddled;
    mount.is_saddled = true;
    // CR 702.171c: record the creatures that saddled this permanent (deduped,
    // accumulated across same-turn saddles). Empty for effect-granted saddles.
    for creature_id in &saddling_creatures {
        if !mount.saddled_by.contains(creature_id) {
            mount.saddled_by.push(*creature_id);
        }
    }
    // CR 702.171b: only emit the "becomes saddled" event on the false→true
    // transition so the trigger fires exactly once per turn.
    if !already_saddled {
        events.push(GameEvent::Saddled {
            mount_id,
            creatures: saddling_creatures,
        });
    }
}

/// Resolve the object target(s) of a `BecomeSaddled` effect. Mirrors the
/// canonical `resolve_defined_or_targets` chokepoint in `counters.rs`:
/// - `LastCreated` → the most recently created token ids
/// - `SelfRef` → the source (a "~ becomes saddled" self-anaphor)
/// - event-context refs (`TriggeringSource` — Kolodin's "it becomes saddled"
///   Mount-enters trigger; `ParentTarget` from a parent slot) resolve from the
///   trigger event via `resolve_event_context_targets`
/// - otherwise → the announced object targets (Guidelight Matrix's chosen Mount)
fn resolve_object_targets(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    let Effect::BecomeSaddled { target } = &ability.effect else {
        return Vec::new();
    };
    if matches!(target, TargetFilter::LastCreated) {
        return state.last_created_token_ids.clone();
    }
    // CR 608.2c: the printed-name anaphor always resolves to the source.
    if matches!(target, TargetFilter::SelfRef) {
        return vec![ability.source_id];
    }
    // CR 608.2c: a triggered "it becomes saddled" binds "it" to a context ref
    // (`TriggeringSource`) that resolves from the trigger event — not from an
    // announced target slot. `resolve_event_context_targets` reads the event;
    // it returns empty for a plain targeted effect, which then falls through to
    // the announced targets below.
    let event_targets =
        crate::game::targeting::resolve_event_context_targets(state, target, ability.source_id);
    if !event_targets.is_empty() {
        return event_targets
            .into_iter()
            .filter_map(|t| match t {
                TargetRef::Object(id) => Some(id),
                TargetRef::Player(_) => None,
            })
            .collect();
    }
    ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .collect()
}

/// CR 702.171b: resolver for `Effect::BecomeSaddled` — the target permanent(s)
/// gain the saddled designation until end of turn (cleared at cleanup / on
/// leaving the battlefield by `reset_for_battlefield_exit`). No saddling
/// creatures are recorded because no saddle cost was paid.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for object_id in resolve_object_targets(state, ability) {
        mark_saddled(state, object_id, Vec::new(), events);
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::BecomeSaddled,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn battlefield_mount(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Test Mount".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Mount".to_string());
        obj.base_power = Some(0);
        obj.base_toughness = Some(4);
        obj.power = Some(0);
        obj.toughness = Some(4);
        id
    }

    #[test]
    fn become_saddled_marks_explicit_object_target() {
        let mut state = GameState::new_two_player(42);
        let mount = battlefield_mount(&mut state);
        let ability = ResolvedAbility::new(
            Effect::BecomeSaddled {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(mount)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&mount].is_saddled);
        // CR 702.171c: an effect-granted saddle records no saddling creatures.
        assert!(state.objects[&mount].saddled_by.is_empty());
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::Saddled { mount_id, creatures } if *mount_id == mount && creatures.is_empty())
        ));
    }

    #[test]
    fn become_saddled_self_ref_targets_source() {
        let mut state = GameState::new_two_player(42);
        let mount = battlefield_mount(&mut state);
        let ability = ResolvedAbility::new(
            Effect::BecomeSaddled {
                target: TargetFilter::SelfRef,
            },
            vec![],
            mount,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.objects[&mount].is_saddled);
    }

    #[test]
    fn become_saddled_resolves_announced_object_target() {
        // CR 115.1: a real `Typed(Mount)` target slot ("Target Mount you control
        // becomes saddled") binds the chosen object via `ability.targets`, the
        // path Guidelight Matrix takes once a target is announced.
        let mut state = GameState::new_two_player(42);
        let mount = battlefield_mount(&mut state);
        let other = battlefield_mount(&mut state);
        let ability = ResolvedAbility::new(
            Effect::BecomeSaddled {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(mount)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.objects[&mount].is_saddled);
        // Only the announced target is saddled, not every Mount.
        assert!(!state.objects[&other].is_saddled);
    }

    #[test]
    fn mark_saddled_noop_off_battlefield() {
        // CR 702.171b: only permanents can become saddled — an object not on the
        // battlefield is not marked and no event fires.
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Exiled Mount".to_string(),
            Zone::Exile,
        );
        let mut events = Vec::new();
        mark_saddled(&mut state, id, Vec::new(), &mut events);
        assert!(!state.objects[&id].is_saddled);
        assert!(events.is_empty());
    }

    #[test]
    fn mark_saddled_records_payers_without_duplicates() {
        // CR 702.171c: the keyword path passes the saddling creatures; mark_saddled
        // accumulates them deduped (the single authority shared with the keyword
        // resolution in stack.rs).
        let mut state = GameState::new_two_player(42);
        let mount = battlefield_mount(&mut state);
        let rider = ObjectId(55);
        let mut events = Vec::new();
        mark_saddled(&mut state, mount, vec![rider], &mut events);
        mark_saddled(&mut state, mount, vec![rider], &mut events);
        assert_eq!(state.objects[&mount].saddled_by, vec![rider]);
        // CR 702.171b: "becomes saddled" is a false→true transition — re-saddling
        // an already-saddled mount must not re-fire the trigger. Exactly one
        // `GameEvent::Saddled` is emitted across both calls. This assertion fails
        // if the transition guard in `mark_saddled` is removed.
        let saddled_events = events
            .iter()
            .filter(|e| matches!(e, GameEvent::Saddled { mount_id, .. } if *mount_id == mount))
            .count();
        assert_eq!(saddled_events, 1);
    }
}
