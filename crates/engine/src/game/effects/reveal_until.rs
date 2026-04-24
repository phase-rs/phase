use rand::seq::SliceRandom;

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::zones;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 701.20a: Reveal cards from the top of the controller's library one at a
/// time until a card matching the filter is found. The matching card goes to
/// `kept_destination`, the remaining revealed cards go to `rest_destination`.
///
/// All revealed cards are marked as publicly revealed and a `CardsRevealed`
/// event is emitted. If the library is exhausted without finding a match, all
/// revealed cards go to `rest_destination`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (player_filter, filter, kept_destination, rest_destination, enter_tapped) =
        match &ability.effect {
            Effect::RevealUntil {
                player,
                filter,
                kept_destination,
                rest_destination,
                enter_tapped,
            } => (
                player,
                filter,
                *kept_destination,
                *rest_destination,
                *enter_tapped,
            ),
            _ => return Err(EffectError::MissingParam("RevealUntil".to_string())),
        };

    // CR 109.5 + CR 701.20a: Resolve which player's library is revealed.
    // `Controller` → activator (Jalira-style "you reveal..."); `ParentTargetController`
    // → controller of the parent ability's targeted object (Polymorph, Proteus Staff,
    // Transmogrify); other player-resolving filters → player extracted from
    // `ability.targets` (e.g., Telemin Performance "target opponent reveals...").
    let revealing_player = resolve_revealing_player(state, ability, player_filter);

    let player = state
        .players
        .iter()
        .find(|p| p.id == revealing_player)
        .ok_or(EffectError::PlayerNotFound)?;

    // Snapshot library (top = index 0) to iterate without borrow conflicts.
    let library: Vec<ObjectId> = player.library.iter().copied().collect();
    let mut revealed_misses: Vec<ObjectId> = Vec::new();
    let mut hit_card: Option<ObjectId> = None;

    // CR 107.3a + CR 601.2b: Evaluate the filter with the ability in scope so
    // dynamic thresholds (e.g. `Variable("X")`) resolve correctly.
    let ctx = FilterContext::from_ability(ability);

    // CR 701.20a: Reveal cards one at a time.
    for &card_id in &library {
        // Mark as revealed (CR 701.20b: card stays in library zone during reveal).
        state.revealed_cards.insert(card_id);

        if matches_target_filter(state, card_id, filter, &ctx) {
            hit_card = Some(card_id);
            break;
        } else {
            revealed_misses.push(card_id);
        }
    }

    // Build the full list of revealed card IDs for the event.
    let mut all_revealed: Vec<ObjectId> = revealed_misses.clone();
    if let Some(hit) = hit_card {
        all_revealed.push(hit);
    }

    // Emit CardsRevealed for all revealed cards.
    let card_names: Vec<String> = all_revealed
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect();
    events.push(GameEvent::CardsRevealed {
        player: revealing_player,
        card_ids: all_revealed.clone(),
        card_names,
    });

    // Store revealed IDs for downstream reference.
    state.last_revealed_ids = all_revealed;

    // Move the matching card to its destination.
    if let Some(hit) = hit_card {
        match kept_destination {
            Zone::Hand => {
                zones::move_to_zone(state, hit, Zone::Hand, events);
            }
            Zone::Battlefield => {
                zones::move_to_zone(state, hit, Zone::Battlefield, events);
                if enter_tapped {
                    if let Some(obj) = state.objects.get_mut(&hit) {
                        obj.tapped = true;
                    }
                }
            }
            other => {
                zones::move_to_zone(state, hit, other, events);
            }
        }
    }

    // Move remaining revealed cards to rest_destination.
    match rest_destination {
        Zone::Library => {
            // "on the bottom of your library in a random order"
            shuffle_to_bottom(state, &revealed_misses, events);
        }
        Zone::Graveyard => {
            for &card_id in &revealed_misses {
                zones::move_to_zone(state, card_id, Zone::Graveyard, events);
            }
        }
        other => {
            for &card_id in &revealed_misses {
                zones::move_to_zone(state, card_id, other, events);
            }
        }
    }

    // Clear reveal markers — cards have moved zones.
    for &card_id in &revealed_misses {
        state.revealed_cards.remove(&card_id);
    }
    if let Some(hit) = hit_card {
        state.revealed_cards.remove(&hit);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RevealUntil,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 109.5: Resolve the `player` filter on a [`RevealUntil`] effect into a
/// concrete [`PlayerId`]. Mirrors [`crate::game::effects::token::resolve_token_owner`]:
/// `Controller` → activator; `ParentTargetController` → controller of the parent
/// ability's targeted object (Polymorph, Proteus Staff, Transmogrify); any other
/// player-resolving filter → `TargetRef::Player` extracted from `ability.targets`
/// (Telemin Performance / Mind Funeral "target opponent reveals..."). Falls
/// back to the activator when the filter cannot be resolved (defensive default
/// matching the historical behavior of this effect).
fn resolve_revealing_player(
    state: &GameState,
    ability: &ResolvedAbility,
    player_filter: &TargetFilter,
) -> PlayerId {
    match player_filter {
        TargetFilter::Controller => ability.controller,
        TargetFilter::ParentTargetController => ability
            .targets
            .iter()
            .find_map(|target| match target {
                TargetRef::Object(id) => state.objects.get(id).map(|obj| obj.controller),
                TargetRef::Player(pid) => Some(*pid),
            })
            .unwrap_or(ability.controller),
        _ => ability
            .targets
            .iter()
            .find_map(|target| match target {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(id) => state.objects.get(id).map(|obj| obj.controller),
            })
            .unwrap_or(ability.controller),
    }
}

/// Put cards on the bottom of the player's library in random order.
fn shuffle_to_bottom(state: &mut GameState, cards: &[ObjectId], events: &mut Vec<GameEvent>) {
    let mut shuffled = cards.to_vec();
    shuffled.shuffle(&mut state.rng);

    for &card_id in &shuffled {
        zones::move_to_library_position(state, card_id, false, events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_reveal_until_ability(
        controller: PlayerId,
        filter: TargetFilter,
        kept_destination: Zone,
        rest_destination: Zone,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RevealUntil {
                player: TargetFilter::Controller,
                filter,
                kept_destination,
                rest_destination,
                enter_tapped: false,
            },
            vec![],
            ObjectId(100),
            controller,
        )
    }

    fn make_reveal_until_ability_with_player(
        controller: PlayerId,
        player: TargetFilter,
        targets: Vec<TargetRef>,
        filter: TargetFilter,
        kept_destination: Zone,
        rest_destination: Zone,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RevealUntil {
                player,
                filter,
                kept_destination,
                rest_destination,
                enter_tapped: false,
            },
            targets,
            ObjectId(100),
            controller,
        )
    }

    #[test]
    fn reveal_until_finds_creature_puts_to_hand() {
        let mut state = GameState::new_two_player(42);

        // Library: land, land, creature (top to bottom by creation order)
        let land1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let land2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature should be in hand
        assert!(state.players[0].hand.contains(&creature));
        // Lands should be on bottom of library
        assert!(state.players[0].library.contains(&land1));
        assert!(state.players[0].library.contains(&land2));
        // CardsRevealed event should include all three
        let revealed = events.iter().find_map(|e| match e {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        });
        assert_eq!(revealed.unwrap().len(), 3);
    }

    #[test]
    fn reveal_until_puts_to_battlefield() {
        let mut state = GameState::new_two_player(42);

        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Battlefield,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature should be on the battlefield
        assert!(state.battlefield.contains(&creature));
    }

    #[test]
    fn reveal_until_rest_to_graveyard() {
        let mut state = GameState::new_two_player(42);

        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Graveyard,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature in hand, land in graveyard
        assert!(state.players[0].hand.contains(&creature));
        assert!(state.players[0].graveyard.contains(&land));
    }

    #[test]
    fn reveal_until_no_match_all_to_rest() {
        let mut state = GameState::new_two_player(42);

        let land1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let land2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No creature found — all cards go to bottom of library
        assert!(state.players[0].hand.is_empty());
        assert_eq!(state.players[0].library.len(), 2);
    }

    #[test]
    fn reveal_until_empty_library() {
        let mut state = GameState::new_two_player(42);

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No crash, effect resolves cleanly
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    /// CR 109.5 + CR 701.20a: When `player = ParentTargetController`, the library
    /// of the parent ability's target's controller is revealed — the activator's
    /// own library is left untouched. This is the Polymorph / Proteus Staff /
    /// Transmogrify pattern.
    #[test]
    fn reveal_until_parent_target_controller_reveals_target_owner_library() {
        let mut state = GameState::new_two_player(42);

        // Activator is PlayerId(0); the targeted creature (and its library) belongs
        // to PlayerId(1). The activator's library must NOT be touched.
        let opponent_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opponent_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Opponent's library: a land then a creature (top→bottom).
        let opp_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&opp_land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        let opp_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Bear2".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&opp_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Activator's library: a creature on top — must NOT be touched.
        let activator_creature = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "ActivatorBear".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&activator_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = make_reveal_until_ability_with_player(
            PlayerId(0),
            TargetFilter::ParentTargetController,
            vec![TargetRef::Object(opponent_creature)],
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Battlefield,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Opponent's creature card moved to the battlefield (under its owner's control).
        assert!(state.battlefield.contains(&opp_creature));
        assert_eq!(
            state.objects.get(&opp_creature).unwrap().controller,
            PlayerId(1)
        );
        // Activator's library is undisturbed — their bear is still on top.
        assert_eq!(
            state.players[0].library.front().copied(),
            Some(activator_creature)
        );
        // The CardsRevealed event names the revealing player (the opponent), not the activator.
        let revealing_player = events.iter().find_map(|e| match e {
            GameEvent::CardsRevealed { player, .. } => Some(*player),
            _ => None,
        });
        assert_eq!(revealing_player, Some(PlayerId(1)));
    }
}
