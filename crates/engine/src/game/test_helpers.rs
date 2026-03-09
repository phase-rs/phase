//! Reusable test helpers for loading Forge card definitions and spawning game objects.
//!
//! These helpers load the Forge card database lazily from the local filesystem
//! and gracefully return `None` when the database is unavailable (CI-safe).

use std::path::Path;
use std::sync::OnceLock;

use crate::database::CardDatabase;
use crate::game::deck_loading::derive_colors_from_mana_cost;
use crate::game::keywords::parse_keywords;
use crate::game::zones::create_object;
use crate::types::card::{CardFace, CardLayout, CardRules};
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

static FORGE_DB: OnceLock<Option<CardDatabase>> = OnceLock::new();

/// Extract the primary (front) face from any card layout.
fn primary_face(layout: &CardLayout) -> &CardFace {
    match layout {
        CardLayout::Single(face)
        | CardLayout::Split(face, _)
        | CardLayout::Flip(face, _)
        | CardLayout::Transform(face, _)
        | CardLayout::Meld(face, _)
        | CardLayout::Adventure(face, _)
        | CardLayout::Modal(face, _)
        | CardLayout::Omen(face, _)
        | CardLayout::Specialize(face, _) => face,
    }
}

/// Path to the Forge card definitions directory relative to the engine crate root.
const FORGE_CARDS_PATH: &str = "../../forge/forge-gui/res/cardsfolder/";

/// Returns a reference to the lazily-loaded Forge card database, or `None` if
/// the Forge data directory is not available on disk.
pub fn forge_db() -> Option<&'static CardDatabase> {
    FORGE_DB
        .get_or_init(|| {
            let path = Path::new(FORGE_CARDS_PATH);
            if !path.exists() {
                return None;
            }
            CardDatabase::load(path).ok()
        })
        .as_ref()
}

/// Look up a card by name from the Forge database.
/// Returns `None` if the database is unavailable or the card is not found.
pub fn load_card(name: &str) -> Option<&'static CardRules> {
    forge_db()?.get_by_name(name)
}

/// Spawn a creature on the battlefield from a Forge card definition.
///
/// Loads the card by name, creates a `GameObject` on the battlefield with
/// the card's types, keywords, power/toughness, and color populated.
/// Returns `None` if the Forge DB is unavailable or the card is not found.
pub fn spawn_creature(state: &mut GameState, name: &str, owner: PlayerId) -> Option<ObjectId> {
    let card = load_card(name)?;
    let face = primary_face(&card.layout);

    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, face.name.clone(), Zone::Battlefield);

    let obj = state.objects.get_mut(&id)?;

    // Set card types
    obj.card_types = face.card_type.clone();

    // Set keywords (CardFace stores keywords as strings; parse to typed Keyword values)
    obj.keywords = parse_keywords(&face.keywords);

    // Set power/toughness
    obj.power = face
        .power
        .as_ref()
        .and_then(|s: &String| s.parse::<i32>().ok());
    obj.toughness = face
        .toughness
        .as_ref()
        .and_then(|s: &String| s.parse::<i32>().ok());

    // Set color: use explicit color_override if present, otherwise derive from mana cost
    let color = face
        .color_override
        .clone()
        .unwrap_or_else(|| derive_colors_from_mana_cost(&face.mana_cost));
    obj.color = color.clone();
    obj.base_color = color;

    // Set mana cost
    obj.mana_cost = face.mana_cost.clone();

    // Mark as entered previous turn (not summoning sick)
    obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));

    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_db_loads_or_skips() {
        // This test always passes: it verifies the lazy loading doesn't panic
        let _db = forge_db();
    }

    #[test]
    fn load_card_returns_none_for_nonexistent() {
        // If DB is available, a nonsense name returns None
        // If DB is unavailable, also returns None
        assert!(load_card("ZZZZZ_NOT_A_CARD_99999").is_none());
    }

    #[test]
    fn spawn_creature_without_db_returns_none() {
        if forge_db().is_none() {
            let mut state = GameState::new_two_player(42);
            state.turn_number = 2;
            assert!(spawn_creature(&mut state, "Lightning Bolt", PlayerId(0)).is_none());
        }
    }
}
