use crate::game::layers::compute_current_copiable_values;
use crate::game::quantity::resolve_quantity;
use crate::game::zones;
use crate::types::ability::{
    ContinuousModification, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::zones::Zone;
use std::sync::Arc;

/// CR 707.2 / CR 707.5: Create a token that's a copy of a permanent.
/// Copies copiable characteristics from the target to a newly created token.
///
/// CR 707.10: When `count` resolves to N > 1, N independent copy-tokens are
/// created (e.g., Rite of Replication kicked = 5, Adrix and Nev doubling).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Extract fields from effect
    let (
        target_filter,
        enters_attacking,
        tapped,
        count_expr,
        extra_keywords,
        additional_modifications,
    ) = match &ability.effect {
        Effect::CopyTokenOf {
            target,
            enters_attacking,
            tapped,
            count,
            extra_keywords,
            additional_modifications,
        } => (
            target,
            *enters_attacking,
            *tapped,
            count.clone(),
            extra_keywords.clone(),
            additional_modifications.clone(),
        ),
        _ => return Err(EffectError::MissingParam("CopyTokenOf".to_string())),
    };
    let count = resolve_quantity(state, &count_expr, ability.controller, ability.source_id).max(0);

    // Step 1: Resolve the copy source list.
    // CR 608.2c + 603.10a: LTB self-trigger patterns such as Vaultborn Tyrant
    // ("create a token that's a copy of it") and Ochre Jelly's delayed trigger
    // emit `target: ParentTarget` / `SelfRef` with empty `ability.targets`.
    // In a top-level trigger there is no parent chain, so the anaphor refers to
    // the source object itself. `TriggeringSource` is deliberately excluded:
    // it resolves via `state.current_trigger_event`, not `source_id`.
    //
    // CR 115.1d + CR 601.2c: For "any number of target X" / "for each of them,
    // create a token …" (e.g., Twinflame), `ability.targets` carries N >= 1
    // object refs and the resolver creates one copy per target.
    //
    // Zone-eligibility: unlike `Bounce` / `ChangeZone`, `CopyTokenOf` reads
    // copiable values via `compute_current_copiable_values`, which is
    // zone-agnostic — so a source in the graveyard is fine.
    let use_self = matches!(
        target_filter,
        TargetFilter::None | TargetFilter::SelfRef | TargetFilter::ParentTarget
    ) && ability.targets.is_empty();

    let copy_source_ids: Vec<ObjectId> = if use_self {
        vec![ability.source_id]
    } else {
        let ids: Vec<ObjectId> = ability
            .targets
            .iter()
            .filter_map(|t| match t {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            return Err(EffectError::MissingParam(
                "CopyTokenOf requires a target".to_string(),
            ));
        }
        ids
    };

    // CR 707.10 + CR 115.1d: Create `count` independent copy-tokens per copy
    // source. Each is snapshotted from the source values so that subsequent
    // SBAs (e.g., legendary rule) see identical copies.
    let mut created_ids: Vec<ObjectId> = Vec::with_capacity(count as usize * copy_source_ids.len());
    for copy_source_id in &copy_source_ids {
        let copy_source_id = *copy_source_id;
        let values = compute_current_copiable_values(state, copy_source_id)
            .ok_or(EffectError::ObjectNotFound(copy_source_id))?;
        let name = values.name.clone();
        for _ in 0..count {
            // Step 3: Create a new token object on the battlefield.
            let token_id = zones::create_object(
                state,
                CardId(0),
                ability.controller,
                name.clone(),
                Zone::Battlefield,
            );

            // Step 4: Apply snapshotted characteristics to the token (CR 707.2).
            let token = state.objects.get_mut(&token_id).unwrap();
            token.is_token = true;
            token.name = values.name.clone();
            token.base_name = values.name.clone();
            token.mana_cost = values.mana_cost.clone();
            token.base_mana_cost = values.mana_cost.clone();
            token.base_color = values.color.clone();
            token.color = values.color.clone();
            token.base_card_types = values.card_types.clone();
            token.card_types = values.card_types.clone();
            token.base_power = values.power;
            token.power = values.power;
            token.base_toughness = values.toughness;
            token.toughness = values.toughness;
            token.base_loyalty = values.loyalty;
            token.loyalty = values.loyalty;
            token.base_keywords = values.keywords.clone();
            token.keywords = values.keywords.clone();
            // All four ability sets are Arc-shared — refcount bumps, no deep copy.
            token.base_abilities = Arc::clone(&values.abilities);
            token.abilities = Arc::clone(&values.abilities);
            token.base_trigger_definitions = Arc::clone(&values.trigger_definitions);
            token.trigger_definitions = Arc::clone(&values.trigger_definitions).into();
            token.base_replacement_definitions = Arc::clone(&values.replacement_definitions);
            token.replacement_definitions = Arc::clone(&values.replacement_definitions).into();
            token.base_static_definitions = Arc::clone(&values.static_definitions);
            token.static_definitions = Arc::clone(&values.static_definitions).into();
            token.base_characteristics_initialized = true;
            // CR 400.7 + CR 302.6: Single authority for ETB state. Haste
            // granted below via `extra_keywords` (Twinflame, etc.) is folded
            // in at query time by `has_summoning_sickness`.
            token.reset_for_battlefield_entry(state.turn_number);

            // CR 707.2 + CR 702: "except it has [keyword]" — grant additional
            // keywords on top of the copied characteristics. Twinflame's haste
            // copies are the canonical case. Idempotent under repeats.
            for kw in &extra_keywords {
                if !token.keywords.contains(kw) {
                    token.keywords.push(kw.clone());
                }
                if !token.base_keywords.contains(kw) {
                    token.base_keywords.push(kw.clone());
                }
            }

            // CR 707.9 + CR 707.2: "except <body>" non-keyword modifications.
            // Tokens are synthesized with copiable values baked in (CR 707.2),
            // so each modification is stamped onto BOTH the layered view and
            // the base view rather than queued as a transient continuous
            // effect. `AddCounterOnEnter` is consumed via the counter
            // primitive; supertype add/remove and other type-changing
            // modifications mutate `card_types` in place. Drops the mutable
            // borrow before re-borrowing `state` for counter placement.
            let _ = token;
            apply_token_modifications(state, token_id, &additional_modifications, events);

            // Re-borrow for the remaining tapped/attacking adjustments.
            let token = state.objects.get_mut(&token_id).unwrap();

            // Step 5: If tapped, set tapped state.
            if tapped {
                token.tapped = true;
            }

            // Step 6: If enters_attacking, add to combat attackers.
            // CR 508.4: Uses shared helper for defending player resolution.
            if enters_attacking {
                crate::game::combat::enter_attacking(
                    state,
                    token_id,
                    ability.source_id,
                    ability.controller,
                );
            }

            // Step 6b: Inject predefined abilities, record entry, and mark layers dirty.
            // CR 111.10a-v: Predefined token abilities for known subtypes (Treasure, Food, etc.).
            super::token::inject_predefined_token_abilities(state, token_id);
            state.layers_dirty = true;
            crate::game::restrictions::record_battlefield_entry(state, token_id);
            crate::game::restrictions::record_token_created(state, token_id);

            // Step 7: Emit events.
            // CR 111.1 + CR 603.6a: Token creation is a zone change from outside
            // the game. Emit `ZoneChanged { from: None }` so every ETB trigger
            // matcher fires for copied tokens (Elvish Vanguard, Soul Warden,
            // Panharmonicon) without token-specific matcher code. `TokenCreated`
            // is preserved for token-specific consumers.
            let zone_change_record = state
                .objects
                .get(&token_id)
                .expect("token just created")
                .snapshot_for_zone_change(token_id, None, Zone::Battlefield);
            events.push(GameEvent::ZoneChanged {
                object_id: token_id,
                from: None,
                to: Zone::Battlefield,
                record: Box::new(zone_change_record),
            });
            events.push(GameEvent::TokenCreated {
                object_id: token_id,
                name: name.clone(),
            });
            created_ids.push(token_id);
        }
    }

    // CR 603.7 + CR 701.36a: Record created token IDs so sub-abilities can
    // reference them via `TargetFilter::LastCreated` ("the token created this
    // way", "it") and so "those tokens" plural anaphor in delayed triggers
    // captures the full list. Mirrors `token::apply_create_token`.
    state.last_created_token_ids = created_ids;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 707.2 + CR 707.9: Apply non-keyword `, except <body>` modifications to
/// a synthesized token. Tokens are created with copiable values baked in, so
/// each modification mutates BOTH the layered view (`card_types`,
/// `keywords`, etc.) AND the base view (`base_card_types`, `base_keywords`)
/// directly — there is no "before exception" state to layer over the way a
/// `BecomeCopy` modification layers over an existing object.
///
/// Variants consumed here:
/// - `RemoveSupertype` / `AddSupertype` — Miirym, Sentinel Wyrm; Sarkhan-class.
/// - `AddCounterOnEnter` — Spark Double-class. Counter placed via the shared
///   `counters::add_counter_with_replacement` primitive (which handles
///   replacements such as Doubling Season).
/// - `SetName` — copy-name override (rare for token-copy, harmless if present).
/// - `AddType` / `RemoveType` / `AddSubtype` / `RemoveSubtype` — type
///   exception support for token-copy (compose with type-modifying except
///   bodies that share grammar with `BecomeCopy`).
/// - `AddKeyword` is NOT consumed here — keywords flow through the typed
///   `extra_keywords` channel earlier in the resolver.
///
/// Modifications not relevant to token-copy semantics (e.g. `CopyValues`,
/// `ChangeController`, dynamic P/T) are skipped silently — they have no
/// meaningful "stamp at creation" interpretation. A future card with such
/// an except body will surface as an unimplemented modification, which is
/// strictly better than silently mutating the token incorrectly.
fn apply_token_modifications(
    state: &mut GameState,
    token_id: ObjectId,
    modifications: &[ContinuousModification],
    events: &mut Vec<GameEvent>,
) {
    for modification in modifications {
        match modification {
            // CR 205.4 + CR 707.9b: "the token isn't legendary" (Miirym class).
            ContinuousModification::RemoveSupertype { supertype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.card_types.supertypes.retain(|s| s != supertype);
                    token.base_card_types.supertypes.retain(|s| s != supertype);
                }
            }
            // CR 205.4 + CR 707.9d: "it's <supertype> in addition to its other types".
            ContinuousModification::AddSupertype { supertype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.card_types.supertypes.contains(supertype) {
                        token.card_types.supertypes.push(*supertype);
                    }
                    if !token.base_card_types.supertypes.contains(supertype) {
                        token.base_card_types.supertypes.push(*supertype);
                    }
                }
            }
            // CR 122.1 + CR 614.1c: Counter at creation, optionally gated by
            // the resolved core type. Read core types from the just-stamped
            // `card_types` (already includes any AddType/RemoveType applied
            // earlier in this loop) before placing the counter.
            ContinuousModification::AddCounterOnEnter {
                counter_type,
                count,
                if_type,
            } => {
                let controller = state
                    .objects
                    .get(&token_id)
                    .map(|o| o.controller)
                    .unwrap_or(crate::types::player::PlayerId(0));
                let n = resolve_quantity(state, count, controller, token_id).max(0) as u32;
                if n == 0 {
                    continue;
                }
                let gate_passes = match if_type {
                    None => true,
                    Some(t) => state
                        .objects
                        .get(&token_id)
                        .map(|obj| obj.card_types.core_types.contains(t))
                        .unwrap_or(false),
                };
                if !gate_passes {
                    continue;
                }
                let ct = crate::types::counter::parse_counter_type(counter_type);
                super::counters::add_counter_with_replacement(state, token_id, ct, n, events);
            }
            // CR 707.9b: Name override applied at copy time.
            ContinuousModification::SetName { name } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.name = name.clone();
                    token.base_name = name.clone();
                }
            }
            // CR 205.1a: Type/subtype additions/removals as copy exceptions.
            ContinuousModification::AddType { core_type } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.card_types.core_types.contains(core_type) {
                        token.card_types.core_types.push(*core_type);
                    }
                    if !token.base_card_types.core_types.contains(core_type) {
                        token.base_card_types.core_types.push(*core_type);
                    }
                }
            }
            ContinuousModification::RemoveType { core_type } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.card_types.core_types.retain(|t| t != core_type);
                    token.base_card_types.core_types.retain(|t| t != core_type);
                }
            }
            ContinuousModification::AddSubtype { subtype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.card_types.subtypes.iter().any(|s| s == subtype) {
                        token.card_types.subtypes.push(subtype.clone());
                    }
                    if !token.base_card_types.subtypes.iter().any(|s| s == subtype) {
                        token.base_card_types.subtypes.push(subtype.clone());
                    }
                }
            }
            ContinuousModification::RemoveSubtype { subtype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.card_types.subtypes.retain(|s| s != subtype);
                    token.base_card_types.subtypes.retain(|s| s != subtype);
                }
            }
            // CR 707.2 + CR 702 keyword grants flow through `extra_keywords`,
            // not here. Other layered-only modifications (CopyValues,
            // ChangeController, dynamic P/T, etc.) are intentionally
            // skipped — their "stamp at copy time" interpretation is
            // ambiguous, and a future except body needing them should
            // route through the BecomeCopy layered path instead.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetFilter, TargetRef};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::ObjectId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;

    #[test]
    fn copy_token_of_self_creates_copy() {
        let mut state = GameState::new_two_player(42);

        // Create a creature to copy
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mist-Syndicate Naga".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(3);
            source.base_toughness = Some(1);
            source.power = Some(3);
            source.toughness = Some(1);
            source.base_color = vec![ManaColor::Blue];
            source.color = vec![ManaColor::Blue];
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Snake".to_string(), "Ninja".to_string()],
            };
            source.card_types = source.base_card_types.clone();
            source.base_keywords = vec![Keyword::Ninjutsu(Default::default())];
            source.keywords = source.base_keywords.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // Find the token (it's the newest object)
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        assert_eq!(token.name, "Mist-Syndicate Naga");
        assert_eq!(token.power, Some(3));
        assert_eq!(token.toughness, Some(1));
        assert_eq!(token.color, vec![ManaColor::Blue]);
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(token.card_types.subtypes.contains(&"Snake".to_string()));
        assert!(token.is_token);
        assert!(token.zone == Zone::Battlefield);
        assert!(state.layers_dirty);
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::TokenCreated { name, .. } if name == "Mist-Syndicate Naga")
        ));
        // Verify record_battlefield_entry and record_token_created were called
        assert!(
            state
                .players_who_created_token_this_turn
                .contains(&PlayerId(0)),
            "should record token creation"
        );
    }

    #[test]
    fn copy_token_of_target_creates_copy() {
        let mut state = GameState::new_two_player(42);

        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_power = Some(2);
            target.base_toughness = Some(2);
            target.power = Some(2);
            target.toughness = Some(2);
        }

        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copier".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.name, "Grizzly Bears");
        assert_eq!(token.power, Some(2));
        assert_eq!(token.toughness, Some(2));
        assert!(token.is_token);
    }

    /// CR 603.10a / Vaultborn Tyrant + Ochre Jelly class: LTB self-copy triggers
    /// fire after the source has moved to the graveyard. The parsed effect is
    /// `CopyTokenOf { target: ParentTarget }` with empty `ability.targets`; the
    /// resolver must copy the source object from the graveyard.
    #[test]
    fn copy_token_of_parent_target_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vaultborn Tyrant".to_string(),
            Zone::Graveyard,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(6);
            source.base_toughness = Some(6);
            source.power = Some(6);
            source.toughness = Some(6);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dinosaur".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::ParentTarget,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert_eq!(token.name, "Vaultborn Tyrant");
        assert_eq!(token.power, Some(6));
        assert_eq!(token.toughness, Some(6));
        // Source remains in graveyard (we only copy it, we don't move it).
        assert_eq!(state.objects[&source_id].zone, Zone::Graveyard);
    }

    #[test]
    fn copy_token_enters_tapped_and_attacking() {
        let mut state = GameState::new_two_player(42);

        // Set up combat
        state.combat = Some(crate::game::combat::CombatState::default());

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(2);
            source.power = Some(2);
            source.toughness = Some(2);
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                enters_attacking: true,
                tapped: true,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        // CR 508.4: Token enters tapped and attacking
        assert!(token.tapped);
        let combat = state.combat.as_ref().unwrap();
        assert!(combat.attackers.iter().any(|a| a.object_id == token_id));
    }

    /// CR 707.2 + CR 702.10 (Haste): Twinflame's "except it has haste" — copy
    /// tokens carry the source's keywords plus the granted extra keyword.
    #[test]
    fn copy_token_extra_keywords_grant_haste() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(2);
            s.base_toughness = Some(2);
            s.power = Some(2);
            s.toughness = Some(2);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![Keyword::Haste],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert!(token.keywords.contains(&Keyword::Haste));
        assert!(token.base_keywords.contains(&Keyword::Haste));
    }

    /// CR 115.1d + CR 601.2c: Twinflame's "for each of them" — multi-target
    /// CopyTokenOf creates one copy per object in `ability.targets`, and all
    /// created token IDs are recorded in `state.last_created_token_ids` so the
    /// "those tokens" anaphor in the delayed exile trigger captures the full
    /// set.
    #[test]
    fn copy_token_multi_target_creates_one_per_target() {
        let mut state = GameState::new_two_player(42);
        let bear_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear A".to_string(),
            Zone::Battlefield,
        );
        let bear_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear B".to_string(),
            Zone::Battlefield,
        );
        for id in [bear_a, bear_b] {
            let s = state.objects.get_mut(&id).unwrap();
            s.base_power = Some(2);
            s.base_toughness = Some(2);
            s.power = Some(2);
            s.toughness = Some(2);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }
        let twinflame_src = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Twinflame".to_string(),
            Zone::Stack,
        );
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::ParentTarget,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![Keyword::Haste],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(bear_a), TargetRef::Object(bear_b)],
            twinflame_src,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        // Two new tokens, both with haste.
        assert_eq!(state.last_created_token_ids.len(), 2);
        for token_id in &state.last_created_token_ids {
            let t = state.objects.get(token_id).unwrap();
            assert!(t.is_token);
            assert!(t.keywords.contains(&Keyword::Haste));
        }
        // Names follow each respective source.
        let names: Vec<&str> = state
            .last_created_token_ids
            .iter()
            .map(|id| state.objects[id].name.as_str())
            .collect();
        assert!(names.contains(&"Bear A"));
        assert!(names.contains(&"Bear B"));
    }

    /// CR 205.4 + CR 707.9b + CR 704.5j: Miirym, Sentinel Wyrm class —
    /// `additional_modifications: [RemoveSupertype(Legendary)]` strips the
    /// Legendary supertype from the synthesized token. The legend rule
    /// (CR 704.5j) only collapses legendary permanents, so two such tokens
    /// must coexist on the battlefield without state-based action collapse.
    #[test]
    fn copy_token_remove_supertype_strips_legendary_from_token() {
        use crate::types::ability::ContinuousModification;
        use crate::types::card_type::Supertype;

        let mut state = GameState::new_two_player(42);
        // Source is a legendary creature (e.g., a Dragon).
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bahamut".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(7);
            s.base_toughness = Some(7);
            s.power = Some(7);
            s.toughness = Some(7);
            s.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        // Synthesize Miirym's CopyTokenOf with the RemoveSupertype modification.
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::RemoveSupertype {
                    supertype: Supertype::Legendary,
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        // Layered view: Legendary stripped.
        assert!(
            !token.card_types.supertypes.contains(&Supertype::Legendary),
            "token must not be Legendary; got {:?}",
            token.card_types.supertypes
        );
        // Base view: Legendary stripped from the copiable values too — the
        // exception is part of the copy effect's bake-in (CR 707.2), so future
        // copies-of-this-token also start without Legendary.
        assert!(
            !token
                .base_card_types
                .supertypes
                .contains(&Supertype::Legendary),
            "token's base_card_types must not contain Legendary; got {:?}",
            token.base_card_types.supertypes
        );
    }

    /// CR 122.1 + CR 614.1c: AddCounterOnEnter with matching `if_type` places
    /// the counter on the synthesized token. Spark Double's planeswalker copy
    /// branch is exercised at the BecomeCopy resolver site; this test pins
    /// the same primitive on the token-copy path.
    #[test]
    fn copy_token_add_counter_on_enter_unconditional() {
        use crate::types::ability::{ContinuousModification, QuantityExpr};

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Soldier".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(2);
            s.base_toughness = Some(2);
            s.power = Some(2);
            s.toughness = Some(2);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::AddCounterOnEnter {
                    counter_type: "P1P1".to_string(),
                    count: QuantityExpr::Fixed { value: 1 },
                    if_type: None,
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        let p1p1 = token
            .counters
            .get(&crate::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            p1p1, 1,
            "token should have one +1/+1 counter; counters={:?}",
            token.counters
        );
    }

    /// CR 707.9f: Conditional `if_type` declines when the resolved object's
    /// core type doesn't match. Token-copy of a non-creature with
    /// `AddCounterOnEnter { if_type: Some(Creature) }` must NOT place the
    /// counter (mirrors Spark Double's "if it's a creature" branch on a
    /// planeswalker copy).
    #[test]
    fn copy_token_add_counter_on_enter_if_type_mismatch_skips() {
        use crate::types::ability::{ContinuousModification, QuantityExpr};

        let mut state = GameState::new_two_player(42);
        // Copy source: a planeswalker (no Creature core type).
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_loyalty = Some(3);
            s.loyalty = Some(3);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Jace".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::AddCounterOnEnter {
                    counter_type: "P1P1".to_string(),
                    count: QuantityExpr::Fixed { value: 1 },
                    if_type: Some(CoreType::Creature),
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        let p1p1 = token
            .counters
            .get(&crate::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            p1p1, 0,
            "if_type=Creature must skip on a Planeswalker copy; counters={:?}",
            token.counters
        );
    }

    /// Regression: Helm of the Host (DOM, MH3, BLC) — pin the already-shipped
    /// non-legendary token-copy behavior so a future refactor cannot silently
    /// drop the `RemoveSupertype { Legendary }` stamp.
    ///
    /// Helm of the Host's begin-combat trigger creates a token that's a copy
    /// of equipped creature, "except the token isn't legendary." When the
    /// equipped creature IS legendary, the synthesized token must not be
    /// legendary — both the layered view (`card_types.supertypes`) and the
    /// copiable-values view (`base_card_types.supertypes`) must be free of
    /// `Supertype::Legendary`. Otherwise the legend rule (CR 704.5j) would
    /// collapse the token alongside its source.
    ///
    /// This test exercises the resolver with Helm's full ability shape:
    /// `Effect::CopyTokenOf { target: Typed[Creature]+EquippedBy,
    /// additional_modifications: [RemoveSupertype(Legendary)] }`. The general
    /// resolver behavior is also pinned by
    /// `copy_token_remove_supertype_strips_legendary_from_token` (Miirym
    /// class); this test anchors the named card so the behavior cannot
    /// regress without an explicit failure pointing at Helm of the Host.
    ///
    /// CR 707.9b + CR 205.4 + CR 301.5a: copy modifications, supertype
    /// semantics, and the equipped-creature relationship.
    #[test]
    fn helm_of_the_host_token_copy_strips_legendary_from_equipped_creature() {
        use crate::types::ability::{ContinuousModification, FilterProp, TypeFilter, TypedFilter};
        use crate::types::card_type::Supertype;

        let mut state = GameState::new_two_player(42);

        // Equipped creature: a legendary 7/7 Dragon (e.g., Bahamut).
        let equipped_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bahamut".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&equipped_id).unwrap();
            s.base_power = Some(7);
            s.base_toughness = Some(7);
            s.power = Some(7);
            s.toughness = Some(7);
            s.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        // Helm of the Host: non-legendary Equipment artifact attached to the
        // equipped creature. The trigger source for the begin-combat trigger.
        let helm_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Helm of the Host".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&helm_id).unwrap();
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Equipment".to_string()],
            };
            s.card_types = s.base_card_types.clone();
            s.attached_to = Some(equipped_id.into());
        }

        // Resolve Helm's begin-combat trigger: CopyTokenOf with the exact
        // Helm AST shape (`target: Typed[Creature]+EquippedBy`,
        // `additional_modifications: [RemoveSupertype(Legendary)]`). After
        // trigger resolution the engine has bound `EquippedBy` to the
        // equipped creature, so the resolved ability carries
        // `targets: [Object(equipped_id)]`.
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::EquippedBy],
                }),
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::RemoveSupertype {
                    supertype: Supertype::Legendary,
                }],
            },
            vec![TargetRef::Object(equipped_id)],
            helm_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        // CR 707.2: token copies the equipped creature's name, P/T, and types.
        assert!(token.is_token);
        assert_eq!(token.name, "Bahamut");
        assert_eq!(token.power, Some(7));
        assert_eq!(token.toughness, Some(7));

        // CR 707.9b + CR 205.4: layered view has Legendary stripped.
        assert!(
            !token.card_types.supertypes.contains(&Supertype::Legendary),
            "token must not be Legendary; got supertypes={:?}",
            token.card_types.supertypes
        );

        // CR 707.9b: copiable-values view also has Legendary stripped — the
        // exception is part of the copy effect's bake-in, so future copies
        // of this token also start without Legendary.
        assert!(
            !token
                .base_card_types
                .supertypes
                .contains(&Supertype::Legendary),
            "token's base_card_types must not contain Legendary; got {:?}",
            token.base_card_types.supertypes
        );

        // CR 704.5j: with the original legendary creature and the
        // non-legendary token-copy both on the battlefield, the legend rule
        // SBA must NOT fire — there is exactly one Legendary permanent named
        // "Bahamut" (the source); the token shares the name but is not
        // legendary, so it is not a candidate for collapse.
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);

        assert!(
            !matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ChooseLegend { .. }
            ),
            "legend rule must not present a choice when the token is not legendary; \
             got waiting_for={:?}",
            state.waiting_for
        );
        // Both permanents survive on the battlefield.
        assert_eq!(
            state.objects[&equipped_id].zone,
            Zone::Battlefield,
            "original legendary creature must remain on battlefield"
        );
        assert_eq!(
            state.objects[&token_id].zone,
            Zone::Battlefield,
            "non-legendary token-copy must remain on battlefield"
        );
    }
}
