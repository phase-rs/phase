//! CR 702.26: Phase Out / Phase In resolvers for the `Effect::PhaseOut` and
//! `Effect::PhaseIn` variants. All phasing primitives live in
//! `game::phasing`; this module is the thin effect-handler glue that
//! dispatches resolved targets to those primitives and emits the
//! `EffectResolved` bookkeeping event.
//!
//! Both resolvers handle player and object targets in a single pass:
//! explicit `TargetRef::Player` targets and player-typed mass filters
//! (`Controller`, `Player`, `Typed { type_filters: [], … }`) route through
//! `phase_out_player`/`phase_in_player`; everything else routes through the
//! permanent path (CR 702.26 proper). Player phasing has no formal CR rule
//! and follows the small set of card Oracle text that says "you phase out".

use std::collections::HashSet;

use crate::game::filter::{
    matches_target_filter, matches_target_filter_including_phased_out, FilterContext,
};
use crate::game::game_object::PhaseOutCause;
use crate::game::phasing::{phase_in_object, phase_in_player, phase_out_object, phase_out_player};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// CR 702.26a: Resolve `Effect::PhaseOut` by phasing out every targeted
/// permanent (or every permanent matching the effect's mass filter, e.g.
/// "All permanents you control phase out" from Teferi's Protection). Phased-
/// out objects remain on the battlefield (CR 702.26d); we delegate to
/// `phase_out_object` which also cascades to indirectly-phased attachments
/// and removes everything from combat (CR 506.4 + CR 702.26g).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target = match &ability.effect {
        Effect::PhaseOut { target } => target.clone(),
        _ => return Ok(()),
    };

    // Player-phasing branch. Mirrors `collect_object_targets` for the
    // permanent path: explicit `TargetRef::Player` targets win, then a
    // player-typed mass filter (`Controller`, `Typed { type_filters: [], … }`,
    // `Player`) expands to the matching set of player ids. This dispatches
    // before the object branch so a player target never silently becomes a
    // no-op via `collect_object_targets`.
    let player_targets =
        crate::game::ability_utils::collect_player_targets(state, ability, &target);
    for pid in &player_targets {
        phase_out_player(state, *pid, events);
    }

    let object_targets = collect_object_targets(state, ability, &target);
    let target_set: HashSet<ObjectId> = object_targets.iter().copied().collect();
    for oid in object_targets {
        // CR 702.26h: attachments whose host is also in this mass set phase out
        // only indirectly via the host's CR 702.26g cascade, not as direct targets.
        if attachment_host_in_set(state, oid, &target_set) {
            continue;
        }
        phase_out_object(state, oid, PhaseOutCause::Directly, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PhaseOut,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 702.26c: Resolve `Effect::PhaseIn` by phasing in every targeted
/// permanent. Rare; most phasing-in happens during the untap-step TBA.
pub fn resolve_phase_in(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target = match &ability.effect {
        Effect::PhaseIn { target } => target.clone(),
        _ => return Ok(()),
    };

    // Player-phasing branch — same idiom as `resolve` for symmetry. Phased-out
    // players don't appear in the targeting choke point, so callers wanting
    // to phase them back in must use an explicit `TargetRef::Player` target
    // (or a player-typed mass filter such as `Controller`).
    let player_targets =
        crate::game::ability_utils::collect_player_targets(state, ability, &target);
    for pid in &player_targets {
        phase_in_player(state, *pid, events);
    }

    // CR 702.26b: Filter choke point normally excludes phased-out objects, so
    // we can't rely on the standard target expansion for phase-in. Instead,
    // enumerate state.battlefield directly and match the filter manually,
    // skipping the phased-out exclusion.
    let targets = collect_phase_in_targets(state, ability, &target);
    for oid in targets {
        // CR 101.2: an explicit "phase in" effect can't override an active
        // "can't phase in" restriction. The restriction is condition-gated
        // (CR 611.2b), so The Pandorica's own delayed-trigger phase-in — which
        // resolves only after the source has untapped or left, when the lock has
        // lapsed — passes, while any phase-in attempted while the lock is active
        // is a no-op.
        if crate::game::static_abilities::object_has_active_cant_phase_in(state, oid) {
            continue;
        }
        phase_in_object(state, oid, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PhaseIn,
        source_id: ability.source_id,
    });
    Ok(())
}

/// True when `oid` is attached to another permanent that is also in `targets`.
fn attachment_host_in_set(state: &GameState, oid: ObjectId, targets: &HashSet<ObjectId>) -> bool {
    state
        .objects
        .get(&oid)
        .and_then(|obj| obj.attached_to.as_ref())
        .and_then(|t| t.as_object())
        .is_some_and(|host| targets.contains(&host))
}

/// Resolve the target object set for a `PhaseOut` effect. Explicit
/// `ability.targets` (from the targeting phase) take precedence; mass filters
/// (e.g., `Typed Permanent / You`) are expanded against the battlefield.
fn collect_object_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    let from_targets: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    if !from_targets.is_empty() {
        return from_targets;
    }

    // Mass filter — expand against the phased-in battlefield.
    let ctx = FilterContext::from_ability(ability);
    state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| matches_target_filter(state, *id, target, &ctx))
        .collect()
}

/// Resolve target object set for a `PhaseIn` effect. Because the filter
/// choke point treats phased-out objects as nonexistent, we iterate
/// `state.battlefield` directly and evaluate only the non-phased-out aspects
/// of the filter here.
fn collect_phase_in_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    let from_targets: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    if !from_targets.is_empty() {
        return from_targets;
    }

    // CR 702.26b: phasing-in is one of the rare effects that specifically
    // mentions phased-out permanents, so the effect's filter must be applied to
    // the phased-out permanents themselves. `matches_target_filter_including_
    // phased_out` evaluates the filter (controller scope, type, etc.) while
    // bypassing the choke point's phased-out exclusion, so a card such as "phase
    // in each phased-out permanent you control" no longer indiscriminately
    // phases in every phased-out permanent (including an opponent's).
    let ctx = FilterContext::from_ability(ability);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let phased_out = state.objects.get(id).is_some_and(|obj| obj.is_phased_out());
            phased_out && matches_target_filter_including_phased_out(state, *id, target, &ctx)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn add_creature(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    /// CR 702.26b: A mass phase-in effect specifically mentioning phased-out
    /// permanents must still honor the effect's object filter. Reverting
    /// `collect_phase_in_targets` to return every phased-out battlefield object
    /// phases in the opponent's creature and fails this regression.
    #[test]
    fn phase_in_mass_filter_only_returns_matching_phased_out_objects() {
        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "Phase Source");
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");

        let mut events = Vec::new();
        phase_out_object(&mut state, mine, PhaseOutCause::Directly, &mut events);
        phase_out_object(&mut state, theirs, PhaseOutCause::Directly, &mut events);

        let ability = ResolvedAbility::new(
            Effect::PhaseIn {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
            Vec::new(),
            source,
            PlayerId(0),
        );

        resolve_phase_in(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.objects[&mine].is_phased_out(),
            "controller's matching phased-out creature must phase in"
        );
        assert!(
            state.objects[&theirs].is_phased_out(),
            "opponent's phased-out creature must remain phased out"
        );
    }

    /// CR 702.26c + CR 101.2 + CR 611.2b: `resolve_phase_in` is the second
    /// lock-enforcement site (alongside the untap-step TBA). An explicit
    /// `Effect::PhaseIn` against a permanent held by an active `CantPhaseIn`
    /// restriction must be a no-op while the lock holds, then succeed once the
    /// lock lapses — mirroring The Pandorica's own delayed-trigger phase-in,
    /// which only resolves after the source untaps or leaves. Removing the
    /// `object_has_active_cant_phase_in` skip in `resolve_phase_in` fails this.
    #[test]
    fn resolve_phase_in_respects_cant_phase_in_lock() {
        use crate::types::ability::{ContinuousModification, Duration, StaticCondition};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "The Pandorica");
        let target = add_creature(&mut state, PlayerId(0), "Held");
        state.objects.get_mut(&source).unwrap().tapped = true;

        let mut events = Vec::new();
        phase_out_object(&mut state, target, PhaseOutCause::Directly, &mut events);
        assert!(state.objects[&target].is_phased_out());

        // CR 611.2b: the lock lives only while the source is tapped (SourceIsTapped).
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::ForAsLongAs {
                condition: StaticCondition::SourceIsTapped,
            },
            TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CantPhaseIn,
            }],
            None,
        );

        // The delayed-trigger phase-in carries a concrete resolved target
        // (ParentTarget snapshot); model it with an explicit object target.
        let ability = ResolvedAbility::new(
            Effect::PhaseIn {
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );

        // Lock active: the explicit phase-in must NOT override it (CR 101.2).
        events.clear();
        resolve_phase_in(&mut state, &ability, &mut events).unwrap();
        assert!(
            state.objects[&target].is_phased_out(),
            "explicit phase-in must not override an active CantPhaseIn lock"
        );

        // Lock lapses (source untaps): the same explicit phase-in now succeeds.
        state.objects.get_mut(&source).unwrap().tapped = false;
        events.clear();
        resolve_phase_in(&mut state, &ability, &mut events).unwrap();
        assert!(
            state.objects[&target].is_phased_in(),
            "explicit phase-in must succeed once the lock lapses"
        );
    }

    /// CR 603.7c + CR 502.3 + CR 702.26c: The Pandorica's full re-entry
    /// lifecycle through the production runtime path — registration, end-of-turn
    /// cleanup, later-turn untap firing, and phase-in resolution.
    ///
    /// This is the regression that the isolated `resolve_phase_in`/parser tests
    /// could not catch: the re-entry delayed trigger is created on the
    /// activation turn but its qualifying event (the source's untap) occurs on a
    /// LATER turn. Modeling it with a `ThisTurn` lifetime — as the original
    /// `WhenNextEvent` did — prunes it at end-of-turn cleanup, so it never fires
    /// and the permanent only re-enters one untap-cycle late via the phasing TBA.
    /// The `Persistent` lifetime must survive cleanup and fire on the untap.
    #[test]
    fn pandorica_reentry_survives_cleanup_and_fires_on_later_untap() {
        use crate::game::effects::delayed_trigger;
        use crate::game::triggers::check_delayed_triggers;
        use crate::game::turns::execute_cleanup;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ContinuousModification, DelayedTriggerCondition,
            DelayedTriggerLifetime, Duration, StaticCondition, TriggerDefinition,
        };
        use crate::types::game_state::DelayedTrigger;
        use crate::types::statics::StaticMode;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "The Pandorica");
        let target = add_creature(&mut state, PlayerId(0), "Held");
        // The Pandorica taps to activate; the lock holds "for as long as it
        // remains tapped" (CR 611.2b).
        state.objects.get_mut(&source).unwrap().tapped = true;

        let mut events = Vec::new();
        phase_out_object(&mut state, target, PhaseOutCause::Directly, &mut events);
        assert!(state.objects[&target].is_phased_out());

        // CR 611.2b: the can't-phase-in lock, granted by the activation.
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::ForAsLongAs {
                condition: StaticCondition::SourceIsTapped,
            },
            TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CantPhaseIn,
            }],
            None,
        );

        // CR 603.7c: register the open-ended re-entry trigger through the real
        // `CreateDelayedTrigger` resolver, mirroring the parsed shape
        // (Untaps OR LeavesBattlefield, both scoped to the source; inner PhaseIn
        // bound to the parent target).
        let untaps = TriggerDefinition::new(TriggerMode::Untaps).valid_card(TargetFilter::SelfRef);
        let leaves = TriggerDefinition::new(TriggerMode::LeavesBattlefield)
            .valid_card(TargetFilter::SelfRef);
        let create = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new(untaps),
                    or_trigger: Some(Box::new(leaves)),
                    lifetime: DelayedTriggerLifetime::Persistent,
                },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PhaseIn {
                        target: TargetFilter::ParentTarget,
                    },
                )),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        delayed_trigger::resolve(&mut state, &create, &mut events).unwrap();

        // Control: a `ThisTurn` "when you next cast a spell" delayed trigger that
        // SHOULD be pruned at cleanup, proving the survival below is the lifetime
        // gate rather than a blanket "keep everything".
        state.delayed_triggers.push(DelayedTrigger {
            condition: DelayedTriggerCondition::WhenNextEvent {
                trigger: Box::new(TriggerDefinition::new(TriggerMode::SpellCast)),
                or_trigger: None,
                lifetime: DelayedTriggerLifetime::ThisTurn,
            },
            ability: crate::game::ability_utils::build_resolved_from_def(
                &AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ),
                source,
                PlayerId(0),
            ),
            controller: PlayerId(0),
            source_id: source,
            one_shot: true,
        });
        assert_eq!(state.delayed_triggers.len(), 2);

        // Snapshot the re-entry ability before cleanup/firing consumes it.
        let reentry = state
            .delayed_triggers
            .iter()
            .find(|dt| {
                matches!(
                    dt.condition,
                    DelayedTriggerCondition::WhenNextEvent {
                        lifetime: DelayedTriggerLifetime::Persistent,
                        ..
                    }
                )
            })
            .expect("re-entry trigger must be registered")
            .ability
            .clone();
        assert!(
            matches!(reentry.effect, Effect::PhaseIn { .. }),
            "re-entry ability must be a PhaseIn"
        );
        assert_eq!(
            reentry.targets,
            vec![TargetRef::Object(target)],
            "CR 603.7c: re-entry must snapshot the phased-out parent target"
        );

        // CR 513.2 + CR 603.7b: end-of-turn cleanup. The `ThisTurn` control
        // trigger is pruned; the `Persistent` re-entry trigger survives.
        let mut cleanup_events = Vec::new();
        execute_cleanup(&mut state, &mut cleanup_events);
        assert_eq!(
            state.delayed_triggers.len(),
            1,
            "cleanup must prune the ThisTurn trigger but keep the Persistent re-entry"
        );
        assert!(
            state.delayed_triggers.iter().any(|dt| matches!(
                dt.condition,
                DelayedTriggerCondition::WhenNextEvent {
                    lifetime: DelayedTriggerLifetime::Persistent,
                    ..
                }
            )),
            "the open-ended re-entry trigger must survive end-of-turn cleanup"
        );

        // CR 502.3: on a later turn's untap step the source untaps (the lock
        // lapses) and emits `PermanentUntapped`, which fires the re-entry trigger.
        state.objects.get_mut(&source).unwrap().tapped = false;
        let fired = check_delayed_triggers(
            &mut state,
            &[GameEvent::PermanentUntapped { object_id: source }],
        );
        assert!(
            !fired.is_empty(),
            "the source's untap must fire the persistent re-entry trigger"
        );
        assert!(
            state.delayed_triggers.is_empty(),
            "the one-shot re-entry trigger must be consumed once it fires"
        );

        // CR 702.26c: resolving the fired re-entry (stack resolution) phases the
        // permanent back in now that the lock has lapsed.
        let mut phase_in_events = Vec::new();
        resolve_phase_in(&mut state, &reentry, &mut phase_in_events).unwrap();
        assert!(
            state.objects[&target].is_phased_in(),
            "the held permanent must re-enter on the untap that fires the trigger"
        );
    }
}
