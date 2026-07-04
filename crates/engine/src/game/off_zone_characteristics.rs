use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::layers::{
    active_continuous_effects_from_base_static_source, collect_shared_active_continuous_effects,
    evaluate_condition_with_recipient, order_active_continuous_effects,
};
use crate::game::quantity::resolve_quantity;
use crate::types::ability::ContinuousModification;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::layers::{ActiveContinuousEffect, Layer};
use crate::types::zones::Zone;

pub fn effective_off_zone_keywords(state: &GameState, object_id: ObjectId) -> Vec<Keyword> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    if obj.zone == Zone::Battlefield {
        return obj.keywords.clone();
    }

    let mut keywords = obj.base_keywords.clone();
    let effects = collect_applicable_off_zone_keyword_effects(state, object_id);
    let ordered = order_active_continuous_effects(Layer::Ability, &effects, state);

    for effect in ordered {
        apply_keyword_modification(state, &mut keywords, &effect);
    }

    keywords
}

pub fn effective_off_zone_keyword(
    state: &GameState,
    object_id: ObjectId,
    kind: crate::types::keywords::KeywordKind,
) -> Option<Keyword> {
    effective_off_zone_keywords(state, object_id)
        .into_iter()
        .find(|keyword| keyword.kind() == kind)
}

pub fn off_zone_has_keyword_kind(
    state: &GameState,
    object_id: ObjectId,
    kind: crate::types::keywords::KeywordKind,
) -> bool {
    effective_off_zone_keyword(state, object_id, kind).is_some()
}

fn collect_applicable_off_zone_keyword_effects(
    state: &GameState,
    object_id: ObjectId,
) -> Vec<ActiveContinuousEffect> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    let mut effects = collect_shared_active_continuous_effects(state);
    if obj.zone != Zone::Battlefield && !(obj.zone == Zone::Command && obj.is_emblem) {
        effects.extend(active_continuous_effects_from_base_static_source(
            state, obj,
        ));
    }

    effects
        .into_iter()
        .filter(|effect| {
            effect.layer == Layer::Ability
                && supports_off_zone_keyword_query(&effect.modification)
                && matches_target_filter(
                    state,
                    object_id,
                    &effect.affected_filter,
                    &FilterContext::from_source_with_controller(
                        effect.source_id,
                        effect.controller,
                    ),
                )
                && effect.condition.as_ref().is_none_or(|condition| {
                    evaluate_condition_with_recipient(
                        state,
                        condition,
                        effect.controller,
                        effect.source_id,
                        object_id,
                    )
                })
        })
        .collect()
}

fn supports_off_zone_keyword_query(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddKeyword { .. }
            | ContinuousModification::RemoveKeyword { .. }
            | ContinuousModification::AddDynamicKeyword { .. }
            | ContinuousModification::RemoveAllAbilities
            // CR 608.2d + CR 613.1f: `RemoveChosenKeyword` strips by
            // discriminant the keyword stored in the source's
            // `chosen_attributes` (Urborg / Walking Sponge). Same off-zone
            // applicability as `RemoveKeyword` — the granted/printed keyword
            // it targets may live on an object outside the battlefield.
            | ContinuousModification::RemoveChosenKeyword
            // CR 608.2d + CR 613.1f: `AddChosenKeyword` grants the keyword
            // stored in the source's `chosen_attributes`. Same off-zone
            // applicability as `AddKeyword`.
            | ContinuousModification::AddChosenKeyword
    )
}

fn apply_keyword_modification(
    state: &GameState,
    keywords: &mut Vec<Keyword>,
    effect: &ActiveContinuousEffect,
) {
    match &effect.modification {
        ContinuousModification::AddKeyword { keyword } => upsert_keyword(keywords, keyword.clone()),
        ContinuousModification::RemoveKeyword { keyword } => {
            keywords.retain(|existing| {
                std::mem::discriminant(existing) != std::mem::discriminant(keyword)
            });
        }
        ContinuousModification::AddDynamicKeyword { kind, value } => {
            let dynamic_value = resolve_quantity(state, value, effect.controller, effect.source_id);
            let keyword = kind.with_value(dynamic_value.max(0) as u32);
            upsert_keyword(keywords, keyword);
        }
        ContinuousModification::RemoveAllAbilities => keywords.clear(),
        // CR 608.2d + CR 613.1f + CR 702.14: Strip the *exact* keyword
        // chosen at resolution time — mirrors the battlefield
        // `RemoveChosenKeyword` arm in `layers.rs`. Uses `PartialEq` rather
        // than discriminant equality so that removing swampwalk leaves
        // islandwalk intact (CR 702.14 treats each landwalk subtype as a
        // distinct keyword). If the source has no stored chosen keyword
        // (e.g. the static is gathered before the choose effect has
        // resolved), this is a no-op rather than a panic, matching
        // `layers.rs` semantics.
        ContinuousModification::RemoveChosenKeyword => {
            if let Some(kw) = state
                .objects
                .get(&effect.source_id)
                .and_then(|src| src.chosen_keyword())
            {
                keywords.retain(|existing| existing != kw);
            }
        }
        // CR 608.2d + CR 613.1f: Grant EACH keyword chosen at resolution time —
        // the additive mirror of `RemoveChosenKeyword`, matching the `AddKeyword`
        // upsert semantics above. Reads the PLURAL list so a multi-keyword choice
        // (Greymond's "each of the chosen abilities") grants every chosen ability
        // off-zone, not just the first. No-op when the source has no stored
        // chosen keyword. Source-scoped via `effect.source_id`.
        ContinuousModification::AddChosenKeyword => {
            let chosen: Vec<Keyword> = state
                .objects
                .get(&effect.source_id)
                .map(|src| src.chosen_keywords().into_iter().cloned().collect())
                .unwrap_or_default();
            for kw in chosen {
                upsert_keyword(keywords, kw);
            }
        }
        _ => {}
    }
}

fn upsert_keyword(keywords: &mut Vec<Keyword>, keyword: Keyword) {
    // CR 702.164b: summing keywords (Toxic) accumulate — never overwrite; push so
    // every instance is counted by the aggregate reader (effective_total_toxic_value).
    // Gate on the INCOMING keyword's summing flag (not a kind comparison) so a
    // granted off-zone Toxic pushes rather than clobbering an unrelated printed
    // keyword that shares its (Unknown) kind. Non-summing keywords keep the
    // upsert-by-kind dedup below unchanged.
    if !keyword.sums_across_instances() {
        if let Some(existing) = keywords
            .iter_mut()
            .find(|existing| existing.kind() == keyword.kind())
        {
            *existing = keyword;
            return;
        }
    }

    keywords.push(keyword);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, Duration, QuantityExpr, StaticCondition, StaticDefinition,
        TargetFilter,
    };
    use crate::types::identifiers::CardId;
    use crate::types::keywords::{DynamicKeywordKind, FlashbackCost, Keyword, KeywordKind};
    use crate::types::mana::{ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn create_card(state: &mut GameState, owner: PlayerId, name: &str, zone: Zone) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let timestamp = state.next_timestamp();
        let object_id = create_object(state, card_id, owner, name.to_string(), zone);
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.timestamp = timestamp;
        }
        object_id
    }

    /// CR 702.164b (issue #955): `upsert_keyword` must PUSH summing keywords
    /// (Toxic) so the off-zone aggregate reader counts every instance, while
    /// keeping the upsert-by-kind dedup for all non-summing keywords. The gate
    /// keys on the INCOMING keyword's summing flag, not a kind comparison, so a
    /// granted Toxic (which collapses to `KeywordKind::Unknown`) does NOT clobber
    /// an unrelated printed keyword that happens to share that `Unknown` kind.
    #[test]
    fn upsert_keyword_sums_toxic_but_dedups_others() {
        use crate::types::keywords::WardCost;

        // Summing keyword: two Toxic(1) accumulate (push) -> len 2.
        let mut toxic = Vec::new();
        upsert_keyword(&mut toxic, Keyword::Toxic(1));
        upsert_keyword(&mut toxic, Keyword::Toxic(1));
        assert_eq!(toxic.len(), 2, "two granted Toxic(1) must both be retained");
        assert_eq!(
            toxic
                .iter()
                .filter(|kw| matches!(kw, Keyword::Toxic(_)))
                .count(),
            2
        );

        // Non-summing keyword sharing a distinct kind: the second Ward overwrites
        // the first (upsert-by-kind dedup preserved) -> len 1.
        let mut ward = Vec::new();
        upsert_keyword(
            &mut ward,
            Keyword::Ward(WardCost::Mana(ManaCost::Cost {
                generic: 1,
                shards: vec![],
            })),
        );
        upsert_keyword(
            &mut ward,
            Keyword::Ward(WardCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })),
        );
        assert_eq!(ward.len(), 1, "non-summing Ward keeps upsert-by-kind dedup");
        assert_eq!(
            ward[0],
            Keyword::Ward(WardCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })),
            "the later Ward instance overwrites the earlier one"
        );

        // Clobber-protection residual: an incoming Toxic grant must NOT overwrite
        // an unrelated printed keyword that also collapses to KeywordKind::Unknown
        // (StartingIntensity). Both must survive.
        assert_eq!(Keyword::StartingIntensity(1).kind(), KeywordKind::Unknown);
        assert_eq!(Keyword::Toxic(1).kind(), KeywordKind::Unknown);
        let mut mixed = vec![Keyword::StartingIntensity(1)];
        upsert_keyword(&mut mixed, Keyword::Toxic(1));
        assert_eq!(
            mixed.len(),
            2,
            "Toxic grant must not clobber an unrelated same-(Unknown)-kind printed keyword"
        );
        assert!(mixed.contains(&Keyword::StartingIntensity(1)));
        assert!(mixed.contains(&Keyword::Toxic(1)));
    }

    #[test]
    fn printed_graveyard_keyword_is_returned_unchanged() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(
            &mut state,
            PlayerId(0),
            "Faithless Looting",
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            })));
        let base_keywords = state.objects.get(&card_id).unwrap().base_keywords.clone();
        state.objects.get_mut(&card_id).unwrap().keywords = base_keywords;

        let keywords = effective_off_zone_keywords(&state, card_id);
        assert_eq!(keywords.len(), 1);
        assert_eq!(
            keywords[0],
            Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            }))
        );
    }

    #[test]
    fn transient_add_keyword_applies_to_graveyard_card() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(
            &mut state,
            PlayerId(0),
            "Snapcaster Mage",
            Zone::Battlefield,
        );
        let target_id = create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard);

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
            }],
            None,
        );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(FlashbackCost::Mana(
                ManaCost::SelfManaCost
            )))
        );
    }

    /// V8 — CR 608.2d + CR 613.1f: the off-zone `AddChosenKeyword` arm must read
    /// the PLURAL chosen-keyword list off the granting source (Greymond's two
    /// chosen abilities), not just the first. A battlefield source carrying TWO
    /// `ChosenAttribute::Keyword` grants both to an off-battlefield recipient.
    #[test]
    fn add_chosen_keyword_off_zone_reads_all_chosen_keywords() {
        use crate::types::ability::ChosenAttribute;

        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Greymond", Zone::Battlefield);
        let target_id = create_card(&mut state, PlayerId(0), "Exiled Human", Zone::Exile);

        // Two abilities chosen as Greymond entered.
        {
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.chosen_attributes
                .push(ChosenAttribute::Keyword(Keyword::FirstStrike));
            obj.chosen_attributes
                .push(ChosenAttribute::Keyword(Keyword::Lifelink));
        }

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::AddChosenKeyword],
            None,
        );

        let kws = effective_off_zone_keywords(&state, target_id);
        assert!(
            kws.contains(&Keyword::FirstStrike) && kws.contains(&Keyword::Lifelink),
            "off-zone AddChosenKeyword must surface BOTH chosen keywords, got {kws:?}"
        );
    }

    /// CR 702.138a + CR 601.2g/h: a transient `AddKeyword(Escape)` carrying the
    /// COMPOUND granted cost (mana sub-cost + "exile N other cards from your
    /// graveyard" residual) makes a graveyard card castable via escape —
    /// `effective_escape_data` resolves the mana sub-cost (CR 601.2g) and surfaces
    /// the exile residual for `pay_additional_cost` (CR 601.2h). Runtime proof for
    /// the parser front door `try_parse_grant_graveyard_keyword_to_target`
    /// (Confession Dial / Desdemona). Tests the building block — a transient
    /// off-zone Escape grant — not a single card.
    #[test]
    fn transient_granted_compound_escape_makes_graveyard_card_castable() {
        use crate::types::ability::AbilityCost;
        use crate::types::keywords::EscapeCost;

        let mut state = GameState::new_two_player(42);
        let source_id = create_card(
            &mut state,
            PlayerId(0),
            "Snapcaster Mage",
            Zone::Battlefield,
        );
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Scrubland Mongoose",
            Zone::Graveyard,
        );

        let exile_residual = AbilityCost::Exile {
            count: 3,
            zone: Some(Zone::Graveyard),
            filter: None,
        };

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Escape(EscapeCost::NonMana(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::SelfManaCost,
                        },
                        exile_residual.clone(),
                    ],
                })),
            }],
            None,
        );

        let (_, residual) = crate::game::keywords::effective_escape_data(&state, target_id)
            .expect("granted compound escape must make the graveyard card castable");
        assert_eq!(residual, exile_residual);
    }

    #[test]
    fn battlefield_static_grants_sneak_to_graveyard_creature() {
        // CR 702.190a: Ninja Teen Level 3 grants Sneak to creature cards in GY.
        // Verifies the off-zone pipeline routes the static's AddKeyword::Sneak
        // through to the GY object, so `effective_sneak_cost` (used by the cost
        // substitution branch in casting.rs) will resolve correctly.
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Ninja Teen", Zone::Battlefield);
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Scrubland Mongoose",
            Zone::Graveyard,
        );

        let sneak_cost = ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black],
        };
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Sneak(sneak_cost.clone()),
                    }]),
            );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Sneak),
            Some(Keyword::Sneak(sneak_cost.clone()))
        );
        assert_eq!(
            crate::game::keywords::effective_sneak_cost(&state, target_id),
            Some(sneak_cost)
        );
    }

    #[test]
    fn battlefield_static_grants_keyword_to_graveyard_card() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Lier", Zone::Battlefield);
        let target_id = create_card(&mut state, PlayerId(0), "Consider", Zone::Graveyard);

        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                    }]),
            );

        assert!(off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn off_zone_keyword_static_respects_condition() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(
            &mut state,
            PlayerId(0),
            "Conditional Source",
            Zone::Battlefield,
        );
        let target_id = create_card(&mut state, PlayerId(0), "Consider", Zone::Graveyard);

        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                    }])
                    .condition(StaticCondition::IsPresent {
                        filter: Some(TargetFilter::SpecificObject { id: source_id }),
                    }),
            );
        assert!(off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));

        state.objects.get_mut(&source_id).unwrap().zone = Zone::Graveyard;
        state.battlefield.retain(|id| *id != source_id);
        state.players[0].graveyard.push_back(source_id);

        assert!(!off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn self_static_in_graveyard_grants_keyword_to_self() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Viral Spawning", Zone::Graveyard);

        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&card_id)
                .unwrap()
                .base_static_definitions,
        )
        .push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                        generic: 2,
                        shards: vec![ManaCostShard::Green],
                    })),
                }]),
        );
        let base_static_definitions = state
            .objects
            .get(&card_id)
            .unwrap()
            .base_static_definitions
            .clone();
        state.objects.get_mut(&card_id).unwrap().static_definitions =
            (*base_static_definitions).clone().into();

        assert_eq!(
            effective_off_zone_keyword(&state, card_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            })))
        );
    }

    #[test]
    fn command_zone_emblem_grants_keyword_to_non_battlefield_card() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_card(&mut state, PlayerId(0), "Emblem", Zone::Command);
        let target_id = create_card(&mut state, PlayerId(0), "Think Twice", Zone::Exile);

        {
            let emblem = state.objects.get_mut(&emblem_id).unwrap();
            emblem.is_emblem = true;
            emblem.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                    }]),
            );
        }

        assert!(off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn remove_keyword_suppresses_matching_keyword_kind() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Faithless Looting",
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            })));
        let base_keywords = state.objects.get(&target_id).unwrap().base_keywords.clone();
        state.objects.get_mut(&target_id).unwrap().keywords = base_keywords;

        let source_id = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::RemoveKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Red],
                        })),
                    }]),
            );

        assert!(!off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn off_zone_queries_ignore_non_base_keyword_residue() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Creature", Zone::Graveyard);
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        assert!(effective_off_zone_keywords(&state, card_id).is_empty());
    }

    #[test]
    fn off_zone_self_statics_use_base_static_definitions() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Viral Spawning", Zone::Graveyard);
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                    generic: 2,
                    shards: vec![ManaCostShard::Green],
                })),
            }]);
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .base_static_definitions = Arc::new(vec![static_def]);
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .static_definitions
            .clear();

        assert_eq!(
            effective_off_zone_keyword(&state, card_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            })))
        );
    }

    #[test]
    fn remove_all_abilities_clears_keywords_for_query() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Faithless Looting",
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            })));
        let base_keywords = state.objects.get(&target_id).unwrap().base_keywords.clone();
        state.objects.get_mut(&target_id).unwrap().keywords = base_keywords;

        let source_id = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::RemoveAllAbilities]),
            );

        assert!(effective_off_zone_keywords(&state, target_id).is_empty());
    }

    #[test]
    fn add_dynamic_keyword_uses_quantity_resolution() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        let target_id = create_card(&mut state, PlayerId(0), "Arcbound Ravager", Zone::Graveyard);

        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddDynamicKeyword {
                        kind: DynamicKeywordKind::Modular,
                        value: QuantityExpr::Fixed { value: 3 },
                    }]),
            );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Modular),
            Some(Keyword::Modular(3))
        );
    }

    #[test]
    fn later_effect_replaces_same_keyword_kind_payload() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_card(&mut state, PlayerId(0), "Think Twice", Zone::Graveyard);
        let earlier_id = create_card(&mut state, PlayerId(0), "Earlier", Zone::Battlefield);
        let later_id = create_card(&mut state, PlayerId(0), "Later", Zone::Battlefield);

        state
            .objects
            .get_mut(&earlier_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::Blue],
                        })),
                    }]),
            );
        state
            .objects
            .get_mut(&later_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Blue],
                        })),
                    }]),
            );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Blue],
            })))
        );
    }
}
