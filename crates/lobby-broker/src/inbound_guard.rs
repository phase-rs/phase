//! Inbound lobby frame guard — shared validation at the broker dispatch boundary.
//!
//! The Cloudflare Worker shell validates through [`crate::protocol::parse_lobby_client_message`],
//! which calls [`crate::validation::validate_lobby_message`]. The native `phase-server` shell
//! deserializes the wider `server_core::protocol::ClientMessage` and projects lobby frames
//! onto [`crate::protocol::LobbyClientMessage`] without re-parsing, so those frames must be
//! checked here before any handler runs. Without this gate, oversized display names, passwords,
//! and deck payloads can be stored, cloned, and broadcast to every lobby subscriber.

use crate::protocol::DraftLobbyMetadata;
use crate::protocol::LobbyClientMessage;
use crate::validation::{
    validate_create_game_settings_fields, validate_join_game_with_password_fields,
    validate_lobby_message, validate_lookup_join_target_fields, CreateGameSettingsFields,
    JoinGameWithPasswordFields, LookupJoinTargetFields, MAX_PLAYER_COUNT,
};
use engine::starter_decks::DeckData;
use engine::types::card_printing::{validate_deck_printings_bounds, MAX_DECK_PRINTING_ENTRIES};
use engine::types::format::FormatConfig;

/// Generous ceiling on main-deck entries at the wire boundary. Engine deck
/// validation enforces format legality later; this rejects multi-megabyte lists
/// before they are cloned through the native projection path.
pub const MAX_MAIN_DECK_ENTRIES: usize = 500;
/// Max sideboard entries accepted on the wire.
pub const MAX_SIDEBOARD_ENTRIES: usize = 100;
/// Max commander slots accepted on the wire.
pub const MAX_COMMANDER_ENTRIES: usize = 4;
/// Max supplementary Planechase planar-deck entries accepted on the wire.
pub const MAX_PLANAR_DECK_ENTRIES: usize = 200;
/// Max supplementary Archenemy scheme-deck entries accepted on the wire.
pub const MAX_SCHEME_DECK_ENTRIES: usize = 200;
/// Max companion entries accepted on the wire (CR 702.139a: a player reveals one
/// companion; 4 mirrors the commander slots' headroom).
pub const MAX_COMPANION_ENTRIES: usize = 4;
/// Max signature-spell entries accepted on the wire (an Oathbreaker deck has one
/// signature spell per oathbreaker, so at most as many as commander slots).
pub const MAX_SIGNATURE_SPELL_ENTRIES: usize = 4;
/// Max supplementary Attraction-deck entries accepted on the wire (CR 717.2a sets a
/// minimum number of cards and no maximum; 200 mirrors the other supplementary decks).
pub const MAX_ATTRACTION_DECK_ENTRIES: usize = 200;
/// Max supplementary Contraption-deck entries accepted on the wire (Contraptions are
/// outside the Comprehensive Rules, CR 701.45a; 200 mirrors the other supplementary decks).
pub const MAX_CONTRAPTION_DECK_ENTRIES: usize = 200;
/// Max byte length of a single card name string inside a deck payload. Defined once
/// in the engine (`engine::types::card_printing`), where the printings sidecar's
/// bound reads it too; re-exported so this path stays valid.
pub use engine::types::card_printing::MAX_DECK_CARD_NAME_LEN;

// A sidecar annotates at most every card copy of its list, so the engine's entry cap
// must cover a deck at every name-list ceiling. Raising a name-list ceiling must not
// make a legitimate sidecar abusive.
const _: () = assert!(
    MAX_DECK_PRINTING_ENTRIES
        >= MAX_MAIN_DECK_ENTRIES
            + MAX_SIDEBOARD_ENTRIES
            + MAX_COMMANDER_ENTRIES
            + MAX_COMPANION_ENTRIES
            + MAX_ATTRACTION_DECK_ENTRIES
            + MAX_PLANAR_DECK_ENTRIES
            + MAX_SCHEME_DECK_ENTRIES
            + MAX_CONTRAPTION_DECK_ENTRIES
            + MAX_SIGNATURE_SPELL_ENTRIES
);

fn validate_card_name(field: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if name.len() > MAX_DECK_CARD_NAME_LEN {
        return Err(format!(
            "{field} must be at most {MAX_DECK_CARD_NAME_LEN} bytes"
        ));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(())
}

pub fn validate_deck_list(field: &str, cards: &[String], max_entries: usize) -> Result<(), String> {
    if cards.len() > max_entries {
        return Err(format!(
            "{field} must contain at most {max_entries} entries"
        ));
    }
    for (index, name) in cards.iter().enumerate() {
        validate_card_name(&format!("{field}[{index}]"), name)?;
    }
    Ok(())
}

/// Bound the deck half of Create/Join lobby messages: every card-section name list,
/// and the printings sidecar through the engine's single bound
/// (`validate_deck_printings_bounds`). Sticker sheets are not bounded here (recorded
/// pre-existing gap). Lobby-only mode ignores deck contents for matchmaking, but the
/// native shell still deserializes and clones the full structure on every frame.
pub fn validate_deck_payload(field: &str, deck: &DeckData) -> Result<(), String> {
    validate_deck_list(
        &format!("{field}.main_deck"),
        &deck.main_deck,
        MAX_MAIN_DECK_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.sideboard"),
        &deck.sideboard,
        MAX_SIDEBOARD_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.commander"),
        &deck.commander,
        MAX_COMMANDER_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.planar_deck"),
        &deck.planar_deck,
        MAX_PLANAR_DECK_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.scheme_deck"),
        &deck.scheme_deck,
        MAX_SCHEME_DECK_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.companion"),
        &deck.companion,
        MAX_COMPANION_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.attraction_deck"),
        &deck.attraction_deck,
        MAX_ATTRACTION_DECK_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.contraption_deck"),
        &deck.contraption_deck,
        MAX_CONTRAPTION_DECK_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.signature_spell"),
        &deck.signature_spell,
        MAX_SIGNATURE_SPELL_ENTRIES,
    )?;
    validate_deck_printings_bounds(&format!("{field}.printings"), &deck.printings)
}

pub struct CreateGameSettingsInbound<'a> {
    pub deck: &'a DeckData,
    pub display_name: &'a str,
    pub password: Option<&'a str>,
    pub timer_seconds: Option<u32>,
    pub player_count: u8,
    pub format_config: Option<&'a FormatConfig>,
    pub room_name: Option<&'a str>,
    pub host_peer_id: Option<&'a str>,
    pub draft_metadata: Option<&'a DraftLobbyMetadata>,
    pub requested_code: Option<&'a str>,
}

/// Validate a settings-create frame without constructing the owned broker enum.
pub fn validate_create_game_settings_inbound_fields(
    fields: &CreateGameSettingsInbound<'_>,
) -> Result<(), String> {
    validate_create_game_settings_fields(CreateGameSettingsFields {
        display_name: fields.display_name,
        password: fields.password,
        timer_seconds: fields.timer_seconds,
        player_count: fields.player_count,
        room_name: fields.room_name,
        host_peer_id: fields.host_peer_id,
        draft_metadata: fields.draft_metadata,
        requested_code: fields.requested_code,
    })
}

/// Validate a settings-create frame without constructing the owned broker enum.
pub fn guard_create_game_settings_inbound(
    fields: CreateGameSettingsInbound<'_>,
) -> Result<(), String> {
    validate_create_game_settings_inbound_fields(&fields)?;
    if let Some(format_config) = fields.format_config {
        format_config.validate_for_player_count(fields.player_count.clamp(2, MAX_PLAYER_COUNT))?;
        format_config.reject_unimplemented_range_of_influence()?;
    }
    validate_deck_payload("deck", fields.deck)
}

pub struct JoinGameWithPasswordInbound<'a> {
    pub game_code: &'a str,
    pub deck: &'a DeckData,
    pub display_name: &'a str,
    pub password: Option<&'a str>,
    pub reservation_token: Option<&'a str>,
}

/// Validate a settings-join frame without constructing the owned broker enum.
pub fn guard_join_game_with_password_inbound(
    fields: JoinGameWithPasswordInbound<'_>,
) -> Result<(), String> {
    validate_join_game_with_password_fields(JoinGameWithPasswordFields {
        game_code: fields.game_code,
        display_name: fields.display_name,
        password: fields.password,
        reservation_token: fields.reservation_token,
    })?;
    validate_deck_payload("deck", fields.deck)
}

pub type LookupJoinTargetInbound<'a> = LookupJoinTargetFields<'a>;

/// Validate a lookup frame without constructing the owned broker enum.
pub fn guard_lookup_join_target_inbound(fields: LookupJoinTargetInbound<'_>) -> Result<(), String> {
    validate_lookup_join_target_fields(fields)
}

/// Validate every inbound lobby message before handler dispatch. Applies the
/// string/shape bounds from [`validate_lobby_message`] plus deck payload limits
/// on the two messages that carry a [`DeckData`] body.
pub fn guard_inbound(msg: &LobbyClientMessage) -> Result<(), String> {
    match msg {
        LobbyClientMessage::CreateGameWithSettings {
            deck,
            display_name,
            password,
            timer_seconds,
            player_count,
            format_config,
            room_name,
            host_peer_id,
            draft_metadata,
            requested_code,
            ..
        } => guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck,
            display_name,
            password: password.as_deref(),
            timer_seconds: *timer_seconds,
            player_count: *player_count,
            format_config: format_config.as_ref(),
            room_name: room_name.as_deref(),
            host_peer_id: host_peer_id.as_deref(),
            draft_metadata: draft_metadata.as_ref(),
            requested_code: requested_code.as_deref(),
        })?,
        LobbyClientMessage::JoinGameWithPassword {
            game_code,
            deck,
            display_name,
            password,
            reservation_token,
        } => guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code,
            deck,
            display_name,
            password: password.as_deref(),
            reservation_token: reservation_token.as_deref(),
        })?,
        _ => validate_lobby_message(msg)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck(main: usize, sideboard: usize) -> DeckData {
        DeckData {
            main_deck: vec!["Forest".to_string(); main],
            sideboard: vec!["Forest".to_string(); sideboard],
            ..Default::default()
        }
    }

    #[test]
    fn borrowed_create_guard_rejects_oversized_deck_without_owned_message() {
        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck(MAX_MAIN_DECK_ENTRIES + 1, 0),
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            requested_code: None,
        })
        .unwrap_err();

        assert!(err.contains("main_deck"));
    }

    #[test]
    fn borrowed_create_guard_rejects_malformed_requested_code() {
        let deck = deck(1, 0);
        let guard = |requested_code: Option<&str>| {
            guard_create_game_settings_inbound(CreateGameSettingsInbound {
                deck: &deck,
                display_name: "Host",
                password: None,
                timer_seconds: None,
                player_count: 2,
                format_config: None,
                room_name: None,
                host_peer_id: None,
                draft_metadata: None,
                requested_code,
            })
        };
        let err = guard(Some("bad")).unwrap_err();
        assert!(err.contains("requested_code"), "{err}");
        assert!(guard(Some("AB12CD")).is_ok());
    }

    #[test]
    fn borrowed_create_guard_rejects_oversized_lobby_field() {
        let display_name = "a".repeat(crate::validation::MAX_DISPLAY_NAME_LEN + 1);
        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck(1, 0),
            display_name: &display_name,
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            requested_code: None,
        })
        .unwrap_err();

        assert!(err.contains("display_name"));
    }

    #[test]
    fn borrowed_create_guard_rejects_archenemy_seat_outside_player_count() {
        let mut format_config = FormatConfig::archenemy();
        format_config.archenemy_player = Some(engine::types::player::PlayerId(3));

        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck(1, 0),
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: Some(&format_config),
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            requested_code: None,
        })
        .unwrap_err();

        assert!(err.contains("archenemy_player"));
    }

    #[test]
    fn borrowed_create_guard_rejects_limited_range_until_supported() {
        let mut format_config = FormatConfig::standard();
        format_config.range_of_influence =
            Some(Box::new(engine::types::format::RangeOfInfluenceConfig {
                default_range: 0,
                player_overrides: std::collections::BTreeMap::new(),
            }));

        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck(1, 0),
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: Some(&format_config),
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            requested_code: None,
        })
        .unwrap_err();

        assert!(err.contains("range_of_influence"));
    }

    /// Phase 1d production seam: `guard_create_game_settings_inbound` is the
    /// only inbound gate `format_config.validate_for_player_count` runs
    /// behind on this path — a deleted `?` here would leave an
    /// out-of-registry-range `player_count` admitted straight through to
    /// session creation. Commander's registry range is 2..=6; `player_count`
    /// is clamped to `MAX_PLAYER_COUNT` (8) before the bounds check, so 8
    /// still lands outside the range and must be rejected.
    #[test]
    fn borrowed_create_guard_rejects_player_count_outside_format_registry_range() {
        let format_config = FormatConfig::commander();

        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck(1, 0),
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 8,
            format_config: Some(&format_config),
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            requested_code: None,
        })
        .unwrap_err();

        assert!(err.contains("player_count"));
    }

    #[test]
    fn borrowed_join_guard_rejects_oversized_deck_without_owned_message() {
        let err = guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code: "GAME01",
            deck: &deck(1, MAX_SIDEBOARD_ENTRIES + 1),
            display_name: "Guest",
            password: None,
            reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("sideboard"));
    }

    #[test]
    fn borrowed_create_guard_rejects_oversized_planar_deck() {
        let mut deck = deck(1, 0);
        deck.planar_deck = vec!["Plane".to_string(); MAX_PLANAR_DECK_ENTRIES + 1];
        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck,
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            requested_code: None,
        })
        .unwrap_err();

        assert!(err.contains("planar_deck"));
    }

    #[test]
    fn borrowed_join_guard_rejects_invalid_planar_deck_entry() {
        let mut deck = deck(1, 0);
        deck.planar_deck = vec!["Bad\nPlane".to_string()];
        let err = guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code: "GAME01",
            deck: &deck,
            display_name: "Guest",
            password: None,
            reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("planar_deck[0]"));
    }

    #[test]
    fn borrowed_create_guard_rejects_oversized_scheme_deck() {
        let mut deck = deck(1, 0);
        deck.scheme_deck = vec!["Scheme".to_string(); MAX_SCHEME_DECK_ENTRIES + 1];
        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck,
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            requested_code: None,
        })
        .unwrap_err();

        assert!(err.contains("scheme_deck"));
    }

    #[test]
    fn borrowed_join_guard_rejects_invalid_scheme_deck_entry() {
        let mut deck = deck(1, 0);
        deck.scheme_deck = vec!["Bad\nScheme".to_string()];
        let err = guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code: "GAME01",
            deck: &deck,
            display_name: "Guest",
            password: None,
            reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("scheme_deck[0]"));
    }

    #[test]
    fn borrowed_join_guard_rejects_oversized_lobby_field() {
        let game_code = "G".repeat(crate::validation::MAX_GAME_CODE_LEN + 1);
        let err = guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code: &game_code,
            deck: &deck(1, 0),
            display_name: "Guest",
            password: None,
            reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("game_code"));
    }

    #[test]
    fn borrowed_lookup_guard_rejects_oversized_lobby_field() {
        let game_code = "G".repeat(crate::validation::MAX_GAME_CODE_LEN + 1);
        let err = guard_lookup_join_target_inbound(LookupJoinTargetInbound {
            game_code: &game_code,
            password: None,
            display_name: None,
            release_reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("game_code"));
    }

    /// Runs `deck` through both deck-carrying guards, which must agree.
    fn guard_both(deck: &DeckData) -> (Result<(), String>, Result<(), String>) {
        let create = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck,
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            requested_code: None,
        });
        let join = guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code: "GAME01",
            deck,
            display_name: "Guest",
            password: None,
            reservation_token: None,
        });
        (create, join)
    }

    /// P2-V3 (D9): the four sections that had no ceiling take the existing sections'
    /// count and per-name checks: a reach guard at the ceiling, then ceiling + 1 and a
    /// control-character name refused naming the section.
    #[test]
    fn d9_sections_are_bounded_like_the_existing_name_lists() {
        type Section = fn(&mut DeckData) -> &mut Vec<String>;
        let sections: [(&str, usize, Section); 4] = [
            ("companion", MAX_COMPANION_ENTRIES, |d| &mut d.companion),
            ("attraction_deck", MAX_ATTRACTION_DECK_ENTRIES, |d| {
                &mut d.attraction_deck
            }),
            ("contraption_deck", MAX_CONTRAPTION_DECK_ENTRIES, |d| {
                &mut d.contraption_deck
            }),
            ("signature_spell", MAX_SIGNATURE_SPELL_ENTRIES, |d| {
                &mut d.signature_spell
            }),
        ];
        for (name, ceiling, section) in sections {
            let mut at_ceiling = deck(1, 0);
            *section(&mut at_ceiling) = vec!["Card".to_string(); ceiling];
            let (create, join) = guard_both(&at_ceiling);
            assert_eq!(create, Ok(()), "{name} at the ceiling");
            assert_eq!(join, Ok(()), "{name} at the ceiling");

            let mut over = deck(1, 0);
            *section(&mut over) = vec!["Card".to_string(); ceiling + 1];
            let (create, join) = guard_both(&over);
            let expected = format!("deck.{name} must contain at most {ceiling} entries");
            assert_eq!(create, Err(expected.clone()), "{name} over the ceiling");
            assert_eq!(join, Err(expected), "{name} over the ceiling");

            let mut bad = deck(1, 0);
            *section(&mut bad) = vec!["Bad\nName".to_string()];
            let (create, join) = guard_both(&bad);
            let expected = format!("deck.{name}[0] must not contain control characters");
            assert_eq!(create, Err(expected.clone()), "{name} control name");
            assert_eq!(join, Err(expected), "{name} control name");
        }
    }

    fn printing(
        name: &str,
        set_code: &str,
        collector_number: &str,
    ) -> engine::types::card_printing::DeckPrinting {
        use engine::types::card_printing::{CardPrinting, DeckPrinting, DeckSection};
        DeckPrinting {
            section: DeckSection::MainDeck,
            name: name.to_string(),
            count: 1,
            printing: CardPrinting {
                set_code: set_code.to_string(),
                collector_number: collector_number.to_string(),
            },
        }
    }

    /// P2-V3: the server guard delegates to the engine's sidecar bound, naming the
    /// printings field; an in-bounds sidecar of invalid annotations is admitted (R1).
    #[test]
    fn sidecar_resource_bounds_are_enforced_and_invalid_annotations_admitted() {
        let mut abusive = deck(1, 0);
        abusive.printings = vec![
            printing("Plains", "M21", "1"),
            printing("Plains", &"s".repeat(33), "1"),
        ];
        let (create, join) = guard_both(&abusive);
        let expected = "deck.printings[1].printing.set_code must be at most 32 bytes".to_string();
        assert_eq!(create, Err(expected.clone()));
        assert_eq!(join, Err(expected));

        let mut too_many = deck(1, 0);
        too_many.printings = vec![printing("Plains", "M21", "1"); MAX_DECK_PRINTING_ENTRIES + 1];
        let (create, join) = guard_both(&too_many);
        let expected = "deck.printings must contain at most 2048 entries".to_string();
        assert_eq!(create, Err(expected.clone()));
        assert_eq!(join, Err(expected));

        // Reach guard: the same guard admits a full-size sidecar at the cap and a
        // sidecar of well-typed but unusable annotations.
        let mut at_cap = deck(1, 0);
        at_cap.printings = vec![printing("Plains", "M21", "1"); MAX_DECK_PRINTING_ENTRIES];
        let (create, join) = guard_both(&at_cap);
        assert_eq!((create, join), (Ok(()), Ok(())));

        let mut invalid = deck(1, 0);
        invalid.printings = vec![
            printing("", "", ""),
            printing("Bad\nName", "M\u{0}21", "1\t"),
            printing("No Such Card Anywhere", "ZZZ", "999"),
        ];
        let (create, join) = guard_both(&invalid);
        assert_eq!((create, join), (Ok(()), Ok(())));
    }
}
