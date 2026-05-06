use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::database::CardDatabase;
use crate::types::card::CardFace;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::printed_cards::apply_card_face_to_object;
use super::zones::create_object;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckEntry {
    pub card: CardFace,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerDeckPayload {
    pub main_deck: Vec<DeckEntry>,
    #[serde(default)]
    pub sideboard: Vec<DeckEntry>,
    #[serde(default)]
    pub commander: Vec<DeckEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckPayload {
    pub player: PlayerDeckPayload,
    pub opponent: PlayerDeckPayload,
    #[serde(default)]
    pub ai_decks: Vec<PlayerDeckPayload>,
}

/// Lightweight deck format using card names only.
/// Resolved into a DeckPayload via a CardDatabase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerDeckList {
    pub main_deck: Vec<String>,
    #[serde(default)]
    pub sideboard: Vec<String>,
    #[serde(default)]
    pub commander: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckList {
    pub player: PlayerDeckList,
    pub opponent: PlayerDeckList,
    #[serde(default)]
    pub ai_decks: Vec<PlayerDeckList>,
}

/// Resolve a flat name list into DeckEntry entries using the card database.
/// Groups duplicate names and skips unresolvable names.
fn resolve_names(db: &CardDatabase, names: &[String]) -> Vec<DeckEntry> {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for name in names {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    let mut entries = Vec::new();
    for (name, count) in counts {
        if let Some(face) = db.get_face_by_name(name) {
            entries.push(DeckEntry {
                card: face.clone(),
                count,
            });
        }
    }
    entries
}

/// Resolve a single player's deck list (name-only) into a `PlayerDeckPayload`
/// using a `CardDatabase` for lookup. Unresolvable names are silently skipped.
pub fn resolve_player_deck_list(db: &CardDatabase, list: &PlayerDeckList) -> PlayerDeckPayload {
    PlayerDeckPayload {
        main_deck: resolve_names(db, &list.main_deck),
        sideboard: resolve_names(db, &list.sideboard),
        commander: resolve_names(db, &list.commander),
    }
}

/// Resolve a DeckList (name-only) into a DeckPayload (full CardFace objects)
/// using a CardDatabase for lookup. Unresolvable names are silently skipped.
pub fn resolve_deck_list(db: &CardDatabase, list: &DeckList) -> DeckPayload {
    DeckPayload {
        player: PlayerDeckPayload {
            main_deck: resolve_names(db, &list.player.main_deck),
            sideboard: resolve_names(db, &list.player.sideboard),
            commander: resolve_names(db, &list.player.commander),
        },
        opponent: PlayerDeckPayload {
            main_deck: resolve_names(db, &list.opponent.main_deck),
            sideboard: resolve_names(db, &list.opponent.sideboard),
            commander: resolve_names(db, &list.opponent.commander),
        },
        ai_decks: list
            .ai_decks
            .iter()
            .map(|deck| PlayerDeckPayload {
                main_deck: resolve_names(db, &deck.main_deck),
                sideboard: resolve_names(db, &deck.sideboard),
                commander: resolve_names(db, &deck.commander),
            })
            .collect(),
    }
}

/// Create a fully-populated GameObject from a CardFace and place it in the owner's library.
pub fn create_object_from_card_face(
    state: &mut GameState,
    card_face: &CardFace,
    owner: PlayerId,
) -> crate::types::identifiers::ObjectId {
    let card_id = CardId(state.next_object_id);
    let obj_id = create_object(state, card_id, owner, card_face.name.clone(), Zone::Library);

    let obj = state.objects.get_mut(&obj_id).expect("just created");
    apply_card_face_to_object(obj, card_face);

    obj_id
}

/// Create a commander GameObject from a CardFace, placing it in the command zone.
pub fn create_commander_from_card_face(
    state: &mut GameState,
    card_face: &CardFace,
    owner: PlayerId,
) -> crate::types::identifiers::ObjectId {
    let card_id = CardId(state.next_object_id);
    let obj_id = create_object(state, card_id, owner, card_face.name.clone(), Zone::Command);

    let obj = state.objects.get_mut(&obj_id).expect("just created");
    apply_card_face_to_object(obj, card_face);
    obj.is_commander = true;

    obj_id
}

/// Load deck data into a GameState, creating GameObjects in each player's library and shuffling.
pub fn load_deck_into_state(state: &mut GameState, payload: &DeckPayload) {
    state.deck_pools.clear();
    state.sideboard_submitted.clear();

    // Build each Arc<Vec<_>> once and share between registered_X and current_X —
    // they start identical and diverge via Arc::make_mut on first mutation.
    let p0_main = std::sync::Arc::new(payload.player.main_deck.clone());
    let p0_side = std::sync::Arc::new(payload.player.sideboard.clone());
    let p0_cmdr = std::sync::Arc::new(payload.player.commander.clone());
    state
        .deck_pools
        .push(crate::types::game_state::PlayerDeckPool {
            player: PlayerId(0),
            registered_main: std::sync::Arc::clone(&p0_main),
            registered_sideboard: std::sync::Arc::clone(&p0_side),
            current_main: p0_main,
            current_sideboard: p0_side,
            registered_commander: std::sync::Arc::clone(&p0_cmdr),
            current_commander: p0_cmdr,
        });
    let p1_main = std::sync::Arc::new(payload.opponent.main_deck.clone());
    let p1_side = std::sync::Arc::new(payload.opponent.sideboard.clone());
    let p1_cmdr = std::sync::Arc::new(payload.opponent.commander.clone());
    state
        .deck_pools
        .push(crate::types::game_state::PlayerDeckPool {
            player: PlayerId(1),
            registered_main: std::sync::Arc::clone(&p1_main),
            registered_sideboard: std::sync::Arc::clone(&p1_side),
            current_main: p1_main,
            current_sideboard: p1_side,
            registered_commander: std::sync::Arc::clone(&p1_cmdr),
            current_commander: p1_cmdr,
        });
    for (i, ai_deck) in payload.ai_decks.iter().enumerate() {
        let player_id = PlayerId((2 + i) as u8);
        let main = std::sync::Arc::new(ai_deck.main_deck.clone());
        let side = std::sync::Arc::new(ai_deck.sideboard.clone());
        let cmdr = std::sync::Arc::new(ai_deck.commander.clone());
        state
            .deck_pools
            .push(crate::types::game_state::PlayerDeckPool {
                player: player_id,
                registered_main: std::sync::Arc::clone(&main),
                registered_sideboard: std::sync::Arc::clone(&side),
                current_main: main,
                current_sideboard: side,
                registered_commander: std::sync::Arc::clone(&cmdr),
                current_commander: cmdr,
            });
    }

    for entry in &payload.player.main_deck {
        for _ in 0..entry.count {
            create_object_from_card_face(state, &entry.card, PlayerId(0));
        }
    }

    for entry in &payload.opponent.main_deck {
        for _ in 0..entry.count {
            create_object_from_card_face(state, &entry.card, PlayerId(1));
        }
    }

    // Load additional AI decks into PlayerId(2), PlayerId(3), etc.
    for (i, ai_deck) in payload.ai_decks.iter().enumerate() {
        let player_id = PlayerId((2 + i) as u8);
        for entry in &ai_deck.main_deck {
            for _ in 0..entry.count {
                create_object_from_card_face(state, &entry.card, player_id);
            }
        }
    }

    // CR 903.6 + CR 408.1: Place commanders in the command zone at game start.
    let commander_decks: Vec<(PlayerId, &[DeckEntry])> =
        std::iter::once((PlayerId(0), payload.player.commander.as_slice()))
            .chain(std::iter::once((
                PlayerId(1),
                payload.opponent.commander.as_slice(),
            )))
            .chain(
                payload
                    .ai_decks
                    .iter()
                    .enumerate()
                    .map(|(i, d)| (PlayerId((2 + i) as u8), d.commander.as_slice())),
            )
            .collect();
    for (owner, entries) in commander_decks {
        for entry in entries {
            for _ in 0..entry.count {
                create_commander_from_card_face(state, &entry.card, owner);
            }
        }
    }

    // Collect all creature subtypes for Changeling CDA expansion
    let mut creature_types: HashSet<String> = HashSet::new();
    let all_entries = payload
        .player
        .main_deck
        .iter()
        .chain(&payload.player.commander)
        .chain(&payload.opponent.main_deck)
        .chain(&payload.opponent.commander)
        .chain(
            payload
                .ai_decks
                .iter()
                .flat_map(|d| d.main_deck.iter().chain(d.commander.iter())),
        );
    for entry in all_entries {
        if entry
            .card
            .card_type
            .core_types
            .contains(&crate::types::card_type::CoreType::Creature)
        {
            creature_types.extend(entry.card.card_type.subtypes.iter().cloned());
        }
    }
    let mut sorted: Vec<String> = creature_types.into_iter().collect();
    sorted.sort();
    state.all_creature_types = sorted;

    // Shuffle each player's library
    let GameState { players, rng, .. } = state;
    for player in players.iter_mut() {
        crate::util::im_ext::shuffle_vector(&mut player.library, rng);
    }
}

/// Canonical init sequence for every transport layer: load the decks into
/// the state, then hydrate runtime-only fields (back_face, layout_kind)
/// from the CardDatabase.
///
/// Rehydration populates `GameObject::back_face` for dual-faced cards
/// (Adventure, Omen, Modal DFC, Transform, Meld, Prepare). Without it,
/// `is_adventure_card`, `swap_to_adventure_face`, and the MDFC face-choice
/// gate all silently no-op because `back_face` stays `None`. The WASM
/// bridge, `server-core`, and Tauri commands must all route through here
/// so the three transports can't drift apart again (see the Sagu Wildling
/// multiplayer regression that motivated this consolidation).
///
/// `db` is `Option` only because some call paths (Tauri desktop today)
/// don't yet thread a CardDatabase into their init. Passing `None` emits
/// a `tracing::warn!` so the gap is visible in logs rather than hidden.
pub fn load_and_hydrate_decks(
    state: &mut GameState,
    payload: &DeckPayload,
    db: Option<&CardDatabase>,
) {
    load_deck_into_state(state, payload);
    match db {
        Some(db) => {
            super::printed_cards::rehydrate_game_from_card_db(state, db);
        }
        None => {
            // Latch the warning so a long-running desktop session that
            // starts many games doesn't spam the log on each match.
            // The invariant "some transport is still not passing a db"
            // only needs to be seen once per process.
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!(
                    "load_and_hydrate_decks called without a CardDatabase — \
                     dual-faced cards (Adventure, Omen, MDFC, Transform, Meld) \
                     will have back_face=None and their face-specific behavior \
                     will be disabled. Thread a CardDatabase through this call \
                     site to fix. (This warning is emitted once per process.)"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, Effect, PtValue, QuantityExpr,
        StaticDefinition, TargetFilter,
    };
    use crate::types::card_type::CardType;
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};

    use super::super::printed_cards::derive_colors_from_mana_cost;

    fn make_creature_face() -> CardFace {
        CardFace {
            name: "Grizzly Bears".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            },
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![Keyword::Trample],
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Pump {
                    power: PtValue::Fixed(0),
                    toughness: PtValue::Fixed(0),
                    target: TargetFilter::Any,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap)],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            color_identity: vec![],
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            solve_condition: None,
            parse_warnings: vec![],
            brawl_commander: false,
            metadata: Default::default(),
            rarities: Default::default(),
        }
    }

    fn make_instant_face() -> CardFace {
        CardFace {
            name: "Lightning Bolt".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![crate::types::card_type::CoreType::Instant],
                subtypes: vec![],
            },
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![],
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                    damage_source: None,
                },
            )],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            color_identity: vec![],
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            solve_condition: None,
            parse_warnings: vec![],
            brawl_commander: false,
            metadata: Default::default(),
            rarities: Default::default(),
        }
    }

    #[test]
    fn create_object_from_card_face_populates_characteristics() {
        let mut state = GameState::new_two_player(42);
        let face = make_creature_face();
        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));

        let obj = &state.objects[&obj_id];
        assert_eq!(obj.name, "Grizzly Bears");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(obj.base_power, Some(2));
        assert_eq!(obj.base_toughness, Some(2));
        assert_eq!(obj.keywords, vec![Keyword::Trample]);
        assert_eq!(obj.base_keywords, vec![Keyword::Trample]);
        assert_eq!(obj.color, vec![ManaColor::Green]);
        assert_eq!(obj.base_color, vec![ManaColor::Green]);
        assert_eq!(
            obj.mana_cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            }
        );
        assert_eq!(obj.abilities.len(), 1);
        assert_eq!(obj.zone, Zone::Library);
        assert_eq!(obj.owner, PlayerId(0));
    }

    #[test]
    fn create_object_from_card_face_color_override() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.color_override = Some(vec![ManaColor::White, ManaColor::Green]);

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.color, vec![ManaColor::White, ManaColor::Green]);
    }

    #[test]
    fn create_object_variable_pt_defaults_to_zero() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.power = Some(PtValue::Variable("*".to_string()));
        face.toughness = Some(PtValue::Variable("*".to_string()));

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.power, Some(0));
        assert_eq!(obj.toughness, Some(0));
        assert_eq!(obj.base_power, Some(0));
        assert_eq!(obj.base_toughness, Some(0));
    }

    #[test]
    fn create_object_no_pt_stays_none() {
        let mut state = GameState::new_two_player(42);
        let face = make_instant_face();

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert!(obj.power.is_none());
        assert!(obj.toughness.is_none());
    }

    #[test]
    fn load_deck_creates_correct_object_count() {
        let mut state = GameState::new_two_player(42);
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: vec![
                    DeckEntry {
                        card: make_creature_face(),
                        count: 4,
                    },
                    DeckEntry {
                        card: make_instant_face(),
                        count: 2,
                    },
                ],
                sideboard: vec![],
                commander: vec![],
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![DeckEntry {
                    card: make_creature_face(),
                    count: 3,
                }],
                sideboard: vec![],
                commander: vec![],
            },
            ai_decks: vec![],
        };

        load_deck_into_state(&mut state, &payload);

        assert_eq!(state.players[0].library.len(), 6); // 4 + 2
        assert_eq!(state.players[1].library.len(), 3);
        assert_eq!(state.objects.len(), 9); // 6 + 3
    }

    #[test]
    fn load_deck_shuffles_libraries() {
        // Use a large enough deck that shuffle is virtually guaranteed to change order
        let mut entries = Vec::new();
        for i in 0..20 {
            entries.push(DeckEntry {
                card: CardFace {
                    name: format!("Card {}", i),
                    ..make_creature_face()
                },
                count: 1,
            });
        }

        let mut state = GameState::new_two_player(42);
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: entries,
                sideboard: vec![],
                commander: vec![],
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![],
                sideboard: vec![],
                commander: vec![],
            },
            ai_decks: vec![],
        };
        load_deck_into_state(&mut state, &payload);

        // Collect names in library order
        let names: Vec<String> = state.players[0]
            .library
            .iter()
            .map(|id| state.objects[id].name.clone())
            .collect();

        // Check that the order differs from insertion order (Card 0, Card 1, ...)
        let insertion_order: Vec<String> = (0..20).map(|i| format!("Card {}", i)).collect();
        assert_ne!(names, insertion_order, "Library should be shuffled");
    }

    #[test]
    fn create_object_with_trigger_definitions() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.triggers = vec![crate::types::ability::TriggerDefinition::new(
            crate::types::triggers::TriggerMode::ChangesZone,
        )
        .destination(Zone::Battlefield)];

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.trigger_definitions.len(), 1);
        assert_eq!(
            obj.trigger_definitions[0].mode,
            crate::types::triggers::TriggerMode::ChangesZone
        );
    }

    #[test]
    fn create_object_with_static_definitions() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.static_abilities = vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddPower { value: 2 }])];

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.static_definitions.len(), 1);
        assert_eq!(
            obj.static_definitions[0].mode,
            crate::types::statics::StaticMode::Continuous
        );
    }

    #[test]
    fn create_object_with_replacement_definitions() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.replacements = vec![crate::types::ability::ReplacementDefinition::new(
            crate::types::replacements::ReplacementEvent::DamageDone,
        )
        .valid_card(TargetFilter::SelfRef)];

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert_eq!(
            obj.replacement_definitions[0].event,
            crate::types::replacements::ReplacementEvent::DamageDone
        );
    }

    #[test]
    fn derive_colors_multicolor() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
            generic: 1,
        };
        let colors = derive_colors_from_mana_cost(&cost);
        assert_eq!(colors, vec![ManaColor::White, ManaColor::Blue]);
    }

    #[test]
    fn derive_colors_no_cost() {
        let colors = derive_colors_from_mana_cost(&ManaCost::NoCost);
        assert!(colors.is_empty());
    }

    #[test]
    fn derive_colors_hybrid() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue],
            generic: 0,
        };
        let colors = derive_colors_from_mana_cost(&cost);
        assert_eq!(colors, vec![ManaColor::White, ManaColor::Blue]);
    }

    #[test]
    fn derive_colors_deduplicates() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 0,
        };
        let colors = derive_colors_from_mana_cost(&cost);
        assert_eq!(colors, vec![ManaColor::Red]);
    }

    #[test]
    fn deck_payload_serializes_roundtrips() {
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: vec![DeckEntry {
                    card: make_creature_face(),
                    count: 4,
                }],
                sideboard: vec![],
                commander: vec![],
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![],
                sideboard: vec![],
                commander: vec![],
            },
            ai_decks: vec![],
        };
        let json = serde_json::to_string(&payload).unwrap();
        let deserialized: DeckPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.player.main_deck.len(), 1);
        assert_eq!(deserialized.player.main_deck[0].count, 4);
        assert_eq!(deserialized.player.main_deck[0].card.name, "Grizzly Bears");
    }

    #[test]
    fn load_deck_with_commanders_creates_command_zone_objects() {
        let mut state = GameState::new_two_player(42);
        let commander_face = CardFace {
            name: "Kaalia".to_string(),
            card_type: CardType {
                supertypes: vec![crate::types::card_type::Supertype::Legendary],
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Angel".to_string()],
            },
            ..make_creature_face()
        };

        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: vec![DeckEntry {
                    card: make_creature_face(),
                    count: 3,
                }],
                sideboard: vec![],
                commander: vec![DeckEntry {
                    card: commander_face,
                    count: 1,
                }],
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![DeckEntry {
                    card: make_creature_face(),
                    count: 3,
                }],
                sideboard: vec![],
                commander: vec![],
            },
            ai_decks: vec![],
        };

        load_deck_into_state(&mut state, &payload);

        // Commander is in command zone, not library
        assert_eq!(state.players[0].library.len(), 3);
        assert_eq!(state.command_zone.len(), 1);

        let cmd_id = state.command_zone[0];
        let cmd = &state.objects[&cmd_id];
        assert_eq!(cmd.name, "Kaalia");
        assert_eq!(cmd.zone, Zone::Command);
        assert!(cmd.is_commander);
        assert_eq!(cmd.owner, PlayerId(0));
    }

    #[test]
    fn load_deck_commander_subtypes_collected() {
        let mut state = GameState::new_two_player(42);
        let commander_face = CardFace {
            name: "Kaalia".to_string(),
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Angel".to_string(), "Cleric".to_string()],
            },
            ..make_creature_face()
        };

        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: vec![],
                sideboard: vec![],
                commander: vec![DeckEntry {
                    card: commander_face,
                    count: 1,
                }],
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![],
                sideboard: vec![],
                commander: vec![],
            },
            ai_decks: vec![],
        };

        load_deck_into_state(&mut state, &payload);

        // Commander creature subtypes are collected for Changeling CDA
        assert!(state.all_creature_types.contains(&"Angel".to_string()));
        assert!(state.all_creature_types.contains(&"Cleric".to_string()));
    }
}
