pub mod bracket_lists;
pub mod card_db;
pub mod embalm_eternalize;
#[cfg(feature = "forge")]
pub mod forge;
pub mod legality;
pub mod mtgjson;
pub mod oracle_loader;
pub mod search;
pub mod synthesis;
pub mod unearth;

#[cfg(test)]
mod embalm_eternalize_tests;
#[cfg(test)]
mod unearth_tests;

pub use bracket_lists::{BracketLists, BracketSignals};
pub use card_db::CardDatabase;
pub use search::{CardSearchQuery, CardSearchResult, CardSearchResults};

/// Single authority for "is this card runnable by the engine right now?"
///
/// Used by the deck ingestion subcommand to filter preconstructed decks and by
/// any other tooling that needs the same guarantee the runtime face index
/// provides. Vanilla creatures and basic lands (zero abilities) are playable;
/// cards with abilities are playable only when none of those abilities contain
/// an `Effect::Unimplemented` or other unimplemented-marker part.
pub fn is_card_playable(db: &CardDatabase, name: &str) -> bool {
    match db.get_face_by_name(name) {
        Some(face) => !crate::game::coverage::card_face_has_unimplemented_parts(face),
        None => false,
    }
}
