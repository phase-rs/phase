use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::layers::compute_current_copiable_values;
use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterAddition, PendingEffectResolved};

/// CR 707.2 / CR 613.1a: Become a copy of target permanent via a layer-1 copy effect.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (recipient, duration, additional_modifications) = match &ability.effect {
        Effect::BecomeCopy {
            recipient,
            duration,
            additional_modifications,
            ..
        } => (
            recipient.clone(),
            duration
                .clone()
                .or(ability.duration.clone())
                .unwrap_or(Duration::Permanent),
            additional_modifications.clone(),
        ),
        _ => (
            TargetFilter::SelfRef,
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

    // Display identity follows the copy: carry the source's image routing so the
    // copying object renders the copied source's art. Not a CR 707.2 copiable
    // value (kept off `CopiableValues`); rides on the modification so it reverts
    // with the effect. The source is guaranteed present — the copiable-values
    // lookup above returned `Some` for `target_id`.
    //
    // CR 111.1 + CR 707.2: when the source is a true token, `printed_ref` is
    // `None` and the token's art lives only in the token database — so capture
    // `display_source` + `token_image_ref` too, otherwise a copy-of-token (e.g.
    // Mockingbird copying a Rabbit token) is stranded on the real-card name path
    // for a name that has no real-card printing and renders blank.
    let (source_display_source, source_printed_ref, source_token_image_ref) = state
        .objects
        .get(&target_id)
        .map(|o| {
            (
                o.display_source,
                o.printed_ref.clone(),
                o.token_image_ref.clone(),
            )
        })
        .unwrap_or_default();

    // CR 202.1b + CR 707.9: "except it has no mana cost" is a copy-value
    // exception consumed at resolution — strip the copied mana cost from the
    // values themselves so the continuous copy carries mana value 0 on every
    // layer pass (BecomeCopy re-applies `CopyValues` each pass; a one-shot bake
    // would be overwritten). Mirrors token_copy.rs, which bakes the strip into
    // the freshly created token's base mana cost.
    let mut values = values;
    if additional_modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::RemoveManaCost))
    {
        values.mana_cost = crate::types::mana::ManaCost::NoCost;
    }
    if let Some(loyalty) =
        super::token_copy::copy_starting_loyalty_override(&additional_modifications)
    {
        values.loyalty = Some(loyalty);
    }

    // CR 122.1 + CR 614.1c + CR 202.1b + CR 707.9b: `AddCounterOnEnter`
    // (counter placement), `RemoveManaCost`, and `SetStartingLoyalty` are
    // resolution-time exceptions, not layered modifications — partition them
    // out so the layer pipeline only sees layered variants. Counter-on-enter is
    // applied via the counter primitive after layer evaluation; the mana-cost
    // and starting-loyalty exceptions were already consumed into `values`.
    let (resolution_mods, layered_mods): (Vec<_>, Vec<_>) =
        additional_modifications.into_iter().partition(|m| {
            matches!(
                m,
                ContinuousModification::AddCounterOnEnter { .. }
                    | ContinuousModification::RemoveManaCost
                    | ContinuousModification::SetStartingLoyalty { .. }
            )
        });

    let mut modifications = vec![ContinuousModification::CopyValues {
        values: Box::new(values),
        display_source: source_display_source,
        printed_ref: source_printed_ref,
        token_image_ref: source_token_image_ref,
    }];
    modifications.extend(layered_mods);

    // CR 611.2c: resolve the locked recipient set at resolution time. `SelfRef`
    // (every existing single-subject card) is the source `~`; a typed group
    // filter ("Shards you control", Niko) or `ParentTarget` selects a mass set,
    // snapshotted to concrete ids now — later-entering members never join.
    match &recipient {
        // Existing single-subject cards — EXACT prior behavior, byte-identical.
        TargetFilter::SelfRef => {
            let tce_id = state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration,
                TargetFilter::SpecificObject {
                    id: ability.source_id,
                },
                modifications,
                None,
            );
            // CR 611.2b + CR 110.5d: the copy modification applies to the SOURCE
            // (`affected = SpecificObject { source_id }`), but a "for as long as
            // that creature remains tapped" duration (Zygon Infiltrator) tracks
            // the copy *target*'s tap state — a third object distinct from both
            // source and affected. Bind the donor as the duration subject so the
            // target-relative `IsTapped { scope: Target }` condition resolves
            // against the copy target.
            state.set_transient_duration_subject(tce_id, target_id);
        }
        // CR 611.2c: mass recipient set. `ParentTarget` reads the inherited
        // object target(s); a typed group filter resolves against the
        // battlefield at resolution (Niko: "Shards you control").
        _ => {
            let recipient_ids: Vec<crate::types::identifiers::ObjectId> = match &recipient {
                TargetFilter::ParentTarget => ability
                    .targets
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Object(id) => Some(*id),
                        TargetRef::Player(_) => None,
                    })
                    .collect(),
                _ => {
                    let ctx = FilterContext::from_ability(ability);
                    state
                        .battlefield
                        .iter()
                        .copied()
                        .filter(|id| matches_target_filter(state, *id, &recipient, &ctx))
                        .collect()
                }
            };
            for id in recipient_ids {
                let tce_id = state.add_transient_continuous_effect(
                    ability.source_id,
                    ability.controller,
                    duration.clone(),
                    TargetFilter::SpecificObject { id },
                    modifications.clone(),
                    None,
                );
                state.set_transient_duration_subject(tce_id, id);
            }
            // ponytail: `resolution_mods` (enter-as-copy counter / mana-cost
            // exceptions, applied below on `source_id`) apply only on the SelfRef
            // path — no mass become-copy card class carries those exceptions
            // (Niko has none), so they stay a no-op here.
        }
    }

    // CR 707.9f: "Some exceptions to the copying process apply only if the
    // copy is or has certain characteristics" — flush the layer re-evaluation
    // queued by `add_transient_continuous_effect` so the copied card_types is
    // realized. This is required for keyword grants (e.g., "except it has
    // myriad") to synthesize their associated triggers. Counters are then
    // placed via the shared replacement-aware primitive (Doubling Season etc.
    // apply normally).
    crate::game::layers::flush_layers(state);

    if !resolution_mods.is_empty() {
        let mut additions = Vec::new();
        for modification in resolution_mods {
            // RemoveManaCost was already consumed into `values`; only the
            // counter-placement exceptions remain to apply here.
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
                additions.push(PendingCounterAddition::Object {
                    actor: ability.controller,
                    object_id: ability.source_id,
                    counter_type,
                    count: n,
                });
            }
        }
        for (index, addition) in additions.iter().cloned().enumerate() {
            let PendingCounterAddition::Object {
                actor,
                object_id,
                counter_type,
                count,
            } = addition
            else {
                continue;
            };
            if !super::counters::add_counter_with_replacement(
                state,
                actor,
                object_id,
                counter_type,
                count,
                events,
            ) {
                super::counters::stash_pending_counter_additions(
                    state,
                    additions[index + 1..].to_vec(),
                    PendingEffectResolved::new(
                        EffectKind::from(&ability.effect),
                        ability.source_id,
                    ),
                );
                return Ok(());
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::database::synthesis::KeywordTriggerInstaller;
    use crate::game::layers::{compute_current_copiable_values, evaluate_layers};
    use crate::game::printed_cards::intrinsic_copiable_values;
    use crate::game::turns::execute_cleanup;
    use crate::game::zones::{create_object, move_to_zone};
    use crate::types::ability::{Effect, StaticCondition, TargetFilter, TargetRef};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::counter::CounterType;
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
                recipient: TargetFilter::SelfRef,
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
                recipient: TargetFilter::SelfRef,
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
                recipient: TargetFilter::SelfRef,
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
    fn become_copy_propagates_source_printed_ref_and_reverts_at_cleanup() {
        // Display identity follows the copy: a creature that becomes a copy of
        // another renders the copied card's art (its `printed_ref`), and on a
        // temporary copy that art reverts to its own when the effect expires —
        // the same lifecycle as name/P-T. Drives the real pipeline (resolve →
        // evaluate_layers → cleanup → evaluate_layers), asserting the revert.
        let mut state = GameState::new_two_player(42);

        let copied_ref = crate::types::card::PrintedCardRef {
            oracle_id: "copied-oracle-id".to_string(),
            face_name: "Target Bear".to_string(),
        };
        let own_ref = crate::types::card::PrintedCardRef {
            oracle_id: "own-oracle-id".to_string(),
            face_name: "Copy Source".to_string(),
        };

        let target_id = create_creature(&mut state, 1, PlayerId(0), "Target Bear", 2, 2);
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.printed_ref = Some(copied_ref.clone());
            target.base_printed_ref = Some(copied_ref.clone());
        }
        let source_id = create_creature(&mut state, 2, PlayerId(0), "Copy Source", 1, 1);
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.printed_ref = Some(own_ref.clone());
            source.base_printed_ref = Some(own_ref.clone());
        }

        let mut events = Vec::new();
        let ability = make_copy_ability(
            target_id,
            source_id,
            PlayerId(0),
            Some(Duration::UntilEndOfTurn),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&source_id].printed_ref,
            Some(copied_ref),
            "while the copy is active, the copying object renders the copied card's art"
        );

        execute_cleanup(&mut state, &mut events);
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&source_id].printed_ref,
            Some(own_ref),
            "when the temporary copy expires, the art reverts to the object's own"
        );
    }

    #[test]
    fn become_copy_of_token_carries_token_display_routing_and_reverts() {
        // CR 111.1 + CR 707.2: a nontoken that becomes a copy of a *token* (e.g.
        // Mockingbird copying a Rabbit token) stays a nontoken but takes the
        // token's name — which only resolves in the token art database. The copy
        // must therefore carry the source token's `display_source = Token` +
        // `token_image_ref` (not `printed_ref`), or it renders blank on the
        // real-card name path. On a temporary copy that routing reverts to the
        // copier's own when the effect expires. Drives the real pipeline
        // (resolve → evaluate_layers → cleanup → evaluate_layers). Token-source
        // analog of `become_copy_propagates_source_printed_ref_and_reverts_at_cleanup`.
        let mut state = GameState::new_two_player(42);

        let token_ref = crate::types::card::TokenImageRef {
            scryfall_id: "rabbit-scryfall-id".to_string(),
            scryfall_oracle_id: Some("rabbit-oracle-id".to_string()),
            face_name: None,
            preset_id: "rabbit-preset".to_string(),
        };
        let own_ref = crate::types::card::PrintedCardRef {
            oracle_id: "mockingbird-oracle-id".to_string(),
            face_name: "Mockingbird".to_string(),
        };

        // Copy source: a true 1/1 Rabbit token (no printed identity).
        let token_id = create_creature(&mut state, 1, PlayerId(0), "Rabbit", 1, 1);
        {
            let token = state.objects.get_mut(&token_id).unwrap();
            token.is_token = true;
            token.display_source = crate::game::game_object::DisplaySource::Token;
            token.token_image_ref = Some(token_ref.clone());
        }
        // Copier: a real nontoken card with its own printed identity.
        let copier_id = create_creature(&mut state, 2, PlayerId(0), "Mockingbird", 1, 1);
        {
            let copier = state.objects.get_mut(&copier_id).unwrap();
            copier.printed_ref = Some(own_ref.clone());
            copier.base_printed_ref = Some(own_ref.clone());
        }

        let mut events = Vec::new();
        let ability = make_copy_ability(
            token_id,
            copier_id,
            PlayerId(0),
            Some(Duration::UntilEndOfTurn),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        let copy = &state.objects[&copier_id];
        assert_eq!(
            copy.display_source,
            crate::game::game_object::DisplaySource::Token,
            "a copy of a token must route art through the token database"
        );
        assert_eq!(
            copy.token_image_ref,
            Some(token_ref),
            "the copy must carry the source token's exact art pointer"
        );
        assert_eq!(
            copy.printed_ref, None,
            "a token source has no printed identity to carry"
        );
        assert_eq!(copy.name, "Rabbit");
        assert!(
            !copy.is_token,
            "CR 111.1: copying a token does not make the copy a token"
        );

        // Temporary copy expires: display routing reverts to the copier's own.
        execute_cleanup(&mut state, &mut events);
        evaluate_layers(&mut state);
        let reverted = &state.objects[&copier_id];
        assert_eq!(
            reverted.display_source,
            crate::game::game_object::DisplaySource::Card,
            "when the copy expires, a nontoken reverts to card-database routing"
        );
        assert_eq!(
            reverted.token_image_ref, None,
            "the stale token-art pointer is cleared on revert"
        );
        assert_eq!(
            reverted.printed_ref,
            Some(own_ref),
            "the copier's own printed identity is restored"
        );
        assert_eq!(reverted.name, "Mockingbird");
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
                recipient: TargetFilter::SelfRef,
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

        move_to_zone(&mut state, source_id, Zone::Graveyard, &mut events);
        assert_eq!(
            state.objects[&source_id].name, "Copy Source",
            "copy identity must not persist in graveyard after leaving the battlefield"
        );

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
                recipient: TargetFilter::SelfRef,
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

    /// CR 202.1b + CR 707.9: a BecomeCopy "except it has no mana cost" exception
    /// strips the copied mana cost from the copy's copiable values. Because
    /// BecomeCopy re-applies `CopyValues` on every layer pass, the strip must
    /// survive re-evaluation — a one-shot bake would be overwritten with the
    /// copied {4}{R}{R}.
    #[test]
    fn become_copy_strips_mana_cost_and_survives_layer_reeval() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pricey Source".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_name = "Pricey Source".to_string();
            target.base_power = Some(3);
            target.base_toughness = Some(3);
            target.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            target.base_mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red, ManaCostShard::Red],
                generic: 4,
            };
        }
        let clone_id = create_creature(&mut state, 2, PlayerId(0), "Clone", 0, 0);

        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                recipient: TargetFilter::SelfRef,
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![ContinuousModification::RemoveManaCost],
            },
            vec![TargetRef::Object(target_id)],
            clone_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        // Gains the source's characteristics but with no mana cost (mana value 0).
        assert_eq!(state.objects[&clone_id].name, "Pricey Source");
        assert_eq!(state.objects[&clone_id].mana_cost, ManaCost::NoCost);

        // The strip rides on the copied values, not a one-shot bake, so a second
        // layer pass must NOT restore the copied {4}{R}{R}.
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&clone_id].mana_cost, ManaCost::NoCost);
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
                owner: TargetFilter::Controller,
                source_filter: None,
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
                recipient: TargetFilter::SelfRef,
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
                recipient: TargetFilter::SelfRef,
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
                recipient: TargetFilter::SelfRef,
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
                .any(|t| matches!(t.definition.mode, TriggerMode::Phase)),
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
                recipient: TargetFilter::SelfRef,
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
                .any(|t| matches!(t.definition.mode, TriggerMode::Phase)),
            "the propagated trigger must be the printed BoC phase trigger"
        );
    }

    #[test]
    fn granted_trigger_propagates_through_chained_copy() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ContinuousModification, TriggerDefinition,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let bear = create_creature(&mut state, 1, PlayerId(0), "Bear", 2, 2);
        let assassin = create_creature(&mut state, 2, PlayerId(0), "Callidus Assassin", 3, 3);
        let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Destroy {
                    target: TargetFilter::Typed(
                        crate::types::ability::TypedFilter::creature().properties(vec![
                            crate::types::ability::FilterProp::Another,
                            crate::types::ability::FilterProp::SameName,
                        ]),
                    ),
                    cant_regenerate: false,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield);

        let assassin_to_bear = ResolvedAbility::new(
            Effect::BecomeCopy {
                recipient: TargetFilter::SelfRef,
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![ContinuousModification::GrantTrigger {
                    trigger: Box::new(trigger.clone()),
                }],
            },
            vec![TargetRef::Object(bear)],
            assassin,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &assassin_to_bear, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert!(
            state.objects[&assassin]
                .trigger_definitions
                .iter_all()
                .any(|t| t.definition == trigger),
            "the first copy must receive the granted trigger"
        );

        let image = create_creature(&mut state, 3, PlayerId(0), "Phantasmal Image", 0, 0);
        let image_to_assassin = make_copy_ability(assassin, image, PlayerId(0), None);
        resolve(&mut state, &image_to_assassin, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert!(
            state.objects[&image]
                .trigger_definitions
                .iter_all()
                .any(|t| t.definition == trigger),
            "CR 707.9a: copy-effect granted triggers are copiable values"
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
                recipient: TargetFilter::SelfRef,
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

    // CR 707.9a: Activated-ability "except it has this ability" (Thespian's
    // Stage class) retains the source's printed activated ability on the copy.
    #[test]
    fn become_copy_retains_printed_ability_from_source() {
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification,
        };

        let mut state = GameState::new_two_player(42);
        let target = create_creature(&mut state, 1, PlayerId(0), "Urza's Saga", 2, 2);
        state.objects.get_mut(&target).unwrap().base_keywords = vec![Keyword::Trample];

        let source = create_creature(&mut state, 2, PlayerId(0), "Thespian's Stage", 0, 0);
        let copy_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::BecomeCopy {
                recipient: TargetFilter::SelfRef,
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![],
            },
        )
        .cost(AbilityCost::Tap);
        state.objects.get_mut(&source).unwrap().base_abilities =
            Arc::new(vec![copy_ability.clone()]);

        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                recipient: TargetFilter::SelfRef,
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![
                    ContinuousModification::RetainPrintedAbilityFromSource {
                        source_ability_index: 0,
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
        assert!(
            copied.abilities.iter().any(|a| a == &copy_ability),
            "retained activated copy ability must survive Layer 1; got {:?}",
            copied.abilities
        );
    }

    /// CR 707.9a (issue #6009): Sakashima of a Thousand Faces — "except it has
    /// ~'s other abilities" retains the SOURCE's entire other ability surface
    /// (static abilities + keywords here) on the copy, not just a single
    /// indexed ability like `RetainPrintedAbilityFromSource`.
    #[test]
    fn become_copy_retains_all_other_abilities_from_source() {
        use crate::game::static_abilities::{check_static_ability, StaticCheckContext};
        use crate::types::ability::{
            ContinuousModification, ControllerRef, StaticDefinition, TypedFilter,
        };
        use crate::types::keywords::PartnerType;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let target = create_creature(&mut state, 1, PlayerId(0), "Target Bear", 2, 2);

        let source = create_creature(&mut state, 2, PlayerId(0), "Sakashima", 3, 1);
        {
            let src = state.objects.get_mut(&source).unwrap();
            src.base_keywords = vec![Keyword::Partner(PartnerType::Generic)];
            src.base_static_definitions = Arc::new(vec![StaticDefinition::new(
                StaticMode::LegendRuleDoesntApply,
            )
            .affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))]);
        }

        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                recipient: TargetFilter::SelfRef,
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![
                    ContinuousModification::RetainAllOtherAbilitiesFromSource,
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
        // The copy target's characteristics (name, P/T) are copied normally.
        assert_eq!(copied.name, "Target Bear");
        assert_eq!(copied.power, Some(2));
        assert_eq!(copied.toughness, Some(2));
        // Sakashima's own other abilities survive the copy.
        assert!(
            copied
                .keywords
                .contains(&Keyword::Partner(PartnerType::Generic)),
            "Partner keyword must be retained; got {:?}",
            copied.keywords
        );
        assert!(
            check_static_ability(
                &state,
                StaticMode::LegendRuleDoesntApply,
                &StaticCheckContext {
                    target_id: Some(source),
                    ..Default::default()
                },
            ),
            "LegendRuleDoesntApply static must be retained on the copy"
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

    // ── Issue #1558: Keyword grants via except clause synthesize triggers ─────
    #[test]
    fn become_copy_with_except_it_has_myriad_synthesizes_trigger() {
        use crate::types::ability::ContinuousModification;
        use crate::types::keywords::Keyword;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let target = create_creature(&mut state, 1, PlayerId(0), "Target", 2, 2);
        let source = create_creature(&mut state, 2, PlayerId(0), "Muddle", 1, 1);

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                recipient: TargetFilter::SelfRef,
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Myriad,
                }],
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();
        // evaluate_layers is now called unconditionally in resolve()

        let source_obj = state.objects.get(&source).unwrap();
        assert!(
            source_obj.keywords.contains(&Keyword::Myriad),
            "Myriad keyword should be granted via except clause"
        );
        let has_myriad_trigger = source_obj.trigger_definitions.iter_all().any(|trigger| {
            matches!(trigger.definition.mode, TriggerMode::Attacks)
                && matches!(trigger.definition.valid_card, Some(TargetFilter::SelfRef))
                && trigger
                    .definition
                    .execute
                    .as_deref()
                    .is_some_and(|ability| {
                        ability.optional && matches!(ability.effect.as_ref(), Effect::Myriad)
                    })
        });
        assert!(
            has_myriad_trigger,
            "Myriad attack trigger should be synthesized when keyword is granted"
        );
    }

    // ── Issue #1514: Dark Depths copied by Thespian's Stage → Marit Lage ──────
    //
    // CR 707.2: A copy does not copy counters, so Thespian's Stage becoming a
    // copy of Dark Depths produces a Dark Depths with ZERO ice counters. CR
    // 603.8: the copied state-triggered ability ("When Dark Depths has no ice
    // counters on it, sacrifice it. If you do, create Marit Lage") is re-checked
    // when a player would receive priority; with no ice counters it fires
    // immediately. Resolving the trigger sacrifices the copy and creates the
    // 20/20 flying indestructible legendary Avatar token.
    //
    // This drives the real runtime path: BecomeCopy resolution → layer
    // evaluation → check_state_triggers (via the priority window) → stack
    // resolution of the Sacrifice + "if you do, create" chain.
    #[test]
    fn copying_dark_depths_with_zero_counters_creates_marit_lage() {
        use crate::game::scenario::GameRunner;
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::card_type::{CoreType, Supertype};
        use crate::types::counter::CounterType;
        use crate::types::game_state::WaitingFor;
        use crate::types::keywords::Keyword;
        use crate::types::triggers::TriggerMode;

        const DARK_DEPTHS_TEXT: &str = "Dark Depths enters the battlefield with ten ice counters on it.\n{3}: Remove an ice counter from Dark Depths.\nWhen Dark Depths has no ice counters on it, sacrifice it. If you do, create Marit Lage, a legendary 20/20 black Avatar creature token with flying and indestructible.";

        let parsed = parse_oracle_text(
            DARK_DEPTHS_TEXT,
            "Dark Depths",
            &[],
            &["Land".to_string()],
            &[],
        );
        // Sanity: the parser yields the state-triggered "has no ice counters"
        // ability that the copy must inherit (CR 603.8).
        assert!(
            parsed
                .triggers
                .iter()
                .any(|t| t.mode == TriggerMode::StateCondition),
            "Dark Depths must parse a StateCondition trigger; got {:?}",
            parsed.triggers
        );

        let mut state = GameState::new_two_player(42);
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Helper to install Dark Depths' parsed printed abilities onto a land.
        let install_dark_depths =
            |state: &mut GameState, id: crate::types::identifiers::ObjectId| {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.base_card_types = CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Land],
                    subtypes: vec![],
                };
                obj.card_types = obj.base_card_types.clone();
                obj.base_abilities = Arc::new(parsed.abilities.clone());
                obj.base_trigger_definitions = Arc::new(parsed.triggers.clone());
                obj.base_replacement_definitions = Arc::new(parsed.replacements.clone());
            };

        // Target: a real Dark Depths on the battlefield with its 10 ice counters.
        let dark_depths = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dark Depths".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&dark_depths).unwrap().base_name = "Dark Depths".to_string();
        install_dark_depths(&mut state, dark_depths);
        state
            .objects
            .get_mut(&dark_depths)
            .unwrap()
            .counters
            .insert(CounterType::Generic("ice".to_string()), 10);

        // Source: Thespian's Stage (a Land). Its "{2}, {T}: becomes a copy of
        // target land, except it has this ability" produces a BecomeCopy effect.
        let stage = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Thespian's Stage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&stage).unwrap();
            obj.base_name = "Thespian's Stage".to_string();
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Land],
                subtypes: vec![],
            };
            obj.card_types = obj.base_card_types.clone();
        }

        evaluate_layers(&mut state);

        // Thespian's Stage becomes a copy of Dark Depths (CR 707.2). The copy
        // inherits Dark Depths' abilities but NOT its ice counters.
        let mut events = Vec::new();
        let ability = make_copy_ability(dark_depths, stage, PlayerId(0), None);
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        // The copy is a Dark Depths with zero ice counters and the state trigger.
        let stage_obj = state.objects.get(&stage).unwrap();
        assert_eq!(stage_obj.name, "Dark Depths", "copy is named Dark Depths");
        assert_eq!(
            stage_obj
                .counters
                .get(&CounterType::Generic("ice".to_string()))
                .copied()
                .unwrap_or(0),
            0,
            "CR 707.2: a copy does not copy counters — the Stage copy has no ice counters"
        );
        assert!(
            stage_obj
                .trigger_definitions
                .iter_all()
                .any(|t| t.definition.mode == TriggerMode::StateCondition),
            "the copy must inherit Dark Depths' state-triggered ability (CR 603.8)"
        );

        // Drive the runtime: granting priority runs `check_state_triggers`,
        // which sees zero ice counters and puts the sacrifice trigger on the
        // stack; resolving it sacrifices the copy and creates Marit Lage.
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        // CR 603.8 + CR 117.1: state triggers are checked whenever a player would
        // receive priority. Run the engine's state-trigger check (the same call
        // the priority pipeline makes), which puts the sacrifice trigger on the
        // stack, then drain the stack to resolve the Sacrifice + "if you do,
        // create Marit Lage" chain.
        crate::game::triggers::check_state_triggers(&mut state);
        let mut runner = GameRunner::from_state(state);
        runner.advance_until_stack_empty();
        let state = runner.state();

        // The copy must have been sacrificed (CR 603.8 → CR 701.21 Sacrifice).
        assert!(
            !state.battlefield.contains(&stage),
            "the Dark Depths copy must be sacrificed when it has no ice counters"
        );

        // A Marit Lage token (legendary 20/20 black Avatar, flying + indestructible)
        // must have been created (CR 111.1 / 707.2 combo payoff).
        let marit_lage = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .find(|obj| obj.is_token && obj.name == "Marit Lage")
            .expect("Marit Lage token must be created");
        assert_eq!(marit_lage.power, Some(20), "Marit Lage is 20 power");
        assert_eq!(marit_lage.toughness, Some(20), "Marit Lage is 20 toughness");
        assert!(
            marit_lage
                .card_types
                .supertypes
                .contains(&Supertype::Legendary),
            "Marit Lage is legendary"
        );
        assert!(
            marit_lage.card_types.subtypes.iter().any(|s| s == "Avatar"),
            "Marit Lage is an Avatar"
        );
        assert!(
            marit_lage.keywords.contains(&Keyword::Flying),
            "Marit Lage has flying"
        );
        assert!(
            marit_lage.keywords.contains(&Keyword::Indestructible),
            "Marit Lage has indestructible"
        );
    }

    // ---- Zygon Infiltrator: target-relative `ForAsLongAs { IsTapped }` ----

    /// CR 611.2b + CR 110.5d: a `BecomeCopy` with a "for as long as that creature
    /// remains tapped" duration tracks the COPY TARGET's tap state. The copy
    /// holds while the target is tapped and lapses when the target untaps — even
    /// though the copy modification applies to the SOURCE and the source's own
    /// tap state never changes. This is the exact Zygon Infiltrator bug.
    #[test]
    fn become_copy_for_as_long_as_target_tapped_tracks_target() {
        use crate::types::ability::ObjectScope;

        let mut state = GameState::new_two_player(7);
        // Target: a tapped 3/3 the source will copy.
        let target_id = create_creature(&mut state, 1, PlayerId(0), "Tapped Bear", 3, 3);
        state.objects.get_mut(&target_id).unwrap().tapped = true;
        // Source: a 1/1 that becomes the copy. It is UNTAPPED and stays untapped
        // (Zygon's activation cost has no {T}).
        let source_id = create_creature(&mut state, 2, PlayerId(0), "Zygon", 1, 1);
        state.objects.get_mut(&source_id).unwrap().tapped = false;

        let duration = Some(Duration::ForAsLongAs {
            condition: StaticCondition::IsTapped {
                scope: ObjectScope::Target,
            },
        });
        let ability = make_copy_ability(target_id, source_id, PlayerId(0), duration);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        // While the target is tapped, the copy applies even though the source is
        // untapped — the duration tracks the target, not the source.
        let source = state.objects.get(&source_id).unwrap();
        assert_eq!(
            source.power,
            Some(3),
            "copy must apply while the TARGET is tapped (source is untapped)"
        );
        assert_eq!(source.name, "Tapped Bear");

        // Untap the target → the `ForAsLongAs { IsTapped { Target } }` duration
        // ends and the copy lapses.
        state.objects.get_mut(&target_id).unwrap().tapped = false;
        evaluate_layers(&mut state);
        let source = state.objects.get(&source_id).unwrap();
        assert_eq!(
            source.power,
            Some(1),
            "copy must lapse once the target untaps (CR 611.2b)"
        );
        assert_eq!(source.name, "Zygon");
    }

    /// Divergence guard: with `affected = SpecificObject { source }` but the
    /// duration subject bound to the TARGET, toggling the SOURCE's tap state must
    /// have NO effect on the duration — only the target's tap state matters. This
    /// pins the exact bug (source-binding) so it cannot silently return.
    #[test]
    fn become_copy_duration_ignores_source_tap_state() {
        use crate::types::ability::ObjectScope;

        let mut state = GameState::new_two_player(11);
        let target_id = create_creature(&mut state, 1, PlayerId(0), "Tapped Bear", 3, 3);
        state.objects.get_mut(&target_id).unwrap().tapped = true;
        let source_id = create_creature(&mut state, 2, PlayerId(0), "Zygon", 1, 1);
        // Source starts UNTAPPED — under the old (buggy) SourceIsTapped binding
        // the copy would never apply.
        state.objects.get_mut(&source_id).unwrap().tapped = false;

        let duration = Some(Duration::ForAsLongAs {
            condition: StaticCondition::IsTapped {
                scope: ObjectScope::Target,
            },
        });
        let ability = make_copy_ability(target_id, source_id, PlayerId(0), duration);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&source_id).unwrap().power,
            Some(3),
            "copy applies with untapped source because the TARGET is tapped"
        );

        // Tap the source (target still tapped) → still applies.
        state.objects.get_mut(&source_id).unwrap().tapped = true;
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&source_id).unwrap().power,
            Some(3),
            "tapping the source must not change the duration"
        );

        // Untap the source while the target stays tapped → STILL applies. Source
        // tap state is irrelevant; only the target's matters.
        state.objects.get_mut(&source_id).unwrap().tapped = false;
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&source_id).unwrap().power,
            Some(3),
            "source tap state must not gate a target-relative duration"
        );
    }

    /// `duration_subject` is captured at resolution time and carried on the TCE.
    #[test]
    fn become_copy_captures_duration_subject_on_tce() {
        use crate::types::ability::ObjectScope;

        let mut state = GameState::new_two_player(13);
        let target_id = create_creature(&mut state, 1, PlayerId(0), "Tapped Bear", 3, 3);
        state.objects.get_mut(&target_id).unwrap().tapped = true;
        let source_id = create_creature(&mut state, 2, PlayerId(0), "Zygon", 1, 1);

        let duration = Some(Duration::ForAsLongAs {
            condition: StaticCondition::IsTapped {
                scope: ObjectScope::Target,
            },
        });
        let ability = make_copy_ability(target_id, source_id, PlayerId(0), duration);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let tce = state
            .transient_continuous_effects
            .iter()
            .find(|t| t.source_id == source_id)
            .expect("BecomeCopy must register a TCE");
        assert_eq!(
            tce.duration_subject,
            Some(target_id),
            "the copy TARGET must be captured as the duration subject"
        );
        assert_eq!(
            tce.affected,
            TargetFilter::SpecificObject { id: source_id },
            "the copy modification still applies to the SOURCE"
        );
    }

    #[test]
    fn assimilation_aegis_copy_follows_attached_host_and_ends_on_detach() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::attach::{attach_to, unattach};
        use crate::game::effects::resolve_ability_chain;
        use crate::game::filter::{matches_target_filter, FilterContext};
        use crate::parser::oracle_trigger::parse_trigger_line;

        let mut state = GameState::new_two_player(17);
        let donor = create_creature(&mut state, 1, PlayerId(1), "Exiled Beast", 5, 4);
        let mut events = Vec::new();
        let first_host = create_creature(&mut state, 2, PlayerId(0), "First Host", 1, 1);
        let second_host = create_creature(&mut state, 3, PlayerId(0), "Second Host", 2, 2);
        let equipment = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Assimilation Aegis".to_string(),
            Zone::Battlefield,
        );
        {
            let aegis = state.objects.get_mut(&equipment).unwrap();
            aegis.base_name = "Assimilation Aegis".to_string();
            aegis.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Equipment".to_string()],
            };
            aegis.card_types = aegis.base_card_types.clone();
        }

        let etb = parse_trigger_line(
            "When ~ enters, exile up to one target creature until ~ leaves the battlefield.",
            "Assimilation Aegis",
        );
        let etb_ability = build_resolved_from_def_with_targets(
            etb.execute.as_ref().expect("ETB must execute"),
            equipment,
            PlayerId(0),
            vec![TargetRef::Object(donor)],
        );
        resolve_ability_chain(&mut state, &etb_ability, &mut events, 0).unwrap();
        assert_eq!(state.objects[&donor].zone, Zone::Exile);
        assert!(state
            .exile_links
            .iter()
            .any(|link| link.source_id == equipment && link.exiled_id == donor));

        attach_to(&mut state, equipment, first_host);
        let attached = parse_trigger_line(
            "Whenever ~ becomes attached to a creature, for as long as ~ remains attached to it, that creature becomes a copy of a creature card exiled with ~.",
            "Assimilation Aegis",
        );
        let execute = attached
            .execute
            .as_ref()
            .expect("attached trigger must execute");
        let Effect::BecomeCopy { target, .. } = &*execute.effect else {
            panic!("attached trigger must resolve BecomeCopy");
        };
        let context = FilterContext::from_source(&state, equipment);
        assert!(matches_target_filter(&state, donor, target, &context));
        assert!(!matches_target_filter(&state, first_host, target, &context));
        let ability = build_resolved_from_def_with_targets(
            execute,
            equipment,
            PlayerId(0),
            vec![TargetRef::Object(donor)],
        );

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&first_host].name, "Exiled Beast");
        assert_eq!(state.objects[&first_host].power, Some(5));
        assert_eq!(state.objects[&equipment].name, "Assimilation Aegis");
        assert_eq!(state.objects[&equipment].power, None);

        unattach(&mut state, equipment);
        assert_eq!(state.objects[&first_host].name, "First Host");
        assert_eq!(state.objects[&first_host].power, Some(1));

        attach_to(&mut state, equipment, first_host);
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&first_host].name, "First Host");
        assert_eq!(state.objects[&first_host].power, Some(1));

        attach_to(&mut state, equipment, second_host);
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&first_host].name, "First Host");
        assert_eq!(state.objects[&second_host].name, "Second Host");

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&first_host].name, "First Host");
        assert_eq!(state.objects[&second_host].name, "Exiled Beast");
        assert_eq!(state.objects[&second_host].power, Some(5));
        assert_eq!(state.objects[&equipment].name, "Assimilation Aegis");
    }

    /// CR 707.2: Keyword abilities such as Persist are copiable values. When
    /// the copied object carries the keyword but its printed trigger list was
    /// not populated (or was stripped before the copy snapshot), the copy must
    /// still receive the synthesized dies trigger so Persist/Undying function.
    #[test]
    fn become_copy_installs_keyword_triggers_for_copied_keywords() {
        let mut state = GameState::new_two_player(42);
        let target = create_creature(&mut state, 1, PlayerId(0), "Persist Bear", 2, 2);
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.base_keywords = vec![Keyword::Persist];
            obj.keywords = vec![Keyword::Persist];
        }

        let clone = create_creature(&mut state, 2, PlayerId(0), "Clone", 0, 0);
        let ability = make_copy_ability(target, clone, PlayerId(0), None);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert!(
            state.objects[&clone].keywords.contains(&Keyword::Persist),
            "copy must receive Persist keyword"
        );
        assert!(
            state.objects[&clone]
                .trigger_definitions
                .iter_all()
                .any(|trigger| {
                    KeywordTriggerInstaller::trigger_matches_keyword_kind(
                        &trigger.definition,
                        &Keyword::Persist,
                    )
                }),
            "copy must carry Persist's dies trigger even when the target had no printed trigger entry"
        );

        state.objects.get_mut(&clone).unwrap().damage_marked = 99;
        let mut death_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut death_events);
        crate::game::triggers::process_triggers(&mut state, &death_events);
        while !state.stack.is_empty() {
            crate::game::stack::resolve_top(&mut state, &mut Vec::new());
        }

        assert_eq!(
            state.objects[&clone].zone,
            Zone::Battlefield,
            "Persist copy must return to the battlefield"
        );
        assert!(
            state.objects[&clone]
                .counters
                .get(&CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0)
                >= 1,
            "Persist copy must re-enter with a -1/-1 counter"
        );
    }
}
