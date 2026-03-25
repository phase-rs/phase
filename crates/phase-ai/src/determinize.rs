use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;

use engine::game::players;
use engine::game::printed_cards::apply_card_face_to_object;
use engine::types::card::CardFace;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::planner::InformationSetSampler;

/// Implements Information Set Monte Carlo Tree Search (IS-MCTS) determinization.
///
/// For each opponent of the planning player, this replaces the unknown portions
/// of their hand and library with a random permutation drawn from their deck pool,
/// minus any cards already visible in public zones (battlefield, graveyard, exile, stack).
/// The player's own zones are untouched.
pub struct RandomizedDeterminizer {
    rng: SmallRng,
}

impl RandomizedDeterminizer {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: SmallRng::seed_from_u64(seed),
        }
    }
}

impl InformationSetSampler for RandomizedDeterminizer {
    fn determinize(&mut self, visible_state: &GameState, player: PlayerId) -> GameState {
        let mut state = visible_state.clone();
        let opponents = players::opponents(&state, player);

        for opponent in opponents {
            determinize_opponent(&mut state, opponent, &mut self.rng);
        }

        state
    }
}

/// Rebuild an opponent's hand and library by sampling from their unknown card pool.
///
/// Public zones (battlefield, graveyard, exile, stack) reveal which cards are already
/// accounted for. The remaining cards in the deck pool are shuffled and distributed:
/// first `hand_size` cards go to hand, the rest go to library (also shuffled).
fn determinize_opponent(state: &mut GameState, opponent: PlayerId, rng: &mut SmallRng) {
    let Some(pool) = state
        .deck_pools
        .iter()
        .find(|p| p.player == opponent)
        .cloned()
    else {
        // No deck pool registered for this opponent — nothing to determinize.
        return;
    };

    let hand_size = state.players[opponent.0 as usize].hand.len();
    let library_size = state.players[opponent.0 as usize].library.len();

    // Count cards visible in public zones belonging to this opponent.
    let known_counts = collect_public_zone_names(state, opponent);

    // Build the unknown pool: deck card faces minus publicly-visible copies.
    let mut unknown_pool = build_unknown_pool(&pool.current_main, &known_counts);
    unknown_pool.shuffle(rng);

    // Distribute: first hand_size faces → hand, remainder → library (capped to library_size).
    let hand_faces: Vec<CardFace> = unknown_pool.iter().take(hand_size).cloned().collect();
    let library_faces: Vec<CardFace> = unknown_pool
        .into_iter()
        .skip(hand_size)
        .take(library_size)
        .collect();

    // Replace hand objects with new objects carrying full card data.
    let old_hand: Vec<ObjectId> = state.players[opponent.0 as usize].hand.clone();
    let old_library: Vec<ObjectId> = state.players[opponent.0 as usize].library.clone();

    // Remove old hand and library objects from the object map.
    for &id in old_hand.iter().chain(old_library.iter()) {
        state.objects.remove(&id);
    }
    state.players[opponent.0 as usize].hand.clear();
    state.players[opponent.0 as usize].library.clear();

    // Create new hand objects with full card data.
    for face in hand_faces {
        let id = allocate_object(state, opponent, &face, Zone::Hand);
        state.players[opponent.0 as usize].hand.push(id);
    }

    // Create new library objects (already in a random order from the shuffle).
    for face in library_faces {
        let id = allocate_object(state, opponent, &face, Zone::Library);
        state.players[opponent.0 as usize].library.push(id);
    }
}

/// Allocate a fully-populated GameObject from a CardFace, without touching zone lists
/// (we manage those manually). The object gets card types, mana cost, keywords, abilities,
/// and all other fields from the card face — making it functionally identical to a real card
/// for search evaluation purposes.
fn allocate_object(
    state: &mut GameState,
    owner: PlayerId,
    card_face: &CardFace,
    zone: Zone,
) -> ObjectId {
    let id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let card_id = CardId(id.0);
    let mut obj = engine::game::game_object::GameObject::new(
        id,
        card_id,
        owner,
        card_face.name.clone(),
        zone,
    );
    apply_card_face_to_object(&mut obj, card_face);
    state.objects.insert(id, obj);
    id
}

/// Collect the name of each non-token card in public zones belonging to `opponent`.
///
/// Returns a map of `name → count` for cards in battlefield, graveyard, exile, and stack.
fn collect_public_zone_names(
    state: &GameState,
    opponent: PlayerId,
) -> std::collections::HashMap<String, u32> {
    let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    // Battlefield and exile are stored at the top-level state.
    let public_ids: Vec<ObjectId> = state
        .battlefield
        .iter()
        .chain(state.exile.iter())
        .copied()
        .collect();

    // Graveyard is per-player.
    let graveyard_ids = state.players[opponent.0 as usize].graveyard.clone();

    // Stack entries: source_id is the object on the stack for spells/abilities.
    let stack_ids: Vec<ObjectId> = state.stack.iter().map(|entry| entry.source_id).collect();

    for &id in public_ids
        .iter()
        .chain(graveyard_ids.iter())
        .chain(stack_ids.iter())
    {
        let Some(obj) = state.objects.get(&id) else {
            continue;
        };
        if obj.owner != opponent || obj.is_token {
            continue;
        }
        *counts.entry(obj.name.clone()).or_insert(0) += 1;
    }

    counts
}

/// Build the pool of card faces not yet accounted for by public-zone knowledge.
///
/// For each entry in `deck`, subtract the number of copies already known visible.
/// Returns a flat `Vec<CardFace>` with one entry per unknown copy, carrying full card data.
fn build_unknown_pool(
    deck: &[engine::game::deck_loading::DeckEntry],
    known_counts: &std::collections::HashMap<String, u32>,
) -> Vec<CardFace> {
    let mut pool = Vec::new();
    for entry in deck {
        let known = known_counts.get(&entry.card.name).copied().unwrap_or(0);
        let unknown = entry.count.saturating_sub(known);
        for _ in 0..unknown {
            pool.push(entry.card.clone());
        }
    }
    pool
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;

    fn make_state_with_opponent_cards(hand_count: usize, library_count: usize) -> GameState {
        let mut state = GameState::new_two_player(42);

        // Clear any existing objects for player 1 (opponent).
        let opponent = PlayerId(1);
        let old_hand: Vec<ObjectId> = state.players[opponent.0 as usize].hand.clone();
        let old_library: Vec<ObjectId> = state.players[opponent.0 as usize].library.clone();
        for id in old_hand.iter().chain(old_library.iter()) {
            state.objects.remove(id);
        }
        state.players[opponent.0 as usize].hand.clear();
        state.players[opponent.0 as usize].library.clear();

        // Add synthetic hand cards. create_object already calls add_to_zone.
        for i in 0..hand_count {
            create_object(
                &mut state,
                CardId(100 + i as u64),
                opponent,
                format!("Card {i}"),
                Zone::Hand,
            );
        }

        // Add synthetic library cards. create_object already calls add_to_zone.
        for i in 0..library_count {
            create_object(
                &mut state,
                CardId(200 + i as u64),
                opponent,
                format!("Card {i}"),
                Zone::Library,
            );
        }

        // Provide a deck pool for the opponent with enough entries.
        let total = hand_count + library_count;
        let mut entries = Vec::new();
        for i in 0..total {
            entries.push(engine::game::deck_loading::DeckEntry {
                card: engine::types::card::CardFace {
                    name: format!("Card {i}"),
                    ..Default::default()
                },
                count: 1,
            });
        }
        state
            .deck_pools
            .push(engine::types::game_state::PlayerDeckPool {
                player: opponent,
                current_main: entries,
                ..Default::default()
            });

        state
    }

    #[test]
    fn determinized_state_preserves_hand_size() {
        let state = make_state_with_opponent_cards(3, 10);
        let opponent = PlayerId(1);
        let original_hand_size = state.players[opponent.0 as usize].hand.len();

        let mut determinizer = RandomizedDeterminizer::new(42);
        let det = determinizer.determinize(&state, PlayerId(0));

        assert_eq!(
            det.players[opponent.0 as usize].hand.len(),
            original_hand_size,
            "determinized hand size must match original"
        );
    }

    #[test]
    fn determinized_state_preserves_library_size() {
        let state = make_state_with_opponent_cards(3, 10);
        let opponent = PlayerId(1);
        let original_library_size = state.players[opponent.0 as usize].library.len();

        let mut determinizer = RandomizedDeterminizer::new(42);
        let det = determinizer.determinize(&state, PlayerId(0));

        assert_eq!(
            det.players[opponent.0 as usize].library.len(),
            original_library_size,
            "determinized library size must match original"
        );
    }

    #[test]
    fn different_seeds_produce_different_hands() {
        let state = make_state_with_opponent_cards(3, 10);
        let opponent = PlayerId(1);

        let mut det_a = RandomizedDeterminizer::new(1);
        let mut det_b = RandomizedDeterminizer::new(99999);

        let state_a = det_a.determinize(&state, PlayerId(0));
        let state_b = det_b.determinize(&state, PlayerId(0));

        // Collect names from each determinization's hand.
        let hand_names_a: Vec<String> = state_a.players[opponent.0 as usize]
            .hand
            .iter()
            .filter_map(|id| state_a.objects.get(id).map(|o| o.name.clone()))
            .collect();
        let hand_names_b: Vec<String> = state_b.players[opponent.0 as usize]
            .hand
            .iter()
            .filter_map(|id| state_b.objects.get(id).map(|o| o.name.clone()))
            .collect();

        // With 13 distinct cards dealt to 3 + 10 slots, two independent shuffles
        // should produce different orderings with overwhelming probability.
        assert_ne!(
            hand_names_a, hand_names_b,
            "different seeds should produce different hands"
        );
    }

    #[test]
    fn known_battlefield_cards_stay_fixed() {
        let mut state = make_state_with_opponent_cards(3, 7);
        let opponent = PlayerId(1);

        // Put one card onto the battlefield — it should be excluded from the unknown pool.
        // create_object calls add_to_zone which pushes to state.battlefield automatically.
        let bf_id = create_object(
            &mut state,
            CardId(999),
            opponent,
            "Card 0".to_string(),
            Zone::Battlefield,
        );

        let mut determinizer = RandomizedDeterminizer::new(42);
        let det = determinizer.determinize(&state, PlayerId(0));

        // The battlefield object must be unchanged.
        assert!(
            det.battlefield.contains(&bf_id),
            "battlefield card must remain after determinization"
        );
        assert_eq!(
            det.objects[&bf_id].name, "Card 0",
            "battlefield card name must be unchanged"
        );

        // Hand size is preserved. Library shrinks by 1 because "Card 0" is known on the
        // battlefield and subtracted from the unknown pool (10 deck entries - 1 known = 9
        // unknowns; 3 go to hand → 6 remain for library).
        assert_eq!(det.players[opponent.0 as usize].hand.len(), 3);
        assert_eq!(det.players[opponent.0 as usize].library.len(), 6);
    }
}
