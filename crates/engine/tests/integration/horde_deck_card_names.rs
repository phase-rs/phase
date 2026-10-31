//! Every Horde challenge deck's non-token card names must resolve in the card DB.
//!
//! `deck_loading::load_horde_library` PANICS on a name it cannot resolve — a
//! fixed engine resource with a typo would take the game down at load rather than
//! silently shipping a short library. That makes the decklists' spelling
//! load-bearing, and a transcription slip is the single most likely way to break
//! a deck (the lists are hand-copied from hordemagic.com).
//!
//! This is the class-level guard: it walks `ChallengeDeck::ALL` rather than
//! naming decks one at a time, so a NEW deck is covered the moment it is added —
//! no per-deck test to remember. `deck_nontoken_cards` below is an exhaustive
//! match, so adding a `ChallengeDeck` variant fails to compile until its list is
//! wired in here too.
//!
//! Loads the committed fixture via `support::shared_card_db` (regenerated with
//! `python3 scripts/gen-test-fixture.py`, which scans `src/game/decks/`), so
//! these checks run in CI instead of self-skipping on the gitignored export.

use crate::support::shared_card_db;
use engine::game::deck_loading::load_horde_library;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::game_state::GameState;

/// The curated fixture card database. It always exists: the fixture is
/// committed, and `gen-test-fixture.py` covers every Horde deck card.
fn fixture_db() -> &'static engine::database::card_db::CardDatabase {
    shared_card_db().expect("the checked-in fixture contains the Horde deck cards")
}

/// Each deck's non-token `(count, name)` list. Exhaustive match: a new
/// `ChallengeDeck` variant will not compile until it is listed here.
fn deck_nontoken_cards(deck: ChallengeDeck) -> &'static [(u32, &'static str)] {
    use engine::game::decks;
    match deck {
        ChallengeDeck::CybermanHorde => decks::cyberman_horde::CYBERMAN_HORDE_NONTOKEN_CARDS,
        ChallengeDeck::DndHorde => decks::dnd_horde::DND_OOZE_NONTOKEN_CARDS,
        ChallengeDeck::ZombiesHorde => decks::zombies_horde::ZOMBIES_HORDE_NONTOKEN_CARDS,
        ChallengeDeck::SliversHorde => decks::slivers_horde::SLIVERS_HORDE_NONTOKEN_CARDS,
        ChallengeDeck::HumansGodzillaHorde => {
            decks::humans_godzilla_horde::HUMANS_GODZILLA_HORDE_NONTOKEN_CARDS
        }
        ChallengeDeck::SauronHorde => decks::sauron_horde::SAURON_HORDE_NONTOKEN_CARDS,
        ChallengeDeck::SarumanHorde => decks::saruman_horde::SARUMAN_HORDE_NONTOKEN_CARDS,
        // The combined "Two Towers" pairing loads Sauron's library at seat 0 (its
        // co-Horde Saruman library is covered by the `SarumanHorde` arm above), so
        // this name-resolution guard checks the same seat-0 list `load_horde_library`
        // resolves for it.
        ChallengeDeck::LotrTwoTowersHorde => decks::sauron_horde::SAURON_HORDE_NONTOKEN_CARDS,
        // Turtle Team-Up fields no card-DB cards (every enemy-deck card is
        // token-layout on Scryfall): an empty list, whose pile sizes the
        // token/boss helpers below pin instead.
        ChallengeDeck::TurtleTeamUpHorde => {
            decks::turtle_teamup_horde::TURTLE_TEAMUP_NONTOKEN_CARDS
        }
    }
}

/// Each deck's token `(count, selector)` list. Exhaustive like
/// `deck_nontoken_cards` above: a new `ChallengeDeck` variant will not compile
/// until it is listed here too.
fn deck_token_cards(deck: ChallengeDeck) -> &'static [(u32, &'static str)] {
    use engine::game::decks;
    match deck {
        ChallengeDeck::CybermanHorde => decks::cyberman_horde::CYBERMAN_HORDE_TOKENS,
        ChallengeDeck::DndHorde => decks::dnd_horde::DND_OOZE_TOKENS,
        ChallengeDeck::ZombiesHorde => decks::zombies_horde::ZOMBIES_HORDE_TOKENS,
        ChallengeDeck::SliversHorde => decks::slivers_horde::SLIVERS_HORDE_TOKENS,
        ChallengeDeck::HumansGodzillaHorde => {
            decks::humans_godzilla_horde::HUMANS_GODZILLA_HORDE_TOKENS
        }
        ChallengeDeck::SauronHorde => decks::sauron_horde::SAURON_HORDE_TOKENS,
        ChallengeDeck::SarumanHorde => decks::saruman_horde::SARUMAN_HORDE_TOKENS,
        ChallengeDeck::LotrTwoTowersHorde => decks::sauron_horde::SAURON_HORDE_TOKENS,
        ChallengeDeck::TurtleTeamUpHorde => {
            decks::turtle_teamup_horde::TURTLE_TEAMUP_EVENT_DECK_TOKENS
        }
    }
}

/// Each deck's Boss-deck `(count, preset-id)` list, if it stages Bosses.
/// Exhaustive (explicit `None`s, not a wildcard) so a future boss-deck game
/// fails to compile until its pile is pinned here.
fn deck_boss_cards(deck: ChallengeDeck) -> Option<&'static [(u32, &'static str)]> {
    use engine::game::decks;
    match deck {
        ChallengeDeck::CybermanHorde => None,
        ChallengeDeck::DndHorde => None,
        ChallengeDeck::ZombiesHorde => None,
        ChallengeDeck::SliversHorde => None,
        ChallengeDeck::HumansGodzillaHorde => None,
        ChallengeDeck::SauronHorde => None,
        ChallengeDeck::SarumanHorde => None,
        ChallengeDeck::LotrTwoTowersHorde => None,
        ChallengeDeck::TurtleTeamUpHorde => {
            Some(decks::turtle_teamup_horde::TURTLE_TEAMUP_BOSS_DECK_TOKENS)
        }
    }
}

#[test]
fn every_horde_deck_nontoken_name_resolves_in_the_card_db() {
    let db = fixture_db();

    let mut failures: Vec<String> = Vec::new();
    for &deck in ChallengeDeck::ALL {
        for (_, name) in deck_nontoken_cards(deck) {
            if db.get_face_by_name(name).is_none() {
                failures.push(format!("{deck:?}: '{name}'"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "these Horde deck card names do not resolve in the card DB and would panic \
         at game load:\n  {}",
        failures.join("\n  ")
    );
}

/// Guards the guard: if the card DB loaded but somehow matched nothing, the test
/// above would pass vacuously. Assert a known-good name really does resolve.
#[test]
fn card_db_lookup_is_actually_working() {
    let db = fixture_db();
    assert!(
        db.get_face_by_name("Island").is_some(),
        "positive control: a basic land must resolve, otherwise the name-resolution \
         check above proves nothing"
    );
}

/// The D&D Horde's rarity-based wave depends on each non-token library card
/// carrying a rarity. The runtime `card-data.json` export does NOT include rarity
/// (`CardFace::rarities` is empty), so the rarity is PINNED in the deck and
/// threaded through `create_horde_library_card`. This drives the REAL library
/// load and asserts EVERY non-token card ends up with a rarity — the wiring
/// guard for the live bug where a rarity-less wave revealed the entire library
/// every turn.
#[test]
fn dnd_horde_library_cards_all_carry_a_rarity() {
    let db = fixture_db();
    let mut state = GameState::new(
        FormatConfig::horde(ChallengeDeck::DndHorde.default_ruleset()),
        2,
        42,
    );
    let horde = state.format_config.archenemy_player().expect("Horde seat");
    load_horde_library(&mut state, db, horde, ChallengeDeck::DndHorde);

    let library: Vec<_> = state
        .players
        .iter()
        .find(|p| p.id == horde)
        .expect("horde player")
        .library
        .iter()
        .copied()
        .collect();

    let mut nontoken_cards = 0usize;
    let mut with_rarity = 0usize;
    let mut uncommon_or_better = 0usize;
    for id in &library {
        let obj = state.objects.get(id).expect("library object");
        if obj.is_token {
            continue;
        }
        nontoken_cards += 1;
        if let Some(r) = obj.horde_library_rarity {
            with_rarity += 1;
            if r >= engine::types::card::Rarity::Uncommon {
                uncommon_or_better += 1;
            }
        }
    }

    assert_eq!(
        nontoken_cards, 62,
        "D&D Ooze library has 62 non-token cards"
    );
    assert_eq!(
        with_rarity, nontoken_cards,
        "EVERY non-token D&D library card must carry a rarity — a rarity-less card \
         makes the UntilRarityAtLeast wave reveal the whole library"
    );
    assert!(
        uncommon_or_better > 0,
        "the library must contain uncommon-or-better cards so waves can terminate"
    );
}

/// Every deck must actually have cards — an empty list would make the
/// name-resolution test above pass trivially for that deck.
#[test]
fn every_horde_deck_has_a_nonempty_decklist() {
    for &deck in ChallengeDeck::ALL {
        let nontokens: u32 = deck_nontoken_cards(deck)
            .iter()
            .map(|(count, _)| *count)
            .sum();
        let tokens: u32 = deck_token_cards(deck).iter().map(|(count, _)| *count).sum();
        match deck_boss_cards(deck) {
            // Boss-deck games field a small event-deck library plus a Boss
            // pile, not a ~100-card mill-race library: pin both exact
            // published counts (Turtle Team-Up: 27 + 11 = 38).
            Some(bosses) => {
                let boss_total: u32 = bosses.iter().map(|(count, _)| *count).sum();
                assert_eq!(tokens, 27, "{deck:?} event deck must be exactly 27 cards");
                assert_eq!(
                    boss_total, 11,
                    "{deck:?} Boss deck must be exactly 11 cards"
                );
            }
            None => {
                assert!(
                    !deck_nontoken_cards(deck).is_empty(),
                    "{deck:?} has an empty non-token decklist"
                );
                assert!(
                    nontokens >= 50,
                    "{deck:?} has only {nontokens} non-token cards — Horde libraries are ~100-150"
                );
            }
        }
    }
}
