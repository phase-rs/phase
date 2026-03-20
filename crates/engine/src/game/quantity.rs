//! Dynamic quantity resolution for QuantityExpr values.
//!
//! Evaluates QuantityRef variants (ObjectCount, PlayerCount, CountersOnSelf, etc.)
//! against the current game state at resolution time. Used by effect resolvers
//! to support "for each [X]" patterns on Draw, DealDamage, GainLife, LoseLife, Mill.

use std::collections::HashSet;

use crate::game::filter::matches_target_filter_controlled;
use crate::game::game_object::parse_counter_type;
use crate::types::ability::{
    AggregateFunction, CountScope, ObjectProperty, PlayerFilter, QuantityExpr, QuantityRef,
    ResolvedAbility, RoundingMode, TargetRef, TypeFilter, ZoneRef,
};
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// Resolve a QuantityExpr to a concrete integer value.
///
/// `controller` is the player who controls the ability (used for relative filters).
/// `source_id` is the object that owns the ability (used for CountersOnSelf, filter matching).
pub fn resolve_quantity(
    state: &GameState,
    expr: &QuantityExpr,
    controller: PlayerId,
    source_id: ObjectId,
) -> i32 {
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(state, qty, controller, source_id, &[]),
        QuantityExpr::HalfRounded { inner, rounding } => {
            let base = resolve_quantity(state, inner, controller, source_id);
            half_rounded(base, *rounding)
        }
        QuantityExpr::Offset { inner, offset } => {
            resolve_quantity(state, inner, controller, source_id) + offset
        }
        QuantityExpr::Multiply { factor, inner } => {
            factor * resolve_quantity(state, inner, controller, source_id)
        }
    }
}

/// Resolve a QuantityExpr with access to the ability's targets.
///
/// Required for TargetPower which needs to look up the targeted permanent.
pub fn resolve_quantity_with_targets(
    state: &GameState,
    expr: &QuantityExpr,
    ability: &ResolvedAbility,
) -> i32 {
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(
            state,
            qty,
            ability.controller,
            ability.source_id,
            &ability.targets,
        ),
        QuantityExpr::HalfRounded { inner, rounding } => {
            let base = resolve_quantity_with_targets(state, inner, ability);
            half_rounded(base, *rounding)
        }
        QuantityExpr::Offset { inner, offset } => {
            resolve_quantity_with_targets(state, inner, ability) + offset
        }
        QuantityExpr::Multiply { factor, inner } => {
            factor * resolve_quantity_with_targets(state, inner, ability)
        }
    }
}

/// CR 107.2: Divide by 2, rounding in the specified direction.
fn half_rounded(value: i32, rounding: RoundingMode) -> i32 {
    match rounding {
        RoundingMode::Up => (value + 1) / 2,
        RoundingMode::Down => value / 2,
    }
}

fn resolve_ref(
    state: &GameState,
    qty: &QuantityRef,
    controller: PlayerId,
    source_id: ObjectId,
    targets: &[TargetRef],
) -> i32 {
    let player = state.players.iter().find(|p| p.id == controller);
    match qty {
        QuantityRef::HandSize => player.map_or(0, |p| p.hand.len() as i32),
        QuantityRef::LifeTotal => player.map_or(0, |p| p.life),
        QuantityRef::GraveyardSize => player.map_or(0, |p| p.graveyard.len() as i32),
        QuantityRef::LifeAboveStarting => {
            player.map_or(0, |p| p.life - state.format_config.starting_life)
        }
        QuantityRef::ObjectCount { filter } => state
            .battlefield
            .iter()
            .filter(|&&id| {
                matches_target_filter_controlled(state, id, filter, source_id, controller)
            })
            .count() as i32,
        QuantityRef::PlayerCount { filter } => resolve_player_count(state, filter, controller),
        QuantityRef::CountersOnSelf { counter_type } => state
            .objects
            .get(&source_id)
            .map(|obj| {
                let ct = parse_counter_type(counter_type);
                obj.counters.get(&ct).copied().unwrap_or(0) as i32
            })
            .unwrap_or(0),
        QuantityRef::Variable { .. } => {
            // Variable amounts (X) are resolved during mana payment, not here.
            // Default to 0 for unresolved variables.
            0
        }
        // CR 208.3: A creature's power/toughness is a characteristic derived from its card.
        QuantityRef::SelfPower => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.power)
            .unwrap_or(0),
        QuantityRef::SelfToughness => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.toughness)
            .unwrap_or(0),
        // CR 107.3e: Aggregate queries over battlefield objects.
        QuantityRef::Aggregate {
            function,
            property,
            filter,
        } => {
            let values = state.battlefield.iter().filter_map(|&id| {
                if matches_target_filter_controlled(state, id, filter, source_id, controller) {
                    state.objects.get(&id).map(|obj| match property {
                        ObjectProperty::Power => obj.power.unwrap_or(0),
                        ObjectProperty::Toughness => obj.toughness.unwrap_or(0),
                        // CR 202.3e: Use mana_value() which correctly excludes X.
                        ObjectProperty::ManaValue => obj.mana_cost.mana_value() as i32,
                    })
                } else {
                    None
                }
            });
            match function {
                AggregateFunction::Max => values.max().unwrap_or(0),
                AggregateFunction::Min => values.min().unwrap_or(0),
                AggregateFunction::Sum => values.sum(),
            }
        }
        QuantityRef::TargetPower => {
            // Find the first object target and return its power.
            targets
                .iter()
                .find_map(|t| {
                    if let TargetRef::Object(id) = t {
                        state.objects.get(id)
                    } else {
                        None
                    }
                })
                .and_then(|obj| obj.power)
                .unwrap_or(0)
        }
        QuantityRef::Devotion { colors } => {
            crate::game::devotion::count_devotion(state, controller, colors) as i32
        }
        QuantityRef::TargetLifeTotal => {
            // CR 119.3 + CR 107.2: Find the first player target and return their life total.
            targets
                .iter()
                .find_map(|t| {
                    if let TargetRef::Player(pid) = t {
                        state.players.iter().find(|p| p.id == *pid)
                    } else {
                        None
                    }
                })
                .map_or(0, |p| p.life)
        }
        // CR 604.3: Count distinct card types (CoreType) across graveyards.
        QuantityRef::CardTypesInGraveyards { scope } => {
            let mut seen = HashSet::new();
            for player in scoped_players(state, scope, controller) {
                for &obj_id in &player.graveyard {
                    if let Some(obj) = state.objects.get(&obj_id) {
                        for ct in &obj.card_types.core_types {
                            seen.insert(*ct);
                        }
                    }
                }
            }
            seen.len() as i32
        }
        // CR 604.3: Count cards in a zone matching optional type filters.
        QuantityRef::ZoneCardCount {
            zone,
            card_types,
            scope,
        } => {
            let mut count = 0;
            // Per-player zones (graveyard, library)
            match zone {
                ZoneRef::Graveyard | ZoneRef::Library => {
                    for player in scoped_players(state, scope, controller) {
                        let zone_ids = match zone {
                            ZoneRef::Graveyard => &player.graveyard,
                            ZoneRef::Library => &player.library,
                            ZoneRef::Exile => unreachable!(),
                        };
                        for &obj_id in zone_ids {
                            if matches_zone_card_filter(state, obj_id, card_types) {
                                count += 1;
                            }
                        }
                    }
                }
                // Exile is global; filter by owner matching scope
                ZoneRef::Exile => {
                    for &obj_id in &state.exile {
                        if let Some(obj) = state.objects.get(&obj_id) {
                            let owner_matches = match scope {
                                CountScope::Controller => obj.owner == controller,
                                CountScope::All => true,
                                CountScope::Opponents => obj.owner != controller,
                            };
                            if owner_matches && matches_zone_card_filter(state, obj_id, card_types)
                            {
                                count += 1;
                            }
                        }
                    }
                }
            }
            count
        }
        // CR 609.3: "for each [thing] this way" — read the most recent tracked set size.
        QuantityRef::TrackedSetSize => state
            .tracked_object_sets
            .iter()
            .max_by_key(|(id, _)| id.0)
            .map(|(_, ids)| ids.len() as i32)
            .unwrap_or(0),
        // CR 603.7c: Numeric value from the triggering event.
        QuantityRef::EventContextAmount => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_amount_from_event)
            .unwrap_or(0),
        // CR 603.7c: Power of the source object from the triggering event.
        // CR 400.7: Falls back to LKI cache for objects that have left their zone.
        QuantityRef::EventContextSourcePower => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| {
                state
                    .objects
                    .get(&id)
                    .and_then(|obj| obj.power)
                    .or_else(|| state.lki_cache.get(&id).and_then(|lki| lki.power))
            })
            .unwrap_or(0),
        // CR 603.7c: Toughness of the source object from the triggering event.
        QuantityRef::EventContextSourceToughness => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| {
                state
                    .objects
                    .get(&id)
                    .and_then(|obj| obj.toughness)
                    .or_else(|| state.lki_cache.get(&id).and_then(|lki| lki.toughness))
            })
            .unwrap_or(0),
        // CR 603.7c: Mana value of the source object from the triggering event.
        QuantityRef::EventContextSourceManaValue => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| {
                state
                    .objects
                    .get(&id)
                    .map(|obj| obj.mana_cost.mana_value() as i32)
                    .or_else(|| state.lki_cache.get(&id).map(|lki| lki.mana_value as i32))
            })
            .unwrap_or(0),
        // CR 305.6: Count distinct basic land types among lands the controller controls.
        QuantityRef::BasicLandTypeCount => {
            let basic_subtypes = ["Plains", "Island", "Swamp", "Mountain", "Forest"];
            let mut found = HashSet::new();
            for &id in state.battlefield.iter() {
                if let Some(obj) = state.objects.get(&id) {
                    if obj.controller == controller
                        && obj.card_types.core_types.contains(&CoreType::Land)
                    {
                        for subtype in &basic_subtypes {
                            if obj.card_types.subtypes.iter().any(|s| s == subtype) {
                                found.insert(*subtype);
                            }
                        }
                    }
                }
            }
            found.len() as i32
        }
    }
}

/// Check if an object matches a set of type filters for zone card counting.
/// Empty `card_types` means all cards match.
fn matches_zone_card_filter(
    state: &GameState,
    obj_id: ObjectId,
    card_types: &[TypeFilter],
) -> bool {
    if card_types.is_empty() {
        return true;
    }
    state.objects.get(&obj_id).is_some_and(|obj| {
        card_types.iter().any(|tf| {
            type_filter_to_core_type(tf).is_some_and(|ct| obj.card_types.core_types.contains(&ct))
        })
    })
}

/// Return an iterator over players matching the given `CountScope`.
fn scoped_players<'a>(
    state: &'a GameState,
    scope: &'a CountScope,
    controller: PlayerId,
) -> impl Iterator<Item = &'a crate::types::player::Player> {
    state.players.iter().filter(move |p| match scope {
        CountScope::Controller => p.id == controller,
        CountScope::All => true,
        CountScope::Opponents => p.id != controller,
    })
}

/// Map a `TypeFilter` to its corresponding `CoreType`, if applicable.
/// Only core type filters are valid for zone-based card counting.
fn type_filter_to_core_type(tf: &TypeFilter) -> Option<CoreType> {
    match tf {
        TypeFilter::Creature => Some(CoreType::Creature),
        TypeFilter::Instant => Some(CoreType::Instant),
        TypeFilter::Sorcery => Some(CoreType::Sorcery),
        TypeFilter::Land => Some(CoreType::Land),
        TypeFilter::Artifact => Some(CoreType::Artifact),
        TypeFilter::Enchantment => Some(CoreType::Enchantment),
        TypeFilter::Planeswalker => Some(CoreType::Planeswalker),
        _ => None,
    }
}

/// Count players matching a PlayerFilter relative to the controller.
fn resolve_player_count(state: &GameState, filter: &PlayerFilter, controller: PlayerId) -> i32 {
    state
        .players
        .iter()
        .filter(|p| {
            !p.is_eliminated
                && match filter {
                    PlayerFilter::Opponent => p.id != controller,
                    PlayerFilter::OpponentLostLife => {
                        p.id != controller && p.life_lost_this_turn > 0
                    }
                    PlayerFilter::OpponentGainedLife => {
                        p.id != controller && p.life_gained_this_turn > 0
                    }
                    PlayerFilter::All => true,
                }
        })
        .count() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::CounterType;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, Effect, TargetFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    #[test]
    fn resolve_quantity_fixed_returns_value() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Fixed { value: 3 };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 3);
    }

    #[test]
    fn resolve_quantity_hand_size() {
        let mut state = GameState::new_two_player(42);
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {i}"),
                Zone::Hand,
            );
        }
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::HandSize,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(99)),
            4
        );
    }

    #[test]
    fn resolve_quantity_object_count_creatures_you_control() {
        let mut state = GameState::new_two_player(42);
        // Add 3 creatures for player 0
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Creature {i}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        // Add 1 creature for player 1 (should not count)
        let opp = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        // Source is controlled by player 0
        let source = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 3);
    }

    #[test]
    fn resolve_quantity_player_count_opponent_lost_life() {
        let mut state = GameState::new_two_player(42);
        // Opponent (player 1) lost life this turn
        state.players[1].life_lost_this_turn = 3;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn resolve_quantity_player_count_opponent_lost_life_none_lost() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 0);
    }

    #[test]
    fn resolve_quantity_player_count_opponent() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::Opponent,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn resolve_quantity_counters_on_self() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Planeswalker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .counters
            .insert(CounterType::Loyalty, 4);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CountersOnSelf {
                counter_type: "loyalty".to_string(),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 4);
    }

    #[test]
    fn resolve_quantity_player_filter_opponent_gained_life() {
        let mut state = GameState::new_two_player(42);
        state.players[1].life_gained_this_turn = 5;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentGainedLife,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn resolve_quantity_player_filter_all() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::All,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 2);
    }

    #[test]
    fn half_rounded_up_even() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::HalfRounded {
            inner: Box::new(QuantityExpr::Fixed { value: 20 }),
            rounding: crate::types::ability::RoundingMode::Up,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            10
        );
    }

    #[test]
    fn half_rounded_up_odd() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::HalfRounded {
            inner: Box::new(QuantityExpr::Fixed { value: 7 }),
            rounding: crate::types::ability::RoundingMode::Up,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 4);
    }

    #[test]
    fn half_rounded_down_odd() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::HalfRounded {
            inner: Box::new(QuantityExpr::Fixed { value: 7 }),
            rounding: crate::types::ability::RoundingMode::Down,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 3);
    }

    #[test]
    fn resolve_target_life_total() {
        let state = GameState::new_two_player(42);
        // Player 1 starts at 20 life
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::TargetLifeTotal,
        };
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: expr.clone(),
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(1),
            PlayerId(0),
        );
        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 20);
    }

    #[test]
    fn resolve_self_power() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.power = Some(5);
        obj.toughness = Some(3);
        obj.card_types.core_types.push(CoreType::Creature);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::SelfPower,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 5);

        let expr_t = QuantityExpr::Ref {
            qty: QuantityRef::SelfToughness,
        };
        assert_eq!(resolve_quantity(&state, &expr_t, PlayerId(0), source), 3);
    }

    #[test]
    fn resolve_aggregate_max_power() {
        use crate::types::ability::AggregateFunction;
        use crate::types::ability::ObjectProperty;

        let mut state = GameState::new_two_player(42);
        // Create creatures with power 2, 5, 3
        for (i, pwr) in [2, 5, 3].iter().enumerate() {
            let id = create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                format!("Creature {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(*pwr);
            obj.toughness = Some(1);
            obj.card_types.core_types.push(CoreType::Creature);
        }
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Max,
                property: ObjectProperty::Power,
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 5);
    }

    #[test]
    fn resolve_aggregate_sum_power() {
        use crate::types::ability::AggregateFunction;
        use crate::types::ability::ObjectProperty;

        let mut state = GameState::new_two_player(42);
        for (i, pwr) in [2, 5, 3].iter().enumerate() {
            let id = create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                format!("Creature {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(*pwr);
            obj.toughness = Some(1);
            obj.card_types.core_types.push(CoreType::Creature);
        }
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 10);
    }

    #[test]
    fn resolve_multiply() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Multiply {
            factor: 3,
            inner: Box::new(QuantityExpr::Fixed { value: 4 }),
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            12
        );
    }

    #[test]
    fn resolve_event_context_amount_from_damage() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 5,
            is_combat: false,
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 5);
    }

    #[test]
    fn resolve_event_context_amount_none_returns_zero() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 0);
    }

    #[test]
    fn resolve_event_context_source_power_live_object() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().power = Some(4);
        state.objects.get_mut(&source).unwrap().toughness = Some(3);
        state.current_trigger_event = Some(crate::types::events::GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(PlayerId(1)),
            amount: 4,
            is_combat: true,
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextSourcePower,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(99)),
            4
        );
    }

    #[test]
    fn resolve_event_context_source_power_lki_fallback() {
        use crate::types::game_state::LKISnapshot;
        let mut state = GameState::new_two_player(42);
        let dead_id = ObjectId(42);
        // Object is gone from state.objects but has LKI entry
        state.lki_cache.insert(
            dead_id,
            LKISnapshot {
                power: Some(6),
                toughness: Some(5),
                mana_value: 3,
                controller: PlayerId(0),
                owner: PlayerId(0),
            },
        );
        state.current_trigger_event =
            Some(crate::types::events::GameEvent::CreatureDestroyed { object_id: dead_id });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextSourcePower,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(99)),
            6
        );
    }

    #[test]
    fn lki_cleared_on_advance_phase() {
        use crate::types::game_state::LKISnapshot;
        let mut state = GameState::new_two_player(42);
        state.lki_cache.insert(
            ObjectId(1),
            LKISnapshot {
                power: Some(3),
                toughness: Some(3),
                mana_value: 2,
                controller: PlayerId(0),
                owner: PlayerId(0),
            },
        );
        assert!(!state.lki_cache.is_empty());
        let mut events = Vec::new();
        crate::game::turns::advance_phase(&mut state, &mut events);
        assert!(state.lki_cache.is_empty());
    }

    #[test]
    fn resolve_multiply_negative() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Multiply {
            factor: -1,
            inner: Box::new(QuantityExpr::Fixed { value: 5 }),
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            -5
        );
    }
}
