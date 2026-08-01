use crate::game::transform::{is_double_faced_permanent, transform_permanent};
use crate::types::ability::{
    Effect, EffectError, EffectKind, EffectScope, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.27a: Transform — turn a double-faced card to its other face.
///
/// `scope` is load-bearing and genuinely divergent (mirrors
/// `tap_untap::resolve_set_tap_state`):
/// - `EffectScope::Single` (legacy targeted/anaphoric transform) acts on the
///   single chosen or source permanent (`resolve_single`).
/// - `EffectScope::All` ("Transform all Humans" — Moonmist) is a non-targeting
///   mass transform that enumerates the population filter over the battlefield
///   (`resolve_all`).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::Transform {
            scope: EffectScope::All,
            target,
            ..
        } => {
            let target = target.clone();
            return resolve_all(state, ability, &target, events);
        }
        Effect::Transform { .. } => {}
        _ => {
            return Err(EffectError::InvalidParam(
                "expected Transform effect".to_string(),
            ))
        }
    }

    // CR 701.27c: If a spell or ability instructs a player to transform a permanent
    // that isn't represented by a double-faced card, nothing happens.
    let object_id = match ability.targets.as_slice() {
        [TargetRef::Object(object_id)] => *object_id,
        [] => ability.source_id,
        _ => {
            return Err(EffectError::InvalidParam(
                "transform expects exactly one object target".to_string(),
            ))
        }
    };

    // CR 701.27f: A self-transform instruction does nothing if the permanent
    // has already transformed or converted since the ability was put onto the stack.
    let stale_self_transform = object_id == ability.source_id
        && (!ability.source_is_current(state)
            || ability
                .context
                .source_transformation_count
                .is_some_and(|captured| {
                    state
                        .objects
                        .get(&object_id)
                        .is_some_and(|object| object.transformation_count != captured)
                }));
    if !stale_self_transform {
        transform_permanent(state, object_id, events)
            .map_err(|err| EffectError::InvalidParam(err.to_string()))?;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Transform,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 701.27a + CR 115.10 / CR 115.10a: Mass transform of every permanent
/// matching the (non-targeting) population filter — "Transform all Humans"
/// (Moonmist). Unlike the single scope this never declares a target: it
/// enumerates the resolved population filter over the battlefield and turns each
/// matching permanent over, mirroring `tap_untap::resolve_all`.
///
/// CR 701.27a + CR 701.27c: "all X" matches mostly SINGLE-FACED permanents, but
/// only permanents represented by double-faced tokens/cards can transform, and a
/// permanent that can't transform does nothing. The matched population is
/// therefore PRE-FILTERED to double-faced permanents (the authoritative
/// `is_double_faced_permanent`) before `transform_permanent`, and any residual
/// per-object error is caught as a no-op rather than propagated — a single
/// non-DFC in the population must never abort the whole mass transform.
fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &crate::types::ability::TargetFilter,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let effective_filter = crate::game::effects::resolved_object_filter(ability, target);

    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            crate::game::filter::matches_target_filter(state, *id, &effective_filter, &ctx)
        })
        // CR 701.27a + CR 701.27c: only double-faced permanents can transform;
        // every other match does nothing (never an error).
        .filter(|id| state.objects.get(id).is_some_and(is_double_faced_permanent))
        .collect();

    for obj_id in matching {
        // CR 701.27c: never `?`-propagate — a permanent that can't transform
        // (CantTransform static, meld, or a filtered-in edge) is a per-object
        // no-op, so a single failure must not abort the remaining population.
        let _ = transform_permanent(state, obj_id, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Transform,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityDefinition, AbilityKind, EffectScope, TargetFilter};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::sync::Arc;

    fn setup_dfc(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Front Face".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string()],
        };
        obj.keywords = vec![Keyword::Vigilance];
        obj.base_keywords = vec![Keyword::Vigilance];
        obj.abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Transform {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
            },
        )]);
        obj.base_abilities = Arc::clone(&obj.abilities);
        obj.color = vec![ManaColor::Green];
        obj.base_color = vec![ManaColor::Green];
        obj.back_face = Some(crate::game::game_object::BackFaceData {
            name: "Back Face".to_string(),
            power: Some(4),
            toughness: Some(4),
            loyalty: None,
            printed_loyalty: None,
            defense: None,
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Werewolf".to_string()],
            },
            mana_cost: crate::types::mana::ManaCost::default(),
            keywords: vec![Keyword::Trample],
            abilities: vec![],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![ManaColor::Green, ManaColor::Red],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            // CR 712.16: a transform DFC records the Transform layout on its back
            // face so `is_double_faced_permanent` recognizes it (the mass-transform
            // resolver pre-filters on that authority).
            layout_kind: Some(crate::types::card::LayoutKind::Transform),
        });
        id
    }

    #[test]
    fn transform_effect_uses_source_when_no_explicit_target() {
        let mut state = GameState::new_two_player(42);
        let source_id = setup_dfc(&mut state);
        let ability = ResolvedAbility::new(
            Effect::Transform {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let object = &state.objects[&source_id];
        assert!(object.transformed);
        assert_eq!(object.name, "Back Face");
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Transform,
                source_id: emitted_source,
            ..} if *emitted_source == source_id
        )));
    }

    #[test]
    fn transform_effect_uses_explicit_object_target() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target_id = setup_dfc(&mut state);
        let ability = ResolvedAbility::new(
            Effect::Transform {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&target_id].transformed);
        assert!(!state.objects[&source_id].transformed);
    }

    #[test]
    fn repeated_activated_self_transform_ignores_the_stale_instruction() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::stack::push_to_stack;
        use crate::types::ability::QuantityExpr;
        use crate::types::counter::CounterType;
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        crate::game::effects::incubate::resolve(
            &mut state,
            &ResolvedAbility::new(
                Effect::Incubate {
                    count: QuantityExpr::Fixed { value: 5 },
                },
                vec![],
                ObjectId(99),
                PlayerId(0),
            ),
            &mut events,
        )
        .expect("Sunfall-style Incubator is created");
        let source_id = *state
            .battlefield
            .iter()
            .find(|id| state.objects[id].name == "Incubator")
            .expect("Incubator on battlefield");
        let definition = state.objects[&source_id]
            .abilities
            .first()
            .expect("Incubator has a transform ability")
            .clone();
        let transform = || build_resolved_from_def(&definition, source_id, PlayerId(0));

        for entry_id in [ObjectId(100), ObjectId(101)] {
            push_to_stack(
                &mut state,
                StackEntry {
                    id: entry_id,
                    source_id,
                    controller: PlayerId(0),
                    kind: StackEntryKind::ActivatedAbility {
                        source_id,
                        ability: Box::new(transform()),
                    },
                },
                &mut events,
            );
        }

        for _ in 0..2 {
            let entry = state.stack.pop_back().expect("transform ability on stack");
            resolve(
                &mut state,
                entry.ability().expect("activated ability"),
                &mut events,
            )
            .expect("transform ability resolves");
        }

        assert!(
            state.objects[&source_id].transformed,
            "CR 701.27f: the second self-transform instruction must be ignored"
        );
        assert_eq!(state.objects[&source_id].name, "Phyrexian Token");
        assert_eq!(
            state.objects[&source_id]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&5)
        );
        assert_eq!(state.objects[&source_id].transformation_count, 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::Transformed { object_id } if *object_id == source_id))
                .count(),
            1,
            "only the first resolving activation transforms the Incubator"
        );
    }

    #[test]
    fn self_transform_does_not_follow_a_blinked_source() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::stack::push_to_stack;
        use crate::game::zones::move_to_zone;
        use crate::types::game_state::{StackEntry, StackEntryKind};

        for triggered in [false, true] {
            let mut state = GameState::new_two_player(42);
            let source_id = setup_dfc(&mut state);
            let initial_incarnation = state.objects[&source_id].incarnation;
            let definition = state.objects[&source_id].abilities[0].clone();
            let ability = build_resolved_from_def(&definition, source_id, PlayerId(0));
            let kind = if triggered {
                StackEntryKind::TriggeredAbility {
                    source_id,
                    ability: Box::new(ability),
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: "Front Face".to_string(),
                    subject_match_count: None,
                    die_result: None,
                }
            } else {
                StackEntryKind::ActivatedAbility {
                    source_id,
                    ability: Box::new(ability),
                }
            };
            let mut events = Vec::new();
            push_to_stack(
                &mut state,
                StackEntry {
                    id: ObjectId(100),
                    source_id,
                    controller: PlayerId(0),
                    kind,
                },
                &mut events,
            );
            assert_eq!(
                state
                    .stack
                    .back()
                    .and_then(|entry| entry.ability())
                    .and_then(|ability| ability.source_incarnation),
                Some(initial_incarnation)
            );

            move_to_zone(&mut state, source_id, Zone::Exile, &mut events);
            move_to_zone(&mut state, source_id, Zone::Battlefield, &mut events);
            assert_ne!(state.objects[&source_id].incarnation, initial_incarnation);
            assert_eq!(state.objects[&source_id].transformation_count, 0);

            let entry = state.stack.pop_back().expect("transform ability on stack");
            resolve(
                &mut state,
                entry.ability().expect("transform ability"),
                &mut events,
            )
            .expect("transform ability resolves");

            assert!(
                !state.objects[&source_id].transformed,
                "CR 400.7: a stale {} self-transform must not affect the re-entered source",
                if triggered { "triggered" } else { "activated" }
            );
        }
    }

    #[test]
    fn delayed_self_transform_ignores_an_intervening_transform() {
        use crate::game::stack::push_to_stack;
        use crate::types::ability::DelayedTriggerCondition;
        use crate::types::game_state::{StackEntry, StackEntryKind};
        use crate::types::phase::Phase;

        let mut state = GameState::new_two_player(42);
        let source_id = setup_dfc(&mut state);
        let mut events = Vec::new();
        let create_delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Transform {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                    },
                )),
                uses_tracked_set: false,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        crate::game::effects::delayed_trigger::resolve(&mut state, &create_delayed, &mut events)
            .expect("delayed transform is created");

        transform_permanent(&mut state, source_id, &mut events)
            .expect("source transforms before the delayed ability fires");
        let delayed = state.delayed_triggers.remove(0);
        push_to_stack(
            &mut state,
            StackEntry {
                id: ObjectId(100),
                source_id,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id,
                    ability: delayed.ability,
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: "Front Face".to_string(),
                    subject_match_count: None,
                    die_result: None,
                },
            },
            &mut events,
        );
        let entry = state.stack.pop_back().expect("delayed transform on stack");
        resolve(
            &mut state,
            entry.ability().expect("triggered ability"),
            &mut events,
        )
        .expect("delayed transform resolves");

        assert!(
            state.objects[&source_id].transformed,
            "CR 701.27f: a delayed self-transform must be ignored if its source transformed since the delayed ability was created"
        );
        assert_eq!(state.objects[&source_id].transformation_count, 1);
    }

    #[test]
    fn delayed_self_transform_does_not_follow_a_blinked_source() {
        use crate::game::stack::push_to_stack;
        use crate::game::zones::move_to_zone;
        use crate::types::ability::DelayedTriggerCondition;
        use crate::types::game_state::{StackEntry, StackEntryKind};
        use crate::types::phase::Phase;

        let mut state = GameState::new_two_player(42);
        let source_id = setup_dfc(&mut state);
        let initial_incarnation = state.objects[&source_id].incarnation;
        let mut events = Vec::new();
        let create_delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Transform {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                    },
                )),
                uses_tracked_set: false,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        crate::game::effects::delayed_trigger::resolve(&mut state, &create_delayed, &mut events)
            .expect("delayed transform is created");

        move_to_zone(&mut state, source_id, Zone::Exile, &mut events);
        move_to_zone(&mut state, source_id, Zone::Battlefield, &mut events);
        let delayed = state.delayed_triggers.remove(0);
        assert_eq!(
            delayed.ability.source_incarnation,
            Some(initial_incarnation)
        );
        push_to_stack(
            &mut state,
            StackEntry {
                id: ObjectId(100),
                source_id,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id,
                    ability: delayed.ability,
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: "Front Face".to_string(),
                    subject_match_count: None,
                    die_result: None,
                },
            },
            &mut events,
        );
        let entry = state.stack.pop_back().expect("delayed transform on stack");
        let ability = entry.ability().expect("triggered ability");
        assert_eq!(ability.source_incarnation, Some(initial_incarnation));
        resolve(&mut state, ability, &mut events).expect("delayed transform resolves");

        assert!(
            !state.objects[&source_id].transformed,
            "CR 400.7: the delayed self-transform must not affect the re-entered source"
        );
        assert_eq!(state.objects[&source_id].transformation_count, 0);
    }

    /// A single-faced (non-DFC) creature with an arbitrary subtype, on the
    /// battlefield. `back_face` is `None`, so `transform_permanent` would return
    /// the "Card has no back face" error if it were ever called on it.
    fn make_single_faced(state: &mut GameState, name: &str, subtype: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(7),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![subtype.to_string()],
        };
        obj.base_card_types = obj.card_types.clone();
        id
    }

    fn human_all_filter() -> TargetFilter {
        use crate::types::ability::{TypeFilter, TypedFilter};
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature).subtype("Human".to_string()))
    }

    /// B1 (issue #6403, the bug-fix linchpin, CR 115.10a): the mass (`All`) scope
    /// exposes NO target slot — so the cast/trigger pipeline builds no
    /// one-target prompt — while the `Single` scope still surfaces its target.
    /// Reverting the `Effect::target_filter()` scope-split (leaving Transform in
    /// the unconditional `Some(target)` group) makes the `All` arm return `Some`
    /// ⇒ a prompt ⇒ the first assertion flips red.
    #[test]
    fn mass_transform_exposes_no_target_slot() {
        let mass = Effect::Transform {
            target: human_all_filter(),
            scope: EffectScope::All,
        };
        assert!(
            mass.target_filter().is_none(),
            "mass Transform must expose no target slot (CR 115.10a)"
        );
        let single = Effect::Transform {
            target: human_all_filter(),
            scope: EffectScope::Single,
        };
        assert!(
            single.target_filter().is_some(),
            "single Transform must surface its target (CR 115.1)"
        );
    }

    /// PRIMARY revert-guard (issue #6403, production path): Moonmist's verbatim
    /// Oracle text parses to a mass Transform and resolves over a battlefield of
    /// two transformable Humans (DFC) plus a Goblin and a Werewolf — BOTH Humans
    /// transform, the non-Humans are untouched, and NO prompt is installed.
    /// Reverting the parser mass branch (parses `scope: Single`) or `resolve_all`
    /// flips this red.
    #[test]
    fn moonmist_transforms_all_humans_without_a_prompt() {
        let parsed = crate::parser::parse_oracle_text(
            "Transform all Humans. Prevent all combat damage that would be dealt this turn by creatures other than Werewolves and Wolves.",
            "Moonmist",
            &[],
            &["Instant".to_string()],
            &[],
        );
        let def = parsed
            .abilities
            .first()
            .expect("Moonmist parses a spell ability");
        // Production-path parser shape: the head is a mass Transform.
        assert!(
            matches!(
                *def.effect,
                Effect::Transform {
                    scope: EffectScope::All,
                    ..
                }
            ),
            "Moonmist must parse to a mass Transform, got {:?}",
            def.effect
        );
        assert!(
            def.effect.target_filter().is_none(),
            "mass Transform must build no target slot (CR 115.10a)"
        );
        // Sibling intact: the prevent-combat-damage clause is preserved as the
        // sub-ability (the mass branch must not swallow the rest of the card).
        let sibling = def
            .sub_ability
            .as_deref()
            .expect("Moonmist's prevent-combat-damage sibling must be preserved");
        assert!(
            matches!(*sibling.effect, Effect::PreventDamage { .. }),
            "the second sentence must parse to PreventDamage, got {:?}",
            sibling.effect
        );

        let mut state = GameState::new_two_player(42);
        let human_a = setup_dfc(&mut state);
        let human_b = setup_dfc(&mut state);
        let goblin = make_single_faced(&mut state, "Goblin Raider", "Goblin");
        let werewolf = make_single_faced(&mut state, "Lone Wolf", "Werewolf");
        let source = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Moonmist".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new((*def.effect).clone(), vec![], source, PlayerId(0));

        let waiting_before = std::mem::discriminant(&state.waiting_for);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("mass transform resolves");

        assert!(
            state.objects[&human_a].transformed,
            "first Human transforms"
        );
        assert!(
            state.objects[&human_b].transformed,
            "second Human transforms"
        );
        assert!(
            !state.objects[&goblin].transformed,
            "the Goblin is not a Human — untouched"
        );
        assert!(
            !state.objects[&werewolf].transformed,
            "the Werewolf is not a Human — untouched"
        );
        assert_eq!(
            std::mem::discriminant(&state.waiting_for),
            waiting_before,
            "mass transform must not install any WaitingFor prompt"
        );
    }

    /// B2 (issue #6403, CR 701.27c): "all X" matches mostly SINGLE-FACED
    /// permanents. A single-faced Human in the population must NOT abort
    /// resolution — the DFC transforms, the non-DFC does nothing. Reverting the
    /// `resolve_all` DFC pre-filter (letting `transform_permanent`'s "no back
    /// face" error `?`-propagate) makes `resolve` return `Err` ⇒ this fails.
    #[test]
    fn mass_transform_skips_single_faced_human_without_error() {
        let mut state = GameState::new_two_player(42);
        let dfc_human = setup_dfc(&mut state);
        let single_human = make_single_faced(&mut state, "Village Ironsmith", "Human");
        let source = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Src".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::Transform {
                target: human_all_filter(),
                scope: EffectScope::All,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events)
            .expect("a non-DFC Human in the population must not error (CR 701.27c)");

        assert!(
            state.objects[&dfc_human].transformed,
            "the Human-faced DFC transforms"
        );
        assert!(
            !state.objects[&single_human].transformed,
            "the single-faced Human is untouched (CR 701.27c)"
        );
    }
}
