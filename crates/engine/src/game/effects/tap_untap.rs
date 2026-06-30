use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, EffectScope, ResolvedAbility, TapStateChange,
    TargetChoiceTiming, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 603.7e + CR 608.2c: Resolve the objects a `Tap`/`Untap` effect acts on.
///
/// - `SelfRef` → the source object — the printed-name "tap ~"/"untap ~"
///   anaphor that always refers to the source regardless of `ability.targets`.
///   A chained "untap him"/"untap it" anaphor after a `SelfRef`-subject head
///   (The Incredible Hulk: "put a +1/+1 counter on him ... untap him") is
///   rewritten from `ParentTarget` to `SelfRef` at parse time by
///   `sequence::patch_self_ref_head_tap_anaphor`, so it arrives here as
///   `SelfRef` and binds the source. Resolving the anaphor at parse time (where
///   the head's subject is still visible) is mandatory: by resolution time the
///   discriminator is erased — a declined "up to one target" anaphor (Tyvar
///   Kell) reaches the `_` arm with the SAME empty `ability.targets`, and per
///   CR 608.2b that case must do nothing, so a blanket source fallback here
///   would wrongly untap the source.
/// - `TrackedSet` → the chain's tracked object set published by a preceding
///   effect (e.g. `ChooseObjectsIntoTrackedSet`'s "untap those creatures"
///   tail). The `TrackedSetId(0)` sentinel binds to the highest tracked-set
///   id — the set the most recent effect in this chain published — exactly
///   as `grant_permission::resolve` binds it. Empty sets are not skipped: an
///   empty current set means the preceding effect affected nothing.
/// - Any other filter → the ability's chosen targets (object refs only).
fn tap_untap_target_ids(
    state: &GameState,
    ability: &ResolvedAbility,
    effect_target: &TargetFilter,
) -> Vec<ObjectId> {
    match effect_target {
        TargetFilter::SelfRef => vec![ability.source_id],
        TargetFilter::TrackedSet {
            id: TrackedSetId(0),
        } => state
            .tracked_object_sets
            .iter()
            .max_by_key(|(id, _)| id.0)
            .map(|(_, objects)| objects.clone())
            .unwrap_or_default(),
        TargetFilter::TrackedSet { id } => state
            .tracked_object_sets
            .get(id)
            .cloned()
            .unwrap_or_default(),
        _ => ability
            .targets
            .iter()
            .filter_map(|t| match t {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .collect(),
    }
}

/// CR 701.26a (tap) / CR 701.26b (untap): Resolve `Effect::SetTapState`.
///
/// The `scope` field is load-bearing and genuinely divergent:
/// - `EffectScope::Single` (legacy `Tap`/`Untap`) resolves a single chosen or
///   source permanent through the target/SelfRef/TrackedSet/resolution-prompt
///   path (`resolve_single`).
/// - `EffectScope::All` (legacy `TapAll`/`UntapAll`) iterates every permanent
///   matching the population filter (`resolve_all`).
///
/// `state: TapStateChange` selects the tap/untap polarity within each scope.
pub fn resolve_set_tap_state(
    game_state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::SetTapState {
        target,
        scope,
        state,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("SetTapState".to_string()));
    };
    match scope {
        EffectScope::Single => resolve_single(game_state, ability, target, *state, events),
        EffectScope::All => resolve_all(game_state, ability, target, *state, events),
    }
}

/// CR 701.26a/b + CR 608.2c: Single-permanent tap/untap (legacy
/// `Effect::Tap`/`Effect::Untap`). The subject is resolved from the effect's
/// own `target` filter — `SelfRef` (the printed-name "tap ~"/"untap ~" anaphor)
/// and `TrackedSet` ("tap/untap those creatures") resolve regardless of
/// `ability.targets`, so chained tap/untap sub-abilities don't inherit the
/// parent's targets via chain propagation in
/// `effects::mod.rs::resolve_ability_chain` (issue #323 class). `SelfRef` is
/// also the runtime path for trigger shapes like Ragost's untap-self (CR 603.4
/// intervening-if + CR 514 end step); `TrackedSet` is the chain-unified
/// "untap those creatures" tail of a `ChooseObjectsIntoTrackedSet` chain
/// (CR 603.7e — Magnetic Mountain / Dream Tides / Thelon's Curse).
fn resolve_single(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    change: TapStateChange,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let effect_kind = match change {
        TapStateChange::Tap => EffectKind::Tap,
        TapStateChange::Untap => EffectKind::Untap,
    };
    if prompt_resolution_tap_untap_choice(state, ability, target, effect_kind, events) {
        return Ok(());
    }
    let target_ids = tap_untap_target_ids(state, ability, target);
    for obj_id in target_ids {
        let outcome = match change {
            TapStateChange::Tap => process_one_tap(state, obj_id, ability.source_id, events)?,
            TapStateChange::Untap => process_one_untap(state, obj_id, events)?,
        };
        if let TapUntapOutcome::NeedsChoice(player) = outcome {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        };
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

pub(crate) enum TapUntapOutcome {
    Complete,
    NeedsChoice(crate::types::player::PlayerId),
}

pub(crate) fn process_one_tap(
    state: &mut GameState,
    object_id: ObjectId,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<TapUntapOutcome, EffectError> {
    let proposed = ProposedEvent::Tap {
        object_id,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::Tap { object_id, .. } = event {
                let obj = state
                    .objects
                    .get_mut(&object_id)
                    .ok_or(EffectError::ObjectNotFound(object_id))?;
                obj.tapped = true;
                events.push(GameEvent::PermanentTapped {
                    object_id,
                    caused_by: Some(source_id),
                });
            }
            Ok(TapUntapOutcome::Complete)
        }
        ReplacementResult::Prevented => Ok(TapUntapOutcome::Complete),
        ReplacementResult::NeedsChoice(player) => Ok(TapUntapOutcome::NeedsChoice(player)),
    }
}

pub(crate) fn process_one_untap(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<TapUntapOutcome, EffectError> {
    let proposed = ProposedEvent::Untap {
        object_id,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::Untap { object_id, .. } = event {
                let obj = state
                    .objects
                    .get_mut(&object_id)
                    .ok_or(EffectError::ObjectNotFound(object_id))?;
                obj.tapped = false;
                events.push(GameEvent::PermanentUntapped { object_id });
            }
            Ok(TapUntapOutcome::Complete)
        }
        ReplacementResult::Prevented => Ok(TapUntapOutcome::Complete),
        ReplacementResult::NeedsChoice(player) => Ok(TapUntapOutcome::NeedsChoice(player)),
    }
}

fn prompt_resolution_tap_untap_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    effect_kind: EffectKind,
    events: &mut Vec<GameEvent>,
) -> bool {
    if ability.target_choice_timing != TargetChoiceTiming::Resolution || !ability.targets.is_empty()
    {
        return false;
    }
    let Some(spec) = ability.multi_target.as_ref() else {
        return false;
    };

    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let eligible: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| crate::game::filter::matches_target_filter(state, *id, target, &ctx))
        .collect();
    let Ok(bounds) = crate::game::ability_utils::resolve_multi_target_bounds(
        state,
        ability,
        spec,
        eligible.len(),
    ) else {
        return false;
    };

    if bounds.max == 0 && bounds.min == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return true;
    }

    state.waiting_for = WaitingFor::EffectZoneChoice {
        player: ability.controller,
        cards: eligible,
        count: bounds.max,
        min_count: bounds.min,
        up_to: bounds.min != bounds.max,
        source_id: ability.source_id,
        effect_kind,
        zone: Zone::Battlefield,
        destination: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        // CR 708.2a: tap/untap selection is not a face-down entry.
        face_down_profile: None,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: 0,
        library_position: None,
        is_cost_payment: false,
    };
    true
}

/// CR 701.26a (tap) / CR 701.26b (untap): Mass tap/untap of every permanent
/// matching the filter (legacy `Effect::TapAll`/`Effect::UntapAll`). Unlike the
/// single scope this never declares targets — it iterates the resolved
/// population filter and applies the change to each matching permanent.
fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    change: TapStateChange,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let effective_filter = crate::game::effects::resolved_object_filter(ability, target);

    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            crate::game::filter::matches_target_filter(state, **id, &effective_filter, &ctx)
        })
        .copied()
        .collect();

    for obj_id in matching {
        let outcome = match change {
            TapStateChange::Tap => process_one_tap(state, obj_id, ability.source_id, events)?,
            TapStateChange::Untap => process_one_untap(state, obj_id, events)?,
        };
        if let TapUntapOutcome::NeedsChoice(player) = outcome {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        Effect, EffectScope, MultiTargetSpec, QuantityExpr, TapStateChange, TargetChoiceTiming,
        TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_tap_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn make_untap_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn tap_sets_tapped_true() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &make_tap_ability(obj_id), &mut events).unwrap();

        assert!(state.objects[&obj_id].tapped);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
    }

    /// CR 701.26b: When a triggered ability has
    /// `Effect::Untap { target: SelfRef }` and the source is the trigger's
    /// own object (Ragost, Famished Paladin, Pristine Angel, etc.), the
    /// resolver must untap the source even when `ability.targets` is empty.
    /// SelfRef is a context-ref (no target slot is surfaced and the
    /// event-context resolver does not bind it), so the resolver itself
    /// must expand SelfRef to the source.
    #[test]
    fn untap_self_ref_with_empty_targets_untaps_source() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ragost".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![], // empty — SelfRef must resolve via source_id
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.objects[&obj_id].tapped,
            "SelfRef untap must untap the source object"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentUntapped { .. })));
    }

    /// CR 701.26a: Same SelfRef expansion for tap (e.g. "tap ~" triggered
    /// effects).
    #[test]
    fn tap_self_ref_with_empty_targets_taps_source() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "SomeCreature".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.objects[&obj_id].tapped,
            "SelfRef tap must tap the source object"
        );
    }

    /// CR 608.2b: a chained tap/untap anaphor that lowers to `ParentTarget` with
    /// an EMPTY `ability.targets` slot must be an inert NO-OP. This is the
    /// *declined optional-target* case — Tyvar Kell "Put a +1/+1 counter on up to
    /// one target Elf. Untap it." with no Elf chosen: the anaphor "it" has no
    /// referent, so per CR 608.2b the part of the effect that needs it doesn't
    /// happen. (Hulk's `SelfRef`-head "untap him" never reaches this arm — it is
    /// rewritten to `SelfRef` at parse time by
    /// `sequence::patch_self_ref_head_tap_anaphor`, then binds the source via the
    /// `SelfRef` arm above.)
    ///
    /// Discrimination: this is the regression fence for the rejected
    /// `ParentTarget | None if ability.targets.is_empty() => source` arm —
    /// reintroduce it and the source is wrongly untapped AND a spurious
    /// `PermanentUntapped` event fires, flipping BOTH assertions red.
    #[test]
    fn untap_parent_target_with_empty_targets_is_noop() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tyvar Kell".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::ParentTarget,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![], // declined optional target — the anaphor "it" has no referent
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.objects[&obj_id].tapped,
            "declined optional-target anaphor (ParentTarget + empty slot) must NOT untap the source (CR 608.2b)"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::PermanentUntapped { .. })),
            "no PermanentUntapped event may fire for a declined-optional no-op"
        );
    }

    /// NARROWNESS FENCE — guards against an OVER-BROAD "`ParentTarget` always →
    /// source" resolver arm. When a parent target WAS chosen, `ParentTarget`
    /// binds that object via the chosen-targets `_` arm, and the source must stay
    /// untouched. Discrimination: any arm that maps `ParentTarget` to the source
    /// regardless of `ability.targets` would untap the source here, flipping the
    /// second assertion red.
    #[test]
    fn untap_parent_target_with_chosen_object_does_not_untap_source() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().tapped = true;
        state.objects.get_mut(&other).unwrap().tapped = true;
        assert_ne!(source, other);

        let ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::ParentTarget,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![TargetRef::Object(other)], // a parent target WAS chosen
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.objects[&other].tapped,
            "the chosen parent-target object must be untapped"
        );
        assert!(
            state.objects[&source].tapped,
            "the source must stay tapped: the empty-slot fallback must NOT fire when a target was chosen"
        );
    }

    /// CR 608.2b: class fence. `TriggeringSource` is an event-context filter
    /// resolved from `ability.targets`; a `SelfRef`-head → `TriggeringSource`
    /// sub-effect (Blistercoil Weird: SpellCast trigger `Pump{SelfRef}` →
    /// `SetTapState{TriggeringSource, "untap it"}`) with an EMPTY slot has no
    /// materialized event object, so per CR 608.2b it must stay an inert no-op —
    /// it must NOT untap the source.
    ///
    /// Discrimination: any over-broad arm mapping `ParentTarget`/`None`/
    /// `TriggeringSource` to the source on an empty slot would untap the source
    /// here and emit a spurious event, flipping both assertions red. The `_` arm
    /// reads `ability.targets` → `[]` → no-op.
    #[test]
    fn untap_triggering_source_with_empty_targets_is_noop() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Blistercoil Weird".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::TriggeringSource,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![], // empty — no event-context object is materialized into the slot
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.objects[&obj_id].tapped,
            "TriggeringSource with an empty target slot must stay a no-op (source stays tapped)"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::PermanentUntapped { .. })),
            "no PermanentUntapped event may fire for a TriggeringSource no-op"
        );
    }

    #[test]
    fn untap_sets_tapped_false() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &make_untap_ability(obj_id), &mut events).unwrap();

        assert!(!state.objects[&obj_id].tapped);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentUntapped { .. })));
    }

    #[test]
    fn resolution_timed_multi_untap_prompts_for_battlefield_lands() {
        let mut state = GameState::new_two_player(42);
        let land_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        let land_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state
            .objects
            .get_mut(&land_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Land],
                    controller: None,
                    properties: vec![],
                }),
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 3 }));
        ability.target_choice_timing = TargetChoiceTiming::Resolution;
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                min_count,
                up_to,
                effect_kind,
                zone,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 2);
                assert_eq!(*min_count, 0);
                assert!(*up_to);
                assert_eq!(*effect_kind, EffectKind::Untap);
                assert_eq!(*zone, Zone::Battlefield);
                assert!(cards.contains(&land_a));
                assert!(cards.contains(&land_b));
                assert!(!cards.contains(&creature));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
        assert!(events.is_empty());
    }

    #[test]
    fn untap_all_nonland_permanents_you_control() {
        use crate::types::ability::{ControllerRef, TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);

        // 3 nonland permanents (tapped, controller P0)
        let creature1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&creature1).unwrap().tapped = true;

        let creature2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Elf".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&creature2).unwrap().tapped = true;

        let artifact = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Signet".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        state.objects.get_mut(&artifact).unwrap().tapped = true;

        // 1 land (tapped, controller P0) — should NOT be untapped
        let land = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state.objects.get_mut(&land).unwrap().tapped = true;

        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![
                TypeFilter::Permanent,
                TypeFilter::Non(Box::new(TypeFilter::Land)),
            ],
            controller: Some(ControllerRef::You),
            properties: vec![],
        });

        let ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: filter,
                scope: EffectScope::All,
                state: TapStateChange::Untap,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &ability, &mut events).unwrap();

        // All 3 nonland permanents should be untapped
        assert!(
            !state.objects[&creature1].tapped,
            "creature1 should be untapped"
        );
        assert!(
            !state.objects[&creature2].tapped,
            "creature2 should be untapped"
        );
        assert!(
            !state.objects[&artifact].tapped,
            "artifact should be untapped"
        );
        // Land should remain tapped
        assert!(state.objects[&land].tapped, "land should remain tapped");
        // Should have 3 PermanentUntapped events
        let untap_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::PermanentUntapped { .. }))
            .count();
        assert_eq!(untap_count, 3);
    }

    #[test]
    fn tap_all_creatures() {
        use crate::types::ability::{TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);

        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![],
        });

        let ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: filter,
                scope: EffectScope::All,
                state: TapStateChange::Tap,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_set_tap_state(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&creature].tapped, "creature should be tapped");
        assert!(!state.objects[&land].tapped, "land should not be tapped");
    }

    /// Building-block test: `resolve_set_tap_state` routes every
    /// (scope, state) quadrant correctly. CR 701.26a (tap) / CR 701.26b (untap).
    #[test]
    fn set_tap_state_routes_all_four_quadrants() {
        use crate::types::ability::{ControllerRef, TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        // Helper: a single battlefield permanent in a known tap state.
        fn one_creature(tapped: bool) -> (GameState, ObjectId) {
            let mut state = GameState::new_two_player(42);
            let id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Bear".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
            state.objects.get_mut(&id).unwrap().tapped = tapped;
            (state, id)
        }

        let single = |state: TapStateChange, id: ObjectId| {
            ResolvedAbility::new(
                Effect::SetTapState {
                    target: TargetFilter::Any,
                    scope: EffectScope::Single,
                    state,
                },
                vec![TargetRef::Object(id)],
                ObjectId(100),
                PlayerId(0),
            )
        };
        let all = |state: TapStateChange| {
            ResolvedAbility::new(
                Effect::SetTapState {
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: Some(ControllerRef::You),
                        properties: vec![],
                    }),
                    scope: EffectScope::All,
                    state,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            )
        };

        // (Single, Tap): untapped → tapped via the target path.
        let (mut state, id) = one_creature(false);
        resolve_set_tap_state(
            &mut state,
            &single(TapStateChange::Tap, id),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(state.objects[&id].tapped, "Single/Tap must tap the target");

        // (Single, Untap): tapped → untapped via the target path.
        let (mut state, id) = one_creature(true);
        resolve_set_tap_state(
            &mut state,
            &single(TapStateChange::Untap, id),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(
            !state.objects[&id].tapped,
            "Single/Untap must untap the target"
        );

        // (All, Tap): untapped → tapped via the population-filter path.
        let (mut state, id) = one_creature(false);
        resolve_set_tap_state(&mut state, &all(TapStateChange::Tap), &mut Vec::new()).unwrap();
        assert!(
            state.objects[&id].tapped,
            "All/Tap must tap each matching permanent"
        );

        // (All, Untap): tapped → untapped via the population-filter path.
        let (mut state, id) = one_creature(true);
        resolve_set_tap_state(&mut state, &all(TapStateChange::Untap), &mut Vec::new()).unwrap();
        assert!(
            !state.objects[&id].tapped,
            "All/Untap must untap each matching permanent"
        );
    }

    /// CR 701.26b + CR 614.6: Blossombind — "Enchanted creature can't become
    /// untapped" is the BROAD untap prohibition: it must stop EVERY untap, not
    /// just the untap step. This drives an actual untap-effect path
    /// (`resolve_set_tap_state` Single/Untap, i.e. "untap target creature"), which
    /// the untap-step turn-based-action loop never runs, and asserts the enchanted
    /// host stays tapped. The replacement is parsed from the real Oracle text and
    /// installed on an attached Aura, then consulted via `process_one_untap` →
    /// `replace_event`. Reverting the untap-prevention replacement (or its routing
    /// through the Priority-6e splitter) lets the host untap and flips this
    /// assertion. A `StaticMode::CantUntap` static — the previous modeling — would
    /// NOT discriminate here: `process_one_untap` never consults it.
    #[test]
    fn blossombind_enchanted_creature_cant_be_untapped_by_an_effect() {
        let mut state = GameState::new_two_player(42);

        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bound Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.tapped = true;
        }

        let unbound = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Free Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&unbound).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.tapped = true;
        }

        // Parse the real Blossombind static line; pull the Untap-prevention
        // replacement out of the cross-layer split and install it on an Aura.
        let parsed = crate::parser::parse_oracle_text(
            "Enchant creature\nWhen this Aura enters, tap enchanted creature.\nEnchanted creature can't become untapped and can't have counters put on it.",
            "Blossombind",
            &[],
            &["Enchantment".to_string()],
            &["Aura".to_string()],
        );
        assert!(
            parsed
                .replacements
                .iter()
                .any(|def| def.event == crate::types::replacements::ReplacementEvent::Untap),
            "Blossombind must yield an Untap-prevention replacement, got {:?}",
            parsed.replacements
        );

        let aura = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Blossombind".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.replacement_definitions = parsed.replacements.clone().into();
            obj.attached_to = Some(host.into());
        }
        state.objects.get_mut(&host).unwrap().attachments.push(aura);

        // "Untap target creature" on the enchanted host — a real effect path,
        // distinct from the untap step. The prohibition must keep it tapped.
        let untap_host = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![TargetRef::Object(host)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &untap_host, &mut events).unwrap();
        assert!(
            state.objects[&host].tapped,
            "an effect-driven untap of the enchanted creature must be prevented"
        );
        assert!(
            !events.iter().any(
                |e| matches!(e, GameEvent::PermanentUntapped { object_id } if *object_id == host)
            ),
            "no PermanentUntapped event should fire for the prevented host"
        );

        // A non-enchanted creature is untouched by the prohibition.
        let untap_other = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![TargetRef::Object(unbound)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &untap_other, &mut events).unwrap();
        assert!(
            !state.objects[&unbound].tapped,
            "a non-enchanted creature must untap normally"
        );
    }

    /// CR 701.26b + CR 614.6 + CR 611.2b: Spider-Woman, Secret Agent end-to-end.
    /// Parses the real Oracle text, drives the ETB trigger through
    /// `resolve_ability_chain`, and asserts the full duration-bound can't-untap
    /// class:
    ///
    /// 1. The ETB taps the chosen opponent's creature.
    /// 2. While you control Spider-Woman, an *effect* untap ("untap target
    ///    creature") of that creature is prevented (broad prohibition — drives
    ///    `resolve_set_tap_state` Single/Untap, which the untap-step loop never
    ///    runs).
    /// 3. It also stays tapped through its controller's untap step
    ///    (`execute_untap`).
    /// 4. Once you no longer control Spider-Woman (it leaves play, CR 611.2b),
    ///    the prohibition lapses and the creature untaps.
    ///
    /// Revert-probe: reverting `stamp_for_as_long_as_controlled_gate` makes the
    /// installed replacement permanent (no `ControllerControlsSource` gate), so
    /// step 4's final `!tapped` assertion FAILS (the creature stays locked even
    /// after Spider-Woman is gone). Reverting the rider parser
    /// (`try_parse_cant_become_untapped_target_rider`) leaves the sub-ability an
    /// `Effect::Unimplemented`, so no replacement installs and step 2's "stays
    /// tapped" assertion FAILS (the effect untap succeeds). A shape-only assert on
    /// the parsed `AddTargetReplacement` would NOT discriminate either: it never
    /// drives the untap pipeline.
    #[test]
    fn spider_woman_secret_agent_cant_untap_for_as_long_as_controlled() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;
        use crate::game::turns::execute_untap;
        use crate::types::events::GameEvent;

        let mut state = GameState::new_two_player(42);

        // Spider-Woman under our control (PlayerId 0).
        let spider_woman = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spider-Woman, Secret Agent".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&spider_woman)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // The opponent's creature (PlayerId 1), untapped to start.
        let foe_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opposing Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&foe_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Parse the real card and pull the ETB trigger's effect chain.
        let parsed = crate::parser::parse_oracle_text(
            "Flash\nWhen Spider-Woman enters, tap target creature an opponent controls. \
             That creature can't become untapped for as long as you control Spider-Woman.",
            "Spider-Woman, Secret Agent",
            &[],
            &["Creature".to_string()],
            &["Spider".to_string()],
        );
        let trigger = parsed
            .triggers
            .first()
            .expect("Spider-Woman must parse an ETB trigger");
        let execute = trigger
            .execute
            .as_deref()
            .expect("the ETB trigger must carry an effect chain");
        // The sub-ability rider must be the broad untap prohibition, not an
        // Unimplemented residue (parse-shape sanity — the discrimination is the
        // runtime assertions below).
        let sub = execute
            .sub_ability
            .as_deref()
            .expect("the tap clause must carry a can't-untap rider");
        assert!(
            matches!(*sub.effect, Effect::AddTargetReplacement { .. }),
            "rider must install a replacement, got {:?}",
            sub.effect
        );

        // Drive the ETB with the opponent's creature as the chosen target.
        let resolved = build_resolved_from_def_with_targets(
            execute,
            spider_woman,
            PlayerId(0),
            vec![TargetRef::Object(foe_creature)],
        );
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();

        // 1. ETB tapped the opponent's creature.
        assert!(
            state.objects[&foe_creature].tapped,
            "the ETB must tap the chosen opponent's creature"
        );

        // 2. An effect untap is prevented while we control Spider-Woman.
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            state.objects[&foe_creature].tapped,
            "an effect-driven untap must be prevented while we control Spider-Woman"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::PermanentUntapped { object_id } if *object_id == foe_creature)),
            "no PermanentUntapped event should fire for the locked creature"
        );

        // 3. It also stays tapped through its controller's untap step.
        state.active_player = PlayerId(1);
        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);
        assert!(
            state.objects[&foe_creature].tapped,
            "the creature must stay tapped through its controller's untap step \
             while we control Spider-Woman"
        );

        // 4. CR 611.2b: once we no longer control Spider-Woman (it leaves play),
        // the prohibition lapses and an effect untap succeeds.
        crate::game::zones::move_to_zone(
            &mut state,
            spider_woman,
            Zone::Graveyard,
            &mut Vec::new(),
        );
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            !state.objects[&foe_creature].tapped,
            "the prohibition must lapse once we no longer control Spider-Woman (CR 611.2b)"
        );
    }

    /// CR 611.2b control-swap sibling: the duration ends on a control CHANGE of
    /// Spider-Woman, not only when it leaves play (the Master Thief reading).
    /// Reverting the `ControllerControlsSource` controller comparison to read the
    /// host's controller would keep the lock after the swap and fail the final
    /// assertion.
    #[test]
    fn spider_woman_cant_untap_lapses_on_control_swap() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;

        let mut state = GameState::new_two_player(42);
        let spider_woman = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spider-Woman, Secret Agent".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&spider_woman)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let foe_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opposing Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&foe_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let parsed = crate::parser::parse_oracle_text(
            "Flash\nWhen Spider-Woman enters, tap target creature an opponent controls. \
             That creature can't become untapped for as long as you control Spider-Woman.",
            "Spider-Woman, Secret Agent",
            &[],
            &["Creature".to_string()],
            &["Spider".to_string()],
        );
        let execute = parsed.triggers[0].execute.as_deref().unwrap();
        let resolved = build_resolved_from_def_with_targets(
            execute,
            spider_woman,
            PlayerId(0),
            vec![TargetRef::Object(foe_creature)],
        );
        resolve_ability_chain(&mut state, &resolved, &mut Vec::new(), 0).unwrap();
        assert!(state.objects[&foe_creature].tapped);

        // An opponent gains control of Spider-Woman: we no longer control it.
        state.objects.get_mut(&spider_woman).unwrap().controller = PlayerId(1);

        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            !state.objects[&foe_creature].tapped,
            "the prohibition must lapse once we lose control of Spider-Woman (CR 611.2b)"
        );
    }

    /// Shared scaffolding for the durability/lapse tests: installs the real
    /// Spider-Woman can't-untap lock on `foe`, returns `(spider_woman, foe)`.
    /// Drives the actual parsed ETB chain so the `ControllerControlsSource`
    /// replacement is installed exactly as production installs it (live + base).
    fn install_spider_woman_lock(state: &mut GameState) -> (ObjectId, ObjectId) {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;

        let spider_woman = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Spider-Woman, Secret Agent".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&spider_woman)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let foe_creature = create_object(
            state,
            CardId(2),
            PlayerId(1),
            "Opposing Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&foe_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let parsed = crate::parser::parse_oracle_text(
            "Flash\nWhen Spider-Woman enters, tap target creature an opponent controls. \
             That creature can't become untapped for as long as you control Spider-Woman.",
            "Spider-Woman, Secret Agent",
            &[],
            &["Creature".to_string()],
            &["Spider".to_string()],
        );
        let execute = parsed.triggers[0].execute.as_deref().unwrap();
        let resolved = build_resolved_from_def_with_targets(
            execute,
            spider_woman,
            PlayerId(0),
            vec![TargetRef::Object(foe_creature)],
        );
        resolve_ability_chain(state, &resolved, &mut Vec::new(), 0).unwrap();
        assert!(state.objects[&foe_creature].tapped, "ETB must tap the foe");
        (spider_woman, foe_creature)
    }

    /// TEST D — CR 611.2b durability across a layer pass: the can't-untap lock
    /// must survive `evaluate_layers` (which rebuilds live `replacement_definitions`
    /// from `base_replacement_definitions`). Without the Step-2 base-push the live
    /// def is wiped on the reset (layers.rs ~1363) and the foe untaps.
    ///
    /// Discriminating: reverting the base-push makes the final `tapped` assertion
    /// fail (the effect-untap succeeds after the layer pass). Negative sibling:
    /// an unrelated non-`ControllerControlsSource` rider on a second object is NOT
    /// mirrored into that object's base store.
    #[test]
    fn spider_woman_cant_untap_survives_layer_pass() {
        use crate::types::ability::{Effect, ResolvedAbility};

        let mut state = GameState::new_two_player(42);
        let (spider_woman, foe_creature) = install_spider_woman_lock(&mut state);

        // Negative sibling — install a transient (non-gated) `Moved` rider on a
        // second object through the SAME production resolver
        // (`add_target_replacement::resolve`). Step 2's base-push is scoped to
        // `ControllerControlsSource`, so this rider must land live-only with an
        // EMPTY base store. Asserted immediately, before any `evaluate_layers`
        // one-time base sync (`sync_missing_base_characteristics`) could latch a
        // manually-seeded live def into base — this checks the production path,
        // not the test scaffold.
        let other = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Other Bear".to_string(),
            Zone::Battlefield,
        );
        let mut eot_rider = crate::types::ability::ReplacementDefinition::new(
            crate::types::replacements::ReplacementEvent::Moved,
        )
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Graveyard);
        eot_rider.expiry = Some(crate::types::ability::RestrictionExpiry::EndOfTurn);
        let other_install = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(eot_rider),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(other)],
            ObjectId(0),
            PlayerId(0),
        );
        crate::game::effects::add_target_replacement::resolve(
            &mut state,
            &other_install,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            state.objects[&other]
                .replacement_definitions
                .iter_all()
                .count(),
            1,
            "non-gated rider installs live"
        );
        assert!(
            state.objects[&other]
                .base_replacement_definitions
                .is_empty(),
            "a non-ControllerControlsSource rider must NOT be pushed to base (gate-scoping)"
        );

        // Mark the host as base-initialized so it matches a real battlefield
        // object. Otherwise `sync_missing_base_characteristics` (game_object.rs,
        // fires only when base is empty AND init == false) would latch the
        // manually-installed LIVE lock def into base before the live-from-base
        // reset (layers.rs ~1476) — masking the production base-push and making
        // this durability test non-discriminating (it would pass even with the
        // base-push reverted). Real battlefield hosts are already initialized,
        // so this makes the test depend on the production base-push alone.
        state
            .objects
            .get_mut(&foe_creature)
            .unwrap()
            .base_characteristics_initialized = true;

        // The layer pass rebuilds live defs from base.
        crate::game::layers::evaluate_layers(&mut state);

        // Spider-Woman is still in play under our control — the lock holds.
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            state.objects[&foe_creature].tapped,
            "the can't-untap lock must survive a layer pass (CR 611.2b base durability)"
        );
        assert!(
            state.objects[&spider_woman].zone == Zone::Battlefield,
            "Spider-Woman remained in play for this case"
        );
    }

    /// TEST a — CR 611.2b control swap, then swap-back: a REAL Layer-2 control
    /// change (the path GainControl uses) of the captured source ends the lock
    /// permanently; regaining control must NOT revive it.
    ///
    /// Discriminating: reverting the Step-3 prune call inside `evaluate_layers`
    /// makes the swap-back re-tap case fail (the foe stays locked because the
    /// gated def is still present in base+live after the swap-back makes the gate
    /// true again).
    #[test]
    fn spider_woman_cant_untap_lapses_on_real_gain_control_no_revive() {
        use crate::types::ability::{ContinuousModification, Duration};

        let mut state = GameState::new_two_player(42);
        let (spider_woman, foe_creature) = install_spider_woman_lock(&mut state);

        // Durability sanity: still locked after a plain flush.
        crate::game::layers::evaluate_layers(&mut state);
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            state.objects[&foe_creature].tapped,
            "lock holds before any control change"
        );

        // P1 gains control of Spider-Woman (the source) via the real Layer-2
        // ChangeController TCE path — routes through evaluate_layers, firing the
        // Step-3 prune. We do NOT mutate obj.controller directly (that would
        // bypass the prune and make the test vacuous).
        state.add_transient_continuous_effect(
            spider_woman,
            PlayerId(1),
            Duration::Permanent,
            TargetFilter::SpecificObject { id: spider_woman },
            vec![ContinuousModification::ChangeController],
            None,
        );
        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&spider_woman].controller,
            PlayerId(1),
            "Layer 2 must have flipped Spider-Woman's controller"
        );

        // Lock lapsed: the foe untaps.
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            !state.objects[&foe_creature].tapped,
            "losing control of the source must lapse the lock (CR 611.2b)"
        );

        // Control of Spider-Woman returns to the installer (P0) via a second,
        // later-timestamp Layer-2 control TCE that overrides the first.
        state.add_transient_continuous_effect(
            spider_woman,
            PlayerId(0),
            Duration::Permanent,
            TargetFilter::SpecificObject { id: spider_woman },
            vec![ContinuousModification::ChangeController],
            None,
        );
        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&spider_woman].controller,
            PlayerId(0),
            "control must return to the installer"
        );

        // Re-tap the foe and attempt an effect untap: the lock is DEAD (pruned on
        // the swap), so it must NOT have revived — the foe untaps.
        state.objects.get_mut(&foe_creature).unwrap().tapped = true;
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            !state.objects[&foe_creature].tapped,
            "the lock must NOT revive when control of the source returns (CR 611.2b ends permanently)"
        );
    }

    /// CR 702.26f + CR 611.2b: a "for as long as you control ~" duration that
    /// tracks the source ends when that source phases out (the effect can no
    /// longer see it), permanently — phasing the source back in must NOT revive
    /// the lock. CR 702.26b/d: phasing never changes the source's zone or
    /// controller, so only the phased-in requirement in the gate makes this lapse.
    ///
    /// Discriminating: reverting the gate's `&& o.is_phased_in()` conjunct leaves
    /// the gate true while the source is phased out (zone/controller unchanged by
    /// phasing, CR 702.26d), so the layer-pass prune never drops the gated def
    /// from base+live. The lock then survives phase-out and revives on phase-in,
    /// and the final `!tapped` assertion fails.
    #[test]
    fn spider_woman_cant_untap_dead_on_phase_out_no_revive() {
        use crate::game::game_object::PhaseOutCause;
        use crate::game::phasing::{phase_in_object, phase_out_object};

        let mut state = GameState::new_two_player(42);
        let (spider_woman, foe_creature) = install_spider_woman_lock(&mut state);

        // Negative baseline: with the source phased in, the lock HOLDS — proves the
        // phase-out (not a pre-broken lock) is what lapses the duration.
        crate::game::layers::evaluate_layers(&mut state);
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            state.objects[&foe_creature].tapped,
            "lock holds while the source is phased in"
        );

        // Phase the SOURCE out via the production path (marks layers full). The
        // HOST (foe) stays phased in and its controller is unchanged — this isolates
        // the source-phase axis from the control-swap and zone-exit axes.
        let mut events = Vec::new();
        let phased = phase_out_object(
            &mut state,
            spider_woman,
            PhaseOutCause::Directly,
            &mut events,
        );
        assert_eq!(phased, vec![spider_woman], "source phased out");
        assert!(
            state.objects[&spider_woman].is_phased_out(),
            "source is phased out"
        );
        assert_eq!(
            state.objects[&spider_woman].zone,
            Zone::Battlefield,
            "CR 702.26d: phasing must not change zone"
        );
        assert_eq!(
            state.objects[&spider_woman].controller,
            PlayerId(0),
            "CR 702.26d: phasing must not change controller"
        );

        // Flush layers: the now-false gate makes prune_lapsed_controller_controls_source
        // drop the gated def from base+live (CR 611.2b permanent lapse).
        crate::game::layers::evaluate_layers(&mut state);

        // The lock lapsed: the foe untaps while the source is phased out.
        state.objects.get_mut(&foe_creature).unwrap().tapped = true;
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            !state.objects[&foe_creature].tapped,
            "phasing out the source must lapse the lock (CR 702.26f + CR 611.2b)"
        );

        // Phase the source back in via the production path, then flush layers.
        let mut events = Vec::new();
        let back = phase_in_object(&mut state, spider_woman, &mut events);
        assert_eq!(back, vec![spider_woman], "source phased back in");
        assert!(
            state.objects[&spider_woman].is_phased_in(),
            "source is phased in again"
        );
        crate::game::layers::evaluate_layers(&mut state);

        // Re-tap the foe and attempt an effect untap: the lock is DEAD (pruned on
        // phase-out), so it must NOT revive on phase-in — the foe untaps.
        state.objects.get_mut(&foe_creature).unwrap().tapped = true;
        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            !state.objects[&foe_creature].tapped,
            "the lock must NOT revive when the source phases back in (CR 611.2b ends permanently)"
        );
    }

    /// TEST b — CR 611.2b + CR 400.7 source re-entry: the captured source leaving
    /// and re-entering as a new object (same storage ObjectId) ends the lock
    /// permanently; it must not revive on a same-ObjectId re-entry.
    ///
    /// Discriminating: reverting the Step-4 source-axis (`source == departed_id`)
    /// arm leaves the gated def in base+live, so after the round-trip the gate is
    /// true again (Spider-Woman back in play, controlled by installer) and the
    /// foe would stay locked — the final `!tapped` assertion fails.
    #[test]
    fn spider_woman_cant_untap_dead_on_source_reentry() {
        let mut state = GameState::new_two_player(42);
        let (spider_woman, foe_creature) = install_spider_woman_lock(&mut state);

        let inc_before = state.objects[&spider_woman].incarnation;

        // Round-trip the source through the graveyard and back (same ObjectId).
        crate::game::zones::move_to_zone(
            &mut state,
            spider_woman,
            Zone::Graveyard,
            &mut Vec::new(),
        );
        crate::game::zones::move_to_zone(
            &mut state,
            spider_woman,
            Zone::Battlefield,
            &mut Vec::new(),
        );
        // Re-establish controller and type for the re-entered incarnation.
        {
            let sw = state.objects.get_mut(&spider_woman).unwrap();
            sw.controller = PlayerId(0);
            if !sw.card_types.core_types.contains(&CoreType::Creature) {
                sw.card_types.core_types.push(CoreType::Creature);
            }
        }
        // Same storage ObjectId, strictly newer incarnation → a real re-entry.
        assert_eq!(
            state.objects[&spider_woman].zone,
            Zone::Battlefield,
            "source must be back on the battlefield"
        );
        assert!(
            state.objects[&spider_woman].incarnation > inc_before,
            "re-entry must bump the incarnation (CR 400.7 new object)"
        );

        crate::game::layers::evaluate_layers(&mut state);

        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            !state.objects[&foe_creature].tapped,
            "the lock must NOT revive on a same-ObjectId source re-entry (CR 611.2b + CR 400.7)"
        );
    }

    /// TEST c — CR 611.2b + CR 400.7 host blink (the B1 fix): the LOCKED HOST
    /// leaving and re-entering as a new object (same storage ObjectId) ends the
    /// lock — the re-entered creature is a new object and must not inherit the
    /// previous incarnation's can't-untap rider.
    ///
    /// Discriminating: reverting the Step-4 host-axis (`host_id == departed_id`)
    /// arm leaves the gated def on the re-entered host in base+live, so with
    /// Spider-Woman still in play the gate is true and the host would stay locked
    /// — the final `!tapped` assertion fails.
    #[test]
    fn spider_woman_cant_untap_dead_on_host_blink() {
        let mut state = GameState::new_two_player(42);
        let (_spider_woman, foe_creature) = install_spider_woman_lock(&mut state);

        let inc_before = state.objects[&foe_creature].incarnation;

        // Spider-Woman stays in play; the HOST (foe) is blinked.
        crate::game::zones::move_to_zone(
            &mut state,
            foe_creature,
            Zone::Graveyard,
            &mut Vec::new(),
        );
        crate::game::zones::move_to_zone(
            &mut state,
            foe_creature,
            Zone::Battlefield,
            &mut Vec::new(),
        );
        {
            let foe = state.objects.get_mut(&foe_creature).unwrap();
            foe.controller = PlayerId(1);
            if !foe.card_types.core_types.contains(&CoreType::Creature) {
                foe.card_types.core_types.push(CoreType::Creature);
            }
            // Re-tap so a successful untap is observable.
            foe.tapped = true;
        }
        assert_eq!(
            state.objects[&foe_creature].zone,
            Zone::Battlefield,
            "host must be back on the battlefield"
        );
        assert!(
            state.objects[&foe_creature].incarnation > inc_before,
            "host re-entry must bump the incarnation (CR 400.7 new object)"
        );

        crate::game::layers::evaluate_layers(&mut state);

        let mut events = Vec::new();
        resolve_set_tap_state(&mut state, &make_untap_ability(foe_creature), &mut events).unwrap();
        assert!(
            !state.objects[&foe_creature].tapped,
            "a blinked host re-enters as a new object — the lock must NOT carry over \
             (CR 611.2b + CR 400.7, B1 fix)"
        );
    }

    /// CR 707.2 / CR 611.2b: The "can't untap for as long as you control ~" lock
    /// is durably stored in the host's `base_replacement_definitions` ONLY so it
    /// survives a layer reset. It is a runtime continuous effect installed by
    /// another permanent, NOT a printed/defining characteristic — so it must NOT
    /// be exposed as a copiable value. A copy of the locked host (becomes-a-copy
    /// or a copy-token) must therefore NOT inherit the lock.
    ///
    /// Discriminating: with the `intrinsic_copiable_values` filter reverted to an
    /// unconditional `Arc::clone(&obj.base_replacement_definitions)`, the gated
    /// `ControllerControlsSource` def leaks into `CopiableValues` and the first
    /// assertion (`!gated_in_copiable`) fails. The companion printed (non-gated)
    /// `Moved` rider proves the filter is selective, not a blanket drop: it MUST
    /// still appear in the copiable values.
    #[test]
    fn locked_host_copiable_values_exclude_control_gated_lock() {
        use crate::game::printed_cards::intrinsic_copiable_values;
        use crate::types::ability::ReplacementCondition;
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        // Installs the real Spider-Woman lock: the host (foe) receives a
        // `ControllerControlsSource`-gated def in BOTH live and base stores via
        // the production resolver (add_target_replacement.rs).
        let (_spider_woman, foe_creature) = install_spider_woman_lock(&mut state);

        // Seed a PRINTED (non-gated) replacement directly into the same host's
        // base store, as a printed "enters tapped"-style `Moved` redirect would
        // appear. This is a copiable value and MUST survive the filter.
        let printed_rider = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield);
        {
            let foe = state.objects.get_mut(&foe_creature).unwrap();
            std::sync::Arc::make_mut(&mut foe.base_replacement_definitions)
                .push(printed_rider.clone());
        }

        // Sanity: the host's base store carries BOTH the gated lock and the
        // printed rider, so the filter has something selective to do.
        let foe = state.objects.get(&foe_creature).unwrap();
        assert!(
            foe.base_replacement_definitions.iter().any(|d| matches!(
                d.condition,
                Some(ReplacementCondition::ControllerControlsSource { .. })
            )),
            "fixture precondition: host base must carry the gated lock"
        );
        assert!(
            foe.base_replacement_definitions.contains(&printed_rider),
            "fixture precondition: host base must carry the printed rider"
        );

        let values = intrinsic_copiable_values(foe);

        let gated_in_copiable = values.replacement_definitions.iter().any(|d| {
            matches!(
                d.condition,
                Some(ReplacementCondition::ControllerControlsSource { .. })
            )
        });
        assert!(
            !gated_in_copiable,
            "CR 707.2: a ControllerControlsSource-gated runtime lock must NOT be a \
             copiable value (a copy of the locked host must not inherit the lock)"
        );
        assert!(
            values.replacement_definitions.contains(&printed_rider),
            "CR 707.2: a printed (non-gated) replacement IS a copiable value — the \
             filter must be selective, not a blanket drop"
        );
    }
}
