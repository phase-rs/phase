use std::collections::HashSet;

use crate::game::deck_loading::DeckEntry;
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::format::SideboardPolicy;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::keywords::{CompanionCondition, Keyword};
use crate::types::mana::ManaCostShard;
use crate::types::player::{CompanionInfo, PlayerId};
use crate::types::zones::Zone;

use super::zones;

/// CR 702.139: Companion costs {3} generic mana to move to hand.
const COMPANION_COST: usize = 3;

/// Permanent card types for companion condition evaluation.
const PERMANENT_TYPES: [CoreType; 5] = [
    CoreType::Artifact,
    CoreType::Creature,
    CoreType::Enchantment,
    CoreType::Planeswalker,
    CoreType::Land,
];

fn is_land(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Land)
}

fn is_permanent(face: &CardFace) -> bool {
    face.card_type
        .core_types
        .iter()
        .any(|ct| PERMANENT_TYPES.contains(ct))
}

fn is_creature(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Creature)
}

// ── Condition Validation ────────────────────────────────────────────────

/// CR 702.139: Validate that a main deck meets a companion's deckbuilding condition.
pub fn validate_companion_condition(
    condition: &CompanionCondition,
    main_deck: &[DeckEntry],
) -> bool {
    match condition {
        CompanionCondition::EvenManaValues => main_deck
            .iter()
            .all(|entry| is_land(&entry.card) || entry.card.mana_cost.mana_value() % 2 == 0),

        CompanionCondition::OddManaValues => main_deck
            .iter()
            .all(|entry| is_land(&entry.card) || entry.card.mana_cost.mana_value() % 2 == 1),

        CompanionCondition::NoRepeatedManaSymbols => main_deck
            .iter()
            .all(|entry| !has_repeated_mana_symbols(&entry.card)),

        CompanionCondition::CreatureTypeRestriction(allowed_types) => {
            main_deck.iter().all(|entry| {
                if !is_creature(&entry.card) {
                    return true;
                }
                entry
                    .card
                    .card_type
                    .subtypes
                    .iter()
                    .any(|st| allowed_types.iter().any(|at| at.eq_ignore_ascii_case(st)))
            })
        }

        CompanionCondition::MinManaValue(min) => main_deck
            .iter()
            .all(|entry| is_land(&entry.card) || entry.card.mana_cost.mana_value() >= *min),

        CompanionCondition::MaxPermanentManaValue(max) => main_deck.iter().all(|entry| {
            !is_permanent(&entry.card)
                || is_land(&entry.card)
                || entry.card.mana_cost.mana_value() <= *max
        }),

        CompanionCondition::Singleton => {
            let mut seen = HashSet::new();
            main_deck.iter().all(|entry| {
                if is_land(&entry.card) {
                    return true;
                }
                // A DeckEntry with count > 1 means multiple copies
                if entry.count > 1 {
                    return false;
                }
                seen.insert(entry.card.name.clone())
            })
        }

        CompanionCondition::SharedCardType => {
            // All nonland cards must share at least one card type
            let nonland_types: Vec<&[CoreType]> = main_deck
                .iter()
                .filter(|e| !is_land(&e.card))
                .map(|e| e.card.card_type.core_types.as_slice())
                .collect();
            if nonland_types.is_empty() {
                return true;
            }
            // Check if there's any CoreType that all nonland cards share
            let candidate_types: Vec<CoreType> = nonland_types[0]
                .iter()
                .filter(|ct| **ct != CoreType::Land)
                .copied()
                .collect();
            candidate_types
                .iter()
                .any(|ct| nonland_types.iter().all(|types| types.contains(ct)))
        }

        CompanionCondition::MinDeckSizeOver(over) => {
            let total: u32 = main_deck.iter().map(|e| e.count).sum();
            // Yorion requires 80+ cards (60 minimum + 20 over)
            total >= 60 + over
        }

        CompanionCondition::PermanentsHaveActivatedAbilities => main_deck.iter().all(|entry| {
            if !is_permanent(&entry.card) || is_land(&entry.card) {
                return true;
            }
            entry
                .card
                .abilities
                .iter()
                .any(|a| a.kind == crate::types::ability::AbilityKind::Activated)
        }),
    }
}

/// CR 702.139 (Jegantha): Check if a card has more than one of the same mana symbol.
fn has_repeated_mana_symbols(face: &CardFace) -> bool {
    let shards = match &face.mana_cost {
        crate::types::mana::ManaCost::Cost { shards, .. } => shards,
        _ => return false,
    };
    let colored: Vec<&ManaCostShard> = shards
        .iter()
        .filter(|s| {
            matches!(
                s,
                ManaCostShard::White
                    | ManaCostShard::Blue
                    | ManaCostShard::Black
                    | ManaCostShard::Red
                    | ManaCostShard::Green
            )
        })
        .collect();
    let unique: HashSet<&&ManaCostShard> = colored.iter().collect();
    colored.len() != unique.len()
}

// ── Pre-game Reveal Flow ────────────────────────────────────────────────

/// CR 702.139a: Find sideboard cards with companion keyword whose condition
/// is met by the main deck.
pub fn find_eligible_companions(
    sideboard: &[DeckEntry],
    main_deck: &[DeckEntry],
) -> Vec<(String, usize)> {
    sideboard
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            let condition = entry.card.keywords.iter().find_map(|kw| {
                if let Keyword::Companion(ref cond) = kw {
                    Some(cond)
                } else {
                    None
                }
            })?;
            if validate_companion_condition(condition, main_deck) {
                Some((entry.card.name.clone(), idx))
            } else {
                None
            }
        })
        .collect()
}

/// CR 702.139a: Check if a player has eligible companions from their sideboard.
/// Returns CompanionReveal WaitingFor if eligible companions exist, otherwise None.
pub fn check_companion_reveal(state: &GameState, player: PlayerId) -> Option<WaitingFor> {
    // CR 702.139a: Companions are revealed from the sideboard. Formats with no
    // sideboard (Commander, Duel Commander, Pauper Commander, Brawl, Historic
    // Brawl) categorically cannot use companions. Rather than enumerate the
    // format list here — which has drifted in the past as commander variants
    // were added — defer to the engine's single authority for sideboard rules.
    if matches!(
        state.format_config.format.sideboard_policy(),
        SideboardPolicy::Forbidden
    ) {
        return None;
    }

    let pool = state.deck_pools.iter().find(|p| p.player == player)?;
    let eligible = find_eligible_companions(&pool.current_sideboard, &pool.current_main);

    if eligible.is_empty() {
        None
    } else {
        Some(WaitingFor::CompanionReveal {
            player,
            eligible_companions: eligible,
        })
    }
}

/// CR 702.139a: Check companion reveals for all players in seat order.
/// Returns the first CompanionReveal WaitingFor found, or None.
pub fn check_all_companion_reveals(state: &GameState) -> Option<WaitingFor> {
    for &player_id in &state.seat_order {
        if let Some(wf) = check_companion_reveal(state, player_id) {
            return Some(wf);
        }
    }
    None
}

/// CR 702.139a: Handle companion declaration or decline.
/// `eligible_companions` is the pre-computed list from `WaitingFor::CompanionReveal`,
/// ensuring the index the player chose maps to the same card that was presented.
/// Returns the next WaitingFor state (next player's reveal or mulligans).
pub fn handle_declare_companion(
    state: &mut GameState,
    player: PlayerId,
    card_index: Option<usize>,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    if let Some(idx) = card_index {
        // Retrieve the eligible list from the WaitingFor state that was presented to the player
        let eligible = match &state.waiting_for {
            WaitingFor::CompanionReveal {
                eligible_companions,
                ..
            } => eligible_companions.clone(),
            _ => vec![],
        };

        if let Some((card_name, sb_idx)) = eligible.get(idx) {
            let pool = state
                .deck_pools
                .iter_mut()
                .find(|p| p.player == player)
                .expect("player deck pool exists");

            // Validate the sideboard index is still valid
            if *sb_idx < pool.current_sideboard.len() {
                // Remove the companion from the sideboard
                let card_entry = pool.current_sideboard[*sb_idx].clone();
                if pool.current_sideboard[*sb_idx].count > 1 {
                    pool.current_sideboard[*sb_idx].count -= 1;
                } else {
                    pool.current_sideboard.remove(*sb_idx);
                }

                // Set companion on the player
                let player_data = state
                    .players
                    .iter_mut()
                    .find(|p| p.id == player)
                    .expect("player exists");
                player_data.companion = Some(CompanionInfo {
                    card: DeckEntry {
                        card: card_entry.card,
                        count: 1,
                    },
                    used: false,
                });

                events.push(GameEvent::CompanionRevealed {
                    player,
                    card_name: card_name.clone(),
                });
            }
        }
    }

    // Advance to next player's companion reveal in seat order
    advance_companion_reveal(state, player, events)
}

/// Move to the next player's companion reveal, or start mulligans if all done.
fn advance_companion_reveal(
    state: &mut GameState,
    current_player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let seat_order = &state.seat_order;
    let current_idx = seat_order
        .iter()
        .position(|&id| id == current_player)
        .unwrap_or(0);

    // Check remaining players for eligible companions
    for &player_id in seat_order.iter().skip(current_idx + 1) {
        if let Some(wf) = check_companion_reveal(state, player_id) {
            return wf;
        }
    }

    // All players done — proceed to mulligans
    super::mulligan::start_mulligan(state, events)
}

// ── Special Action: Pay {3} to Move Companion to Hand ───────────────────

/// CR 702.139a: Check if a player can pay {3} to put their companion into hand.
/// Returns true if all conditions are met (sorcery speed, has companion, not used, enough mana).
pub fn can_activate_companion(state: &GameState, player: PlayerId) -> bool {
    let player_data = &state.players[player.0 as usize];
    let has_unused_companion = player_data.companion.as_ref().is_some_and(|c| !c.used);

    if !has_unused_companion {
        return false;
    }

    // Sorcery-speed timing check
    let is_sorcery_speed = matches!(
        state.phase,
        crate::types::phase::Phase::PreCombatMain | crate::types::phase::Phase::PostCombatMain
    ) && state.stack.is_empty()
        && state.active_player == player;

    if !is_sorcery_speed {
        return false;
    }

    // Mana pool has at least {3} unrestricted mana (restricted mana can't pay special action costs)
    player_data
        .mana_pool
        .mana
        .iter()
        .filter(|m| m.restrictions.is_empty())
        .count()
        >= COMPANION_COST
}

/// CR 702.139a: Pay {3} as a special action to put companion into hand.
pub fn handle_companion_to_hand(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    let player_data = &state.players[player.0 as usize];

    // Validate companion exists and is unused
    let companion_info = player_data
        .companion
        .as_ref()
        .ok_or("No companion declared")?;
    if companion_info.used {
        return Err("Companion has already been put into hand this game".to_string());
    }

    // Validate sorcery-speed timing
    let is_sorcery_speed = matches!(
        state.phase,
        crate::types::phase::Phase::PreCombatMain | crate::types::phase::Phase::PostCombatMain
    ) && state.stack.is_empty()
        && state.active_player == player;
    if !is_sorcery_speed {
        return Err("Companion can only be put into hand at sorcery speed".to_string());
    }

    // Validate and deduct mana
    let player_data = &mut state.players[player.0 as usize];
    if !player_data.mana_pool.spend_generic(COMPANION_COST) {
        return Err("Not enough mana to pay companion cost ({3})".to_string());
    }

    // Take the companion card data and mark as used
    let card_face = player_data
        .companion
        .as_ref()
        .expect("validated above")
        .card
        .card
        .clone();
    let card_name = card_face.name.clone();
    player_data
        .companion
        .as_mut()
        .expect("validated above")
        .used = true;

    // Create a GameObject from the companion's CardFace and put it into hand
    let card_id = crate::types::identifiers::CardId(state.next_object_id);
    let obj_id = zones::create_object(state, card_id, player, card_face.name.clone(), Zone::Hand);

    // Apply the full card face data to the new object
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        super::printed_cards::apply_card_face_to_object(obj, &card_face);
    }

    events.push(GameEvent::CompanionMovedToHand { player, card_name });

    Ok(WaitingFor::Priority { player })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::keywords::CompanionCondition;
    use crate::types::mana::ManaCost;

    fn creature(name: &str, mv: u32, subtypes: Vec<&str>) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                mana_cost: ManaCost::Cost {
                    shards: vec![],
                    generic: mv,
                },
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: subtypes.into_iter().map(String::from).collect(),
                },
                ..Default::default()
            },
            count: 1,
        }
    }

    fn land(name: &str) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                mana_cost: ManaCost::NoCost,
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Land],
                    subtypes: vec!["Plains".to_string()],
                },
                ..Default::default()
            },
            count: 4,
        }
    }

    fn instant(name: &str, mv: u32) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                mana_cost: ManaCost::Cost {
                    shards: vec![],
                    generic: mv,
                },
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Instant],
                    subtypes: vec![],
                },
                ..Default::default()
            },
            count: 1,
        }
    }

    #[test]
    fn even_mana_values_valid() {
        let deck = vec![
            creature("Bear", 2, vec!["Bear"]),
            creature("Angel", 4, vec!["Angel"]),
            land("Plains"),
        ];
        assert!(validate_companion_condition(
            &CompanionCondition::EvenManaValues,
            &deck
        ));
    }

    #[test]
    fn even_mana_values_invalid_odd_card() {
        let deck = vec![
            creature("Bear", 2, vec!["Bear"]),
            creature("Bolt", 1, vec![]),
        ];
        assert!(!validate_companion_condition(
            &CompanionCondition::EvenManaValues,
            &deck
        ));
    }

    #[test]
    fn odd_mana_values_valid() {
        let deck = vec![
            creature("Bolt", 1, vec![]),
            creature("Angel", 3, vec!["Angel"]),
            land("Mountain"),
        ];
        assert!(validate_companion_condition(
            &CompanionCondition::OddManaValues,
            &deck
        ));
    }

    #[test]
    fn singleton_valid() {
        let deck = vec![
            creature("A", 1, vec![]),
            creature("B", 2, vec![]),
            land("Plains"),
        ];
        assert!(validate_companion_condition(
            &CompanionCondition::Singleton,
            &deck
        ));
    }

    #[test]
    fn singleton_invalid_duplicate() {
        let deck = vec![DeckEntry {
            card: CardFace {
                name: "Bolt".to_string(),
                mana_cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 1,
                },
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Instant],
                    subtypes: vec![],
                },
                ..Default::default()
            },
            count: 2,
        }];
        assert!(!validate_companion_condition(
            &CompanionCondition::Singleton,
            &deck
        ));
    }

    #[test]
    fn min_mana_value_valid() {
        let deck = vec![
            creature("Angel", 3, vec!["Angel"]),
            creature("Dragon", 5, vec!["Dragon"]),
            land("Plains"),
        ];
        assert!(validate_companion_condition(
            &CompanionCondition::MinManaValue(3),
            &deck
        ));
    }

    #[test]
    fn min_mana_value_invalid() {
        let deck = vec![creature("Bolt", 1, vec![])];
        assert!(!validate_companion_condition(
            &CompanionCondition::MinManaValue(3),
            &deck
        ));
    }

    #[test]
    fn max_permanent_mana_value_valid() {
        let deck = vec![
            creature("Bear", 2, vec!["Bear"]),
            creature("Elf", 1, vec!["Elf"]),
            instant("Bolt", 1),
            instant("Big Spell", 5), // Instants are non-permanent, exempt
            land("Plains"),
        ];
        assert!(validate_companion_condition(
            &CompanionCondition::MaxPermanentManaValue(2),
            &deck
        ));
    }

    #[test]
    fn max_permanent_mana_value_invalid() {
        let deck = vec![creature("Angel", 4, vec!["Angel"])];
        assert!(!validate_companion_condition(
            &CompanionCondition::MaxPermanentManaValue(2),
            &deck
        ));
    }

    #[test]
    fn creature_type_restriction_valid() {
        let allowed = vec![
            "Cat".to_string(),
            "Elemental".to_string(),
            "Nightmare".to_string(),
        ];
        let deck = vec![
            creature("Cat A", 2, vec!["Cat"]),
            creature("Elem B", 3, vec!["Elemental"]),
            instant("Bolt", 1),
            land("Plains"),
        ];
        assert!(validate_companion_condition(
            &CompanionCondition::CreatureTypeRestriction(allowed),
            &deck
        ));
    }

    #[test]
    fn creature_type_restriction_invalid() {
        let allowed = vec!["Cat".to_string()];
        let deck = vec![creature("Goblin", 1, vec!["Goblin"])];
        assert!(!validate_companion_condition(
            &CompanionCondition::CreatureTypeRestriction(allowed),
            &deck
        ));
    }

    #[test]
    fn shared_card_type_valid_all_creatures() {
        let deck = vec![
            creature("Bear", 2, vec!["Bear"]),
            creature("Elf", 1, vec!["Elf"]),
            land("Forest"),
        ];
        assert!(validate_companion_condition(
            &CompanionCondition::SharedCardType,
            &deck
        ));
    }

    #[test]
    fn shared_card_type_invalid_mixed() {
        let deck = vec![creature("Bear", 2, vec!["Bear"]), instant("Bolt", 1)];
        assert!(!validate_companion_condition(
            &CompanionCondition::SharedCardType,
            &deck
        ));
    }

    #[test]
    fn min_deck_size_over_valid() {
        // Yorion needs 80+ cards (60 + 20)
        let mut deck = Vec::new();
        for i in 0..80 {
            deck.push(DeckEntry {
                card: CardFace {
                    name: format!("Card {i}"),
                    mana_cost: ManaCost::NoCost,
                    card_type: CardType {
                        supertypes: vec![],
                        core_types: vec![CoreType::Land],
                        subtypes: vec![],
                    },
                    ..Default::default()
                },
                count: 1,
            });
        }
        assert!(validate_companion_condition(
            &CompanionCondition::MinDeckSizeOver(20),
            &deck
        ));
    }

    #[test]
    fn min_deck_size_over_invalid() {
        let deck = vec![land("Plains")]; // Only 4 cards
        assert!(!validate_companion_condition(
            &CompanionCondition::MinDeckSizeOver(20),
            &deck
        ));
    }

    #[test]
    fn no_repeated_mana_symbols_valid() {
        use crate::types::mana::ManaCostShard;
        let deck = vec![DeckEntry {
            card: CardFace {
                name: "Niv-Mizzet".to_string(),
                mana_cost: ManaCost::Cost {
                    shards: vec![
                        ManaCostShard::White,
                        ManaCostShard::Blue,
                        ManaCostShard::Black,
                        ManaCostShard::Red,
                        ManaCostShard::Green,
                    ],
                    generic: 0,
                },
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![],
                },
                ..Default::default()
            },
            count: 1,
        }];
        assert!(validate_companion_condition(
            &CompanionCondition::NoRepeatedManaSymbols,
            &deck
        ));
    }

    #[test]
    fn no_repeated_mana_symbols_invalid() {
        use crate::types::mana::ManaCostShard;
        let deck = vec![DeckEntry {
            card: CardFace {
                name: "WW Card".to_string(),
                mana_cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::White, ManaCostShard::White],
                    generic: 0,
                },
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![],
                },
                ..Default::default()
            },
            count: 1,
        }];
        assert!(!validate_companion_condition(
            &CompanionCondition::NoRepeatedManaSymbols,
            &deck
        ));
    }
}
