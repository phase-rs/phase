use std::collections::{HashMap, HashSet};

use engine::game::deck_loading::DeckEntry;
use engine::game::printed_cards::printed_ref_from_face;
use engine::types::card::{CardFace, PrintedCardRef};
use engine::types::game_state::{GameState, PlayerDeckPool, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeckCardKey {
    Printed {
        oracle_id: String,
        face_name: String,
    },
    FaceName(String),
}

#[derive(Debug, Clone, Default)]
pub struct RemainingDeckView {
    pub counts: HashMap<DeckCardKey, u32>,
    pub entries: Vec<DeckEntry>,
}

/// The seat holding the pool that backs `seat`'s deck (the seat itself unless the library is
/// shared).
pub(crate) fn pool_holder(state: &GameState, seat: PlayerId) -> Option<PlayerId> {
    state.deck_pool_of(seat).map(|pool| pool.player)
}

/// Every seat whose deck is the pool held by `holder`.
pub(crate) fn pool_seats(
    state: &GameState,
    holder: PlayerId,
) -> impl Iterator<Item = PlayerId> + '_ {
    state
        .players
        .iter()
        .map(|player| player.id)
        .filter(move |&seat| pool_holder(state, seat) == Some(holder))
}

pub fn known_remaining_deck_counts(
    state: &GameState,
    player: PlayerId,
) -> HashMap<DeckCardKey, u32> {
    let Some(pool) = state.deck_pool_of(player) else {
        return HashMap::new();
    };

    let mut counts = HashMap::new();
    for entry in pool.current_main.iter() {
        if entry.count == 0 {
            continue;
        }
        counts.insert(deck_entry_key(entry), entry.count);
    }

    for object_id in accounted_object_ids(state, player, pool) {
        let Some(object) = state.objects.get(&object_id) else {
            continue;
        };
        if object.is_token || pool_holder(state, object.owner) != Some(pool.player) {
            continue;
        }

        let key = object_key(object.printed_ref.as_ref(), &object.name);
        let Some(count) = counts.get_mut(&key) else {
            continue;
        };
        *count = count.saturating_sub(1);
    }

    counts.retain(|_, count| *count > 0);
    counts
}

pub fn remaining_deck_view(state: &GameState, player: PlayerId) -> RemainingDeckView {
    let counts = known_remaining_deck_counts(state, player);
    let Some(pool) = state.deck_pool_of(player) else {
        return RemainingDeckView {
            counts,
            entries: Vec::new(),
        };
    };

    let entries = pool
        .current_main
        .iter()
        .filter_map(|entry| {
            let key = deck_entry_key(entry);
            let count = counts.get(&key).copied().unwrap_or(0);
            (count > 0).then(|| DeckEntry {
                card: entry.card.clone(),
                count,
            })
        })
        .collect();

    RemainingDeckView { counts, entries }
}

/// CR 400.2: the object ids in `player`'s **public** zones (graveyard,
/// owned-battlefield, owned-exile, stack spells) whose identity every player
/// can see. Deliberately EXCLUDES the hidden hand and library. Splitting this
/// out lets `accounted_object_ids` keep its "public + my own hand" behavior
/// (correct for counting the cards remaining in *my* library) while
/// `unknown_hidden_pool` reuses the public-only account to build the pool an
/// opponent's unknown hidden slots draw from.
fn public_account_object_ids(
    state: &GameState,
    player: PlayerId,
    pool: &PlayerDeckPool,
) -> Vec<ObjectId> {
    let owned_by_pool = |object_id: &&ObjectId| {
        state
            .objects
            .get(object_id)
            .is_some_and(|object| pool_holder(state, object.owner) == Some(pool.player))
    };
    let mut object_ids = Vec::new();

    // CR 400.3: a card goes to its owner's corresponding zone, so ownership does not
    // say where it sits: the graveyard is read once, as `graveyard_of` resolves it.
    object_ids.extend(state.graveyard_of(player).iter().copied());
    object_ids.extend(state.battlefield.iter().filter(owned_by_pool).copied());
    object_ids.extend(state.exile.iter().filter(owned_by_pool).copied());
    object_ids.extend(state.stack.iter().filter_map(|entry| match &entry.kind {
        StackEntryKind::Spell { .. } => Some(entry.source_id),
        // CR 113.3b: Activated abilities (including KeywordAction) are not card
        // sources for deck knowledge — only spells expose their card source.
        StackEntryKind::ActivatedAbility { .. }
        | StackEntryKind::TriggeredAbility { .. }
        | StackEntryKind::KeywordAction { .. }
        // Combat damage on the stack has no card source at all — it is not a
        // card, and its `source_id` is its own entry id.
        | StackEntryKind::CombatDamage { .. } => None,
    }));

    object_ids
}

/// Public-zone objects plus `player`'s own hand — the "cards accounted for
/// outside `player`'s library" set. Behavior-preserving wrapper over
/// `public_account_object_ids`: `known_remaining_deck_counts` (and through it
/// `remaining_deck_view` → tutor/threat_profile) sees exactly the ids it saw
/// before the public/hidden split.
fn accounted_object_ids(
    state: &GameState,
    player: PlayerId,
    pool: &PlayerDeckPool,
) -> Vec<ObjectId> {
    let mut object_ids = public_account_object_ids(state, player, pool);
    object_ids.extend(state.players[player.0 as usize].hand.iter().copied());
    object_ids
}

/// CR 400.2: hand and library are hidden zones. From an observer's perspective
/// an opponent's hidden cards are unknown EXCEPT those the engine has revealed
/// (pinned via `known_ids`). Returns the ordered multiset (decklist order) of
/// card faces that could occupy `player`'s unknown hidden-zone slots — i.e.
/// `decklist − public-zone cards − pinned-known hidden cards`.
///
/// Both hand and library slots draw from this ONE pool: from the observer's
/// perspective, which unknown card sits in the hand vs. the library is itself
/// unknown (CR 401.2), so they redistribute together. Pool order is the
/// deterministic decklist order (the caller seeded-shuffles it — §8 #4878
/// discipline); order-insensitive counting uses a `HashMap` but the expansion
/// back to a `Vec` walks `current_main` in order.
pub fn unknown_hidden_pool(
    state: &GameState,
    player: PlayerId,
    known_ids: &HashSet<ObjectId>,
) -> Vec<CardFace> {
    let Some(pool) = state.deck_pool_of(player) else {
        return Vec::new();
    };

    // Working multiset of remaining deck cards keyed by DeckCardKey.
    let mut counts: HashMap<DeckCardKey, u32> = HashMap::new();
    for entry in pool.current_main.iter() {
        if entry.count == 0 {
            continue;
        }
        *counts.entry(deck_entry_key(entry)).or_insert(0) += entry.count;
    }

    // Decrement one per public-zone object and per pinned-known hidden object.
    // The owner/token guards below make `known_ids` from any zone/player safe
    // to pass wholesale, and `seen` dedups the overlap: a one-shot-revealed
    // card (`public_revealed_cards` never clears) that later reaches a public
    // zone appears in BOTH iterators but must subtract only once.
    //
    // CR 401.2 + CR 400.2: the other pool seats' hands are in neither of `player`'s slot sets, so
    // their cards leave the pool too (a shared pile has exactly two seats, so that seat is the
    // observer; a third would need an observer parameter).
    let co_seat_hands = pool_seats(state, pool.player)
        .filter(|&seat| seat != player)
        .flat_map(|seat| state.players[seat.0 as usize].hand.iter().copied());
    let mut seen: HashSet<ObjectId> = HashSet::new();
    for object_id in public_account_object_ids(state, player, pool)
        .into_iter()
        .chain(co_seat_hands)
        .chain(known_ids.iter().copied())
    {
        if !seen.insert(object_id) {
            continue;
        }
        let Some(object) = state.objects.get(&object_id) else {
            continue;
        };
        if object.is_token || pool_holder(state, object.owner) != Some(pool.player) {
            continue;
        }
        let key = object_key(object.printed_ref.as_ref(), &object.name);
        if let Some(count) = counts.get_mut(&key) {
            *count = count.saturating_sub(1);
        }
    }

    // Expand back to card faces in decklist order (deterministic pre-shuffle).
    // Zero a key after emitting so two entries sharing a key can't double-emit.
    let mut faces = Vec::new();
    for entry in pool.current_main.iter() {
        if entry.count == 0 {
            continue;
        }
        let key = deck_entry_key(entry);
        let remaining = counts.get(&key).copied().unwrap_or(0);
        for _ in 0..remaining {
            faces.push(entry.card.clone());
        }
        counts.insert(key, 0);
    }
    faces
}

fn deck_entry_key(entry: &DeckEntry) -> DeckCardKey {
    object_key(
        printed_ref_from_face(&entry.card).as_ref(),
        &entry.card.name,
    )
}

fn object_key(printed_ref: Option<&PrintedCardRef>, face_name: &str) -> DeckCardKey {
    printed_ref
        .map(|printed_ref| DeckCardKey::Printed {
            oracle_id: printed_ref.oracle_id.clone(),
            face_name: printed_ref.face_name.clone(),
        })
        .unwrap_or_else(|| DeckCardKey::FaceName(face_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::card::CardFace;
    use engine::types::game_state::{CastingVariant, PlayerDeckPool, StackEntry};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;

    fn deck_entry(name: &str, count: u32, oracle_id: Option<&str>) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                scryfall_oracle_id: oracle_id.map(str::to_string),
                mana_cost: ManaCost::zero(),
                ..Default::default()
            },
            count,
        }
    }

    #[test]
    fn subtracts_accounted_non_token_cards() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: std::sync::Arc::new(vec![
                deck_entry("Alpha", 2, Some("a")),
                deck_entry("Beta", 1, None),
            ]),
            ..Default::default()
        });

        let alpha = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Alpha".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&alpha).unwrap().printed_ref = Some(PrintedCardRef {
            oracle_id: "a".to_string(),
            face_name: "Alpha".to_string(),
        });

        let counts = known_remaining_deck_counts(&state, PlayerId(0));
        assert_eq!(
            counts[&DeckCardKey::Printed {
                oracle_id: "a".to_string(),
                face_name: "Alpha".to_string(),
            }],
            1
        );
        assert_eq!(counts[&DeckCardKey::FaceName("Beta".to_string())], 1);
    }

    #[test]
    fn ignores_tokens_and_non_spell_stack_entries() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: std::sync::Arc::new(vec![deck_entry("Alpha", 1, None)]),
            ..Default::default()
        });

        let token = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Alpha".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&token).unwrap().is_token = true;

        state.stack.push_back(StackEntry {
            id: ObjectId(50),
            source_id: token,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: token,
                ability: Box::new(engine::types::ability::ResolvedAbility::new(
                    engine::types::ability::Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: engine::types::ability::TargetFilter::Controller,
                    },
                    Vec::new(),
                    token,
                    PlayerId(0),
                )),
            },
        });

        let counts = known_remaining_deck_counts(&state, PlayerId(0));
        assert_eq!(counts[&DeckCardKey::FaceName("Alpha".to_string())], 1);
    }

    #[test]
    fn subtracts_only_spell_stack_entries() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: std::sync::Arc::new(vec![deck_entry("Alpha", 1, None)]),
            ..Default::default()
        });

        let spell = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Alpha".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: ObjectId(60),
            source_id: spell,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(30),
                ability: Some(Box::new(engine::types::ability::ResolvedAbility::new(
                    engine::types::ability::Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: engine::types::ability::TargetFilter::Controller,
                    },
                    Vec::new(),
                    spell,
                    PlayerId(0),
                ))),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let counts = known_remaining_deck_counts(&state, PlayerId(0));
        assert!(!counts.contains_key(&DeckCardKey::FaceName("Alpha".to_string())));
    }

    #[test]
    fn pool_decrements_once_for_public_object_also_in_known_ids() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: std::sync::Arc::new(vec![deck_entry("Alpha", 2, None)]),
            ..Default::default()
        });

        // A card revealed while hidden (one-shot reveals never clear) that has
        // since moved to the public graveyard: present in BOTH the public
        // account and `known_ids`. It must subtract exactly once, leaving one
        // Alpha in the unknown pool.
        let alpha = create_object(
            &mut state,
            CardId(40),
            PlayerId(0),
            "Alpha".to_string(),
            Zone::Graveyard,
        );
        let known: HashSet<ObjectId> = [alpha].into_iter().collect();

        let pool = unknown_hidden_pool(&state, PlayerId(0), &known);
        assert_eq!(
            pool.iter().filter(|face| face.name == "Alpha").count(),
            1,
            "overlapping public + known object must decrement the pool once, not twice"
        );
    }

    fn dandan_state_with_pool(entries: Vec<DeckEntry>) -> GameState {
        let mut state = GameState::new(engine::types::format::FormatConfig::dandan(), 2, 42);
        state.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: std::sync::Arc::new(entries),
            ..Default::default()
        });
        assert_eq!(
            state.deck_pool_of(PlayerId(1)).map(|pool| pool.player),
            Some(PlayerId(0)),
            "reach: the one pool backs both seats"
        );
        state
    }

    fn place(state: &mut GameState, id: u64, owner: u8, name: &str, zone: Zone) -> ObjectId {
        create_object(state, CardId(id), PlayerId(owner), name.to_string(), zone)
    }

    fn named_counts(counts: &HashMap<DeckCardKey, u32>) -> Vec<(String, u32)> {
        let mut named: Vec<_> = counts
            .iter()
            .map(|(key, count)| match key {
                DeckCardKey::FaceName(name) => (name.clone(), *count),
                DeckCardKey::Printed { face_name, .. } => (face_name.clone(), *count),
            })
            .collect();
        named.sort();
        named
    }

    fn face_names(faces: &[CardFace]) -> Vec<String> {
        let mut names: Vec<_> = faces.iter().map(|face| face.name.clone()).collect();
        names.sort();
        names
    }

    #[test]
    fn shared_pool_accounts_for_every_seats_cards_and_the_shared_graveyard() {
        let mut state = dandan_state_with_pool(vec![
            deck_entry("Alpha", 3, None),
            deck_entry("Beta", 1, None),
        ]);
        place(&mut state, 10, 1, "Alpha", Zone::Battlefield);
        place(&mut state, 11, 0, "Alpha", Zone::Graveyard);
        place(&mut state, 12, 1, "Alpha", Zone::Hand);
        assert!(
            state.players[1].graveyard.is_empty() && state.graveyard_of(PlayerId(1)).len() == 1,
            "reach: the graveyard is shared"
        );

        let p1 = known_remaining_deck_counts(&state, PlayerId(1));
        assert_eq!(named_counts(&p1), vec![("Beta".to_string(), 1)]);
        let p0 = known_remaining_deck_counts(&state, PlayerId(0));
        assert_eq!(
            named_counts(&p0),
            vec![("Alpha".to_string(), 1), ("Beta".to_string(), 1)]
        );
        let unknown = unknown_hidden_pool(&state, PlayerId(1), &HashSet::new());
        assert_eq!(face_names(&unknown), vec!["Alpha", "Beta"]);
    }

    #[test]
    fn shared_graveyard_card_also_pinned_known_subtracts_once() {
        let mut state = dandan_state_with_pool(vec![deck_entry("Alpha", 2, None)]);
        let alpha = place(&mut state, 10, 1, "Alpha", Zone::Graveyard);
        let known: HashSet<ObjectId> = [alpha].into_iter().collect();
        let unknown = unknown_hidden_pool(&state, PlayerId(1), &known);
        assert_eq!(face_names(&unknown), vec!["Alpha"]);
    }

    #[test]
    fn separate_pools_subtract_only_their_own_seats_cards() {
        let mut state = GameState::new_two_player(42);
        for seat in 0..2 {
            state.deck_pools.push(PlayerDeckPool {
                player: PlayerId(seat),
                current_main: std::sync::Arc::new(vec![deck_entry("Alpha", 2, None)]),
                ..Default::default()
            });
        }
        place(&mut state, 10, 1, "Alpha", Zone::Battlefield);
        let p0 = known_remaining_deck_counts(&state, PlayerId(0));
        let p1 = known_remaining_deck_counts(&state, PlayerId(1));
        assert_eq!(named_counts(&p0), vec![("Alpha".to_string(), 2)]);
        assert_eq!(named_counts(&p1), vec![("Alpha".to_string(), 1)]);
    }

    fn eight_name_pile_state() -> (GameState, Vec<String>) {
        let names: Vec<String> = (0..8).map(|i| format!("Card {i}")).collect();
        let mut state =
            dandan_state_with_pool(names.iter().map(|name| deck_entry(name, 1, None)).collect());
        place(&mut state, 100, 0, &names[0], Zone::Hand);
        place(&mut state, 101, 0, &names[1], Zone::Hand);
        place(&mut state, 102, 1, &names[2], Zone::Hand);
        for (i, name) in names.iter().enumerate().skip(3) {
            place(&mut state, 110 + i as u64, 0, name, Zone::Library);
        }
        assert_eq!(state.library_of(PlayerId(1)).len(), 5, "reach: the pile");
        (state, names)
    }

    #[test]
    fn co_seat_hand_leaves_the_unknown_pool() {
        let (state, names) = eight_name_pile_state();
        let unknown = unknown_hidden_pool(&state, PlayerId(1), &HashSet::new());
        let expected: Vec<_> = names[2..].to_vec();
        assert_eq!(face_names(&unknown), expected);
        assert_eq!(
            unknown.len(),
            state.players[1].hand.len() + state.library_of(PlayerId(1)).len(),
            "the pool fills exactly the hand and pile slots"
        );
    }

    #[test]
    fn determinized_opponent_slots_never_receive_the_observers_hand_names() {
        use rand::SeedableRng;
        let (state, names) = eight_name_pile_state();
        for seed in 0..32 {
            let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(seed);
            let sim = crate::determinize::determinize_opponents(&state, PlayerId(0), &mut rng);
            let slot_names: Vec<String> = sim.players[1]
                .hand
                .iter()
                .chain(sim.library_of(PlayerId(1)).iter())
                .map(|id| sim.objects[id].name.clone())
                .collect();
            assert_eq!(slot_names.len(), 6, "reach: every unknown slot is filled");
            assert!(
                !slot_names.contains(&names[0]) && !slot_names.contains(&names[1]),
                "seed {seed}: P0's own hand cards leaked into P1's unknown slots"
            );
        }
    }

    #[test]
    fn threat_profile_exists_for_the_non_holder_seat() {
        use crate::threat_profile::build_threat_profile_multiplayer;
        let state = dandan_state_with_pool(vec![deck_entry("Alpha", 3, None)]);
        for ai in [PlayerId(0), PlayerId(1)] {
            assert!(
                build_threat_profile_multiplayer(&state, ai).is_some(),
                "{ai:?}: the opponent's deck is the shared pool"
            );
        }

        let mut standard = GameState::new_two_player(42);
        standard.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: std::sync::Arc::new(vec![deck_entry("Alpha", 3, None)]),
            ..Default::default()
        });
        assert!(
            build_threat_profile_multiplayer(&standard, PlayerId(0)).is_none(),
            "an unshared opponent without a pool has no profile"
        );
    }
}
