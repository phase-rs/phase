use std::collections::HashSet;

use crate::game::deck_loading::DeckEntry;
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::format::{GameFormat, SideboardPolicy};
use crate::types::game_state::{
    CompanionChoiceSource, CompanionDeclaration, CompanionRevealChoice, GameState,
    ManaAbilityResume, PlayerDeckPool, WaitingFor,
};
use crate::types::keywords::{CompanionCondition, Keyword};
use crate::types::mana::{ManaCost, ManaCostShard, SpecialAction};
use crate::types::player::{CompanionInfo, PlayerId};
use crate::types::zones::Zone;

use super::casting::{self, SpecialActionManaPayment};
use super::engine::EngineError;
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
            .all(|entry| is_land(&entry.card) || entry.off_stack_mana_value() % 2 == 0),

        CompanionCondition::OddManaValues => main_deck
            .iter()
            .all(|entry| is_land(&entry.card) || entry.off_stack_mana_value() % 2 == 1),

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
            .all(|entry| is_land(&entry.card) || entry.off_stack_mana_value() >= *min),

        CompanionCondition::MaxPermanentManaValue(max) => main_deck.iter().all(|entry| {
            !is_permanent(&entry.card)
                || is_land(&entry.card)
                || entry.off_stack_mana_value() <= *max
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

/// Builds the starting deck a companion condition evaluates. Commander-family
/// games include the commander but never the prospective companion (CR
/// 702.139b); other formats evaluate their main deck only.
pub fn companion_starting_deck(
    main_deck: &[DeckEntry],
    commanders: &[DeckEntry],
    format: GameFormat,
) -> Vec<DeckEntry> {
    let mut starting = main_deck.to_vec();
    if format.uses_commander() {
        starting.extend_from_slice(commanders);
    }
    starting
}

fn commander_allows_companion(
    companion: &DeckEntry,
    starting_deck: &[DeckEntry],
    commanders: &[DeckEntry],
    format: GameFormat,
) -> bool {
    if !format.uses_commander() {
        return true;
    }

    // CR 903.11a: a card brought in from outside the game cannot share a
    // name with the starting deck and must fit its commander's color identity.
    if starting_deck
        .iter()
        .any(|entry| entry.card.name.eq_ignore_ascii_case(&companion.card.name))
    {
        return false;
    }
    let commander_colors: HashSet<_> = commanders
        .iter()
        .flat_map(|entry| entry.card.color_identity.iter().copied())
        .collect();
    companion
        .card
        .color_identity
        .iter()
        .all(|color| commander_colors.contains(color))
}

/// Tests one candidate against a complete starting deck. This is the shared
/// authority used by construction validation, pre-game offers, and response
/// revalidation so the UI never has to duplicate companion or Commander rules.
pub fn is_eligible_companion(
    companion: &DeckEntry,
    starting_deck: &[DeckEntry],
    commanders: &[DeckEntry],
    format: GameFormat,
) -> bool {
    // allow-raw-authority: DeckEntry carries a printed CardFace snapshot; deck validation has no GameState or live object to query.
    let Some(condition) = companion.card.keywords.iter().find_map(|keyword| {
        if let Keyword::Companion(condition) = keyword {
            Some(condition)
        } else {
            None
        }
    }) else {
        return false;
    };

    validate_companion_condition(condition, starting_deck)
        && commander_allows_companion(companion, starting_deck, commanders, format)
}

fn companion_offers(pool: &PlayerDeckPool, format: GameFormat) -> Vec<CompanionRevealChoice> {
    let starting = companion_starting_deck(&pool.current_main, &pool.current_commander, format);
    let candidates: Vec<(CompanionChoiceSource, &DeckEntry)> = if format.uses_commander() {
        pool.current_companion
            .first()
            .map(|entry| (CompanionChoiceSource::Dedicated, entry))
            .into_iter()
            .collect()
    } else if !matches!(format.sideboard_policy(), SideboardPolicy::Forbidden) {
        pool.current_sideboard
            .iter()
            .enumerate()
            .map(|(index, entry)| (CompanionChoiceSource::Sideboard { index }, entry))
            .collect()
    } else {
        Vec::new()
    };

    candidates
        .into_iter()
        .filter_map(|(source, entry)| {
            is_eligible_companion(entry, &starting, &pool.current_commander, format).then(|| {
                CompanionRevealChoice {
                    name: entry.card.name.clone(),
                    source,
                }
            })
        })
        .collect()
}

/// CR 702.139a: Check if a player has eligible companions from their permitted
/// outside-game source.
/// Returns CompanionReveal WaitingFor if eligible companions exist, otherwise None.
pub fn check_companion_reveal(state: &GameState, player: PlayerId) -> Option<WaitingFor> {
    let pool = state.deck_pools.iter().find(|p| p.player == player)?;
    let mut eligible = companion_offers(pool, state.format_config.format);
    if state.format_config.format == GameFormat::TinyLeaders {
        eligible
            .retain(|choice| !super::deck_validation::tiny_leaders_companion_banned(&choice.name));
    }

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
/// ensuring the typed response exactly matches a card that was presented.
/// Returns the next WaitingFor state (next player's reveal or mulligans).
pub fn handle_declare_companion(
    state: &mut GameState,
    player: PlayerId,
    choice: CompanionDeclaration,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    if let CompanionDeclaration::Reveal(choice) = choice {
        let eligible = match &state.waiting_for {
            WaitingFor::CompanionReveal {
                eligible_companions,
                ..
            } => eligible_companions.clone(),
            _ => return Err("Companion response without an active offer".to_string()),
        };

        if !eligible.contains(&choice) {
            return Err("Companion choice was not offered".to_string());
        }

        if state
            .players
            .iter()
            .find(|player_data| player_data.id == player)
            .is_some_and(|player_data| player_data.companion.is_some())
        {
            return Err("Companion was already revealed".to_string());
        }

        let pool = state
            .deck_pools
            .iter_mut()
            .find(|pool| pool.player == player)
            .ok_or_else(|| "Player deck pool does not exist".to_string())?;
        let starting = companion_starting_deck(
            &pool.current_main,
            &pool.current_commander,
            state.format_config.format,
        );
        let selected = match choice.source {
            CompanionChoiceSource::Sideboard { index } => {
                let entry = pool
                    .current_sideboard
                    .get(index)
                    .filter(|entry| entry.card.name == choice.name)
                    .cloned()
                    .ok_or_else(|| "Companion sideboard offer is stale".to_string())?;
                if !is_eligible_companion(
                    &entry,
                    &starting,
                    &pool.current_commander,
                    state.format_config.format,
                ) {
                    return Err("Companion is no longer eligible".to_string());
                }
                let sideboard = std::sync::Arc::make_mut(&mut pool.current_sideboard);
                if sideboard[index].count > 1 {
                    sideboard[index].count -= 1;
                } else {
                    sideboard.remove(index);
                }
                entry
            }
            CompanionChoiceSource::Dedicated => {
                let entry = pool
                    .current_companion
                    .first()
                    .filter(|entry| entry.card.name == choice.name)
                    .cloned()
                    .ok_or_else(|| "Dedicated companion offer is stale".to_string())?;
                if !state.format_config.format.uses_commander()
                    || !is_eligible_companion(
                        &entry,
                        &starting,
                        &pool.current_commander,
                        state.format_config.format,
                    )
                {
                    return Err("Companion is no longer eligible".to_string());
                }
                std::sync::Arc::make_mut(&mut pool.current_companion).clear();
                entry
            }
        };

        let player_data = state
            .players
            .iter_mut()
            .find(|entry| entry.id == player)
            .ok_or_else(|| "Player does not exist".to_string())?;
        player_data.companion = Some(CompanionInfo {
            card: DeckEntry {
                card: selected.card,
                count: 1,
            },
            used: false,
        });

        events.push(GameEvent::CompanionRevealed {
            player,
            card_name: choice.name,
        });
    }

    // Advance to next player's companion reveal in seat order
    Ok(advance_companion_reveal(state, player, events))
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

/// CR 116.2g + CR 702.139a: Derive the companion special action's final cost.
/// Cost adjustments apply before payment, and this concrete result is retained
/// by a paused mana-ability continuation rather than recomputed later.
fn companion_to_hand_cost(state: &GameState, player: PlayerId) -> ManaCost {
    casting::apply_special_action_cost_reduction(
        state,
        player,
        SpecialAction::CompanionToHand,
        ManaCost::generic(COMPANION_COST as u32),
    )
}

/// CR 116.2g + CR 702.139a: Validate the non-payment preconditions for moving
/// a companion to hand. Kept separate from payment so a forged direct action
/// fails before auto-tapping any mana source.
fn validate_companion_to_hand_action(
    state: &GameState,
    player: PlayerId,
) -> Result<(), EngineError> {
    let player_data = &state.players[player.0 as usize];
    let companion = player_data
        .companion
        .as_ref()
        .ok_or_else(|| EngineError::InvalidAction("No companion declared".to_string()))?;
    if companion.used {
        return Err(EngineError::InvalidAction(
            "Companion has already been put into hand this game".to_string(),
        ));
    }

    let is_sorcery_speed = matches!(
        state.phase,
        crate::types::phase::Phase::PreCombatMain | crate::types::phase::Phase::PostCombatMain
    ) && state.stack.is_empty()
        && state.active_player == player;
    if !is_sorcery_speed {
        return Err(EngineError::InvalidAction(
            "Companion can only be put into hand at sorcery speed".to_string(),
        ));
    }

    Ok(())
}

/// CR 702.139a: Check whether the player can legally take the companion special
/// action now, including payment after automatic mana-source activation.
pub fn can_activate_companion(state: &GameState, player: PlayerId) -> bool {
    validate_companion_to_hand_action(state, player).is_ok()
        && casting::can_pay_special_action_mana_cost_after_auto_tap(
            state,
            player,
            None,
            &companion_to_hand_cost(state, player),
            SpecialAction::CompanionToHand,
        )
}

/// CR 116.2g + CR 702.139a: Pay the companion special action's already-derived
/// cost. A paused mana-source replacement leaves the companion outside the game
/// and returns that prompt; only a completed payment commits the move.
fn pay_companion_to_hand_cost(
    state: &mut GameState,
    player: PlayerId,
    cost: &ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<SpecialActionManaPayment, EngineError> {
    let resume = ManaAbilityResume::CompanionToHand {
        player,
        cost: cost.clone(),
    };
    casting::pay_special_action_mana_cost_with_resume(
        state,
        player,
        None,
        cost,
        SpecialAction::CompanionToHand,
        Some(&resume),
        events,
    )
}

/// CR 116.2g + CR 702.139a: Commit the once-per-game companion action only
/// after its full payment succeeds. This is also the sole point that clears the
/// player's undoable mana-tap record for this non-mana action.
fn commit_companion_to_hand(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let card_face = state.players[player.0 as usize]
        .companion
        .as_ref()
        .expect("a paid companion continuation retains its declared companion")
        .card
        .card
        .clone();
    let card_name = card_face.name.clone();
    state.players[player.0 as usize]
        .companion
        .as_mut()
        .expect("a paid companion continuation retains its declared companion")
        .used = true;

    let card_id = crate::types::identifiers::CardId(state.next_object_id);
    let obj_id = zones::create_object(state, card_id, player, card_face.name.clone(), Zone::Hand);
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        super::printed_cards::apply_card_face_to_object(obj, &card_face);
    }

    events.push(GameEvent::CompanionMovedToHand { player, card_name });
    state.lands_tapped_for_mana.remove(&player);
    WaitingFor::Priority { player }
}

/// CR 116.2g + CR 702.139a: Initiate the companion special action. Preflight
/// occurs before automatic mana activation, so an unaffordable forged action
/// cannot mutate mana, waiting state, companion state, hand, or undo tracking.
pub fn handle_companion_to_hand(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    validate_companion_to_hand_action(state, player)?;
    let cost = companion_to_hand_cost(state, player);
    if !casting::can_pay_special_action_mana_cost_after_auto_tap(
        state,
        player,
        None,
        &cost,
        SpecialAction::CompanionToHand,
    ) {
        return Err(EngineError::ActionNotAllowed(
            "Cannot pay companion cost".to_string(),
        ));
    }

    match pay_companion_to_hand_cost(state, player, &cost, events)? {
        SpecialActionManaPayment::Paid => Ok(commit_companion_to_hand(state, player, events)),
        SpecialActionManaPayment::Paused => Ok(state.waiting_for.clone()),
    }
}

/// CR 116.2g + CR 702.139a + CR 605.3b + CR 616.1: Resume a companion payment
/// after a mana-source cost replacement choice. `cost` was locked at initiation,
/// and a second pause retains the same typed root for another resumption.
pub(crate) fn resume_companion_to_hand_payment(
    state: &mut GameState,
    player: PlayerId,
    cost: ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match pay_companion_to_hand_cost(state, player, &cost, events)? {
        SpecialActionManaPayment::Paid => Ok(commit_companion_to_hand(state, player, events)),
        SpecialActionManaPayment::Paused => Ok(state.waiting_for.clone()),
    }
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

    #[test]
    fn commander_starting_deck_includes_commander_but_not_companion() {
        let main = vec![creature("Main Card", 2, vec![])];
        let commanders = vec![creature("Commander", 3, vec![])];

        let commander_starting = companion_starting_deck(
            &main,
            &commanders,
            crate::types::format::GameFormat::Commander,
        );
        assert_eq!(
            commander_starting
                .iter()
                .map(|entry| entry.card.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Main Card", "Commander"]
        );

        let standard_starting = companion_starting_deck(
            &main,
            &commanders,
            crate::types::format::GameFormat::Standard,
        );
        assert_eq!(
            standard_starting
                .iter()
                .map(|entry| entry.card.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Main Card"]
        );
    }
}
