use std::collections::HashMap;

use engine::game::deck_loading::DeckEntry;
use engine::game::printed_cards::printed_ref_from_face;
use engine::types::card::PrintedCardRef;
use engine::types::game_state::{GameState, StackEntryKind};
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

pub fn known_remaining_deck_counts(
    state: &GameState,
    player: PlayerId,
) -> HashMap<DeckCardKey, u32> {
    let Some(pool) = state.deck_pools.iter().find(|pool| pool.player == player) else {
        return HashMap::new();
    };

    let mut counts = HashMap::new();
    for entry in pool.current_main.iter() {
        if entry.count == 0 {
            continue;
        }
        counts.insert(deck_entry_key(entry), entry.count);
    }

    for object_id in accounted_object_ids(state, player) {
        let Some(object) = state.objects.get(&object_id) else {
            continue;
        };
        if object.is_token || object.owner != player {
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
    let Some(pool) = state.deck_pools.iter().find(|pool| pool.player == player) else {
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

fn accounted_object_ids(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    let player_state = &state.players[player.0 as usize];
    let mut object_ids = Vec::new();

    object_ids.extend(player_state.hand.iter().copied());
    object_ids.extend(player_state.graveyard.iter().copied());
    object_ids.extend(
        state
            .battlefield
            .iter()
            .filter(|object_id| {
                state
                    .objects
                    .get(object_id)
                    .is_some_and(|object| object.owner == player)
            })
            .copied(),
    );
    object_ids.extend(
        state
            .exile
            .iter()
            .filter(|object_id| {
                state
                    .objects
                    .get(object_id)
                    .is_some_and(|object| object.owner == player)
            })
            .copied(),
    );
    object_ids.extend(state.stack.iter().filter_map(|entry| match &entry.kind {
        StackEntryKind::Spell { .. } => Some(entry.source_id),
        // CR 113.3b: Activated abilities (including KeywordAction) are not card
        // sources for deck knowledge — only spells expose their card source.
        StackEntryKind::ActivatedAbility { .. }
        | StackEntryKind::TriggeredAbility { .. }
        | StackEntryKind::KeywordAction { .. } => None,
    }));

    object_ids
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
                ability: engine::types::ability::ResolvedAbility::new(
                    engine::types::ability::Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: engine::types::ability::TargetFilter::Controller,
                        up_to: false,
                    },
                    Vec::new(),
                    token,
                    PlayerId(0),
                ),
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
                ability: Some(engine::types::ability::ResolvedAbility::new(
                    engine::types::ability::Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: engine::types::ability::TargetFilter::Controller,
                        up_to: false,
                    },
                    Vec::new(),
                    spell,
                    PlayerId(0),
                )),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let counts = known_remaining_deck_counts(&state, PlayerId(0));
        assert!(!counts.contains_key(&DeckCardKey::FaceName("Alpha".to_string())));
    }
}
