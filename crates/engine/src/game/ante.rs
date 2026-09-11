//! CR 407: ante. The single authority for the ante rule's two enforced
//! questions — which cards belong to the class, and whether this game admits
//! them.
//!
//! CR 407.1 makes playing for ante "an optional variation on the game",
//! "strictly forbidden under the Magic: The Gathering Tournament Rules", and
//! this engine implements no part of the CR 407.2 ante zone or the CR 407.4
//! ante action. So [`AntePolicy::Excluded`] is the operative policy for every
//! game it can currently host, and CR 407.3's prohibitions are real
//! restrictions rather than schema.
//!
//! CR 407.3 states three of them, and they are not all in one place in the
//! codebase, which is why this module exists rather than a predicate sitting
//! next to whichever caller needed it first:
//!
//! 1. cards may not be in a **deck** — `game::deck_validation`;
//! 2. cards may not be in a **sideboard** — same;
//! 3. cards may not be **brought into the game from outside the game** —
//!    [`admits_face_from_outside_game`], applied at the offer sites and at
//!    `effects::search_outside_game::put_outside_game_face_into`, the single
//!    materialization authority both outside-game sources funnel through.
//!
//! The third is the one a card list would never have covered: a booster pack
//! opened mid-game (Booster Tutor and friends) draws from a set's whole card
//! pool, not from anything the deck-construction rules already vetted.
//!
//! **All three read [`policy_of`], and 1–2 are enforced once for every format
//! rather than inside any single card-pool authority.** The deck half cannot
//! live in `DeckValidation`'s `CardPoolAuthority`: that seam is reached only by
//! constructed-shaped formats, while commander-shaped ones use their own
//! validator and FreeForAll / TwoHeadedGiant / Limited impose no card-pool
//! check at all. CR 407.3 is not a card-pool restriction that a permissive
//! format may waive — it holds whenever the game is not played for ante — so it
//! belongs beside the other cross-format rules at the dispatch seam.

use crate::types::card::CardFace;
use crate::types::custom_format::AntePolicy;
use crate::types::format::FormatConfig;
use crate::types::game_state::GameState;

/// CR 407.3: "A few cards have the text 'Remove this card from your deck
/// before playing if you're not playing for ante.' These are the only cards
/// that can add or remove cards from the ante zone or change a card's owner."
///
/// The rule identifies the class by the cards' own printed text, so this
/// predicate matches that text rather than a hardcoded roster — a name list
/// would be a snapshot that silently misses anything else printed with the
/// clause, and would have to be repeated by every rule that excludes them.
///
/// Matches the substring `playing for ante`, the templated clause's stable
/// core (unaffected by the "Remove this card"/"Remove ~ from your deck"
/// wording differences across printings). Verified exact at implementation
/// time: Scryfall's `oracle:"playing for ante"` returns 9 cards — Amulet of
/// Quoz, Bronze Tablet, Contract from Below, Darkpact, Demonic Attorney,
/// Jeweled Bird, Rebirth, Tempest Efreet, Timmerian Fiends — which is the
/// whole class with no false positives.
pub(crate) fn face_uses_ante(face: &CardFace) -> bool {
    face.oracle_text
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase()
        .contains("playing for ante")
}

/// The ante policy a format declares.
///
/// A built-in format carries no `custom_rules`, and `unwrap_or_default()`
/// resolves it to [`AntePolicy::Excluded`] — which is not a fallback so much
/// as the rule: CR 407 is not a custom-format concept, and no built-in format
/// in this engine plays for ante. [`AntePolicy::Enabled`] is reachable only by
/// a custom format declaring it, which `passes_legacy_axis_gate` refuses today
/// precisely because the ante zone does not exist yet.
///
/// Takes the `FormatConfig` rather than the `GameState` because deck admission
/// runs before any game exists — `deck_validation` holds only the resolved
/// rules. Both callers must read the same value or the deck gate and the
/// in-game gate could disagree about the very same match.
pub(crate) fn policy_of(format_config: &FormatConfig) -> AntePolicy {
    format_config
        .custom_rules
        .as_deref()
        .map(|rules| rules.legality.legacy.ante)
        .unwrap_or_default()
}

/// The ante policy the in-progress game is played under.
pub(crate) fn policy(state: &GameState) -> AntePolicy {
    policy_of(&state.format_config)
}

/// CR 407.3: "When not playing for ante, players can't include these cards in
/// their decks or sideboards, and these cards can't be brought into the game
/// from outside the game." This answers the last clause.
///
/// Fails CLOSED by construction: the only policy that admits the class is an
/// explicit [`AntePolicy::Enabled`], so any future outside-game source that
/// routes through the materialization authority is covered without being
/// taught the rule.
pub(crate) fn admits_face_from_outside_game(state: &GameState, face: &CardFace) -> bool {
    match policy(state) {
        // CR 407.2: playing for ante makes the class legal again.
        AntePolicy::Enabled => true,
        AntePolicy::Excluded => !face_uses_ante(face),
    }
}
