use crate::types::ability::{
    Effect, EffectError, EffectKind, EffectScope, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::statics::StaticMode;

/// Resolve the object subjects of a Suspect / Unsuspect effect.
///
/// Dispatches on the effect's `scope` (mirrors `tap_untap::resolve_set_tap_state`
/// and the `DestroyAll` mass convention):
///
/// - [`EffectScope::Single`] — the targeted / anaphoric path:
///   - `LastCreated` reads the just-created token set;
///   - `SelfRef` (the printed-name anaphor "~" / "it" on a self-targeting
///     ability) always resolves to the source permanent regardless of
///     `ability.targets` — so "~ is no longer suspected" (Frantic Scapegoat) or
///     "Otherwise, suspect it" (Repeat Offender) acts on its own source rather
///     than no-op against an empty announced-target list;
///   - every other filter reads the announced object targets.
/// - [`EffectScope::All`] — the mass population filter ("all suspected creatures
///   are no longer suspected", Absolving Lammasu): CR 701.60a removes the
///   designation from *each* matching permanent, so this enumerates every
///   battlefield permanent matching `target` with no announced target list,
///   exactly like `Effect::DestroyAll` / `Effect::TapAll`.
///
/// Shared by `resolve` and `resolve_unsuspect` so the designation /
/// un-designation pair selects subjects identically.
fn resolve_object_targets(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    let (filter, scope) = match &ability.effect {
        Effect::Suspect { target, scope } | Effect::Unsuspect { target, scope } => (target, *scope),
        _ => return Vec::new(),
    };
    match scope {
        // CR 701.60a: mass "all/each suspected creatures" — apply the
        // un-designation to every battlefield permanent matching the population
        // filter. No target is announced (`target_filter()` is `None` for the
        // `All` scope), so iterate the battlefield instead of `ability.targets`.
        EffectScope::All => {
            let effective_filter = crate::game::effects::resolved_object_filter(ability, filter);
            let ctx = crate::game::filter::FilterContext::from_ability(ability);
            state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    crate::game::filter::matches_target_filter(state, *id, &effective_filter, &ctx)
                })
                .collect()
        }
        EffectScope::Single => match filter {
            TargetFilter::LastCreated => state.last_created_token_ids.clone(),
            // CR 608.2c + CR 701.60a: `SelfRef` is the printed-name anaphor ("~"
            // / "it" on a self-targeting ability) — it always resolves to the
            // source permanent, regardless of `ability.targets`. Mirrors the
            // `resolve_defined_or_targets` short-circuit.
            TargetFilter::SelfRef => vec![ability.source_id],
            _ => ability
                .targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
                .collect(),
        },
    }
}

/// CR 701.60d + CR 701.60a: A permanent can't gain the suspected designation if
/// it is already suspected, or if a static (Airtight Alibi) prohibits it from
/// becoming suspected. Single authority for the designation gate so the resolver
/// never double-suspects or overrides a "can't become suspected" prohibition.
fn can_become_suspected(state: &GameState, object_id: ObjectId) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    // CR 701.60d: "A suspected permanent can't become suspected again."
    if obj.is_suspected {
        return false;
    }
    // CR 701.60a: an active `CantBecomeSuspected` static (e.g. Airtight Alibi's
    // "can't become suspected") prohibits the designation even while unsuspected.
    !crate::game::functioning_abilities::game_functioning_statics(state).any(|(src, def)| {
        if def.mode != StaticMode::CantBecomeSuspected {
            return false;
        }
        match def.affected.as_ref() {
            None => src.id == object_id,
            Some(filter) => crate::game::filter::matches_target_filter(
                state,
                object_id,
                filter,
                &crate::game::filter::FilterContext::from_source(state, src.id),
            ),
        }
    })
}

/// CR 701.60a: Suspect target creature(s).
/// A suspected creature has menace and "This creature can't block." (CR 701.60c)
///
/// Architecture: the designation (`is_suspected`, CR 701.60b) is the *only*
/// state this resolver mutates. The CR 701.60c menace + "can't block" abilities
/// are NOT stored on the permanent — they are a continuous effect derived from
/// the designation during layer evaluation (`layers::derive_suspected_abilities`,
/// CR 613). Writing them onto `base_*` would conflate the granted abilities with
/// the permanent's printed (copiable) abilities, so a naturally-menace creature
/// would lose its printed menace when it stopped being suspected (CR 701.60b:
/// suspected is "neither an ability nor part of the permanent's copiable
/// values"). Deriving keeps printed abilities untouched.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_ids = resolve_object_targets(state, ability);

    for obj_id in target_ids {
        // CR 701.60d / CR 701.60a: respect the designation gate (already
        // suspected, or a "can't become suspected" static) before flipping.
        if !can_become_suspected(state, obj_id) {
            continue;
        }
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            // CR 701.60b: set the designation only — menace + "can't block" are
            // layer-derived from this flag (CR 701.60c).
            obj.is_suspected = true;
            events.push(GameEvent::CreatureSuspected { object_id: obj_id });
        }
    }

    crate::game::layers::mark_layers_full(state);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Suspect,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.60a: Cause the target creature(s) to no longer be suspected.
///
/// The un-designation counterpart of [`resolve`]: clears the `is_suspected`
/// designation only. The CR 701.60c menace + "can't block" abilities are
/// layer-derived from the designation (`layers::derive_suspected_abilities`), so
/// clearing the flag stops re-deriving them on the next layers pass — this
/// resolver touches NO keyword / static fields, leaving any printed menace or
/// printed "can't block" intact (CR 701.60b). Idempotent — a creature that is
/// not suspected is skipped and emits no `CreatureNoLongerSuspected` event
/// (mirrors `prepare::resolve_become_unprepared`). Single authority for the
/// "no longer suspected" transition so callers never clear the flag directly.
pub fn resolve_unsuspect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_ids = resolve_object_targets(state, ability);

    let mut any_flipped = false;
    for obj_id in target_ids {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            // Idempotent: only flip (and emit) when the designation was present.
            if !obj.is_suspected {
                continue;
            }
            // CR 701.60b: clear the designation only — the layer-derived menace +
            // "can't block" (CR 701.60c) lapse automatically on the recalc below.
            obj.is_suspected = false;
            events.push(GameEvent::CreatureNoLongerSuspected { object_id: obj_id });
            any_flipped = true;
        }
    }

    if any_flipped {
        crate::game::layers::mark_layers_full(state);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Unsuspect,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::layers::evaluate_layers;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, EffectScope, ResolvedAbility, StaticDefinition};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_creature(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.power = Some(2);
        obj.toughness = Some(2);
        id
    }

    /// CR 701.60b + CR 701.60c: Suspecting sets only the `is_suspected`
    /// designation. The menace + "can't block" are NOT stored on the
    /// permanent's (copiable) `base_*` fields — they are layer-derived from the
    /// designation. Asserting the base fields stay clean is what distinguishes
    /// the derived architecture from the prior base-mutating one.
    #[test]
    fn suspect_sets_designation_without_touching_base() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![crate::types::ability::TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.is_suspected);
        // CR 701.60b: the designation is not part of the copiable values, so it
        // must NOT have been written to the base ability fields.
        assert!(
            !obj.base_keywords
                .iter()
                .any(|k| matches!(k, Keyword::Menace)),
            "suspect must not write menace into base_keywords (CR 701.60b)"
        );
        assert!(
            !obj.base_static_definitions
                .iter()
                .any(|s| s.mode == StaticMode::CantBlock),
            "suspect must not write CantBlock into base_static_definitions"
        );
    }

    /// CR 701.60c: After a layers pass, the suspected designation derives menace
    /// and "can't block" onto the live (post-layer) fields. Reverting
    /// `layers::derive_suspected_abilities` leaves both absent, flipping these.
    #[test]
    fn suspect_derives_abilities_on_layer_recalc() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![crate::types::ability::TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The derivation rides along with the Step-1 reset every layers pass.
        evaluate_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.is_suspected);
        assert!(obj.keywords.iter().any(|k| matches!(k, Keyword::Menace)));
        assert!(obj
            .static_definitions
            .iter_all()
            .any(|s| s.mode == StaticMode::CantBlock));
        // The derivation is repeatable: a second pass keeps exactly one of each.
        evaluate_layers(&mut state);
        let obj = state.objects.get(&id).unwrap();
        assert_eq!(
            obj.keywords
                .iter()
                .filter(|k| matches!(k, Keyword::Menace))
                .count(),
            1,
            "derivation is idempotent across passes"
        );
        assert_eq!(
            obj.static_definitions
                .iter_all()
                .filter(|s| s.mode == StaticMode::CantBlock)
                .count(),
            1,
            "derivation is idempotent across passes"
        );
    }

    /// CR 701.60a: `Effect::Unsuspect` resolved through the real effect dispatch
    /// (`resolve_effect`) clears the suspected designation and, after layer
    /// recalc, removes the CR 701.60c menace + "can't block" abilities the
    /// designation conferred. Reverting the `Effect::Unsuspect => resolve_unsuspect`
    /// dispatch arm (or the resolver) leaves `is_suspected == true` and the
    /// abilities present, flipping every assertion below.
    #[test]
    fn unsuspect_through_dispatch_removes_designation_and_abilities() {
        use crate::game::effects::resolve_effect;

        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        // Suspect the creature through the real dispatch first.
        let suspect = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_effect(&mut state, &suspect, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert!(state.objects[&id].is_suspected, "suspect should designate");
        assert!(crate::game::keywords::has_keyword(
            &state.objects[&id],
            &Keyword::Menace
        ));

        // Now un-suspect through the real dispatch.
        let unsuspect = ResolvedAbility::new(
            Effect::Unsuspect {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(id)],
            ObjectId(101),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_effect(&mut state, &unsuspect, &mut events).unwrap();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        assert!(!obj.is_suspected, "unsuspect clears the designation");
        assert!(
            !crate::game::keywords::has_keyword(obj, &Keyword::Menace),
            "menace removed when no longer suspected"
        );
        assert!(
            !obj.static_definitions
                .iter_all()
                .any(|s| s.mode == StaticMode::CantBlock),
            "can't-block removed when no longer suspected"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::CreatureNoLongerSuspected { object_id } if *object_id == id)),
            "flip emits CreatureNoLongerSuspected"
        );
    }

    /// Idempotency: un-suspecting a creature that is not suspected is a no-op and
    /// emits no `CreatureNoLongerSuspected` event (mirrors
    /// `resolve_become_unprepared`).
    #[test]
    fn unsuspect_idempotent_when_not_suspected() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Unsuspect {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_unsuspect(&mut state, &ability, &mut events).unwrap();

        assert!(!state.objects[&id].is_suspected);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::CreatureNoLongerSuspected { .. })),
            "no event when nothing flipped"
        );
    }

    /// CR 701.60d: A suspected permanent can't become suspected again — the
    /// resolver's `can_become_suspected` gate skips an already-suspected target,
    /// so re-suspecting emits no second `CreatureSuspected` event.
    #[test]
    fn suspect_already_suspected_is_gated() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();

        let suspect_events = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CreatureSuspected { .. }))
            .count();
        assert_eq!(
            suspect_events, 1,
            "second suspect must be gated (CR 701.60d)"
        );
    }

    /// CR 701.60a: A creature carrying a `CantBecomeSuspected` static (Airtight
    /// Alibi) can't be suspected — the resolver gate refuses the designation.
    /// Reverting the gate would set `is_suspected` and emit the event.
    #[test]
    fn cant_become_suspected_static_blocks_suspect() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        // Confer the prohibition directly onto the creature (the layer-applied
        // form Airtight Alibi produces via `AddStaticMode`).
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBecomeSuspected));

        let ability = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.objects[&id].is_suspected,
            "a creature that can't become suspected stays unsuspected"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::CreatureSuspected { .. })),
            "no CreatureSuspected event when the prohibition gate fires"
        );
    }

    #[test]
    fn suspect_last_created_token() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        state.last_created_token_ids = vec![id];

        let ability = ResolvedAbility::new(
            Effect::Suspect {
                target: TargetFilter::LastCreated,
                scope: EffectScope::Single,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.is_suspected);
    }
}
