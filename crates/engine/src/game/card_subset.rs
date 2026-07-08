//! Game-scoped AI card-database subset.
//!
//! iOS PvE worker pools spin up several WASM engine instances, each of which
//! would otherwise parse the full ~93MB card-data corpus and OOM WebKit. The
//! main worker keeps the full database; AI-pool workers instead receive a
//! subset containing only the faces THIS game can reach. Games whose card
//! universe is not statically bounded (today: Momir, whose token pool is the
//! entire creature corpus) escalate to the full database on AI workers.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::database::card_db::CardDatabase;
use crate::game::printed_cards::build_conjure_registry;
use crate::types::format::GameFormat;
use crate::types::game_state::GameState;

/// Why a game cannot use a bounded AI card subset and must ship the full DB.
/// Open enum: add a variant + one `game_requires_full_card_db` arm for any
/// future whole-corpus mechanic. Never special-case a card.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum FullDbReason {
    /// CR 707.2 + CR 202.3: Momir seeds its random-token pool from the ENTIRE
    /// creature corpus, keyed by mana value, at rehydrate (printed_cards.rs:1334;
    /// `momir_pool_faces` is `#[serde(skip)]`). The emblem creates a token that's
    /// a copy (CR 707.2) of a random creature card with the chosen mana value
    /// (CR 202.3). A subset DB yields a tiny, wrong pool, so Momir games use the
    /// full DB on AI workers.
    Momir,
}

/// Result of building an AI-worker card subset for one game. `Full` means the
/// caller must load the full database instead (the universe is unbounded).
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiCardSubsetResult {
    Full,
    Subset { json: String, count: usize },
}

/// `Some(reason)` when this game's card universe is not statically bounded and
/// AI workers must load the full database instead of a subset.
pub fn game_requires_full_card_db(state: &GameState) -> Option<FullDbReason> {
    match state.format_config.format {
        // CR 707.2 + CR 202.3: see `FullDbReason::Momir`.
        GameFormat::Momir => Some(FullDbReason::Momir),
        _ => None,
    }
}

/// Every card-face name an AI worker could need for THIS game: every object's
/// printed face, every deck-pool entry, the transitive Conjure closure, and the
/// oracle-id siblings (other faces/printings) of all of the above.
pub fn collect_game_card_universe(state: &GameState, db: &CardDatabase) -> BTreeSet<String> {
    let mut names = BTreeSet::new();

    // 1. Every object that carries a printed face.
    for obj in state.objects.values() {
        if let Some(pref) = &obj.printed_ref {
            names.insert(pref.face_name.clone());
        }
    }

    // 2. Every deck-pool entry (registered + current; main/sideboard/commander/signature).
    for pool in &state.deck_pools {
        for arc in [
            &pool.registered_main,
            &pool.current_main,
            &pool.registered_sideboard,
            &pool.current_sideboard,
            &pool.registered_commander,
            &pool.current_commander,
            &pool.registered_signature_spell,
            &pool.current_signature_spell,
        ] {
            for entry in arc.iter() {
                names.insert(entry.card.name.clone());
            }
        }
    }

    // 3. Transitive Conjure closure — reuse the existing building block.
    let (registry, collected) = build_conjure_registry(state, db);
    names.extend(registry.into_keys());
    names.extend(collected);

    // 4. oracle_id siblings: other face(s)/printings of every name so far, so a
    //    DFC front pulls in its back face (and vice versa) and every printing is
    //    available to the AI workers.
    let snapshot: Vec<String> = names.iter().cloned().collect();
    for name in snapshot {
        if let Some(face) = db.get_face_by_name(&name) {
            if let Some(oracle_id) = face.scryfall_oracle_id.as_deref() {
                if let Some(siblings) = db.oracle_id_index.get(oracle_id) {
                    names.extend(siblings.iter().cloned());
                }
            }
        }
    }

    names
}

/// Build the AI-worker card subset for this game, or signal `Full` when the
/// universe is unbounded (Momir).
pub fn build_ai_card_subset(state: &GameState, db: &CardDatabase) -> AiCardSubsetResult {
    if game_requires_full_card_db(state).is_some() {
        return AiCardSubsetResult::Full;
    }
    let names = collect_game_card_universe(state, db);
    let json = db.export_subset_json(&names);
    AiCardSubsetResult::Subset {
        count: names.len(),
        json,
    }
}

/// Defensive entry point for the WASM transport: when either the card database
/// or the game state is absent, AI workers must fall back to the full database
/// (a subset cannot be computed). Centralizing the `None`-handling keeps the
/// transport layer a thin pass-through.
pub fn build_ai_card_subset_or_full(
    state: Option<&GameState>,
    db: Option<&CardDatabase>,
) -> AiCardSubsetResult {
    match (state, db) {
        (Some(state), Some(db)) => build_ai_card_subset(state, db),
        _ => AiCardSubsetResult::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::deck_loading::DeckEntry;
    use crate::game::printed_cards::printed_ref_from_face;
    use crate::game::zones::create_object;
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{GameState, PlayerDeckPool};
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::sync::Arc;

    fn creature_face(name: &str, oracle_id: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            },
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 2,
            },
            scryfall_oracle_id: Some(oracle_id.to_string()),
            ..CardFace::default()
        }
    }

    /// Serialize a face into a `CardExportEntry`-shaped JSON value, then layer on
    /// the export-only fields (layout/printings/rulings/bracket_signals).
    fn entry_value(
        face: &CardFace,
        layout: Option<&str>,
        printings: &[&str],
        rulings: &[(&str, &str)],
        game_changer: bool,
    ) -> serde_json::Value {
        let mut v = serde_json::to_value(face).unwrap();
        if let Some(layout) = layout {
            v["layout"] = serde_json::json!(layout);
        }
        if !printings.is_empty() {
            v["printings"] = serde_json::json!(printings);
        }
        if !rulings.is_empty() {
            v["rulings"] = serde_json::Value::Array(
                rulings
                    .iter()
                    .map(|(date, text)| serde_json::json!({ "date": date, "text": text }))
                    .collect(),
            );
        }
        v["bracket_signals"] = serde_json::json!({
            "game_changer": game_changer,
            "mass_land_denial": false,
            "extra_turn": false,
            "efficient_tutor": false,
        });
        v
    }

    /// VM-2: the four universe sources (battlefield object, deck-pool entry,
    /// Conjure-closure card, DFC) all resolve in the emitted subset, INCLUDING
    /// the DFC back face reached only via the oracle-id sibling step. A card
    /// present in none of the four sources is absent.
    #[test]
    fn subset_covers_all_four_sources_plus_dfc_back_face() {
        let battlefield = creature_face("Battlefield Guy", "bf-oracle");
        let library = creature_face("Library Card", "lib-oracle");
        let conjured = creature_face("Conjured Token", "conjure-oracle");
        let mut spellbook_src = creature_face("Spellbook Source", "spellbook-oracle");
        spellbook_src.metadata.spellbook = vec!["Conjured Token".to_string()];
        // DFC: front + back share one oracle id; front carries the transform layout.
        let dfc_front = creature_face("Dfc Front", "dfc-oracle");
        let dfc_back = creature_face("Dfc Back", "dfc-oracle");
        let unrelated = creature_face("Unrelated Card", "unrelated-oracle");

        let export = serde_json::json!({
            "Battlefield Guy": entry_value(&battlefield, None, &[], &[], false),
            "Library Card": entry_value(&library, None, &[], &[], false),
            "Conjured Token": entry_value(&conjured, None, &[], &[], false),
            "Spellbook Source": entry_value(&spellbook_src, None, &[], &[], false),
            "Dfc Front": entry_value(&dfc_front, Some("transform"), &[], &[], false),
            "Dfc Back": entry_value(&dfc_back, None, &[], &[], false),
            "Unrelated Card": entry_value(&unrelated, None, &[], &[], false),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db parses");

        let mut state = GameState::new_two_player(7);

        // Source 1: a battlefield object carrying the printed face.
        let bf_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Battlefield Guy".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&bf_id).unwrap().printed_ref = printed_ref_from_face(&battlefield);

        // Source 3 seed: a battlefield object whose spellbook conjures "Conjured Token".
        let sb_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spellbook Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&sb_id).unwrap().printed_ref = printed_ref_from_face(&spellbook_src);

        // Source 4: the DFC front face is in the library deck pool.
        // Source 2: a library deck-pool entry.
        state.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: Arc::new(vec![
                DeckEntry {
                    card: library.clone(),
                    count: 1,
                },
                DeckEntry {
                    card: dfc_front.clone(),
                    count: 1,
                },
            ]),
            ..Default::default()
        });

        let result = build_ai_card_subset(&state, &db);
        let AiCardSubsetResult::Subset { json, .. } = result else {
            panic!("non-Momir game must yield a Subset");
        };
        let subset = CardDatabase::from_json_str(&json).expect("subset parses");

        for name in [
            "Battlefield Guy",
            "Library Card",
            "Conjured Token",
            "Spellbook Source",
            "Dfc Front",
        ] {
            assert!(
                subset.get_face_by_name(name).is_some(),
                "subset must contain {name}"
            );
        }
        // Revert guard: dropping the oracle-id-sibling step drops this back face.
        assert!(
            subset.get_face_by_name("Dfc Back").is_some(),
            "DFC back face must be pulled in via the oracle-id sibling step"
        );
        // Negative: a card in none of the four sources is absent.
        assert!(
            subset.get_face_by_name("Unrelated Card").is_none(),
            "an unreferenced card must NOT be in the subset"
        );
    }

    /// VM-3: Momir games escalate to the full DB; the identical objects in a
    /// non-Momir format yield a subset.
    #[test]
    fn momir_format_escalates_to_full_db() {
        let creature = creature_face("Pool Beast", "pool-oracle");
        let export = serde_json::json!({
            "Pool Beast": entry_value(&creature, None, &[], &[], false),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db parses");

        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pool Beast".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().printed_ref = printed_ref_from_face(&creature);

        // Non-Momir: subset.
        assert!(
            matches!(
                build_ai_card_subset(&state, &db),
                AiCardSubsetResult::Subset { .. }
            ),
            "non-Momir game must yield a Subset"
        );

        // Revert guard: removing the Momir arm of game_requires_full_card_db
        // would make this a Subset instead of Full.
        state.format_config = FormatConfig::momir();
        assert!(
            matches!(build_ai_card_subset(&state, &db), AiCardSubsetResult::Full),
            "Momir game must escalate to the full DB"
        );
    }

    /// VM-5: the defensive transport entry returns Full when state or db is
    /// absent (a subset cannot be computed without both).
    #[test]
    fn missing_state_or_db_falls_back_to_full() {
        let creature = creature_face("Pool Beast", "pool-oracle");
        let export = serde_json::json!({
            "Pool Beast": entry_value(&creature, None, &[], &[], false),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db parses");
        let state = GameState::new_two_player(7);

        assert!(matches!(
            build_ai_card_subset_or_full(None, Some(&db)),
            AiCardSubsetResult::Full
        ));
        assert!(matches!(
            build_ai_card_subset_or_full(Some(&state), None),
            AiCardSubsetResult::Full
        ));
        assert!(matches!(
            build_ai_card_subset_or_full(None, None),
            AiCardSubsetResult::Full
        ));
        // Positive control: both present + non-Momir → Subset.
        assert!(matches!(
            build_ai_card_subset_or_full(Some(&state), Some(&db)),
            AiCardSubsetResult::Subset { .. }
        ));
    }

    /// VM-6: `export_subset_json` round-trips every per-card index — layout,
    /// printings, rulings, and bracket signals — for the named faces, and a
    /// single-face card round-trips with no layout.
    #[test]
    fn export_subset_json_round_trips_all_indices() {
        let dfc_front = creature_face("Rich Front", "rich-oracle");
        let dfc_back = creature_face("Rich Back", "rich-oracle");
        let single = creature_face("Plain Card", "plain-oracle");

        let export = serde_json::json!({
            "Rich Front": entry_value(
                &dfc_front,
                Some("transform"),
                &["TST", "TS2"],
                &[("2024-01-01", "A ruling.")],
                true,
            ),
            "Rich Back": entry_value(&dfc_back, None, &[], &[], false),
            "Plain Card": entry_value(&single, None, &[], &[], false),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db parses");

        let names: BTreeSet<String> = ["Rich Front", "Rich Back", "Plain Card"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let subset_json = db.export_subset_json(&names);
        let subset = CardDatabase::from_json_str(&subset_json).expect("subset parses");

        // Layout: the DFC front keeps its transform layout (keyed by oracle id).
        assert_eq!(
            subset.layout_index.get("rich-oracle").copied(),
            db.layout_index.get("rich-oracle").copied(),
            "DFC layout must round-trip"
        );
        assert!(
            subset.layout_index.contains_key("rich-oracle"),
            "transform layout must survive export"
        );
        // Printings / rulings / bracket signals round-trip (keyed by lowercase name).
        assert_eq!(
            subset.printings_for("Rich Front"),
            db.printings_for("Rich Front")
        );
        assert_eq!(
            subset.rulings_for("Rich Front"),
            db.rulings_for("Rich Front")
        );
        assert_eq!(
            subset.bracket_signals_for("Rich Front").game_changer,
            db.bracket_signals_for("Rich Front").game_changer
        );
        assert!(
            subset.bracket_signals_for("Rich Front").game_changer,
            "game_changer signal must survive export"
        );
        // Single-face card: no layout, still round-trips as a face.
        assert!(
            !subset.layout_index.contains_key("plain-oracle"),
            "single-face card must carry no layout"
        );
        assert!(subset.get_face_by_name("Plain Card").is_some());
    }
}
