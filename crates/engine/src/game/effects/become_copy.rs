use crate::game::layers::compute_current_copiable_values;
use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 707.2 / CR 613.1a: Become a copy of target permanent via a layer-1 copy effect.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (duration, additional_modifications) = match &ability.effect {
        Effect::BecomeCopy {
            duration,
            additional_modifications,
            ..
        } => (
            duration
                .clone()
                .or(ability.duration.clone())
                .unwrap_or(Duration::Permanent),
            additional_modifications.clone(),
        ),
        _ => (
            ability.duration.clone().unwrap_or(Duration::Permanent),
            Vec::new(),
        ),
    };

    let target_id = ability
        .targets
        .iter()
        .find_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .ok_or_else(|| EffectError::MissingParam("BecomeCopy requires a target".to_string()))?;

    let values = compute_current_copiable_values(state, target_id)
        .ok_or(EffectError::ObjectNotFound(target_id))?;

    // CR 122.1 + CR 614.1c: `AddCounterOnEnter` is consumed at resolution
    // (not layered) — partition the modifications so the layer pipeline only
    // sees the layered variants and the counter-on-enter variants are
    // applied via the counter primitive after layer evaluation.
    let (counter_mods, layered_mods): (Vec<_>, Vec<_>) = additional_modifications
        .into_iter()
        .partition(|m| matches!(m, ContinuousModification::AddCounterOnEnter { .. }));

    let mut modifications = vec![ContinuousModification::CopyValues {
        values: Box::new(values),
    }];
    modifications.extend(layered_mods);

    state.add_transient_continuous_effect(
        ability.source_id,
        ability.controller,
        duration,
        TargetFilter::SpecificObject {
            id: ability.source_id,
        },
        modifications,
        None,
    );

    // CR 707.9f: "Some exceptions to the copying process apply only if the
    // copy is or has certain characteristics" — re-evaluate layers so the
    // copied card_types is realized before checking `if_type` on each
    // counter-on-enter modification (Spark Double's branches). Counters are
    // then placed via the shared replacement-aware primitive (Doubling
    // Season etc. apply normally).
    if !counter_mods.is_empty() {
        crate::game::layers::evaluate_layers(state);
        for modification in counter_mods {
            if let ContinuousModification::AddCounterOnEnter {
                counter_type,
                count,
                if_type,
            } = modification
            {
                let n = crate::game::quantity::resolve_quantity(
                    state,
                    &count,
                    ability.controller,
                    ability.source_id,
                )
                .max(0) as u32;
                if n == 0 {
                    continue;
                }
                let gate_passes = match if_type {
                    None => true,
                    Some(t) => state
                        .objects
                        .get(&ability.source_id)
                        .map(|obj| obj.card_types.core_types.contains(&t))
                        .unwrap_or(false),
                };
                if !gate_passes {
                    continue;
                }
                let ct = crate::types::counter::parse_counter_type(&counter_type);
                super::counters::add_counter_with_replacement(
                    state,
                    ability.source_id,
                    ct,
                    n,
                    events,
                );
            }
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
    use std::sync::Arc;

    use super::*;
    use crate::game::layers::{compute_current_copiable_values, evaluate_layers};
    use crate::game::printed_cards::intrinsic_copiable_values;
    use crate::game::turns::execute_cleanup;
    use crate::game::zones::{create_object, move_to_zone};
    use crate::types::ability::{Effect, TargetFilter, TargetRef};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// Helper: create a battlefield creature with base characteristics set.
    fn create_creature(
        state: &mut GameState,
        card_id: u64,
        player: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> crate::types::identifiers::ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.base_name = name.to_string();
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.base_card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        id
    }

    fn make_copy_ability(
        target_id: crate::types::identifiers::ObjectId,
        source_id: crate::types::identifiers::ObjectId,
        player: PlayerId,
        duration: Option<Duration>,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration,
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            player,
        )
    }

    #[test]
    fn become_copy_copies_characteristics_via_layer_one() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Target Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_name = "Target Bear".to_string();
            target.base_power = Some(2);
            target.base_toughness = Some(2);
            target.base_color = vec![ManaColor::Green];
            target.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            target.base_keywords = vec![Keyword::Trample];
        }
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_name = "Copy Source".to_string();
            source.base_power = Some(1);
            source.base_toughness = Some(1);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Shapeshifter".to_string()],
            };
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        let source = state.objects.get(&source_id).unwrap();
        assert_eq!(source.name, "Target Bear");
        assert_eq!(source.power, Some(2));
        assert_eq!(source.toughness, Some(2));
        assert_eq!(source.color, vec![ManaColor::Green]);
        assert!(source.card_types.core_types.contains(&CoreType::Creature));
        assert!(source.card_types.subtypes.contains(&"Bear".to_string()));
        assert!(source.keywords.contains(&Keyword::Trample));
    }

    #[test]
    fn become_copy_until_end_of_turn_reverts_at_cleanup() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Target Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_name = "Target Bear".to_string();
            target.base_power = Some(2);
            target.base_toughness = Some(2);
            target.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
        }
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_name = "Copy Source".to_string();
            source.base_power = Some(1);
            source.base_toughness = Some(1);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Shapeshifter".to_string()],
            };
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: Some(Duration::UntilEndOfTurn),
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&source_id].name, "Target Bear");

        execute_cleanup(&mut state, &mut events);
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&source_id].name, "Copy Source");
        assert_eq!(state.objects[&source_id].power, Some(1));
    }

    #[test]
    fn permanent_become_copy_is_pruned_when_object_leaves_battlefield() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Target Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&target_id).unwrap().base_name = "Target Bear".to_string();
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source_id).unwrap().base_name = "Copy Source".to_string();

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&source_id].name, "Target Bear");

        move_to_zone(&mut state, source_id, Zone::Exile, &mut events);
        move_to_zone(&mut state, source_id, Zone::Battlefield, &mut events);
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&source_id].name, "Copy Source");
    }

    #[test]
    fn become_copy_preserves_additional_modifications() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Target Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_name = "Target Bear".to_string();
            target.base_power = Some(2);
            target.base_toughness = Some(2);
            target.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
        }
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mockingbird".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_name = "Mockingbird".to_string();
            source.base_power = Some(1);
            source.base_toughness = Some(1);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bird".to_string()],
            };
            source.base_keywords = vec![Keyword::Flying];
        }

        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![
                    ContinuousModification::AddSubtype {
                        subtype: "Bird".to_string(),
                    },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Flying,
                    },
                ],
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        let source = state.objects.get(&source_id).unwrap();
        assert_eq!(source.name, "Target Bear");
        assert!(source.card_types.subtypes.contains(&"Bear".to_string()));
        assert!(source.card_types.subtypes.contains(&"Bird".to_string()));
        assert!(source.keywords.contains(&Keyword::Flying));
    }

    // ── Plan test 3/8: Chained copies ─────────────────────────────────────
    // CR 613.2c: After layer-1 application, the resulting values are
    // the object's copiable values. A copies B, then C copies A → C gets
    // B's characteristics (the copy of a copy).
    #[test]
    fn chained_copy_uses_current_copiable_values_not_base() {
        let mut state = GameState::new_two_player(42);
        let bear = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&bear).unwrap().base_color = vec![ManaColor::Green];
        state.objects.get_mut(&bear).unwrap().base_keywords = vec![Keyword::Trample];

        let clone_a = create_creature(&mut state, 2, PlayerId(0), "Clone A", 0, 0);
        let clone_b = create_creature(&mut state, 3, PlayerId(0), "Clone B", 0, 0);

        let mut events = Vec::new();

        // Clone A becomes a copy of Bear
        let ability_a = make_copy_ability(bear, clone_a, PlayerId(0), None);
        resolve(&mut state, &ability_a, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&clone_a].name, "Bear");

        // Clone B becomes a copy of Clone A (which is itself a copy of Bear)
        // CR 707.2: Copiable values include modifications from other copy effects
        let ability_b = make_copy_ability(clone_a, clone_b, PlayerId(0), None);
        resolve(&mut state, &ability_b, &mut events).unwrap();
        evaluate_layers(&mut state);

        let b = &state.objects[&clone_b];
        assert_eq!(b.name, "Bear", "should get Bear's name through the chain");
        assert_eq!(b.power, Some(2));
        assert_eq!(b.toughness, Some(2));
        assert_eq!(b.color, vec![ManaColor::Green]);
        assert!(b.keywords.contains(&Keyword::Trample));
    }

    // ── Plan test 4: intrinsic_copiable_values extraction ─────────────────
    #[test]
    fn intrinsic_copiable_values_reads_base_fields_only() {
        let mut state = GameState::new_two_player(42);
        let id = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_color = vec![ManaColor::Green];
            obj.base_mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            };
            // Set computed fields to different values (as if layer effects applied)
            obj.name = "Pumped Bear".to_string();
            obj.power = Some(5);
            obj.color = vec![ManaColor::Green, ManaColor::Blue];
        }

        let values = intrinsic_copiable_values(state.objects.get(&id).unwrap());
        assert_eq!(values.name, "Bear", "should use base_name, not name");
        assert_eq!(values.power, Some(2), "should use base_power, not power");
        assert_eq!(
            values.color,
            vec![ManaColor::Green],
            "should use base_color"
        );
        assert_eq!(
            values.mana_cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1
            },
            "should capture base_mana_cost"
        );
    }

    // ── Plan test 5: Layer reset with new base fields ─────────────────────
    #[test]
    fn layer_reset_restores_name_mana_cost_loyalty_from_base() {
        let mut state = GameState::new_two_player(42);
        let id = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            };
            obj.base_loyalty = Some(3);
            // Simulate stale computed values from a previous layer evaluation
            obj.name = "Stale Name".to_string();
            obj.mana_cost = ManaCost::default();
            obj.loyalty = Some(99);
        }

        evaluate_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.name, "Bear", "name must reset to base_name");
        assert_eq!(
            obj.mana_cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1
            },
            "mana_cost must reset to base_mana_cost"
        );
        assert_eq!(obj.loyalty, Some(3), "loyalty must reset to base_loyalty");
    }

    // ── Plan test 9: Noncopy later-layer modifications not copied ─────────
    // CR 707.2: Copiable values do not include non-copy modifications.
    #[test]
    fn noncopy_modifications_are_not_copied() {
        let mut state = GameState::new_two_player(42);
        let target = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        let source = create_creature(&mut state, 2, PlayerId(0), "Clone", 0, 0);

        // Give the target a +3/+3 pump via a transient layer-7c effect
        state.add_transient_continuous_effect(
            target,
            PlayerId(0),
            Duration::Permanent,
            TargetFilter::SpecificObject { id: target },
            vec![
                ContinuousModification::AddPower { value: 3 },
                ContinuousModification::AddToughness { value: 3 },
            ],
            None,
        );
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&target].power, Some(5), "target is pumped");

        // Clone copies the target — should get base 2/2, NOT pumped 5/5
        let mut events = Vec::new();
        let ability = make_copy_ability(target, source, PlayerId(0), None);
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        let s = &state.objects[&source];
        assert_eq!(s.power, Some(2), "copy should not inherit pump");
        assert_eq!(s.toughness, Some(2), "copy should not inherit pump");
    }

    // ── Plan test 11: No ETB/LTB events from copy change ─────────────────
    // CR 707.4: Changing what a permanent copies does not trigger ETB or LTB.
    #[test]
    fn become_copy_does_not_emit_etb_or_ltb_events() {
        let mut state = GameState::new_two_player(42);
        let target = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        let source = create_creature(&mut state, 2, PlayerId(0), "Clone", 0, 0);

        let mut events = Vec::new();
        let ability = make_copy_ability(target, source, PlayerId(0), None);
        resolve(&mut state, &ability, &mut events).unwrap();

        // Only EffectResolved should be emitted — no ZoneChange, no ETB
        for event in &events {
            assert!(
                !matches!(event, GameEvent::ZoneChanged { .. }),
                "copy change must not emit ZoneChange events"
            );
        }
    }

    // ── Plan test 12: Cleanup regression for non-copy UntilEndOfTurn ──────
    #[test]
    fn non_copy_until_end_of_turn_effects_still_expire_at_cleanup() {
        let mut state = GameState::new_two_player(42);
        let id = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);

        // Add a non-copy +1/+1 pump until end of turn
        state.add_transient_continuous_effect(
            id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id },
            vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ],
            None,
        );
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&id].power, Some(3), "pumped before cleanup");

        let mut events = Vec::new();
        execute_cleanup(&mut state, &mut events);
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&id].power,
            Some(2),
            "pump expired after cleanup"
        );
    }

    // ── Plan test 13: Token copy of copied permanent ──────────────────────
    // CR 707.2: CopyTokenOf should use current copiable values, not base.
    #[test]
    fn token_copy_of_copied_permanent_gets_copy_characteristics() {
        let mut state = GameState::new_two_player(42);
        let bear = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&bear).unwrap().base_keywords = vec![Keyword::Trample];

        let clone = create_creature(&mut state, 2, PlayerId(0), "Clone", 0, 0);

        let mut events = Vec::new();

        // Clone becomes a copy of Bear
        let ability = make_copy_ability(bear, clone, PlayerId(0), None);
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&clone].name, "Bear");

        // Create a token that's a copy of Clone (which is a copy of Bear)
        let token_ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(clone)],
            clone,
            PlayerId(0),
        );
        crate::game::effects::token_copy::resolve(&mut state, &token_ability, &mut events).unwrap();

        // Find the token — newest object
        let token_id = crate::types::identifiers::ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.name, "Bear", "token should have Bear's name");
        assert_eq!(token.power, Some(2));
        assert!(token.keywords.contains(&Keyword::Trample));
        assert!(token.is_token);
    }

    // ── Plan test 14: DFC transform regression ────────────────────────────
    #[test]
    fn dfc_transform_still_works_after_refactor() {
        use crate::game::game_object::BackFaceData;
        use crate::game::transform::transform_permanent;

        let mut state = GameState::new_two_player(42);
        let id = create_creature(&mut state, 1, PlayerId(0), "Front Face", 2, 3);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            // Set computed fields to match base (as evaluate_layers would)
            obj.power = Some(2);
            obj.toughness = Some(3);
            obj.card_types = obj.base_card_types.clone();
            obj.color = vec![ManaColor::Green];
            obj.base_color = vec![ManaColor::Green];
            obj.back_face = Some(BackFaceData {
                name: "Back Face".to_string(),
                power: Some(5),
                toughness: Some(4),
                loyalty: None,
                defense: None,
                card_types: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec!["Werewolf".to_string()],
                },
                mana_cost: ManaCost::default(),
                keywords: vec![Keyword::Trample],
                abilities: vec![],
                trigger_definitions: Default::default(),
                replacement_definitions: Default::default(),
                static_definitions: Default::default(),
                color: vec![ManaColor::Red],
                printed_ref: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                layout_kind: None,
            });
        }

        let mut events = Vec::new();
        transform_permanent(&mut state, id, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.base_name, "Back Face");
        assert_eq!(obj.base_power, Some(5));
        assert_eq!(obj.base_toughness, Some(4));
        assert_eq!(obj.base_color, vec![ManaColor::Red]);
        assert!(obj.transformed);
        assert!(
            obj.back_face.is_some(),
            "front face stored for reverse transform"
        );

        // Transform back
        transform_permanent(&mut state, id, &mut events).unwrap();
        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.base_name, "Front Face");
        assert_eq!(obj.base_power, Some(2));
        assert!(!obj.transformed);
    }

    // ── Plan test supplement: compute_current_copiable_values building block ──
    #[test]
    fn compute_current_copiable_values_with_no_effects_returns_base() {
        let mut state = GameState::new_two_player(42);
        let id = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().base_keywords = vec![Keyword::Trample];

        let values = compute_current_copiable_values(&state, id).unwrap();
        assert_eq!(values.name, "Bear");
        assert_eq!(values.power, Some(2));
        assert!(values.keywords.contains(&Keyword::Trample));
    }

    // ── Superior Spider-Man: zone-qualified clone + name/PT/type overrides ──
    // CR 707.9b + CR 613.1d + CR 613.1a: When a clone replacement carries
    // additional modifications (name, P/T, type additions), the resulting
    // permanent must end up with the target's abilities (from CopyValues) but
    // the overridden name + P/T (from SetName, SetPower, SetToughness) and
    // additional subtypes layered on top.
    #[test]
    fn become_copy_with_set_name_and_pt_and_subtype_overrides() {
        let mut state = GameState::new_two_player(42);

        // Set up Elesh Norn as the copy source in a graveyard (PlayerId(1)'s).
        let elesh = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Elesh Norn".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&elesh).unwrap();
            obj.base_name = "Elesh Norn".to_string();
            obj.base_power = Some(7);
            obj.base_toughness = Some(7);
            obj.base_card_types = CardType {
                supertypes: vec![crate::types::card_type::Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Phyrexian".to_string(), "Praetor".to_string()],
            };
        }

        // Set up Superior Spider-Man on the battlefield (just-entered clone).
        let spidey = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Superior Spider-Man".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spidey).unwrap();
            obj.base_name = "Superior Spider-Man".to_string();
            obj.base_power = Some(4);
            obj.base_toughness = Some(4);
            obj.base_card_types = CardType {
                supertypes: vec![crate::types::card_type::Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec![
                    "Spider".to_string(),
                    "Human".to_string(),
                    "Hero".to_string(),
                ],
            };
        }

        // Resolve BecomeCopy with exactly the modifications the parser would emit.
        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![
                    ContinuousModification::SetName {
                        name: "Superior Spider-Man".to_string(),
                    },
                    ContinuousModification::SetPower { value: 4 },
                    ContinuousModification::SetToughness { value: 4 },
                    ContinuousModification::AddSubtype {
                        subtype: "Spider".to_string(),
                    },
                    ContinuousModification::AddSubtype {
                        subtype: "Human".to_string(),
                    },
                    ContinuousModification::AddSubtype {
                        subtype: "Hero".to_string(),
                    },
                ],
            },
            vec![TargetRef::Object(elesh)],
            spidey,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        let result = state.objects.get(&spidey).unwrap();

        // Name override (CR 707.9b): not Elesh Norn.
        assert_eq!(result.name, "Superior Spider-Man");

        // P/T override (CR 707.9b + CR 613.4b SetPT): 4/4, not 7/7.
        assert_eq!(result.power, Some(4));
        assert_eq!(result.toughness, Some(4));

        // Types include Elesh Norn's (Phyrexian, Praetor) + Spider-Man's additive
        // list (Spider, Human, Hero) per CR 613.1d. `AddSubtype` is idempotent.
        for subtype in ["Phyrexian", "Praetor", "Spider", "Human", "Hero"] {
            assert!(
                result.card_types.subtypes.iter().any(|s| s == subtype),
                "missing subtype {subtype} in {:?}",
                result.card_types.subtypes
            );
        }
        // Core type preserved (Creature from Elesh Norn).
        assert!(result.card_types.core_types.contains(&CoreType::Creature));
    }

    // CR 707.9b + CR 707.2c: When a second copy effect targets a permanent
    // that already has a copy effect with an overridden name, the second copy
    // must see the overridden name as part of the copiable values, not the
    // original object's base name.
    #[test]
    fn chained_copy_reads_set_name_override_as_copiable_value() {
        let mut state = GameState::new_two_player(42);

        let elesh = create_creature(&mut state, 1, PlayerId(1), "Elesh Norn", 7, 7);
        let spidey = create_creature(&mut state, 2, PlayerId(0), "Superior Spider-Man", 4, 4);

        // Spider-Man copies Elesh Norn with SetName override.
        let spidey_ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![ContinuousModification::SetName {
                    name: "Superior Spider-Man".to_string(),
                }],
            },
            vec![TargetRef::Object(elesh)],
            spidey,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &spidey_ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&spidey].name, "Superior Spider-Man");

        // Now a vanilla Clone copies Spider-Man.
        let clone = create_creature(&mut state, 3, PlayerId(0), "Clone", 0, 0);
        let clone_ability = make_copy_ability(spidey, clone, PlayerId(0), None);
        resolve(&mut state, &clone_ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(
            state.objects[&clone].name, "Superior Spider-Man",
            "clone of Spider-Man copy should see the overridden name as copiable value (CR 707.9b)"
        );
    }

    // ── CR 707.9a: Retain printed trigger from source ─────────────────────
    //
    // Class: Irma, Part-Time Mutant / Cryptoplasm / Volrath's Shapeshifter —
    // the source object copies a target but retains its own printed trigger
    // ("and she has this ability"). The retained trigger must end up in the
    // source's `trigger_definitions` after Layer 1 application alongside any
    // copied triggers; idempotent under repeat application.
    #[test]
    fn become_copy_retains_printed_trigger_from_source() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ContinuousModification, TriggerDefinition,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let target = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&target).unwrap().base_keywords = vec![Keyword::Trample];

        // Source ("Irma") has one printed trigger that must be retained.
        let source = create_creature(&mut state, 2, PlayerId(0), "Irma", 1, 1);
        let printed_trigger = TriggerDefinition::new(TriggerMode::Phase)
            .phase(crate::types::phase::Phase::BeginCombat)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .base_trigger_definitions = Arc::new(vec![printed_trigger.clone()]);
        state.objects.get_mut(&source).unwrap().trigger_definitions =
            crate::types::definitions::Definitions::from(vec![printed_trigger.clone()]);

        // Build the BecomeCopy ability with the retain modification — exactly
        // what the parser emits for "except she has this ability" with
        // current_trigger_index = 0.
        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![
                    ContinuousModification::SetName {
                        name: "Irma".to_string(),
                    },
                    ContinuousModification::RetainPrintedTriggerFromSource {
                        source_trigger_index: 0,
                    },
                ],
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        let copied = state.objects.get(&source).unwrap();
        // CopyValues overwrites trigger_definitions with the target's (Bear's)
        // empty list. SetName overrides the name back to "Irma". Then the
        // RetainPrintedTriggerFromSource pushes the printed trigger back.
        assert_eq!(copied.name, "Irma", "SetName must override the copy name");
        assert_eq!(
            copied.trigger_definitions.iter_all().count(),
            1,
            "exactly one trigger after retain (printed source trigger only — Bear has none); \
             got {:?}",
            copied.trigger_definitions.iter_all().collect::<Vec<_>>()
        );
        assert!(
            copied
                .trigger_definitions
                .iter_all()
                .any(|t| matches!(t.mode, TriggerMode::Phase)),
            "retained trigger must be the printed BeginCombat phase trigger"
        );
    }

    // CR 707.9a: A retained ability is part of the COPIABLE values of the
    // copy. When a *second* copy effect targets a permanent that already
    // retained a trigger via a prior copy, the second copy must see the
    // retained trigger as one of the source's copiable triggers — i.e.
    // `compute_current_copiable_values` must honour
    // `RetainPrintedTriggerFromSource`, not silently drop it.
    //
    // Concrete scenario: Irma becomes a copy of a Bear (Irma's first copy
    // retains her own trigger). Then Phantasmal Image enters as a copy of
    // Irma — it must inherit the retained trigger as part of Irma's
    // copiable values. If `compute_current_copiable_values` short-circuits
    // on the unknown variant, the second copy's `trigger_definitions` will
    // be Bear's (empty) and the cycle breaks.
    #[test]
    fn retained_trigger_propagates_through_chained_copy() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ContinuousModification, TriggerDefinition,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // Bear: vanilla 2/2 with no triggers.
        let bear = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);

        // Irma: 1/1 with a printed BoC phase trigger. Mirror the printed
        // setup the database uses (base_trigger_definitions + trigger_definitions
        // both populated).
        let irma = create_creature(&mut state, 2, PlayerId(0), "Irma", 1, 1);
        let printed_trigger = TriggerDefinition::new(TriggerMode::Phase)
            .phase(crate::types::phase::Phase::BeginCombat)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
        state
            .objects
            .get_mut(&irma)
            .unwrap()
            .base_trigger_definitions = Arc::new(vec![printed_trigger.clone()]);
        state.objects.get_mut(&irma).unwrap().trigger_definitions =
            crate::types::definitions::Definitions::from(vec![printed_trigger.clone()]);

        // Step 1: Irma becomes a copy of Bear (with the retain modification —
        // exactly what the parser emits for "and she has this ability").
        let irma_to_bear = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![
                    ContinuousModification::SetName {
                        name: "Irma".to_string(),
                    },
                    ContinuousModification::RetainPrintedTriggerFromSource {
                        source_trigger_index: 0,
                    },
                ],
            },
            vec![TargetRef::Object(bear)],
            irma,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &irma_to_bear, &mut events).unwrap();
        evaluate_layers(&mut state);
        // Sanity: Irma now has Bear's stats but retains her own trigger.
        assert_eq!(state.objects[&irma].name, "Irma");
        assert_eq!(
            state.objects[&irma].trigger_definitions.iter_all().count(),
            1,
            "step 1: Irma must keep her retained trigger"
        );

        // Step 2: Phantasmal Image (a vanilla 0/0 with no abilities of its
        // own) becomes a copy of Irma — uses the COPIABLE values of Irma,
        // which per CR 707.9a must include the retained trigger.
        let phantasmal = create_creature(&mut state, 3, PlayerId(0), "Phantasmal Image", 0, 0);
        let phantasmal_to_irma = make_copy_ability(irma, phantasmal, PlayerId(0), None);
        resolve(&mut state, &phantasmal_to_irma, &mut events).unwrap();
        evaluate_layers(&mut state);

        // The chained copy must propagate the retained trigger.
        let phantasmal_obj = state.objects.get(&phantasmal).unwrap();
        assert_eq!(
            phantasmal_obj.name, "Irma",
            "chained copy reads the SetName-overridden copiable name"
        );
        assert_eq!(
            phantasmal_obj.trigger_definitions.iter_all().count(),
            1,
            "CR 707.9a: chained copy must inherit the retained trigger as a \
             copiable value (compute_current_copiable_values must honour \
             RetainPrintedTriggerFromSource); got {:?}",
            phantasmal_obj
                .trigger_definitions
                .iter_all()
                .collect::<Vec<_>>()
        );
        assert!(
            phantasmal_obj
                .trigger_definitions
                .iter_all()
                .any(|t| matches!(t.mode, TriggerMode::Phase)),
            "the propagated trigger must be the printed BoC phase trigger"
        );
    }

    // CR 707.9a: When the source's printed trigger list has no entry at the
    // requested index (defensive — should not happen for well-formed parses),
    // retain is a no-op rather than a panic. This guards against parser
    // regressions where the index drift past the printed list.
    #[test]
    fn retain_printed_trigger_with_out_of_bounds_index_is_a_noop() {
        use crate::types::ability::ContinuousModification;

        let mut state = GameState::new_two_player(42);
        let target = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        let source = create_creature(&mut state, 2, PlayerId(0), "Source", 1, 1);
        // Source has zero printed triggers — index 0 is out of bounds.
        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![
                    ContinuousModification::RetainPrintedTriggerFromSource {
                        source_trigger_index: 5,
                    },
                ],
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        // No panic, no extra triggers.
        assert_eq!(
            state.objects[&source]
                .trigger_definitions
                .iter_all()
                .count(),
            0,
            "out-of-bounds retain must be a no-op (no panic, no spurious trigger)"
        );
    }

    // ── Reset regression: abilities revert when copy ends ─────────────────
    #[test]
    fn abilities_revert_to_empty_when_copy_expires() {
        use crate::types::ability::{AbilityDefinition, AbilityKind};

        let mut state = GameState::new_two_player(42);
        let target = create_creature(&mut state, 1, PlayerId(0), "Flyer", 2, 2);
        state.objects.get_mut(&target).unwrap().base_abilities =
            Arc::new(vec![AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )]);

        // Source has no abilities
        let source = create_creature(&mut state, 2, PlayerId(0), "Vanilla", 1, 1);

        let mut events = Vec::new();
        let ability =
            make_copy_ability(target, source, PlayerId(0), Some(Duration::UntilEndOfTurn));
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&source].abilities.len(), 1, "copied ability");

        execute_cleanup(&mut state, &mut events);
        evaluate_layers(&mut state);
        assert!(
            state.objects[&source].abilities.is_empty(),
            "abilities must revert to empty base after copy expires"
        );
    }
}
