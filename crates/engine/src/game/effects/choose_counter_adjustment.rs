//! CR 122.1 + CR 608.2d (Clockspinning): choose a kind of counter present on the
//! single target permanent or suspended card, then add and/or remove one of that
//! kind.
//!
//! The chosen counter KIND is decided at resolution from the kinds actually
//! present on the target (a runtime fact), so the per-kind branch set must be
//! built here rather than at parse time. The resolver delegates to the existing
//! `choose_one_of` machinery: it constructs a flat `Effect::ChooseOneOf` of
//! concrete `PutCounter`/`RemoveCounter` branches (one per present kind × allowed
//! operation) and hands it off, reusing the whole `ChooseOneOfBranch` interactive
//! surface (`WaitingFor`, `GameAction::ChooseBranch`, AI candidates, multiplayer
//! routing, frontend modal) with zero new surface.
//!
//! This is the choose-ONE-kind sibling of the `repeat_for:
//! DistinctCounterKindsAmong` for-EACH-kind loop (Bribe Taker / Quarry Hauler).

use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectError, EffectKind, PlayerFilter, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::counter::positive_counter_types;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 122.1 + CR 608.2d: resolve a `ChooseCounterAdjustment`.
///
/// The single target object is read from `ability.targets` (propagated from the
/// parent `TargetOnly` clause), mirroring `Effect::ChooseOneOf`; this effect has
/// no target slot of its own. The counter mutation operates directly on
/// `state.objects.get_mut(&id)` through the existing counter resolvers, which are
/// zone-agnostic, so a suspended card in exile is modified correctly (CR 122.1: a
/// counter is a marker on any object regardless of zone; CR 702.62b: a suspended
/// card lives in exile bearing a time counter).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (adjustment, count) = match &ability.effect {
        Effect::ChooseCounterAdjustment { adjustment, count } => (*adjustment, count.clone()),
        _ => {
            return Err(EffectError::MissingParam(
                "ChooseCounterAdjustment".to_string(),
            ))
        }
    };

    // Emit the resolution marker for any path that does nothing (no target, target
    // gone, or no counters present) — CR 608.2d.
    let no_op = |state: &mut GameState, events: &mut Vec<GameEvent>| {
        let _ = state;
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseCounterAdjustment,
            source_id: ability.source_id,
        });
    };

    // The single target object, supplied via the parent-target chain.
    let Some(target_id) = ability.targets.iter().find_map(|t| match t {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    }) else {
        no_op(state, events);
        return Ok(());
    };

    let Some(obj) = state.objects.get(&target_id) else {
        no_op(state, events);
        return Ok(());
    };

    // CR 608.2d: with no counters present there is no legal kind to choose, so the
    // effect does nothing (Clockspinning Gatherer ruling: "if the chosen permanent
    // or card has no counters, nothing happens"). Deterministic ordering by
    // `as_str()` keeps branch order stable across HashMap iteration (mirrors the
    // `DistinctCounterKindsAmong` ordering).
    let mut kinds = positive_counter_types(&obj.counters);
    kinds.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    if kinds.is_empty() {
        no_op(state, events);
        return Ok(());
    }

    // Build the flat per-kind branch set. For an `AddOrRemove` clause each kind
    // contributes a remove branch then an add branch (matching the printed Oracle
    // order "remove ... or put ...").
    let mut branches: Vec<AbilityDefinition> = Vec::new();
    for kind in kinds {
        if adjustment.allows_remove() {
            branches.push(AbilityDefinition {
                description: Some(format!("remove a {} counter", kind.display_phrase())),
                ..AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::RemoveCounter {
                        counter_type: Some(kind.clone()),
                        // CR 122.1: remove ONE of the chosen kind — `Fixed { 1 }`,
                        // NOT the `-1` "remove all" sentinel.
                        count: count.clone(),
                        target: TargetFilter::ParentTarget,
                    },
                )
            });
        }
        if adjustment.allows_add() {
            branches.push(AbilityDefinition {
                description: Some(format!("put a {} counter", kind.display_phrase())),
                ..AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        counter_type: kind,
                        count: count.clone(),
                        target: TargetFilter::ParentTarget,
                    },
                )
            });
        }
    }

    // CR 608.2d: emit the `ChooseCounterAdjustment` resolution marker on the
    // active (counters-present) path too, so downstream consumers keying on the
    // EffectResolved kind see this effect regardless of whether the target had
    // counters — matching the three no-op paths above. The delegated
    // `choose_one_of::resolve` below additionally emits its own `ChooseOneOf`
    // marker for the interactive sub-choice.
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseCounterAdjustment,
        source_id: ability.source_id,
    });

    // Delegate to the existing ChooseOneOf machinery. The parent target is
    // forwarded as the sub-ability's targets so each chosen branch's
    // `ParentTarget` resolves to the same object.
    let sub = ResolvedAbility::new(
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches,
        },
        vec![TargetRef::Object(target_id)],
        ability.source_id,
        ability.controller,
    );
    super::choose_one_of::resolve(state, &sub, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{CounterAdjustment, QuantityExpr};

    use crate::game::engine::apply;
    use crate::game::zones::create_object;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn adjust_ability(
        target: crate::types::identifiers::ObjectId,
        source: crate::types::identifiers::ObjectId,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ChooseCounterAdjustment {
                adjustment: CounterAdjustment::AddOrRemove,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        )
    }

    /// CR 122.1 + CR 702.62b: a suspended card in EXILE with time counters can be
    /// adjusted — proves the counter mutation is zone-agnostic.
    #[test]
    fn adjusts_time_counter_on_suspended_card_in_exile() {
        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );
        // The suspended-card filter (suspend keyword + time counter) is exercised
        // in the parser test; the resolver reads `ability.targets` directly, so the
        // scenario only needs a card in EXILE bearing a time counter to prove the
        // counter mutation is zone-agnostic.
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Suspended Card".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Time, 3);

        // Remove path: 3 -> 2.
        let ability = adjust_ability(target, source);
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "single-kind target must surface a remove/add choice, got {:?}",
            state.waiting_for
        );
        apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 0 },
        )
        .unwrap();
        assert_eq!(
            state
                .objects
                .get(&target)
                .unwrap()
                .counters
                .get(&CounterType::Time)
                .copied(),
            Some(2),
            "branch 0 (remove) must take the time counter from 3 to 2 in exile"
        );

        // Add path: 2 -> 3.
        let ability = adjust_ability(target, source);
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 1 },
        )
        .unwrap();
        assert_eq!(
            state
                .objects
                .get(&target)
                .unwrap()
                .counters
                .get(&CounterType::Time)
                .copied(),
            Some(3),
            "branch 1 (add) must put a time counter back, 2 to 3 in exile"
        );
    }

    /// CR 122.1 + CR 608.2d + CR 702.62b (Clockspinning end-to-end): resolve the
    /// PARSED ability shape — a `TargetOnly` parent over the battlefield∪exile `Or`
    /// filter plus an EMPTY-target `ChooseCounterAdjustment` sub — through the real
    /// `resolve_ability_chain`. Discriminating vs the direct-resolve tests above:
    /// the sub carries no targets of its own, so the load-bearing parent→sub
    /// target threading (`sub.targets.is_empty() && !ability.targets.is_empty()` in
    /// `effects::mod`) must copy the parent's resolved object into the slot-less
    /// sub before the per-kind branch set is built. Without that threading the sub
    /// finds no target and the effect silently no-ops; this test would then fail to
    /// surface a prompt. Target is a suspended card in EXILE (time counter), so the
    /// same path also re-confirms zone-agnostic mutation across the full pipeline.
    #[test]
    fn parsed_target_only_threads_parent_target_into_empty_sub() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::{Comparator, FilterProp, TypeFilter, TypedFilter};
        use crate::types::counter::CounterMatch;
        use crate::types::keywords::KeywordKind;

        let mut state = GameState::new_two_player(99);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Suspended Card".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Time, 3);

        // EMPTY-target sub: must inherit the parent's resolved target via threading.
        let sub = ResolvedAbility::new(
            Effect::ChooseCounterAdjustment {
                adjustment: CounterAdjustment::AddOrRemove,
                count: QuantityExpr::Fixed { value: 1 },
            },
            Vec::new(),
            source,
            PlayerId(0),
        );

        // Parent `TargetOnly` over the real `Or[battlefield permanent | exiled
        // suspended card]` filter (the parser's output). The filter is a
        // resolution-time no-op — targeting is established at cast — but it
        // documents the parsed shape the integration point feeds from.
        let mut ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Permanent],
                            controller: None,
                            properties: vec![FilterProp::InZone {
                                zone: Zone::Battlefield,
                            }],
                        }),
                        TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Card],
                            controller: None,
                            properties: vec![
                                FilterProp::InZone { zone: Zone::Exile },
                                FilterProp::HasKeywordKind {
                                    value: KeywordKind::Suspend,
                                },
                                FilterProp::Counters {
                                    counters: CounterMatch::OfType(CounterType::Time),
                                    comparator: Comparator::GE,
                                    count: QuantityExpr::Fixed { value: 1 },
                                },
                            ],
                        }),
                    ],
                },
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(sub));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // The threaded parent target carries a single kind (Time), so the sub
        // surfaces the remove/add choice rather than no-opping.
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "parent target must thread into the empty sub and surface a choice, got {:?}",
            state.waiting_for
        );
        // Branch 0 (remove): 3 -> 2, proving the chosen object resolved through
        // the chain and the mutation is zone-agnostic in exile.
        apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 0 },
        )
        .unwrap();
        assert_eq!(
            state
                .objects
                .get(&target)
                .unwrap()
                .counters
                .get(&CounterType::Time)
                .copied(),
            Some(2),
            "threaded remove branch must take the exiled time counter 3 -> 2"
        );
    }

    /// CR 122.1: with two distinct kinds the controller selects ONE kind (not
    /// for-each) — choosing "remove a lore counter" affects only Lore.
    #[test]
    fn single_kind_selection_leaves_other_kind_untouched() {
        let mut state = GameState::new_two_player(11);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 2);
            obj.counters.insert(CounterType::Lore, 1);
        }

        let ability = adjust_ability(target, source);
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        // Deterministic order by `as_str()`: ["P1P1", "lore"]. AddOrRemove emits
        // remove-then-add per kind, so branches are:
        //   0: remove P1P1, 1: add P1P1, 2: remove lore, 3: add lore.
        let WaitingFor::ChooseOneOfBranch {
            ref branch_descriptions,
            ..
        } = state.waiting_for
        else {
            panic!("expected ChooseOneOfBranch, got {:?}", state.waiting_for);
        };
        assert_eq!(
            branch_descriptions.len(),
            4,
            "two kinds x two ops = 4 branches"
        );

        // Choose "remove a lore counter" (index 2).
        apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 2 },
        )
        .unwrap();
        let obj = state.objects.get(&target).unwrap();
        assert_eq!(
            obj.counters.get(&CounterType::Lore).copied().unwrap_or(0),
            0,
            "removing the lore kind must take lore 1 -> 0"
        );
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2,
            "the unchosen P1P1 kind must be untouched (single-kind, not for-each)"
        );
    }

    /// CR 608.2d: a target with no counters offers no legal kind — the effect does
    /// nothing and surfaces no prompt.
    #[test]
    fn no_counters_is_a_noop() {
        let mut state = GameState::new_two_player(5);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = adjust_ability(target, source);
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "no counters present must not surface a prompt"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChooseCounterAdjustment,
                    ..
                }
            )),
            "no-op must still emit EffectResolved"
        );
        assert!(
            state.objects.get(&target).unwrap().counters.is_empty(),
            "no counters added or removed"
        );
    }

    /// Building-block coverage for the operation-set axis across all three
    /// variants.
    #[test]
    fn counter_adjustment_operation_set() {
        assert!(CounterAdjustment::Add.allows_add());
        assert!(!CounterAdjustment::Add.allows_remove());
        assert!(!CounterAdjustment::Remove.allows_add());
        assert!(CounterAdjustment::Remove.allows_remove());
        assert!(CounterAdjustment::AddOrRemove.allows_add());
        assert!(CounterAdjustment::AddOrRemove.allows_remove());
    }

    /// CR 122.1 + CR 702.62b + CR 115.1: verifies that a suspended card in
    /// exile (bearing the Suspend keyword and a time counter) is accepted by the
    /// battlefield∪exile-suspended `Or` target filter through the live
    /// `find_legal_targets` path, not just via direct target injection.
    ///
    /// Also confirms that a battlefield permanent with counters is offered, while
    /// an exiled card that lacks the Suspend keyword is not.
    #[test]
    fn suspended_card_is_a_legal_target_through_filter() {
        use crate::game::targeting::find_legal_targets;
        use crate::types::ability::{Comparator, FilterProp, TypeFilter, TypedFilter};
        use crate::types::counter::CounterMatch;
        use crate::types::keywords::{Keyword, KeywordKind};
        use crate::types::mana::ManaCost;

        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );

        // A suspended card in exile: must have the Suspend keyword + time counter
        // to satisfy both legs of the exile filter (CR 702.62b).
        //
        // Off-zone keyword evaluation (filter.rs `HasKeywordKind`) delegates to
        // `off_zone_has_keyword_kind` → `effective_off_zone_keywords` which
        // reads `base_keywords`, not the battlefield-layer `keywords` field.
        // Push to `base_keywords` here so the `HasKeywordKind{Suspend}` predicate
        // fires correctly for a card in exile.
        let suspended = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Suspended Card".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&suspended).unwrap();
            obj.base_keywords.push(Keyword::Suspend {
                count: 3,
                cost: ManaCost::zero(),
            });
            obj.counters.insert(CounterType::Time, 3);
        }

        // A battlefield permanent with counters — must also be a legal target.
        let battlefield_perm = create_object(
            &mut state,
            CardId(30),
            PlayerId(1),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&battlefield_perm).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 2);
        }

        // An exiled card WITHOUT the Suspend keyword — must NOT be offered even
        // though it carries a time counter (CR 702.62b: suspend requires the
        // keyword, not just the counter).
        let bare_exile = create_object(
            &mut state,
            CardId(40),
            PlayerId(1),
            "Bare Exiled Card".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&bare_exile)
            .unwrap()
            .counters
            .insert(CounterType::Time, 1);

        // The Or filter mirrors the parser output for Clockspinning sentence 1:
        // battlefield permanent OR suspended card in exile with ≥1 time counter.
        let filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Permanent],
                    controller: None,
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Battlefield,
                    }],
                }),
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: None,
                    properties: vec![
                        FilterProp::InZone { zone: Zone::Exile },
                        FilterProp::HasKeywordKind {
                            value: KeywordKind::Suspend,
                        },
                        FilterProp::Counters {
                            counters: CounterMatch::OfType(CounterType::Time),
                            comparator: Comparator::GE,
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                }),
            ],
        };

        let targets = find_legal_targets(&state, &filter, PlayerId(0), source);

        // The suspended card must be offered as a legal target via the live
        // targeting path (not just via direct injection).
        assert!(
            targets.contains(&TargetRef::Object(suspended)),
            "suspended card in exile must be a legal target; got {targets:?}"
        );
        // The battlefield permanent must also be legal.
        assert!(
            targets.contains(&TargetRef::Object(battlefield_perm)),
            "battlefield permanent must be a legal target; got {targets:?}"
        );
        // The bare exiled card (no Suspend keyword) must NOT be legal.
        assert!(
            !targets.contains(&TargetRef::Object(bare_exile)),
            "exiled card without Suspend must not be offered; got {targets:?}"
        );
    }
}
