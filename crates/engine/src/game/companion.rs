use std::collections::HashSet;

use crate::game::deck_loading::{self, DeckEntry};
use crate::types::card::CardFace;
use crate::types::card_printing::DeckSection;
use crate::types::card_type::CoreType;
use crate::types::display_only::DisplayOnly;
use crate::types::events::GameEvent;
use crate::types::format::{CompanionSlot, GameFormat, SideboardPolicy};
use crate::types::game_state::{
    CompanionChoiceSource, CompanionDeclaration, CompanionRevealChoice, GameState,
    ManaAbilityResume, PlayerDeckPool, WaitingFor,
};
use crate::types::keywords::{CompanionCondition, Keyword};
use crate::types::mana::{ManaCost, ManaCostShard, SpecialAction};
use crate::types::player::{CompanionInfo, PlayerId};
use crate::types::zones::Zone;

use super::casting::{self, SpecialActionManaPayment};
use super::engine::{EngineError, PriorityPrincipal};
use super::zones;

/// CR 702.139: Companion costs {3} generic mana to move to hand.
const COMPANION_COST: usize = 3;

/// An engine-authored Companion special-action announcement for Priority
/// preflight. The private field prevents sibling modules from manufacturing a
/// unit-like announcement without the Companion legality authority.
pub(in crate::game) struct PriorityCompanionAnnouncement {
    _private: (),
}

impl PriorityCompanionAnnouncement {
    fn new() -> Self {
        Self { _private: () }
    }
}

fn is_land(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Land)
}

/// CR 110.4a: a permanent card is an artifact, battle, creature, enchantment,
/// land, or planeswalker card.
fn is_permanent(face: &CardFace) -> bool {
    face.card_type
        .core_types
        .iter()
        .any(|ct| ct.is_permanent_type())
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
    validate_companion_condition_with_min_deck_size(condition, main_deck, 60)
}

/// CR 702.139: Validate that a main deck meets a companion's deckbuilding condition
/// for a format with the given minimum deck size (e.g. 40 for Limited, 60 for Constructed).
pub fn validate_companion_condition_with_min_deck_size(
    condition: &CompanionCondition,
    main_deck: &[DeckEntry],
    min_deck_size: usize,
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
            .all(|entry| !has_repeated_mana_symbols(entry)),

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
            // Yorion requires min_deck_size + 20 cards (e.g. 60 in 40-card Limited, 80 in 60-card Constructed)
            total >= (min_deck_size as u32) + over
        }

        // CR 702.139a: "each permanent card in your starting deck has an activated
        // ability" covers land cards too. A land qualifies through a printed
        // activated ability or the intrinsic mana ability of a basic land type
        // (CR 305.6); a land with neither (Glacial Chasm) fails the condition.
        CompanionCondition::PermanentsHaveActivatedAbilities => main_deck.iter().all(|entry| {
            if !is_permanent(&entry.card) {
                return true;
            }
            entry
                .card
                .abilities
                .iter()
                .any(|a| a.kind == crate::types::ability::AbilityKind::Activated)
                || (is_land(&entry.card)
                    && !super::deck_validation::basic_land_type_colors(&entry.card).is_empty())
        }),
    }
}

/// One mana symbol of a mana cost as Jegantha compares them. The generic
/// amount is a single symbol per cost ({2} is the symbol {2}, not two {1}s).
#[derive(PartialEq, Eq, Hash)]
enum CostSymbol {
    Generic(u32),
    Shard(ManaCostShard),
}

/// CR 702.139a (Jegantha): whether a card's mana cost repeats a mana symbol.
/// Every symbol counts, not only the five colors: a hybrid symbol is one symbol,
/// so {R/W}{R/W}{R/W} (Boros Reckoner) repeats it, as do {X}{X}, {C}{C}, and
/// repeated Phyrexian or snow symbols (Wizards' Ikoria release notes).
/// CR 709.4b: a split card's symbols are those of every half, so Fire // Ice
/// ({1}{R} and {1}{U}) contains the symbol {1} twice.
fn has_repeated_mana_symbols(entry: &DeckEntry) -> bool {
    let mut seen: HashSet<CostSymbol> = HashSet::new();
    entry
        .off_stack_mana_costs()
        .iter()
        .filter_map(|cost| match cost {
            ManaCost::Cost { shards, generic } => Some((shards, *generic)),
            _ => None,
        })
        .flat_map(|(shards, generic)| {
            (generic > 0)
                .then_some(CostSymbol::Generic(generic))
                .into_iter()
                .chain(shards.iter().copied().map(CostSymbol::Shard))
        })
        .any(|symbol| !seen.insert(symbol))
}

// ── Pre-game Reveal Flow ────────────────────────────────────────────────

/// Builds the starting deck a companion condition evaluates from a game-loaded
/// pool, whose `main_deck` is already netted of the command zone (the game
/// loader's `net_command_zone`). Commander-family games include the commander
/// but never the prospective companion (CR 702.139b); other formats evaluate
/// their main deck only.
pub fn companion_starting_deck(
    main_deck: &[DeckEntry],
    commanders: &[DeckEntry],
    uses_commander: bool,
) -> Vec<DeckEntry> {
    let mut starting = main_deck.to_vec();
    if uses_commander {
        starting.extend_from_slice(commanders);
    }
    starting
}

/// `companion_starting_deck` for a decklist-shaped `main_deck`, as submitted by
/// deck construction, a draft pod, or game admission, where the commander may
/// be named in the main list as well as in the command-zone list.
///
/// CR 903.5a: the commander is one of the deck's cards, so a main list that
/// already names it and a separate command-zone list describe ONE physical
/// card. The command-zone copy is netted out of `main_deck` through the
/// loader's own `net_command_zone` before it is appended, so the deck measured
/// here has exactly the size and contents of the deck the game starts with
/// (never one card long, which would let a 79-card Commander deck satisfy
/// Yorion's twenty-card margin over an 80-card threshold).
pub fn companion_starting_deck_from_decklist(
    main_deck: &[DeckEntry],
    commanders: &[DeckEntry],
    uses_commander: bool,
) -> Vec<DeckEntry> {
    if !uses_commander {
        return main_deck.to_vec();
    }
    let mut netted = deck_loading::net_command_zone(main_deck, &[commanders]);
    netted.retain(|entry| entry.count > 0);
    companion_starting_deck(&netted, commanders, uses_commander)
}

fn commander_allows_companion(
    companion: &DeckEntry,
    starting_deck: &[DeckEntry],
    commanders: &[DeckEntry],
    uses_commander: bool,
) -> bool {
    if !uses_commander {
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
    uses_commander: bool,
) -> bool {
    is_eligible_companion_with_min_deck_size(
        companion,
        starting_deck,
        commanders,
        uses_commander,
        60,
    )
}

/// Tests one candidate against a complete starting deck for a format with the
/// given minimum deck size.
pub fn is_eligible_companion_with_min_deck_size(
    companion: &DeckEntry,
    starting_deck: &[DeckEntry],
    commanders: &[DeckEntry],
    uses_commander: bool,
    min_deck_size: usize,
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

    validate_companion_condition_with_min_deck_size(condition, starting_deck, min_deck_size)
        && commander_allows_companion(companion, starting_deck, commanders, uses_commander)
}

/// CR 702.139a: the companions `pool` may reveal, by the format's resolved
/// companion slot and sideboard policy (`FormatConfig::companion_slot` /
/// `.sideboard_policy`; never the bare `GameFormat` methods, whose `Custom`
/// answers cannot see a custom format's declared rules).
///
/// - Commander family: only the dedicated slot. Commander games use no
///   sideboard (CR 903.5e).
/// - Otherwise: "one card you own from outside the game" is any sideboard card
///   (CR 400.11a), and a Limited designation is one more such card. Every
///   eligible candidate is offered, so an ineligible designation (a Lurrus whose
///   deck took on a three-drop between games) never hides an eligible sideboard
///   companion. A name offered from the designated slot is not offered again
///   from the sideboard.
fn companion_offers(
    pool: &PlayerDeckPool,
    companion_slot: CompanionSlot,
    sideboard_policy: SideboardPolicy,
    uses_commander: bool,
    min_deck_size: usize,
) -> Vec<CompanionRevealChoice> {
    let starting =
        companion_starting_deck(&pool.current_main, &pool.current_commander, uses_commander);
    let eligible = |entry: &DeckEntry| {
        is_eligible_companion_with_min_deck_size(
            entry,
            &starting,
            &pool.current_commander,
            uses_commander,
            min_deck_size,
        )
    };

    let mut offers: Vec<CompanionRevealChoice> = pool
        .current_companion
        .first()
        .filter(|_| companion_slot != CompanionSlot::SideboardOnly)
        .filter(|entry| eligible(entry))
        .map(|entry| CompanionRevealChoice {
            name: entry.card.name.clone(),
            source: CompanionChoiceSource::Dedicated,
        })
        .into_iter()
        .collect();

    let sideboard_offers = match companion_slot {
        CompanionSlot::CommanderFamily => false,
        CompanionSlot::SideboardOnly | CompanionSlot::LimitedPool => {
            !matches!(sideboard_policy, SideboardPolicy::Forbidden)
        }
    };
    if sideboard_offers {
        for (index, entry) in pool.current_sideboard.iter().enumerate() {
            if offers.iter().any(|offer| {
                offer.source == CompanionChoiceSource::Dedicated && offer.name == entry.card.name
            }) || !eligible(entry)
            {
                continue;
            }
            offers.push(CompanionRevealChoice {
                name: entry.card.name.clone(),
                source: CompanionChoiceSource::Sideboard { index },
            });
        }
    }
    offers
}

/// CR 100.4b + CR 702.139a: closes the pregame companion reveal window. A
/// Limited designation that was not revealed from its slot (declined, never
/// offered, or passed over for a sideboard companion) is an ordinary pool card
/// outside the deck, i.e. a sideboard card (CR 400.11a), for the rest of the
/// game, where outside-the-game effects can find it. A revealed designation
/// already left the slot when it was revealed.
pub(crate) fn close_companion_reveals(state: &mut GameState) {
    if state.format_config.companion_slot() != CompanionSlot::LimitedPool {
        return;
    }
    for pool in &mut state.deck_pools {
        if pool.current_companion.is_empty() {
            continue;
        }
        let released = std::mem::take(std::sync::Arc::make_mut(&mut pool.current_companion));
        let sideboard = std::sync::Arc::make_mut(&mut pool.current_sideboard);
        for entry in released {
            deck_loading::add_deck_copies(sideboard, &entry, entry.count);
        }
    }
}

/// CR 702.139a: Check if a player has eligible companions from their permitted
/// outside-game source.
/// Returns CompanionReveal WaitingFor if eligible companions exist, otherwise None.
pub fn check_companion_reveal(state: &GameState, player: PlayerId) -> Option<WaitingFor> {
    let pool = state.deck_pools.iter().find(|p| p.player == player)?;
    let min_deck_size = state.format_config.deck_size.min_cards() as usize;
    let mut eligible = companion_offers(
        pool,
        state.format_config.companion_slot(),
        state.format_config.sideboard_policy,
        state.format_config.uses_commander,
        min_deck_size,
    );
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
        if !super::players::is_alive(state, player_id) {
            continue;
        }
        if let Some(wf) = check_companion_reveal(state, player_id) {
            return Some(wf);
        }
    }
    None
}

/// CR 702.139a: Handle companion declaration or decline.
/// `eligible_companions` is the pre-computed list from `WaitingFor::CompanionReveal`,
/// ensuring the typed response exactly matches a card that was presented.
/// Returns the next WaitingFor state (next player's reveal or the agenda step).
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
        let min_deck_size = state.format_config.deck_size.min_cards() as usize;
        let starting = companion_starting_deck(
            &pool.current_main,
            &pool.current_commander,
            state.format_config.uses_commander,
        );
        let (selected, printing) = match choice.source {
            CompanionChoiceSource::Sideboard { index } => {
                let entry = pool
                    .current_sideboard
                    .get(index)
                    .filter(|entry| entry.card.name == choice.name)
                    .cloned()
                    .ok_or_else(|| "Companion sideboard offer is stale".to_string())?;
                if !is_eligible_companion_with_min_deck_size(
                    &entry,
                    &starting,
                    &pool.current_commander,
                    state.format_config.uses_commander,
                    min_deck_size,
                ) {
                    return Err("Companion is no longer eligible".to_string());
                }
                // Read before the removal below: the ordinal counts copies already taken.
                let printing = deck_loading::outside_game_copy_printing(
                    pool,
                    &state.outside_game_cards_brought_in,
                    DeckSection::Sideboard,
                    &entry.card,
                );
                let sideboard = std::sync::Arc::make_mut(&mut pool.current_sideboard);
                if sideboard[index].count > 1 {
                    sideboard[index].count -= 1;
                } else {
                    sideboard.remove(index);
                }
                (entry, printing)
            }
            CompanionChoiceSource::Dedicated => {
                let entry = pool
                    .current_companion
                    .first()
                    .filter(|entry| entry.card.name == choice.name)
                    .cloned()
                    .ok_or_else(|| "Dedicated companion offer is stale".to_string())?;
                if state.format_config.companion_slot() == CompanionSlot::SideboardOnly
                    || !is_eligible_companion_with_min_deck_size(
                        &entry,
                        &starting,
                        &pool.current_commander,
                        state.format_config.uses_commander,
                        min_deck_size,
                    )
                {
                    return Err("Companion is no longer eligible".to_string());
                }
                let printing = deck_loading::outside_game_copy_printing(
                    pool,
                    &state.outside_game_cards_brought_in,
                    DeckSection::Companion,
                    &entry.card,
                );
                std::sync::Arc::make_mut(&mut pool.current_companion).clear();
                (entry, printing)
            }
        };

        let player_data = state
            .players
            .iter_mut()
            .find(|entry| entry.id == player)
            .ok_or_else(|| "Player does not exist".to_string())?;
        // CR 702.139a: the revealed card's printing comes from the source it was revealed from.
        player_data.companion = Some(CompanionInfo {
            card: DeckEntry {
                card: selected.card,
                count: 1,
            },
            used: false,
            printing: DisplayOnly::new(printing),
        });

        events.push(GameEvent::CompanionRevealed {
            player,
            card_name: choice.name,
        });
    }

    // Advance to next player's companion reveal in seat order
    Ok(advance_companion_reveal(state, player, events))
}

/// CR 702.139a + CR 103.2b: Start the pregame companion-reveal step. If any
/// living player has an eligible companion, offer the first reveal; otherwise
/// hand off to the conspiracy agenda step, which hands off to mulligans.
pub(super) fn companion_reveals_or_agenda(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    check_all_companion_reveals(state).unwrap_or_else(|| {
        // CR 100.4b + CR 702.139a: no reveal is offered, so the window closes
        // now and any unrevealed Limited designation returns to the sideboard.
        close_companion_reveals(state);
        super::conspiracy::agenda_choices_or_mulligan(state, events)
    })
}

/// CR 702.139a: Move past `current_player` to the next living player's
/// companion reveal, or start the agenda step (CR 103.2e) once the walk is done.
///
/// CR 800.4a: `elimination::prune_mulligan_pending` resumes a departed chooser's
/// prompt here, so the walk continues from that chooser's seat cursor — later
/// living seats still get their offers, and a companion a living player already
/// declined behind the cursor is never re-offered (unlike the fresh
/// `check_all_companion_reveals` walk).
pub(super) fn advance_companion_reveal(
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
        if !super::players::is_alive(state, player_id) {
            continue;
        }
        if let Some(wf) = check_companion_reveal(state, player_id) {
            return wf;
        }
    }

    // All players done — close the reveal window (CR 100.4b + CR 702.139a),
    // then proceed to the conspiracy agenda step, then mulligans.
    close_companion_reveals(state);
    super::conspiracy::agenda_choices_or_mulligan(state, events)
}

// ── Revealed companion as a card outside the game ──────────────────────

/// CR 702.139a + CR 400.11: the player's revealed companion while it is still
/// outside the game. Revealing a companion does not bring it into the game
/// (it is not in any zone), so it stays "a card you own from outside the game"
/// until it is put into hand (CR 702.139c) — by the special action
/// (`finish_paid_companion_to_hand`) or by an effect that finds it from outside
/// the game ([`bring_revealed_companion_into_game`]); both set `used`. The
/// single authority for what a player's companion contributes to "outside the
/// game".
///
/// CR 903.11 + CR 702.139d: a Commander-family game admits a card from outside
/// the game only through rules, special actions, and effects that specifically
/// bring cards in, and the companion special action is the sole way this card
/// enters. An effect that merely finds "a card you own from outside the game"
/// (Living Wish) does not function there (Commander rule 10), so in that slot
/// the companion contributes nothing to such effects. The special action never
/// reads this function (`validate_companion_to_hand_action`), so it is
/// unaffected.
pub(crate) fn outside_game_companion(
    state: &GameState,
    player: PlayerId,
) -> Option<&CompanionInfo> {
    if state.format_config.companion_slot() == CompanionSlot::CommanderFamily {
        return None;
    }
    state
        .players
        .iter()
        .find(|entry| entry.id == player)?
        .companion
        .as_ref()
        .filter(|companion| !companion.used)
}

/// CR 400.11b + CR 702.139c: an effect that finds cards from outside the game
/// (Living Wish) brings the revealed companion into `destination`. It remains
/// in the game from then on, so the companion is marked used: it is no longer
/// outside the game, and the special action can no longer put it into hand.
/// CR 203.1 + CR 206.1: the created object carries the printing captured at the
/// reveal (display-only).
pub(crate) fn bring_revealed_companion_into_game(
    state: &mut GameState,
    player: PlayerId,
    destination: Zone,
) -> Result<crate::types::identifiers::ObjectId, EngineError> {
    let (card_face, printing) = outside_game_companion(state, player)
        .map(|companion| {
            (
                companion.card.card.clone(),
                companion.printing.get().clone(),
            )
        })
        .ok_or_else(|| {
            EngineError::InvalidAction("Companion is not outside the game".to_string())
        })?;
    // CR 407.3: refused here as at every other outside-the-game source.
    let object_id = super::effects::search_outside_game::put_outside_game_face_into(
        state,
        player,
        &card_face,
        destination,
    )
    .ok_or_else(|| {
        EngineError::InvalidAction(format!(
            "{} can't be brought into the game from outside the game (CR 407.3)",
            card_face.name
        ))
    })?;
    if let Some(companion) = state
        .players
        .iter_mut()
        .find(|entry| entry.id == player)
        .and_then(|entry| entry.companion.as_mut())
    {
        companion.used = true;
    }
    deck_loading::stamp_created_card_shared(state, object_id, printing);
    Ok(object_id)
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
            // Live payment carries `ManaAbilityResume::CompanionToHand`.
            casting::PausedManaPayment::Resumable,
        )
}

/// Returns the finite Companion special action for the current Priority holder
/// when the existing legality and payment authority admits it.
pub(in crate::game) fn priority_companion_announcement(
    state: &GameState,
    principal: &PriorityPrincipal,
) -> Option<PriorityCompanionAnnouncement> {
    can_activate_companion(state, principal.semantic_holder())
        .then(PriorityCompanionAnnouncement::new)
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
/// CR 203.1 + CR 206.1: the created object carries the printing captured at the reveal;
/// the stamp is display-only.
pub(crate) fn finish_paid_companion_to_hand(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let companion = state.players[player.0 as usize]
        .companion
        .as_ref()
        .expect("a paid companion continuation retains its declared companion");
    let card_face = companion.card.card.clone();
    let printing = companion.printing.get().clone();
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
    deck_loading::stamp_created_card_shared(state, obj_id, printing);

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
        // Live payment carries `ManaAbilityResume::CompanionToHand`.
        casting::PausedManaPayment::Resumable,
    ) {
        return Err(EngineError::ActionNotAllowed(
            "Cannot pay companion cost".to_string(),
        ));
    }

    match pay_companion_to_hand_cost(state, player, &cost, events)? {
        SpecialActionManaPayment::Paid => Ok(finish_paid_companion_to_hand(state, player, events)),
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
        SpecialActionManaPayment::Paid => Ok(finish_paid_companion_to_hand(state, player, events)),
        SpecialActionManaPayment::Paused => Ok(state.waiting_for.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType, Supertype};
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
    fn min_deck_size_over_limited_valid() {
        // In 40-card Limited, Yorion (+20) requires 60 cards
        let deck = vec![DeckEntry {
            card: CardFace {
                name: "Plains".to_string(),
                card_type: CardType {
                    supertypes: vec![Supertype::Basic],
                    core_types: vec![CoreType::Land],
                    subtypes: vec![],
                },
                ..Default::default()
            },
            count: 60,
        }];
        assert!(validate_companion_condition_with_min_deck_size(
            &CompanionCondition::MinDeckSizeOver(20),
            &deck,
            40,
        ));
    }

    #[test]
    fn min_deck_size_over_limited_invalid() {
        // 59 cards is not enough for Yorion in 40-card Limited (needs 60)
        let deck = vec![DeckEntry {
            card: CardFace {
                name: "Plains".to_string(),
                card_type: CardType {
                    supertypes: vec![Supertype::Basic],
                    core_types: vec![CoreType::Land],
                    subtypes: vec![],
                },
                ..Default::default()
            },
            count: 59,
        }];
        assert!(!validate_companion_condition_with_min_deck_size(
            &CompanionCondition::MinDeckSizeOver(20),
            &deck,
            40,
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

    fn costed(name: &str, shards: Vec<crate::types::mana::ManaCostShard>) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                mana_cost: ManaCost::Cost { shards, generic: 0 },
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![],
                },
                ..Default::default()
            },
            count: 1,
        }
    }

    /// CR 702.139a (Jegantha): every mana symbol counts, so a repeated hybrid,
    /// {X} or {C} symbol fails the condition, while two distinct hybrid symbols
    /// and a hybrid beside its own colour (different symbols) pass.
    #[test]
    fn no_repeated_mana_symbols_counts_hybrid_x_and_colorless_symbols() {
        use crate::types::mana::ManaCostShard::{Colorless, Red, RedWhite, X};
        let passes = |shards| {
            validate_companion_condition(
                &CompanionCondition::NoRepeatedManaSymbols,
                &[costed("Probe", shards)],
            )
        };
        assert!(
            !passes(vec![RedWhite, RedWhite, RedWhite]),
            "Boros Reckoner"
        );
        assert!(!passes(vec![X, X, Red]), "{{X}}{{X}}{{R}}");
        assert!(!passes(vec![Colorless, Colorless]));
        assert!(
            passes(vec![RedWhite, Red]),
            "a hybrid is not its own colour"
        );
        assert!(passes(vec![
            RedWhite,
            crate::types::mana::ManaCostShard::GreenWhite
        ]));
        assert!(passes(vec![X, Red]));
    }

    /// CR 702.139a + CR 305.6 (Zirda): a land card is a permanent card, so it
    /// needs an activated ability: a basic land type's intrinsic mana ability
    /// or a printed one. A utility land with neither fails.
    #[test]
    fn permanents_have_activated_abilities_covers_land_cards() {
        let land_with = |name: &str, subtypes: Vec<&str>| DeckEntry {
            card: CardFace {
                name: name.to_string(),
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Land],
                    subtypes: subtypes.into_iter().map(str::to_string).collect(),
                },
                ..Default::default()
            },
            count: 1,
        };
        let verdict = |land| {
            validate_companion_condition(
                &CompanionCondition::PermanentsHaveActivatedAbilities,
                &[land],
            )
        };
        assert!(verdict(land_with("Plains", vec!["Plains"])), "CR 305.6");
        assert!(!verdict(land_with("Glacial Chasm", vec![])));
        let mut tapland = land_with("Evolving Wilds", vec![]);
        tapland
            .card
            .abilities
            .push(crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Activated,
                crate::types::ability::Effect::unimplemented("probe", "probe"),
            ));
        assert!(verdict(tapland), "a printed activated ability qualifies");
    }

    #[test]
    fn commander_starting_deck_includes_commander_but_not_companion() {
        let main = vec![creature("Main Card", 2, vec![])];
        let commanders = vec![creature("Commander", 3, vec![])];

        let commander_starting = companion_starting_deck(&main, &commanders, true);
        assert_eq!(
            commander_starting
                .iter()
                .map(|entry| entry.card.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Main Card", "Commander"]
        );

        let standard_starting = companion_starting_deck(&main, &commanders, false);
        assert_eq!(
            standard_starting
                .iter()
                .map(|entry| entry.card.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Main Card"]
        );
    }

    /// CR 709.4b + CR 702.139a: Jegantha reads every half's symbols. Each half's
    /// generic amount is ONE symbol: {1} and {1} repeat, {1} and {2} do not.
    #[test]
    fn jegantha_counts_each_split_half_generic_amount_as_one_symbol() {
        use crate::types::mana::ManaCostShard;
        let half = |shard: ManaCostShard, generic: u32| ManaCost::Cost {
            shards: vec![shard],
            generic,
        };
        let split = |halves: Vec<ManaCost>| {
            let mut entry = creature("Split", 4, vec![]);
            entry.card.metadata.off_stack_mana_costs = halves;
            entry
        };
        let repeats = |halves| has_repeated_mana_symbols(&split(halves));

        assert!(
            repeats(vec![
                half(ManaCostShard::Red, 1),
                half(ManaCostShard::Blue, 1)
            ]),
            "{{1}}{{R}} and {{1}}{{U}} contain {{1}} twice"
        );
        assert!(
            !repeats(vec![
                half(ManaCostShard::Red, 1),
                half(ManaCostShard::Blue, 2)
            ]),
            "{{2}} is one symbol, not two {{1}}s"
        );
        assert!(
            repeats(vec![
                half(ManaCostShard::Red, 0),
                half(ManaCostShard::Red, 3)
            ]),
            "a shard repeated across halves repeats"
        );
        assert!(
            !repeats(vec![
                half(ManaCostShard::Red, 1),
                half(ManaCostShard::White, 0)
            ]),
            "halves sharing no symbol do not repeat"
        );
    }
}
