//! cEDH decks from the TopDeck.gg tournaments API (v2).
//!
//! One `POST /v2/tournaments` call returns every completed EDH event in the
//! window, with final standings and structured decklists (`deckObj`). The feed
//! keeps the best finishes across those events: every event's winner before
//! any second place, larger events first within a placing.
//!
//! API docs: <https://topdeck.gg/docs/tournaments-v2>. The Developer Terms
//! require a visible "Data provided by TopDeck.gg" credit wherever the data is
//! shown; the client renders it from the feed registry's `attribution`.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;

use crate::feed::{DeckEntry, FeedDeck};

const TOURNAMENTS_URL: &str = "https://topdeck.gg/api/v2/tournaments";

pub struct TopdeckConfig {
    pub api_key: String,
    pub days: u32,
    pub min_players: u32,
    pub top_n: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tournament {
    #[serde(rename = "TID")]
    tid: String,
    tournament_name: String,
    /// Final standings, first place first.
    standings: Vec<Standing>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Standing {
    name: Option<String>,
    /// Absent when the player submitted no deck or the deck has no structured
    /// data.
    deck_obj: Option<DeckObj>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DeckObj {
    #[serde(default)]
    commanders: BTreeMap<String, CardCount>,
    #[serde(default)]
    mainboard: BTreeMap<String, CardCount>,
}

#[derive(Debug, Deserialize)]
struct CardCount {
    count: u32,
}

/// One decklisted finish, ranked by placing and event size.
struct Placing<'a> {
    standing: usize,
    event_size: usize,
    tournament: &'a Tournament,
    pilot: &'a str,
    deck: &'a DeckObj,
}

pub fn fetch_top_decks(
    client: &Client,
    config: &TopdeckConfig,
) -> Result<Vec<FeedDeck>, reqwest::Error> {
    let tournaments: Vec<Tournament> = client
        .post(TOURNAMENTS_URL)
        .header("Authorization", &config.api_key)
        .json(&json!({
            "game": "Magic: The Gathering",
            "format": "EDH",
            "last": config.days,
            "participantMin": config.min_players,
            // `decklist` is what makes the API include `deckObj`.
            "columns": ["name", "decklist"],
        }))
        .send()?
        .error_for_status()?
        .json()?;

    eprintln!("Fetched {} EDH tournaments", tournaments.len());
    Ok(select_top_decks(&tournaments, config.top_n))
}

fn select_top_decks(tournaments: &[Tournament], top_n: usize) -> Vec<FeedDeck> {
    let mut placings: Vec<Placing> = tournaments
        .iter()
        .flat_map(|tournament| {
            tournament
                .standings
                .iter()
                .enumerate()
                .filter_map(move |(index, standing)| {
                    let deck = standing.deck_obj.as_ref()?;
                    if deck.commanders.is_empty() || deck.mainboard.is_empty() {
                        return None;
                    }
                    Some(Placing {
                        standing: index + 1,
                        event_size: tournament.standings.len(),
                        tournament,
                        pilot: standing.name.as_deref().unwrap_or("Unknown Player"),
                        deck,
                    })
                })
        })
        .collect();

    placings.sort_by_key(|p| (p.standing, Reverse(p.event_size), &p.tournament.tid));

    // Saved decks are keyed by name. A recurring event can repeat its name
    // with the same winning commander, so a repeat takes its event's TID.
    let mut names = HashSet::new();
    placings
        .iter()
        .take(top_n)
        .map(|placing| {
            let mut deck = to_feed_deck(placing);
            if !names.insert(deck.name.clone()) {
                deck.name = format!("{} [{}]", deck.name, placing.tournament.tid);
            }
            deck
        })
        .collect()
}

fn to_feed_deck(placing: &Placing) -> FeedDeck {
    let commander: Vec<String> = placing.deck.commanders.keys().cloned().collect();
    let main: Vec<DeckEntry> = placing
        .deck
        .mainboard
        .iter()
        .map(|(name, card)| DeckEntry {
            count: card.count,
            name: name.clone(),
        })
        .collect();

    FeedDeck {
        // The event and placing keep two decks with the same commander apart.
        name: format!(
            "{} ({} #{})",
            commander.join(" / "),
            placing.tournament.tournament_name.trim(),
            placing.standing,
        ),
        author: placing.pilot.to_string(),
        // Left empty: the engine derives the deck's color identity from its
        // commanders, and the land-name guess `scrape::infer_colors` makes
        // misses most cEDH mana bases (fetches, duals, rocks).
        colors: Vec::new(),
        tags: vec!["cedh".to_string()],
        main,
        sideboard: Vec::new(),
        commander: Some(commander),
        companion: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tournament(tid: &str, standings: serde_json::Value) -> Tournament {
        serde_json::from_value(json!({
            "TID": tid,
            "tournamentName": format!("{tid} "),
            "standings": standings,
        }))
        .expect("valid tournament")
    }

    fn decklisted(pilot: &str, commander: &str) -> serde_json::Value {
        json!({
            "name": pilot,
            "deckObj": {
                "Commanders": { commander: { "id": "x", "count": 1 } },
                "Mainboard": {
                    "Sol Ring": { "id": "y", "count": 1 },
                    "Island": { "id": "z", "count": 3 },
                },
                "metadata": { "game": "Magic: The Gathering", "format": "EDH" },
            },
        })
    }

    #[test]
    fn ranks_winners_before_runners_up_and_larger_events_first() {
        let small = tournament(
            "small",
            json!([
                decklisted("A", "Kinnan, Bonder Prodigy"),
                decklisted("B", "Vivi Ornitier")
            ]),
        );
        let large = tournament(
            "large",
            json!([
                decklisted("C", "Tymna the Weaver"),
                decklisted("D", "Urza, Lord High Artificer"),
                { "name": "E" },
            ]),
        );

        let decks = select_top_decks(&[small, large], 3);
        let names: Vec<&str> = decks.iter().map(|d| d.name.as_str()).collect();

        assert_eq!(
            names,
            [
                "Tymna the Weaver (large #1)",
                "Kinnan, Bonder Prodigy (small #1)",
                "Urza, Lord High Artificer (large #2)",
            ]
        );
    }

    #[test]
    fn skips_standings_without_a_structured_deck() {
        let event = tournament(
            "event",
            json!([{ "name": "No Deck" }, decklisted("Pilot", "Vivi Ornitier")]),
        );

        let decks = select_top_decks(&[event], 16);

        assert_eq!(decks.len(), 1);
        assert_eq!(decks[0].name, "Vivi Ornitier (event #2)");
        assert_eq!(decks[0].author, "Pilot");
    }

    #[test]
    fn disambiguates_same_named_events_by_tid() {
        let event = |tid: &str| -> Tournament {
            serde_json::from_value(json!({
                "TID": tid,
                "tournamentName": "Weekly",
                "standings": [decklisted("Pilot", "Vivi Ornitier")],
            }))
            .expect("valid tournament")
        };

        let decks = select_top_decks(&[event("week-1"), event("week-2")], 16);
        let names: Vec<&str> = decks.iter().map(|d| d.name.as_str()).collect();

        assert_eq!(
            names,
            [
                "Vivi Ornitier (Weekly #1)",
                "Vivi Ornitier (Weekly #1) [week-2]"
            ]
        );
    }

    #[test]
    fn keeps_commanders_out_of_the_mainboard() {
        let event = tournament("event", json!([decklisted("Pilot", "Vivi Ornitier")]));

        let deck = &select_top_decks(&[event], 1)[0];

        assert_eq!(
            deck.commander.as_deref(),
            Some(&["Vivi Ornitier".to_string()][..])
        );
        assert!(deck.main.iter().all(|entry| entry.name != "Vivi Ornitier"));
        assert_eq!(deck.main.iter().map(|entry| entry.count).sum::<u32>(), 4);
    }
}
