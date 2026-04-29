use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::zones::Zone;

/// CR 701.20e + CR 608.2c: Look at top N cards (shown only to the looking player),
/// select some to keep per the effect's instructions, rest go elsewhere.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (dig_num, keep_num, is_up_to, filter, kept_dest, rest_dest, is_reveal) =
        match &ability.effect {
            Effect::Dig {
                count,
                keep_count,
                up_to,
                filter,
                destination,
                rest_destination,
                reveal,
            } => {
                let resolved_count =
                    resolve_quantity_with_targets(state, count, ability).max(0) as usize;
                let keep_all_for_reorder = destination == &Some(Zone::Library)
                    && rest_destination == &Some(Zone::Library)
                    && keep_count.is_none();
                (
                    resolved_count,
                    if keep_all_for_reorder {
                        resolved_count
                    } else {
                        keep_count.unwrap_or(1) as usize
                    },
                    *up_to,
                    filter.clone(),
                    *destination,
                    *rest_destination,
                    *reveal,
                )
            }
            _ => (1, 1, false, TargetFilter::Any, None, None, false),
        };

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    // CR 401.5: If a library has fewer cards than required, use as many as available.
    let count = dig_num.min(player.library.len());
    if count == 0 {
        return Ok(());
    }

    let cards: Vec<_> = player
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();
    let keep_count = keep_num.min(cards.len());

    // CR 701.20a: Pure-peek pattern (keep_count = 0): "look at the top card" with no
    // player selection — the sub_ability condition decides whether to take it. Set
    // last_revealed_ids so RevealedHasCardType can evaluate, then return without
    // creating a DigChoice interaction.
    if keep_count == 0 && !is_reveal {
        state.last_revealed_ids = cards.clone();
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 701.20a: If this is a reveal-dig, mark all cards as publicly revealed
    // and emit CardsRevealed before the player makes their selection.
    if is_reveal {
        for &card_id in &cards {
            state.revealed_cards.insert(card_id);
        }
        state.last_revealed_ids = cards.clone();
        let card_names: Vec<String> = cards
            .iter()
            .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
            .collect();
        events.push(GameEvent::CardsRevealed {
            player: ability.controller,
            card_ids: cards.clone(),
            card_names,
        });
    }

    // Pre-compute selectable cards by evaluating the filter against each card.
    // CR 107.3a + CR 601.2b: Use ability context so dynamic thresholds (e.g.
    // `CmcLE { Variable("X") }`) resolve against the caster's announced X.
    let selectable_cards = if matches!(filter, TargetFilter::Any) {
        cards.clone()
    } else {
        let ctx = FilterContext::from_ability(ability);
        cards
            .iter()
            .filter(|&&card_id| matches_target_filter(state, card_id, &filter, &ctx))
            .copied()
            .collect()
    };

    state.waiting_for = WaitingFor::DigChoice {
        player: ability.controller,
        selectable_cards,
        cards,
        keep_count,
        up_to: is_up_to,
        kept_destination: kept_dest,
        rest_destination: rest_dest,
        source_id: Some(ability.source_id),
    };

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
        AbilityCondition, AbilityKind, FilterProp, GainLifePlayer, QuantityExpr, TypedFilter,
    };
    use crate::types::card_type::Supertype;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_dig_ability(dig_num: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Dig {
                count: QuantityExpr::Fixed {
                    value: dig_num as i32,
                },
                destination: None,
                keep_count: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                reveal: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn test_dig_5_keep_1_sets_waiting_for_dig_choice() {
        let mut state = GameState::new_two_player(42);
        for i in 0..7 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_5: Vec<_> = state.players[0]
            .library
            .iter()
            .take(5)
            .copied()
            .collect::<Vec<_>>();

        let ability = make_dig_ability(5);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 5);
                assert_eq!(*cards, top_5);
                assert_eq!(*keep_count, 1);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_dig_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_dig_ability(3);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn dig_reorder_mode_sets_keep_count_to_all_seen_cards() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let ability = ResolvedAbility::new(
            Effect::Dig {
                count: QuantityExpr::Fixed { value: 3 },
                destination: Some(Zone::Library),
                keep_count: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: Some(Zone::Library),
                reveal: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                cards, keep_count, ..
            } => {
                assert_eq!(cards.len(), 3);
                assert_eq!(*keep_count, 3);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    /// CR 701.33 + CR 701.18: After the player's `SelectCards` resolves a
    /// `DigChoice`, the kept (revealed) cards must be published to
    /// `state.tracked_object_sets` so downstream sub_abilities can route
    /// them by type via `TargetFilter::TrackedSetFiltered`. Zimone's
    /// Experiment depends on this — its post-Dig `"Put all land cards
    /// revealed this way onto the battlefield tapped"` resolves against
    /// the tracked set the Dig choice publishes.
    #[test]
    fn dig_choice_publishes_kept_cards_as_tracked_set() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::identifiers::TrackedSetId;

        let mut state = GameState::new_two_player(42);
        let mut card_ids = Vec::new();
        for i in 0..5 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
            card_ids.push(id);
        }
        let cards_on_top: Vec<_> = state.players[0]
            .library
            .iter()
            .take(5)
            .copied()
            .collect::<Vec<_>>();
        let kept: Vec<_> = cards_on_top[..2].to_vec();

        // Simulate Zimone's Dig setup: keep up to 2, no inline destination,
        // rest → library bottom. Matches the parse shape of Zimone's post-
        // `parse_dig_from_among`-patch Dig.
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            selectable_cards: cards_on_top.clone(),
            cards: cards_on_top.clone(),
            keep_count: 2,
            up_to: true,
            kept_destination: None,
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
        };
        let action = GameAction::SelectCards {
            cards: kept.clone(),
        };
        let next_id_before = state.next_tracked_set_id;
        let mut events = Vec::new();

        let outcome = handle_resolution_choice(&mut state, waiting, action, &mut events)
            .expect("DigChoice resolution must succeed");
        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));

        // A fresh tracked set must have been inserted with exactly the kept cards.
        let tracked_id = TrackedSetId(next_id_before);
        let set = state
            .tracked_object_sets
            .get(&tracked_id)
            .expect("tracked set must be inserted for the kept cards");
        assert_eq!(
            *set, kept,
            "tracked set must contain exactly the kept cards"
        );
        assert_eq!(
            state.next_tracked_set_id,
            next_id_before + 1,
            "next_tracked_set_id must have advanced"
        );
    }

    #[test]
    fn dig_choice_reorders_all_looked_at_cards_on_top_before_continuation() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;

        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let cards_on_top: Vec<_> = state.players[0]
            .library
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>();
        let remaining_library: Vec<_> = state.players[0]
            .library
            .iter()
            .skip(3)
            .copied()
            .collect::<Vec<_>>();
        let selected_order = vec![cards_on_top[2], cards_on_top[0], cards_on_top[1]];

        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            selectable_cards: cards_on_top.clone(),
            cards: cards_on_top,
            keep_count: 3,
            up_to: false,
            kept_destination: Some(Zone::Library),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
        };
        state.pending_continuation =
            Some(PendingContinuation::new(Box::new(ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            ))));

        let mut events = Vec::new();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: selected_order.clone(),
            },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        assert!(
            state.players[0].hand.contains(&selected_order[0]),
            "draw continuation must draw the first card in the selected order"
        );
        let expected_library: Vec<_> = selected_order[1..]
            .iter()
            .chain(remaining_library.iter())
            .copied()
            .collect();
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            expected_library,
            "selected order must become top-of-library order before drawing"
        );
    }

    #[test]
    fn dig_choice_rejects_duplicate_selected_cards() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let original_library = state.players[0].library.iter().copied().collect::<Vec<_>>();
        let cards_on_top = original_library.clone();

        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            selectable_cards: cards_on_top.clone(),
            cards: cards_on_top.clone(),
            keep_count: 3,
            up_to: false,
            kept_destination: Some(Zone::Library),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
        };

        let mut events = Vec::new();
        let result = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: vec![cards_on_top[0], cards_on_top[0], cards_on_top[1]],
            },
            &mut events,
        );

        assert!(result.is_err(), "duplicate selections must be rejected");
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            original_library,
            "invalid duplicate selection must not mutate library order"
        );
    }

    #[test]
    fn dig_choice_forwards_kept_cards_to_conditional_continuation() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;

        let mut state = GameState::new_two_player(42);
        let kept = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Legendary Creature".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&kept)
            .unwrap()
            .card_types
            .supertypes
            .push(Supertype::Legendary);

        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other Creature".to_string(),
            Zone::Library,
        );
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            selectable_cards: vec![kept, other],
            cards: vec![kept, other],
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Hand),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
        };
        let mut gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        gain_life.kind = AbilityKind::Spell;
        gain_life.condition = Some(AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                },
            ])),
            use_lki: false,
        });
        state.pending_continuation = Some(PendingContinuation::new(Box::new(gain_life)));

        let mut events = Vec::new();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards { cards: vec![kept] },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        assert_eq!(
            state.players[0].life, 23,
            "conditional continuation must evaluate against the selected card"
        );
    }

    #[test]
    fn dig_choice_marks_optional_context_from_kept_selection() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;

        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Library,
        );
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            selectable_cards: vec![first],
            cards: vec![first, second],
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Hand),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
        };
        let mut gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        gain_life.kind = AbilityKind::Spell;
        gain_life.condition = Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IfYouDo),
        });
        state.pending_continuation = Some(PendingContinuation::new(Box::new(gain_life)));

        let mut events = Vec::new();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards { cards: vec![] },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        assert_eq!(
            state.players[0].life, 23,
            "declining an up-to Dig selection must satisfy Not(IfYouDo)"
        );
    }

    #[test]
    fn dig_choice_marks_optional_context_from_nonempty_selection() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;

        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Library,
        );
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            selectable_cards: vec![first],
            cards: vec![first, second],
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Hand),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
        };
        let mut gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        gain_life.kind = AbilityKind::Spell;
        gain_life.condition = Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IfYouDo),
        });
        state.pending_continuation = Some(PendingContinuation::new(Box::new(gain_life)));

        let mut events = Vec::new();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards { cards: vec![first] },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        assert_eq!(
            state.players[0].life, 20,
            "keeping a card must make Not(IfYouDo) false"
        );
    }

    /// CR 107.3a + CR 601.2b: Dig's filter evaluation must flow through
    /// `FilterContext::from_ability`, so dynamic thresholds (e.g. `CmcLE { X }`)
    /// resolve against the caster's announced `chosen_x`. Bucket-B regression test
    /// for the filter-context migration — ensures Dig doesn't lose X resolution.
    #[test]
    fn dig_filter_resolves_x_against_chosen_x() {
        use crate::types::ability::{FilterProp, QuantityExpr, QuantityRef, TypedFilter};
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaCost;
        let mut state = GameState::new_two_player(42);
        // Build three creatures of different CMCs in the library.
        for (i, cmc) in [(1u64, 1u32), (2, 3), (3, 6)].into_iter() {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("CMC {}", cmc),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(cmc);
        }

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: crate::types::ability::Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]));
        let mut ability = ResolvedAbility::new(
            Effect::Dig {
                count: QuantityExpr::Fixed { value: 3 },
                destination: None,
                keep_count: Some(1),
                up_to: false,
                filter,
                rest_destination: None,
                reveal: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.chosen_x = Some(3);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                selectable_cards, ..
            } => {
                // Selectable set should be exactly the CMC-1 and CMC-3 creatures.
                assert_eq!(selectable_cards.len(), 2);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    /// CR 201.2 + CR 201.2a: `FilterProp::NameMatchesAnyPermanent` must restrict
    /// the Dig's selectable set to library cards whose printed name equals the
    /// name of some permanent on the battlefield. Controllers of the on-board
    /// permanents don't matter when `controller = None` — any permanent
    /// anywhere on the battlefield counts. This is the Mitotic Manipulation
    /// primitive: `filter = NameMatchesAnyPermanent { controller: None }`.
    #[test]
    fn dig_with_name_matches_any_permanent_filter() {
        use crate::types::ability::{ControllerRef, FilterProp, QuantityExpr, TypedFilter};
        let mut state = GameState::new_two_player(42);
        // Library has three cards: "Forest", "Goblin", "Island".
        for (i, name) in ["Forest", "Goblin", "Island"].iter().enumerate() {
            create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                (*name).into(),
                Zone::Library,
            );
        }
        // Opponent controls a "Forest" permanent on the battlefield; controller
        // doesn't matter when controller=None.
        create_object(
            &mut state,
            CardId(100),
            PlayerId(1),
            "Forest".into(),
            Zone::Battlefield,
        );

        let filter = TargetFilter::Typed(TypedFilter::default().properties(vec![
            FilterProp::NameMatchesAnyPermanent { controller: None },
        ]));
        let ability = ResolvedAbility::new(
            Effect::Dig {
                count: QuantityExpr::Fixed { value: 3 },
                destination: Some(Zone::Battlefield),
                keep_count: Some(1),
                up_to: true,
                filter: filter.clone(),
                rest_destination: Some(Zone::Library),
                reveal: false,
            },
            vec![],
            ObjectId(200),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                selectable_cards,
                cards,
                kept_destination,
                rest_destination,
                ..
            } => {
                assert_eq!(cards.len(), 3, "all 3 library cards are revealed");
                assert_eq!(
                    selectable_cards.len(),
                    1,
                    "only Forest matches an on-battlefield permanent"
                );
                let forest_obj = state
                    .objects
                    .get(&selectable_cards[0])
                    .expect("selectable object exists");
                assert_eq!(forest_obj.name, "Forest");
                assert_eq!(*kept_destination, Some(Zone::Battlefield));
                assert_eq!(*rest_destination, Some(Zone::Library));
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }

        // Verify the controller-scoped variant: with controller=You, the filter
        // only matches permanents controlled by the ability's controller. The
        // on-board "Forest" is controlled by PlayerId(1), so no library card
        // should match.
        let filter_you = TargetFilter::Typed(TypedFilter::default().properties(vec![
            FilterProp::NameMatchesAnyPermanent {
                controller: Some(ControllerRef::You),
            },
        ]));
        let ability_you = ResolvedAbility::new(
            Effect::Dig {
                count: QuantityExpr::Fixed { value: 3 },
                destination: Some(Zone::Battlefield),
                keep_count: Some(1),
                up_to: true,
                filter: filter_you,
                rest_destination: Some(Zone::Library),
                reveal: false,
            },
            vec![],
            ObjectId(201),
            PlayerId(0),
        );
        let mut events2 = Vec::new();
        resolve(&mut state, &ability_you, &mut events2).unwrap();
        match &state.waiting_for {
            WaitingFor::DigChoice {
                selectable_cards, ..
            } => {
                assert_eq!(
                    selectable_cards.len(),
                    0,
                    "no library card shares a name with a permanent you control"
                );
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    /// CR 608.2c + CR 701.20e: Dig with `destination = Some(Battlefield)` and
    /// `rest_destination = Some(Library)` must route the chosen card to the
    /// battlefield (ETB triggers fire) and the unchosen cards to the bottom of
    /// the owner's library. This is the Mitotic Manipulation primitive at
    /// resolution time — no sub_ability chain required.
    #[test]
    fn dig_resolves_kept_to_battlefield_and_rest_to_library_bottom() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let cards_on_top: Vec<_> = state.players[0]
            .library
            .iter()
            .take(5)
            .copied()
            .collect::<Vec<_>>();
        let kept = vec![cards_on_top[2]]; // pick the middle card
        let rest_ids: Vec<_> = cards_on_top
            .iter()
            .filter(|id| !kept.contains(id))
            .copied()
            .collect();

        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            selectable_cards: cards_on_top.clone(),
            cards: cards_on_top.clone(),
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Battlefield),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
        };
        let action = GameAction::SelectCards {
            cards: kept.clone(),
        };
        let mut events = Vec::new();
        let outcome =
            handle_resolution_choice(&mut state, waiting, action, &mut events).expect("ok");
        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));

        // Kept card is on the battlefield.
        let kept_obj = state.objects.get(&kept[0]).expect("kept object exists");
        assert_eq!(kept_obj.zone, Zone::Battlefield);
        // Rest of the cards are at the bottom of PlayerId(0)'s library.
        let library = &state.players[0].library;
        let bottom: Vec<_> = library
            .iter()
            .rev()
            .take(rest_ids.len())
            .rev()
            .copied()
            .collect();
        for id in &rest_ids {
            assert!(
                bottom.contains(id),
                "card {:?} must be at library bottom",
                id
            );
            let obj = state.objects.get(id).expect("rest object exists");
            assert_eq!(obj.zone, Zone::Library);
        }
    }
}
