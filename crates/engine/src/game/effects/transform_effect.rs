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
/// - `EffectScope::Single` (legacy targeted/anaphoric transform) acts on ONE
///   permanent, whose identity is resolved through `targeting::resolved_targets`
///   (CR 201.5: a printed self-reference binds the ability's own source, never a
///   referent an earlier chain instruction bound).
/// - `EffectScope::All` ("Transform all Humans" — Moonmist) is a non-targeting
///   mass transform that enumerates the population filter over the battlefield
///   (`resolve_all`).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 115.1 + CR 701.27a: the effect's own declared scope decides which
    // resolver runs. Destructured with `let … else` so the dispatch below can be
    // WILDCARD-FREE: a future `EffectScope` variant must be a COMPILE ERROR
    // here, not a runtime `InvalidParam("expected Transform effect")` that
    // misreports a new scope as a wrong effect.
    let Effect::Transform { scope, target, .. } = &ability.effect else {
        return Err(EffectError::InvalidParam(
            "expected Transform effect".to_string(),
        ));
    };
    let single_target = match scope {
        EffectScope::All => {
            let target = target.clone();
            return resolve_all(state, ability, &target, events);
        }
        EffectScope::Single => target.clone(),
    };

    // CR 201.5: "Text that refers to the object it's on by name means just that
    // particular object and not any other objects with that name." A printed
    // self-transform ("transform Runo" / "transform this creature") names the
    // ability's OWN SOURCE — it can never denote an object some EARLIER
    // instruction bound. The chain layer legitimately propagates a parent's
    // object target down to a sub (CR 608.2c: instructions in the order
    // written), which for a `Dig`/`Reveal` parent is an OFF-BATTLEFIELD card —
    // the looked-at library card for Runo and Delver, and for Sidequest the card
    // its `ChangeZone` parent has already moved to hand. Reading
    // `ability.targets` positionally made THAT card the subject instead of the
    // permanent, and `transform_permanent` rejects an object that is not on the
    // battlefield, so the printed transform never happened (issue #8586: Runo
    // Stromkirk, Delver of Secrets, Sidequest: Catch a Fish).
    //
    // `targeting::resolved_targets` is the engine's single authority for the
    // self-ref → event-context → chosen-targets ladder, and its `SelfRef` arm
    // short-circuits BEFORE the `ability.targets` fallback for exactly this
    // reason (its own doc comment names this failure mode). Every other
    // subject-resolving effect module already goes through it, directly or via
    // `effects::resolved_battlefield_object_ids`; this brings `Transform` onto
    // the same authority rather than filtering the chain's target vector, which
    // is ALSO the carrier its own descendants read (a chain-layer filter was
    // measured regressing Necrotic Plague's nested Attach → ParentTarget).
    //
    // NOT `effects::resolved_battlefield_object_ids`, which is this exact
    // pairing PLUS a zone-scan fallback. Two independent reasons, and the FIRST
    // is the load-bearing one:
    //   (1) For a STALE `SelfRef` the explicit set is empty, so that helper
    //       falls through to its zone scan — and the scan re-admits the source
    //       BY ID through `matches_target_filter`'s `SelfRef` arm
    //       (-> `object_matches_trigger_source`, which performs NO incarnation
    //       check). Reusing the composed helper would therefore SILENTLY DEFEAT
    //       the CR 400.7 guard this module exists to enforce.
    //   (2) Its own CR 601.2c / CR 608.2b comment reserves that scan for
    //       non-targeted forms, naming `SelfRef` first; here the required
    //       behaviour is the INVERSE (CR 400.7: a stale self-reference must
    //       transform NOTHING), and for a `Typed` filter the scan would perform
    //       a battlefield-wide mass transform the card never printed (that is
    //       `resolve_all`'s job, and it returned above).
    let subjects = super::resolved_effect_object_ids(state, ability, &single_target);

    // CR 400.7 + CR 603.7c: a delayed transform whose pinned referent became a
    // new object transforms nothing — but that guard governs ONLY a referent
    // this ability actually took from `ability.targets`. After the resolution
    // above, a `SelfRef` subject comes from `ability.source_id`, so a FOREIGN
    // chain-injected target whose pin went stale must not veto it (CR 201.5:
    // the printed name binds the source, and another object's history cannot
    // speak to it).
    //
    // THE TEST BELOW IS BY VALUE IDENTITY, NOT BY PROVENANCE, and the name is a
    // shorthand — read it as "every subject also appears among the declared
    // targets". It cannot distinguish "this subject CAME FROM `ability.targets`"
    // from "this `SelfRef` subject HAPPENS ALSO to appear there", which is the
    // real shape a chain parent that targeted the source itself produces. That
    // overlap is harmless in BOTH directions, which is why value identity is
    // sufficient here:
    //   * source STALE  -> `resolved_targets` returns an empty list, `subjects`
    //     is empty, `.all()` is vacuously true, the guard fires, and the no-op
    //     is the CORRECT outcome (CR 400.7).
    //   * source LIVE   -> `pinned_object_targets_all_stale` is false (the
    //     source's own pin is current), the guard does NOT fire, and the
    //     transform happens — also correct.
    // Do not "strengthen" this into a provenance check: there is no provenance
    // to read. Those two bullets are the shapes this guard EXISTS for, but they
    // are NOT an exhaustive partition: `self_ref_is_current` can report a
    // LATCHED trigger source as current while the pinned incarnation read here
    // is already stale, which is a third shape neither bullet describes. That
    // shape is SAFE rather than impossible — it lands on a conservative no-op,
    // either here or at the `stale_self_transform` check below, which re-tests
    // the source through `source_is_current` (CR 400.7). So there is no defect,
    // but do not reason from the two bullets as if nothing else can occur.
    //
    // PLACEMENT IS STILL LOAD-BEARING, in the mirrored direction: for a subject
    // that DID come from `ability.targets`, this must NO-OP rather than REBIND.
    // `resolved_targets` drops pin-stale entries for `ParentTarget` (via
    // `live_object_targets`), which empties `subjects`; without this guard the
    // `[]` arm below would resolve to `ability.source_id` and transform the
    // ability's OWN SOURCE. "No target declared" and "the declared referent went
    // stale" must not collapse into the same OUTCOME.
    //
    // An all-empty `subjects` satisfies `.all()` VACUOUSLY — that is deliberate,
    // it is exactly the `ParentTarget`-went-stale case this must catch, and it is
    // safe because `pinned_object_targets_all_stale` itself requires a non-empty
    // `target_incarnations` AND at least one object target, so it fails closed on
    // an ability that pinned nothing.
    //
    // Scoped to `EffectScope::Single` by construction: the `All` branch returned
    // above into `resolve_all`, a non-targeting battlefield sweep with no
    // referent to pin.
    //
    // `flip_permanent.rs` (CR 701.28a: converting follows CR 701.27a–f) CARRIES
    // THE SAME DEFECT, STILL LIVE. This is a DEFERRAL, NOT A DIVERGENCE: the two
    // modules are not making different choices, one of them simply has not been
    // fixed yet. It is the SAME MECHANISM, not an analogous one —
    // `effects/mod.rs`'s `inject_last_revealed_targets` writes
    // `last_revealed_ids` into any sub's `targets`, and `flip_permanent.rs` then
    // reads `ability.targets.as_slice()` POSITIONALLY, so an injected
    // off-battlefield card displaces the printed self-reference exactly as it
    // did here. `game::flip::flip_permanent` no-ops on an object that is not on
    // the battlefield (CR 710.2), so the printed flip is silently lost.
    //
    // MEASURED, with the bare two-instruction probe:
    //   "At the beginning of your upkeep, look at the top card of your library.
    //    Flip this creature."                     -> flipped = false  (DEFECT)
    //   "At the beginning of your upkeep, put a +1/+1 counter on this creature.
    //    Flip this creature."                     -> flipped = true   (control)
    // Both runs reached the seam (trigger fired, drive stopped in the upkeep
    // step, `back_face` installed; the defect run looked at exactly one card),
    // so the `false` is a real miss and not an unreached fixture.
    //
    // CORPUS CARDS with the vulnerable shape — `FlipPermanent { target: SelfRef }`
    // sequenced after an instruction that binds an object target — are
    // NEZUMI GRAVEROBBER ("Exile target card from an opponent's graveyard. If no
    // cards are in that graveyard, flip this creature.") and BUDOKA GARDENER
    // ("You may put a land card from your hand onto the battlefield. If you
    // control ten or more lands, flip this creature."). Both were identified BY
    // PARSE SHAPE, not by an end-to-end run — they are candidates the probe
    // above makes credible, not separately measured failures.
    //
    // Deferred rather than fixed here: see the PR body's
    // `## Deferred / known-remaining`, DW#2 (migrate `flip_permanent.rs` onto
    // `targeting::resolved_targets` the same way this module now does).
    let subject_came_from_declared_targets = subjects
        .iter()
        .all(|id| ability.targets.contains(&TargetRef::Object(*id)));
    if subject_came_from_declared_targets && ability.pinned_object_targets_all_stale(state) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Transform,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // CR 701.27c: If a spell or ability instructs a player to transform a
    // permanent that isn't represented by a DOUBLE-FACED TOKEN OR A double-faced
    // card, nothing happens. (The `?` on `transform_permanent` below is retained
    // deliberately — DW#6 in the PR body, the deferred question of whether a
    // non-double-faced subject should keep propagating an error here or become
    // the silent CR 701.27c no-op the rule describes. This change alters WHICH
    // object can reach that error: for Runo / Delver / Sidequest it REMOVES one, because
    // the old positional subject was an off-battlefield card (the library for
    // Runo and Delver, the hand for Sidequest). The suite-wide scan found
    // no new reachability, which is evidence for the defer, not proof.)
    let object_id = match subjects.as_slice() {
        [object_id] => *object_id,
        // CR 400.7 + CR 701.27f: no bound object — either the printed no-target
        // self-transform, or a `SelfRef` whose source is no longer current,
        // which `resolved_targets` reports as an EMPTY list. Both land on
        // `source_id` and are then filtered by `stale_self_transform` below,
        // which is what keeps "no target declared" and "the referent went stale"
        // from collapsing into the same OUTCOME even though they share this arm.
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
            is_swap_snapshot: false,
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
            parse_warnings: vec![],
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
                    provenance: None,
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
                    provenance: None,
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
                    provenance: None,
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

    /// CR 201.5 + CR 701.27a + CR 701.27c: a printed self-transform handed ONLY a
    /// player target transforms its own source.
    ///
    /// SEMANTIC WIDENING — deliberate, and this is its rule basis. Before this
    /// change the positional `ability.targets.as_slice()` match saw
    /// `[TargetRef::Player(_)]`, fell into the `_` arm and returned `InvalidParam`,
    /// which ABORTS THE WHOLE CHAIN. A chain site deliberately keeps
    /// `TargetRef::Player(_)` propagatable, so a `Transform{SelfRef}` sub beneath a
    /// player-targeting parent genuinely receives this shape.
    ///
    /// THE WIDENING IS BROADER THAN THIS ROW'S NAME (r1 N-2): the same `_`-arm
    /// collapse also covers a MIXED vector such as `[Object(a), Player(p)]` —
    /// measured `InvalidParam` + chain abort before, and `a` transformed after.
    /// The justification below carries that shape unchanged: CR 201.5 decides the
    /// subject either way, and the player entry was never a candidate subject.
    /// The second half of this test asserts that mixed shape so the sentence
    /// above is carried by an assertion rather than by a comment.
    ///
    /// CR 201.5 settles what the subject is: the printed name binds the ability's
    /// source, so the handed player was never a candidate subject. CR 701.27a then
    /// turns that source over, and CR 701.27c names the ONLY thing that stops it —
    /// a permanent not represented by a double-faced token or a double-faced card
    /// does nothing. Nothing in the rules makes a stray player target abort a
    /// printed self-transform, so `InvalidParam` was the wrong outcome, not a
    /// behaviour worth preserving.
    ///
    /// FLIPS ON REVERT: with the positional match restored this returns `Err`.
    /// Its negative sibling is
    /// `two_object_targets_still_reject_a_single_scope_transform`, which keeps the
    /// `_` arm demonstrably reachable so this row is not vacuous.
    #[test]
    fn self_ref_transform_with_only_a_player_target_transforms_its_source() {
        let mut state = GameState::new_two_player(42);
        let src = setup_dfc(&mut state);
        let ability = ResolvedAbility::new(
            Effect::Transform {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Player(PlayerId(1))],
            src,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events)
            .expect("CR 201.5: a stray player target must not abort a printed self-transform");

        assert!(
            state.objects[&src].transformed,
            "CR 201.5 + CR 701.27a: the printed name binds the ability's own source"
        );

        // The MIXED vector from the doc comment above: a non-self filter whose
        // chosen targets carry one object and one player. `effect_object_targets`
        // drops the player, leaving exactly one subject, so the `_` arm is no
        // longer reached and the object transforms.
        let mut state = GameState::new_two_player(42);
        let src = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Source".to_string(),
            Zone::Stack,
        );
        let bound = setup_dfc(&mut state);
        let ability = ResolvedAbility::new(
            Effect::Transform {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(bound), TargetRef::Player(PlayerId(1))],
            src,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events)
            .expect("CR 201.5: a trailing player target must not abort the transform");

        assert!(
            state.objects[&bound].transformed,
            "the single OBJECT target is the subject; the player entry was never a candidate"
        );
    }

    /// NEGATIVE SIBLING of
    /// `self_ref_transform_with_only_a_player_target_transforms_its_source`, and
    /// the proof that row is not vacuous: the `_` arm of the subject match stays
    /// genuinely reachable.
    ///
    /// CR 115.1: a `Single`-scope transform declares ONE target. With a non-self
    /// filter and two live object targets, `resolved_targets` falls through to
    /// `chosen_targets_satisfy_filter` -> `ability.targets.clone()`, so
    /// `effect_object_targets` yields two subjects and the effect must still
    /// reject them. Measured `InvalidParam` BOTH before and after this change —
    /// this row does NOT flip on revert, and that is its whole point.
    #[test]
    fn two_object_targets_still_reject_a_single_scope_transform() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Source".to_string(),
            Zone::Stack,
        );
        let first = setup_dfc(&mut state);
        let second = setup_dfc(&mut state);
        let ability = ResolvedAbility::new(
            Effect::Transform {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);

        assert!(
            matches!(result, Err(EffectError::InvalidParam(_))),
            "CR 115.1: two object targets are not a single-scope transform subject; got {result:?}"
        );
        assert!(!state.objects[&first].transformed);
        assert!(!state.objects[&second].transformed);
    }

    /// CR 400.7: a `SelfRef` transform whose source is no longer the pinned object
    /// transforms nothing.
    ///
    /// PRESERVATION ROW — NOT A DISCRIMINATOR, and it must stay labelled that way.
    /// It passes IDENTICALLY before and after this change: `resolved_targets`
    /// reports a stale self-reference as an EMPTY list, which lands on the `[]` arm
    /// -> `source_id` -> `stale_self_transform`, the same outcome the positional read
    /// reached from an empty `ability.targets`. Its job is to prove the new subject
    /// resolution did not LOSE the guard, not to fail on revert. Do not promote it
    /// to a discriminator in a later edit.
    ///
    /// MEASURED CAVEAT ON THE MECHANISM, because this fixture does not reach the
    /// empty-list route and must not pretend to. `self_ref_is_current`
    /// (`types/ability.rs`) only reports a stale self-reference as EMPTY when the
    /// ability carries a latched `trigger_source`; `build_resolved_from_def`
    /// latches none, on EITHER stack shape. So on all four branches below
    /// `resolved_targets` returns `[Object(source_id)]`, the `[object_id]` arm
    /// binds the source, and the guard that is preserved here is
    /// `stale_self_transform` (which reads `source_is_current`) — asserted
    /// inline, not described. The OUTCOME is identical to the empty-list route,
    /// which is what makes this a preservation row either way. Do not "simplify"
    /// the mechanism assertion into the empty-list sentence: it was measured
    /// false for this fixture on both the activated and the triggered shape.
    ///
    /// Positive control in the same test: the live-source sibling DOES transform,
    /// so "did not transform" cannot pass because the fixture is inert.
    ///
    /// The two broader end-to-end rows this one narrows —
    /// `self_transform_does_not_follow_a_blinked_source` and
    /// `delayed_self_transform_does_not_follow_a_blinked_source` — stay green
    /// unmodified.
    #[test]
    fn self_ref_subject_resolution_preserves_the_stale_source_guard() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::stack::push_to_stack;
        use crate::game::zones::move_to_zone;
        use crate::types::game_state::{StackEntry, StackEntryKind};

        // `blinked == false` is the POSITIVE CONTROL: the identical fixture with
        // the source left live must transform.
        for triggered in [false, true] {
            for blinked in [false, true] {
                let mut state = GameState::new_two_player(42);
                let source_id = setup_dfc(&mut state);
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
                        provenance: None,
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
                if blinked {
                    // CR 400.7: the re-entered permanent is a NEW object.
                    move_to_zone(&mut state, source_id, Zone::Exile, &mut events);
                    move_to_zone(&mut state, source_id, Zone::Battlefield, &mut events);
                }

                let entry = state.stack.pop_back().expect("transform ability on stack");
                let ability = entry.ability().expect("transform ability");
                // REACH GUARD: the fixture must actually put the source in the
                // incarnation state this iteration is about, or the four branches
                // all measure the same thing.
                assert_eq!(
                    ability.source_is_current(&state),
                    !blinked,
                    "fixture must make the source {} (CR 400.7)",
                    if blinked { "stale" } else { "current" }
                );
                // MECHANISM, asserted rather than described: with no latched
                // `trigger_source` the SelfRef arm binds the source on every
                // branch, so the surviving guard below is `stale_self_transform`.
                let resolved = crate::game::targeting::resolved_targets(
                    ability,
                    &TargetFilter::SelfRef,
                    &state,
                );
                assert_eq!(
                    resolved,
                    vec![TargetRef::Object(source_id)],
                    "this fixture latches no `trigger_source`, so the SelfRef arm binds the \
                     source on every branch (triggered={triggered}, blinked={blinked})"
                );

                resolve(&mut state, ability, &mut events).expect("transform ability resolves");

                assert_eq!(
                    state.objects[&source_id].transformed, !blinked,
                    "CR 400.7: a stale self-reference transforms nothing, a live one transforms \
                     (triggered={triggered})"
                );
            }
        }
    }

    /// DEFENSIVE — no corpus card produces this shape. The structural card-data
    /// scan found no `Transform{SelfRef}` sub that also carries a PINNED FOREIGN
    /// object target, so this is a guard on the scoping logic, not a regression
    /// test for a live card. Labelled as such deliberately.
    ///
    /// CR 201.5 + CR 400.7: a live printed self-transform must not be vetoed by a
    /// foreign chain-injected target whose pin went stale — the printed name binds
    /// the source, and another object's history cannot speak to it.
    ///
    /// REACH GUARD FIRST, or this row is vacuous.
    #[test]
    fn stale_foreign_pin_does_not_veto_a_live_self_transform() {
        use crate::game::zones::move_to_zone;
        use crate::types::identifiers::ObjectIncarnationRef;

        let mut state = GameState::new_two_player(42);
        let src = setup_dfc(&mut state);
        let foreign = make_single_faced(&mut state, "Foreign", "Goblin");
        let pin = ObjectIncarnationRef::from_object(&state.objects[&foreign]);
        let mut ability = ResolvedAbility::new(
            Effect::Transform {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(foreign)],
            src,
            PlayerId(0),
        );
        ability.set_target_incarnations_recursive(vec![pin]);

        let mut events = Vec::new();
        // CR 400.7: blink the FOREIGN object so its pin — and only its pin — goes
        // stale. The ability's own source is untouched and stays live.
        move_to_zone(&mut state, foreign, Zone::Exile, &mut events);
        move_to_zone(&mut state, foreign, Zone::Battlefield, &mut events);

        assert!(
            ability.pinned_object_targets_all_stale(&state),
            "fixture must actually put the pin guard in its firing state"
        );
        resolve(&mut state, &ability, &mut events).expect("the live self-transform resolves");
        assert!(
            state.objects[&src].transformed,
            "CR 201.5: a foreign target's stale pin cannot veto a printed self-transform"
        );
        assert!(
            !state.objects[&foreign].transformed,
            "the foreign object is never the subject of a printed self-transform"
        );
    }
}
