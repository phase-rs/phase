//! Owner-selected printings at the resolver → physical-card creation boundary.

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_and_hydrate_decks, resolve_deck_list, DeckList, PlayerDeckList,
};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, RandomDraw};
use rand::seq::SliceRandom;
use serde_json::json;

fn database() -> CardDatabase {
    let cards = [
        "Forest",
        "Island",
        "Attraction A",
        "Attraction B",
        "Contraption A",
        "Contraption B",
        "Plane A",
        "Plane B",
        "Scheme A",
        "Scheme B",
    ];
    let entries: serde_json::Map<String, serde_json::Value> = cards.into_iter().map(|name| (name.to_lowercase(), json!({
        "name": name,
        "mana_cost": {"type":"Cost", "shards":[], "generic":0},
        "card_type": {"supertypes":[], "core_types":["Land"], "subtypes":[]},
        "keywords":[], "abilities":[], "triggers":[], "static_abilities":[], "replacements":[],
        "color_identity":[], "scryfall_oracle_id": format!("oracle-{name}")
    }))).collect();
    CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string())
        .expect("fixture database")
}

fn supplementary_database() -> CardDatabase {
    use engine::game::deck_loading::DEFAULT_PLANAR_DECK_NAMES;
    let mut entries = serde_json::Map::new();
    let cards = ["Forest", "Island"]
        .into_iter()
        .map(|name| (name.to_string(), "Land"))
        .chain(
            DEFAULT_PLANAR_DECK_NAMES
                .into_iter()
                .map(|name| (name.to_string(), "Plane")),
        )
        .chain((0..20).map(|index| {
            (
                if index == 0 {
                    "All in Good Time".to_string()
                } else {
                    format!("Default Scheme {index}")
                },
                "Scheme",
            )
        }));
    for (name, kind) in cards {
        entries.insert(
            name.to_lowercase(),
            json!({
                "name": name, "mana_cost": {"type":"Cost","shards":[],"generic":0},
                "card_type": {"supertypes":[],"core_types":[kind],"subtypes":[]},
                "keywords":[],"abilities":[],"triggers":[],"static_abilities":[],"replacements":[],
                "color_identity":[],"scryfall_oracle_id":format!("oracle-{name}")
            }),
        );
    }
    CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string()).unwrap()
}

fn names(rows: &[(&str, usize)]) -> Vec<String> {
    rows.iter()
        .flat_map(|(name, count)| std::iter::repeat_n((*name).to_string(), *count))
        .collect()
}

fn reference_list() -> DeckList {
    let seat = PlayerDeckList {
        main_deck: names(&[("Forest", 5), ("Island", 3)]),
        attraction_deck: names(&[("Attraction A", 1), ("Attraction B", 1)]),
        contraption_deck: names(&[("Contraption A", 1), ("Contraption B", 1)]),
        planar_deck: names(&[("Plane A", 2), ("Plane B", 2)]),
        scheme_deck: names(&[("Scheme A", 2), ("Scheme B", 2)]),
        ..Default::default()
    };
    DeckList {
        player: seat.clone(),
        opponent: seat,
        ..Default::default()
    }
}

fn allocation_reference(state: &GameState) -> (String, Vec<Vec<u64>>, Vec<usize>) {
    let mut objects: Vec<_> = state.objects.values().collect();
    objects.sort_by_key(|o| o.id);
    let identities = objects
        .iter()
        .map(|o| {
            format!(
                "{}:{}:{}:{}",
                o.id.0,
                o.card_id.0,
                o.owner.0,
                o.printed_ref.as_ref().expect("printed reference").oracle_id
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    let mut containers = Vec::new();
    for player in &state.players {
        containers.push(player.library.iter().map(|id| id.0).collect());
        containers.push(player.attraction_deck.iter().map(|id| id.0).collect());
        containers.push(player.contraption_deck.iter().map(|id| id.0).collect());
    }
    containers.push(state.planar_deck.iter().map(|id| id.0).collect());
    containers.push(state.scheme_deck.iter().map(|id| id.0).collect());
    let mut rng = state.rng.clone();
    let mut subsequent: Vec<_> = (0..12).collect();
    subsequent.shuffle(rng.draw(RandomDraw::Placement));
    (identities, containers, subsequent)
}

// Measured before product edits on fb7d812da16c13246b2a7808e1bac3286bdce5d4.
// Two seats, main Forest×5/Island×3; two Attractions and Contraptions per seat;
// seat 0 supplies Plane A/B×2 or Scheme A/B×2; seed 4910.
const BASE_IDENTITIES: &str = "1:1:0:oracle-Island|2:2:0:oracle-Forest|3:3:0:oracle-Forest|4:4:0:oracle-Island|5:5:0:oracle-Forest|6:6:0:oracle-Island|7:7:0:oracle-Forest|8:8:0:oracle-Forest|9:9:1:oracle-Island|10:10:1:oracle-Forest|11:11:1:oracle-Island|12:12:1:oracle-Forest|13:13:1:oracle-Forest|14:14:1:oracle-Island|15:15:1:oracle-Forest|16:16:1:oracle-Forest|17:17:0:oracle-Attraction B|18:18:0:oracle-Attraction A|19:19:1:oracle-Attraction A|20:20:1:oracle-Attraction B|21:21:0:oracle-Contraption B|22:22:0:oracle-Contraption A|23:23:1:oracle-Contraption A|24:24:1:oracle-Contraption B|25:25:0:oracle-Plane B|26:26:0:oracle-Plane A|27:27:0:oracle-Plane B|28:28:0:oracle-Plane A";

#[test]
fn prechange_allocation_order_and_rng_reference() {
    let db = database();
    for format in [FormatConfig::planechase(), FormatConfig::archenemy()] {
        for annotated in [false, true] {
            let mut list = reference_list();
            if annotated {
                for deck in [&mut list.player, &mut list.opponent] {
                    deck.printings = vec![
                        choice(DeckSection::MainDeck, "Forest", 5, "base-main"),
                        choice(
                            DeckSection::AttractionDeck,
                            "Attraction A",
                            1,
                            "base-attraction",
                        ),
                        choice(
                            DeckSection::ContraptionDeck,
                            "Contraption A",
                            1,
                            "base-contraption",
                        ),
                        choice(DeckSection::PlanarDeck, "Plane A", 2, "base-plane"),
                        choice(DeckSection::SchemeDeck, "Scheme A", 2, "base-scheme"),
                    ];
                }
            }
            let mut state = GameState::new(format.clone(), 2, 4910);
            load_and_hydrate_decks(&mut state, &resolve_deck_list(&db, &list), Some(&db));
            assert_eq!(
                state
                    .objects
                    .values()
                    .filter(|o| o.printing.get().is_some())
                    .count(),
                if annotated { 16 } else { 0 }
            );
            assert_eq!(state.objects.len(), 28);
            assert_eq!(
                state
                    .players
                    .iter()
                    .map(|p| p.library.len())
                    .collect::<Vec<_>>(),
                [8, 8]
            );
            let (identities, containers, subsequent) = allocation_reference(&state);
            let is_planar =
                state.format_config.format == engine::types::format::GameFormat::Planechase;
            assert_eq!(
                identities,
                if is_planar {
                    BASE_IDENTITIES.to_string()
                } else {
                    BASE_IDENTITIES.replace("Plane", "Scheme")
                }
            );
            let mut expected = vec![
                vec![7, 4, 3, 8, 5, 2, 1, 6],
                vec![18, 17],
                vec![21, 22],
                vec![10, 15, 9, 12, 14, 11, 13, 16],
                vec![19, 20],
                vec![24, 23],
            ];
            expected.extend(if is_planar {
                [vec![28, 27, 26, 25], vec![]]
            } else {
                [vec![], vec![28, 27, 26, 25]]
            });
            assert_eq!(containers, expected);
            assert_eq!(subsequent, [5, 11, 9, 6, 8, 2, 1, 10, 3, 7, 0, 4]);
        }
    }
}

use engine::types::card_printing::{CardPrinting, DeckPrinting, DeckSection};
use engine::types::player::PlayerId;

fn choice(section: DeckSection, name: &str, count: u32, tag: &str) -> DeckPrinting {
    DeckPrinting {
        section,
        name: name.into(),
        count,
        printing: CardPrinting {
            set_code: tag.into(),
            collector_number: "1".into(),
        },
    }
}

fn start(state: &mut GameState) {
    let result = engine::game::engine::start_game_with_starting_player(state, PlayerId(0));
    assert!(matches!(
        result.disposition,
        engine::types::game_state::ActionDisposition::Applied
    ));
    assert!(
        matches!(result.waiting_for, engine::types::game_state::WaitingFor::MulliganDecision { ref pending, .. } if pending.len() == state.players.len()),
        "{:?}",
        result.waiting_for
    );
    assert!(state.players.iter().all(|p| p.hand.len() == 7));
}

#[test]
fn normalization_is_lenient_section_bound_ordered_and_capped() {
    let db = database();
    let mut list = reference_list();
    list.player.sideboard = names(&[("Island", 2)]);
    list.player.companion = names(&[("Forest", 1)]);
    let mut rows = vec![
        choice(DeckSection::Unknown, "Forest", 1, "unknown"),
        choice(DeckSection::MainDeck, "Forest", 0, "zero"),
        choice(DeckSection::MainDeck, "Missing", 1, "missing"),
        choice(DeckSection::Commander, "Forest", 1, "wrong-section"),
    ];
    for field in 0..3 {
        for invalid in ["", "bad\u{0085}value"] {
            let mut row = choice(DeckSection::MainDeck, "Forest", 1, "invalid");
            match field {
                0 => row.name = invalid.into(),
                1 => row.printing.set_code = invalid.into(),
                2 => row.printing.collector_number = invalid.into(),
                _ => unreachable!(),
            }
            rows.push(row);
        }
    }
    rows.extend([
        choice(DeckSection::MainDeck, "FOREST", 2, "first"),
        choice(DeckSection::Sideboard, "island", u32::MAX, "side"),
        choice(DeckSection::MainDeck, "forest", u32::MAX, "uncatalogued"),
        choice(DeckSection::MainDeck, "Forest", 1, "exhausted"),
        choice(DeckSection::Companion, "Forest", 2, "companion"),
    ]);
    // Exercise the decoder as well as typed validity: malformed readable elements disappear.
    let mut wire = serde_json::to_value(&list).unwrap();
    let mut values = serde_json::to_value(rows)
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    values.insert(3, json!({"printing": 12}));
    wire["player"]["printings"] = json!(values);
    list = serde_json::from_value(wire).unwrap();
    let payload = resolve_deck_list(&db, &list);
    assert_eq!(
        payload.player.printings,
        [
            choice(DeckSection::MainDeck, "Forest", 2, "first"),
            choice(DeckSection::Sideboard, "Island", 2, "side"),
            choice(DeckSection::MainDeck, "Forest", 3, "uncatalogued"),
            choice(DeckSection::Companion, "Forest", 1, "companion"),
        ]
    );
    let mut state = GameState::new(FormatConfig::freeform(), 2, 4910);
    load_and_hydrate_decks(&mut state, &payload, Some(&db));
    assert_eq!(
        state
            .objects
            .values()
            .filter(|o| o.owner == PlayerId(0) && o.printing.get().is_some())
            .count(),
        5
    );
    // The freeform load drops the dedicated companion slot, but the pool still retains the
    // payload's rows (CR 203.1 + CR 206.1), the Companion row included.
    assert!(state.deck_pools[0].current_companion.is_empty());
    let record = state.deck_pools[0]
        .printings
        .get()
        .as_deref()
        .expect("the pool retains the payload's printing record");
    assert_eq!(record.rows(), payload.player.printings.as_slice());
    assert!(record
        .rows()
        .iter()
        .any(|row| row.section == DeckSection::Companion));

    // The lookup agrees with load stamping: copy k of the face carries the k-th row's
    // printing in stream order (b1), and the lookup's multiset is the loaded objects' (b2).
    let forest = &payload
        .player
        .main_deck
        .iter()
        .find(|entry| entry.card.name == "Forest")
        .expect("P0 main deck holds Forest")
        .card;
    let sequence: Vec<Option<String>> = (0..6)
        .map(|copy| {
            record
                .printing_of(DeckSection::MainDeck, forest, copy)
                .map(|printing| printing.set_code.clone())
        })
        .collect();
    let expected_sequence: Vec<Option<String>> = [
        "first",
        "first",
        "uncatalogued",
        "uncatalogued",
        "uncatalogued",
    ]
    .into_iter()
    .map(|tag| Some(tag.to_string()))
    .chain([None])
    .collect();
    assert_eq!(sequence, expected_sequence);
    let mut looked_up: Vec<String> = sequence.into_iter().flatten().collect();
    looked_up.sort();
    let mut loaded: Vec<String> = state
        .objects
        .values()
        .filter(|o| o.owner == PlayerId(0) && o.name == "Forest")
        .filter_map(|o| o.printing.get().as_ref().map(|p| p.set_code.clone()))
        .collect();
    loaded.sort();
    assert_eq!(looked_up, loaded);

    // Database-free loading retains an equal record. `DisplayOnly` equality is always true,
    // so compare through `get`.
    let mut database_free = GameState::new(FormatConfig::freeform(), 2, 4910);
    load_and_hydrate_decks(&mut database_free, &payload, None);
    assert_eq!(
        database_free.deck_pools[0].printings.get(),
        state.deck_pools[0].printings.get()
    );

    // A seat without rows serializes without the key; the annotated seat has it (reach).
    assert!(serde_json::to_value(&state.deck_pools[0])
        .unwrap()
        .get("printings")
        .is_some());
    assert!(serde_json::to_value(&state.deck_pools[1])
        .unwrap()
        .get("printings")
        .is_none());
    start(&mut state);
}

fn stamps(
    state: &GameState,
    ids: impl IntoIterator<Item = engine::types::identifiers::ObjectId>,
) -> Vec<(String, Option<String>)> {
    let mut rows: Vec<_> = ids
        .into_iter()
        .map(|id| {
            let object = &state.objects[&id];
            assert_eq!(
                object.printing.get(),
                object.base_printing.get(),
                "fresh physical card {id:?}"
            );
            (
                object.name.clone(),
                object.printing.get().as_deref().map(|p| p.set_code.clone()),
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn all_seats_sections_and_netted_prefixes_keep_their_own_streams() {
    let db = database();
    for format in [
        FormatConfig::commander(),
        FormatConfig::oathbreaker(),
        FormatConfig::freeform(),
    ] {
        let mut seats = Vec::new();
        for seat in 0..4 {
            let mut list = PlayerDeckList {
                main_deck: names(&[("Forest", 4), ("Island", 20)]),
                commander: names(&[("Forest", 1)]),
                signature_spell: names(&[("Island", 1)]),
                attraction_deck: names(&[("Attraction A", 2)]),
                contraption_deck: names(&[("Contraption A", 2)]),
                ..Default::default()
            };
            if seat != 3 {
                for tag in ["A", "B", "C"] {
                    list.printings.push(choice(
                        DeckSection::MainDeck,
                        "Forest",
                        1,
                        &format!("{seat}{tag}"),
                    ));
                }
                list.printings.extend([
                    choice(DeckSection::MainDeck, "Island", 2, &format!("{seat}I")),
                    choice(DeckSection::Commander, "Forest", 1, &format!("{seat}K")),
                    choice(
                        DeckSection::SignatureSpell,
                        "Island",
                        1,
                        &format!("{seat}S"),
                    ),
                    choice(
                        DeckSection::AttractionDeck,
                        "Attraction A",
                        1,
                        &format!("{seat}T"),
                    ),
                    choice(
                        DeckSection::ContraptionDeck,
                        "Contraption A",
                        1,
                        &format!("{seat}R"),
                    ),
                ]);
            }
            seats.push(list);
        }
        let list = DeckList {
            player: seats.remove(0),
            opponent: seats.remove(0),
            ai_decks: seats,
            ..Default::default()
        };
        let payload = resolve_deck_list(&db, &list);
        assert_eq!(payload.player.main_deck[0].count, 4);
        let commander = format.command_zone_holds_decklist_commander();
        let signature = format.format == engine::types::format::GameFormat::Oathbreaker;
        let mut state = GameState::new(format, 4, 4910);
        load_and_hydrate_decks(&mut state, &payload, Some(&db));
        for seat in 0..4 {
            let player = &state.players[seat];
            assert_eq!(
                player.library.len(),
                24 - usize::from(commander) - usize::from(signature)
            );
            let forest: Vec<_> = stamps(&state, player.library.iter().copied())
                .into_iter()
                .filter(|(n, _)| n == "Forest")
                .map(|(_, p)| p)
                .collect();
            let mut expected = if seat == 3 {
                vec![None; 4 - usize::from(commander)]
            } else {
                let mut v = vec![
                    Some(format!("{seat}A")),
                    Some(format!("{seat}B")),
                    Some(format!("{seat}C")),
                    None,
                ];
                if commander {
                    v.remove(0);
                }
                v
            };
            expected.sort();
            assert_eq!(forest, expected);
            let island = stamps(&state, player.library.iter().copied())
                .into_iter()
                .filter(|(n, _)| n == "Island" && n != "Forest")
                .collect::<Vec<_>>();
            assert_eq!(island.len(), 20 - usize::from(signature));
            assert_eq!(
                island.iter().filter(|(_, p)| p.is_some()).count(),
                if seat == 3 {
                    0
                } else {
                    2 - usize::from(signature)
                }
            );
            for (ids, name, tag) in [
                (&player.attraction_deck, "Attraction A", "T"),
                (&player.contraption_deck, "Contraption A", "R"),
            ] {
                let mut expected = vec![
                    (name.to_string(), None),
                    (
                        name.to_string(),
                        if seat == 3 {
                            None
                        } else {
                            Some(format!("{seat}{tag}"))
                        },
                    ),
                ];
                expected.sort();
                assert_eq!(stamps(&state, ids.iter().copied()), expected);
            }
            for (name, active, tag) in [("Forest", commander, "K"), ("Island", signature, "S")] {
                let rows = stamps(
                    &state,
                    state.command_zone.iter().copied().filter(|id| {
                        state.objects[id].owner == PlayerId(seat as u8)
                            && state.objects[id].name == name
                    }),
                );
                assert_eq!(
                    rows,
                    if active {
                        vec![(
                            name.into(),
                            if seat == 3 {
                                None
                            } else {
                                Some(format!("{seat}{tag}"))
                            },
                        )]
                    } else {
                        vec![]
                    }
                );
            }
        }
        let before = stamps(&state, state.objects.keys().copied());
        start(&mut state);
        assert_eq!(stamps(&state, state.objects.keys().copied()), before);
    }
}

#[test]
fn shared_piles_use_the_submitting_seat_even_when_stored_face_down() {
    let db = database();
    for section in [DeckSection::PlanarDeck, DeckSection::SchemeDeck] {
        let mut list = reference_list();
        list.ai_decks = vec![list.player.clone()];
        for (seat, deck) in [&mut list.player, &mut list.opponent, &mut list.ai_decks[0]]
            .into_iter()
            .enumerate()
        {
            let name = if section == DeckSection::PlanarDeck {
                "Plane A"
            } else {
                "Scheme A"
            };
            deck.printings = vec![choice(section, name, 2, &format!("shared-{seat}"))];
        }
        let mut format = if section == DeckSection::PlanarDeck {
            FormatConfig::planechase()
        } else {
            FormatConfig::archenemy()
        };
        if section == DeckSection::SchemeDeck {
            format.archenemy_player = Some(PlayerId(2));
        }
        let mut state = GameState::new(format, 3, 4910);
        load_and_hydrate_decks(&mut state, &resolve_deck_list(&db, &list), Some(&db));
        let ids = if section == DeckSection::PlanarDeck {
            &state.planar_deck
        } else {
            &state.scheme_deck
        };
        assert_eq!(ids.len(), 4);
        assert!(ids.iter().all(|id| state.objects[id].face_down));
        let rows = stamps(&state, ids.iter().copied());
        let printed: Vec<_> = rows.iter().filter_map(|(_, p)| p.clone()).collect();
        assert_eq!(
            printed,
            vec![
                if section == DeckSection::PlanarDeck {
                    "shared-0"
                } else {
                    "shared-2"
                };
                2
            ]
        );
        start(&mut state);
        assert_eq!(
            stamps(&state, state.objects.keys().copied())
                .iter()
                .filter(|(_, p)| p.as_deref().is_some_and(|p| p.starts_with("shared-")))
                .count(),
            2
        );
    }
}

#[test]
fn printing_sentinels_are_shuffled_with_faces_before_id_allocation() {
    let db = database();
    for section in [
        DeckSection::MainDeck,
        DeckSection::AttractionDeck,
        DeckSection::ContraptionDeck,
        DeckSection::PlanarDeck,
        DeckSection::SchemeDeck,
    ] {
        let mut list = reference_list();
        let (name, other) = match section {
            DeckSection::MainDeck => ("Forest", "Island"),
            DeckSection::AttractionDeck => ("Attraction A", "Attraction B"),
            DeckSection::ContraptionDeck => ("Contraption A", "Contraption B"),
            DeckSection::PlanarDeck => ("Plane A", "Plane B"),
            DeckSection::SchemeDeck => ("Scheme A", "Scheme B"),
            _ => unreachable!(),
        };
        let submitted = names(&[(name, 20), (other, 3)]);
        match section {
            DeckSection::MainDeck => list.player.main_deck = submitted,
            DeckSection::AttractionDeck => list.player.attraction_deck = submitted,
            DeckSection::ContraptionDeck => list.player.contraption_deck = submitted,
            DeckSection::PlanarDeck => list.player.planar_deck = submitted,
            DeckSection::SchemeDeck => list.player.scheme_deck = submitted,
            _ => unreachable!(),
        }
        list.player.printings = (0..20)
            .map(|i| choice(section, name, 1, &format!("sentinel-{i:02}")))
            .chain(std::iter::once(choice(section, other, 3, "other-face")))
            .collect();
        let format = match section {
            DeckSection::PlanarDeck => FormatConfig::planechase(),
            DeckSection::SchemeDeck => FormatConfig::archenemy(),
            _ => FormatConfig::freeform(),
        };
        let run = || {
            let mut state = GameState::new(format.clone(), 2, 4910);
            load_and_hydrate_decks(&mut state, &resolve_deck_list(&db, &list), Some(&db));
            let mut objects: Vec<_> = state
                .objects
                .values()
                .filter(|o| o.owner == PlayerId(0) && (o.name == name || o.name == other))
                .collect();
            objects.sort_by_key(|o| o.id);
            let rows: Vec<_> = objects
                .iter()
                .map(|o| {
                    assert_eq!(o.id.0, o.card_id.0);
                    assert_eq!(o.printing.get(), o.base_printing.get());
                    (
                        o.id.0,
                        o.name.clone(),
                        o.printing
                            .get()
                            .as_ref()
                            .expect("every selected copy stamped")
                            .set_code
                            .clone(),
                    )
                })
                .collect();
            (state, rows)
        };
        let (mut state, rows) = run();
        assert_eq!(rows.len(), 23);
        let actual: Vec<_> = rows
            .iter()
            .filter(|(_, n, _)| n == name)
            .map(|(_, _, p)| p.clone())
            .collect();
        let submitted: Vec<_> = (0..20).map(|i| format!("sentinel-{i:02}")).collect();
        assert_ne!(
            actual, submitted,
            "measured seed must hide submitted per-copy ordering"
        );
        let mut sorted = actual;
        sorted.sort();
        assert_eq!(sorted, submitted);
        assert!(rows
            .iter()
            .filter(|(_, n, _)| n == other)
            .all(|(_, _, p)| p == "other-face"));
        assert_eq!(run().1, rows);
        start(&mut state);
    }
}

type ObjectReading = (
    u64,
    u64,
    String,
    String,
    u8,
    Option<CardPrinting>,
    Option<CardPrinting>,
);

fn exact_objects(state: &GameState) -> Vec<ObjectReading> {
    let mut rows: Vec<_> = state
        .objects
        .values()
        .map(|o| {
            (
                o.id.0,
                o.card_id.0,
                o.printed_ref.as_ref().expect("printed").oracle_id.clone(),
                o.name.clone(),
                o.owner.0,
                o.printing.get().as_deref().cloned(),
                o.base_printing.get().as_deref().cloned(),
            )
        })
        .collect();
    rows.sort_by_key(|r| r.0);
    rows
}

#[test]
fn engine_replay_preserves_per_id_choices_including_submitted_dandan() {
    use engine::types::replay::ReplayHeader;
    use std::sync::Arc;
    let db = Arc::new(database());
    for format in [FormatConfig::freeform(), FormatConfig::dandan()] {
        let mut list = reference_list();
        if format.format == engine::types::format::GameFormat::Dandan {
            list.player.main_deck = names(&[("Forest", 40), ("Island", 40)]);
        } else {
            list.ai_decks = vec![list.player.clone()];
        }
        list.player.printings = vec![
            choice(DeckSection::MainDeck, "Forest", 1, "replay-A"),
            choice(DeckSection::MainDeck, "forest", 2, "replay-B"),
            choice(DeckSection::MainDeck, "Island", u32::MAX, "replay-I"),
            choice(DeckSection::Sideboard, "Forest", 1, "decoy"),
        ];
        list.opponent.printings = vec![choice(DeckSection::MainDeck, "Forest", 2, "opponent")];
        let count = 2 + list.ai_decks.len() as u8;
        let header = ReplayHeader {
            format_config: format.clone(),
            match_config: Default::default(),
            player_count: count,
            first_player: Some(0),
            seed: 4910,
            deck_data: Some(list.clone()),
        };
        let decoded: ReplayHeader =
            serde_json::from_str(&serde_json::to_string(&header).unwrap()).unwrap();
        assert_eq!(
            decoded.deck_data.as_ref().unwrap().player.printings,
            list.player.printings
        );
        let mut live = GameState::new(format.clone(), count, 4910);
        load_and_hydrate_decks(&mut live, &resolve_deck_list(&db, &list), Some(&db));
        start(&mut live);
        let replay = engine::game::replay::reconstruct_initial_state(&decoded, Some(&db))
            .expect("reconstruct");
        let expected_printed = if format.format == engine::types::format::GameFormat::Dandan {
            43
        } else {
            8
        };
        assert_eq!(
            live.objects
                .values()
                .filter(|o| o.printing.get().is_some())
                .count(),
            expected_printed
        );
        assert_eq!(
            replay
                .objects
                .values()
                .filter(|o| o.printing.get().is_some())
                .count(),
            expected_printed
        );
        assert_eq!(exact_objects(&live), exact_objects(&replay));
        assert_eq!(
            live.players
                .iter()
                .map(|p| p.hand.iter().copied().collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            replay
                .players
                .iter()
                .map(|p| p.hand.iter().copied().collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
        let mut legacy = header;
        let decks = legacy.deck_data.as_mut().unwrap();
        decks.player.printings.clear();
        decks.opponent.printings.clear();
        let legacy = engine::game::replay::reconstruct_initial_state(&legacy, Some(&db))
            .expect("legacy reconstruct");
        assert!(legacy
            .objects
            .values()
            .all(|o| o.printing.get().is_none() && o.base_printing.get().is_none()));
        assert_eq!(
            legacy.players.iter().map(|p| p.hand.len()).sum::<usize>(),
            7 * usize::from(count)
        );
    }
}

#[test]
fn synthetic_defaults_and_momir_emblems_never_take_submitted_art() {
    let committed =
        crate::support::shared_card_db().expect("committed integration database is required");
    let supplementary = supplementary_database();
    for format in [
        FormatConfig::dandan(),
        FormatConfig::momir(),
        FormatConfig::planechase(),
        FormatConfig::archenemy(),
    ] {
        let db = if matches!(
            format.format,
            engine::types::format::GameFormat::Planechase
                | engine::types::format::GameFormat::Archenemy
        ) {
            &supplementary
        } else {
            committed
        };
        let mut list = DeckList::default();
        list.player.printings = vec![
            choice(DeckSection::MainDeck, "Forest", 1, "not-a-submitted-copy"),
            choice(
                DeckSection::PlanarDeck,
                "Academy at Tolaria West",
                1,
                "not-a-submitted-plane",
            ),
            choice(
                DeckSection::SchemeDeck,
                "All in Good Time",
                1,
                "not-a-submitted-scheme",
            ),
        ];
        if format.format != engine::types::format::GameFormat::Dandan {
            list.player.main_deck = names(&[("Forest", 8)]);
            list.opponent.main_deck = names(&[("Island", 8)]);
        }
        let mut state = GameState::new(format.clone(), 2, 4910);
        load_and_hydrate_decks(&mut state, &resolve_deck_list(db, &list), Some(db));
        match format.format {
            engine::types::format::GameFormat::Dandan => assert_eq!(state.objects.len(), 80),
            engine::types::format::GameFormat::Momir => {
                assert_eq!(state.objects.len(), 122);
                assert_eq!(
                    state
                        .players
                        .iter()
                        .map(|p| p.library.len())
                        .collect::<Vec<_>>(),
                    [60, 60]
                );
                let emblems: Vec<_> = state
                    .objects
                    .values()
                    .filter(|o| o.emblem_source.is_some())
                    .collect();
                assert_eq!(emblems.len(), 2);
                assert!(emblems.iter().all(|o| o
                    .emblem_source
                    .as_ref()
                    .unwrap()
                    .printing
                    .get()
                    .is_none()));
                assert!(state
                    .objects
                    .values()
                    .filter(|o| o.emblem_source.is_none())
                    .all(|o| engine::game::deck_loading::MOMIR_SNOW_BASICS
                        .contains(&o.name.as_str())));
            }
            engine::types::format::GameFormat::Planechase => {
                assert_eq!(state.planar_deck.len(), 40)
            }
            engine::types::format::GameFormat::Archenemy => assert_eq!(state.scheme_deck.len(), 20),
            _ => unreachable!(),
        }
        // The ordinary Forest in Planechase/Archenemy is submitted and can be stamped;
        // only synthesized shared-pile cards and Momir/Dandan objects must be empty.
        let synthetic: Vec<_> = if matches!(
            format.format,
            engine::types::format::GameFormat::Planechase
                | engine::types::format::GameFormat::Archenemy
        ) {
            state
                .planar_deck
                .iter()
                .chain(state.scheme_deck.iter())
                .copied()
                .collect()
        } else {
            state.objects.keys().copied().collect()
        };
        assert!(synthetic
            .iter()
            .all(|id| state.objects[id].printing.get().is_none()
                && state.objects[id].base_printing.get().is_none()));
        start(&mut state);
    }
}

#[test]
fn loaded_cards_obey_projection_and_creation_only_concealment() {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::game::visibility::{
        filter_events_for_viewer, filter_state_for_unseated_viewer, filter_state_for_viewer,
    };
    use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
    use engine::types::ability::{Effect, ResolvedAbility, TargetFilter};
    use engine::types::zones::Zone;
    let db = database();
    let mut list = reference_list();
    for (seat, deck) in [&mut list.player, &mut list.opponent]
        .into_iter()
        .enumerate()
    {
        for (name, count) in [
            ("Forest", 5),
            ("Island", 3),
            ("Attraction A", 1),
            ("Attraction B", 1),
            ("Contraption A", 1),
            ("Contraption B", 1),
        ] {
            let section = if name.starts_with("Attraction") {
                DeckSection::AttractionDeck
            } else if name.starts_with("Contraption") {
                DeckSection::ContraptionDeck
            } else {
                DeckSection::MainDeck
            };
            for copy in 0..count {
                deck.printings.push(choice(
                    section,
                    name,
                    1,
                    &format!("private-{seat}-{name}-{copy}"),
                ));
            }
        }
    }
    let mut state = GameState::new(FormatConfig::freeform(), 2, 4910);
    load_and_hydrate_decks(&mut state, &resolve_deck_list(&db, &list), Some(&db));
    start(&mut state);
    let views = |state: &GameState, viewer: Option<PlayerId>| {
        let filtered = viewer.map_or_else(
            || filter_state_for_unseated_viewer(state),
            |p| filter_state_for_viewer(state, p),
        );
        vec![
            serde_json::to_string(&filtered).unwrap(),
            serde_json::to_string(&ClientGameStateRef::wrap_filtered(
                state,
                &filtered,
                viewer.filter(|p| p.0 < 2),
            ))
            .unwrap(),
        ]
    };
    for viewer in [
        Some(PlayerId(0)),
        Some(PlayerId(1)),
        Some(PlayerId(255)),
        None,
    ] {
        let forms = views(&state, viewer);
        for object in state.objects.values() {
            let tag = &object
                .printing
                .get()
                .as_ref()
                .expect("all actual objects annotated")
                .set_code;
            let visible = viewer == Some(object.owner) && object.zone == Zone::Hand;
            for form in &forms {
                assert_eq!(
                    form.contains(tag),
                    visible,
                    "{viewer:?} {} {tag}",
                    object.name
                );
            }
        }
    }
    let public = state.players[0].hand[0];
    let hidden = state.players[0].hand[1];
    let public_print = state.objects[&public].printing.get().clone().unwrap();
    let hidden_print = state.objects[&hidden].printing.get().clone().unwrap();
    let mut events = Vec::new();
    move_object_for_test(
        &mut state,
        ZoneMoveRequest::effect(public, Zone::Battlefield, public),
        &mut events,
    );
    move_object_for_test(
        &mut state,
        ZoneMoveRequest::effect(hidden, Zone::Battlefield, hidden),
        &mut events,
    );
    assert_eq!(state.objects[&public].zone, Zone::Battlefield);
    for viewer in [
        Some(PlayerId(0)),
        Some(PlayerId(1)),
        Some(PlayerId(255)),
        None,
    ] {
        assert!(views(&state, viewer)
            .iter()
            .all(|form| form.contains(&public_print.set_code)));
    }
    // A real public zone-change record is the raw-event positive guard.
    for viewer in [PlayerId(0), PlayerId(1), PlayerId(255)] {
        let filtered = filter_events_for_viewer(&events, &state, viewer);
        assert!(serde_json::to_string(&filtered)
            .unwrap()
            .contains(&public_print.set_code));
    }
    let control = ResolvedAbility::new(
        Effect::GainControl {
            target: TargetFilter::SelfRef,
        },
        vec![],
        public,
        PlayerId(1),
    );
    engine::game::effects::gain_control::resolve(&mut state, &control, &mut events)
        .expect("control changes");
    engine::game::layers::flush_layers(&mut state);
    assert_eq!(state.objects[&public].controller, PlayerId(1));
    assert_eq!(state.objects[&public].printing.get(), &Some(public_print));
    let down = ResolvedAbility::new(
        Effect::TurnFaceDown {
            target: TargetFilter::SelfRef,
            profile: None,
        },
        vec![],
        hidden,
        PlayerId(0),
    );
    engine::game::effects::turn_face_down::resolve(&mut state, &down, &mut events)
        .expect("turn down");
    assert!(state.objects[&hidden].face_down);
    assert!(
        state.objects[&hidden].printing.get().is_none()
            && state.objects[&hidden].base_printing.get().is_none()
    );
    let up = ResolvedAbility::new(
        Effect::TurnFaceUp {
            target: TargetFilter::SelfRef,
        },
        vec![],
        hidden,
        PlayerId(0),
    );
    engine::game::effects::turn_face_up::resolve(&mut state, &up, &mut events).expect("turn up");
    engine::game::printed_cards::rehydrate_game_from_card_db(&mut state, &db);
    assert!(!state.objects[&hidden].face_down);
    assert!(
        state.objects[&hidden].printing.get().is_none()
            && state.objects[&hidden].base_printing.get().is_none()
    );
    for viewer in [
        Some(PlayerId(0)),
        Some(PlayerId(1)),
        Some(PlayerId(255)),
        None,
    ] {
        // Earlier-incarnation public LKI legitimately retains its printing.
        // The current incarnation must not recover it after turn-up/hydration.
        let filtered = viewer.map_or_else(
            || filter_state_for_unseated_viewer(&state),
            |p| filter_state_for_viewer(&state, p),
        );
        assert!(!serde_json::to_string(&filtered.objects[&hidden])
            .unwrap()
            .contains(&hidden_print.set_code));
    }
}

#[test]
fn printing_changes_wire_but_not_rules_or_next_rng_progression() {
    use engine::types::display_only::rules_projection;
    let db = database();
    let plain = reference_list();
    let mut decorated = plain.clone();
    decorated.player.printings = vec![choice(DeckSection::MainDeck, "Forest", 3, "rules-neutral")];
    let run = |list: &DeckList| {
        let mut state = GameState::new(FormatConfig::freeform(), 2, 4910);
        load_and_hydrate_decks(&mut state, &resolve_deck_list(&db, list), Some(&db));
        start(&mut state);
        state
    };
    let plain_state = run(&plain);
    let decorated_state = run(&decorated);
    assert_eq!(
        decorated_state
            .objects
            .values()
            .filter(|o| o.printing.get().is_some())
            .count(),
        3
    );
    assert_ne!(
        serde_json::to_value(&plain_state).unwrap(),
        serde_json::to_value(&decorated_state).unwrap()
    );
    assert_eq!(
        rules_projection(&plain_state).unwrap(),
        rules_projection(&decorated_state).unwrap()
    );
    assert_eq!(
        allocation_reference(&plain_state),
        allocation_reference(&decorated_state)
    );
    let mut different = decorated;
    different.player.main_deck[0] = "Island".into();
    assert_ne!(
        rules_projection(&plain_state).unwrap(),
        rules_projection(&run(&different)).unwrap()
    );
}

#[test]
fn incomplete_annotated_payload_panics_before_mutating_state() {
    use engine::game::deck_loading::load_deck_into_state;
    let db = database();
    let mut list = reference_list();
    list.player.printings = vec![choice(DeckSection::MainDeck, "Forest", 1, "bound")];
    let mut payload = resolve_deck_list(&db, &list);
    payload.player.printing_entry_names.clear();
    let mut state = GameState::new(FormatConfig::freeform(), 2, 4910);
    let before = serde_json::to_value(&state).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        load_deck_into_state(&mut state, &payload)
    }));
    let panic = result.expect_err("missing retained identities must not silently drop art");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap();
    assert!(message.contains("annotated payload requires normalized printing entry bindings"));
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    engine::game::deck_loading::normalize_deck_printings(&db, &mut payload.player);
    load_deck_into_state(&mut state, &payload);
    assert_eq!(
        state
            .objects
            .values()
            .filter(|o| o.printing.get().is_some())
            .count(),
        1
    );
    start(&mut state);
}

#[test]
fn format_gate_keeps_valid_and_invalid_name_deck_verdicts() {
    use engine::game::deck_validation::validate_name_deck_for_format;
    use engine::types::format::GameFormat;
    let db = database();
    for count in [79, 80] {
        for rows in [
            vec![],
            vec![choice(
                DeckSection::MainDeck,
                "Forest",
                u32::MAX,
                "unregistered",
            )],
        ] {
            let list = DeckList {
                player: PlayerDeckList {
                    main_deck: names(&[("Forest", count)]),
                    printings: rows,
                    ..Default::default()
                },
                ..Default::default()
            };
            let payload = resolve_deck_list(&db, &list);
            assert_eq!(payload.player.main_deck[0].count as usize, count);
            let result = validate_name_deck_for_format(
                &db,
                &list.player.main_deck,
                &[],
                &[],
                GameFormat::Dandan,
                None,
            );
            assert_eq!(result.is_ok(), count == 80, "{result:?}");
            if count == 80 {
                let mut state = GameState::new(FormatConfig::dandan(), 2, 4910);
                load_and_hydrate_decks(&mut state, &payload, Some(&db));
                start(&mut state);
                assert_eq!(
                    state
                        .objects
                        .values()
                        .filter(|o| o.printing.get().is_some())
                        .count(),
                    if list.player.printings.is_empty() {
                        0
                    } else {
                        80
                    }
                );
            }
        }
    }
}

/// The key of the one card [`qualified_only_database`] holds: its bare name resolves to
/// nothing, so only this qualified key reaches the face.
const QUALIFIED_ONLY_KEY: &str = "shifty face [engine-qualified]";

fn qualified_only_database() -> CardDatabase {
    CardDatabase::from_json_str(&json!({
        (QUALIFIED_ONLY_KEY): {
            "name": "Shifty Face",
            "mana_cost": {"type":"Cost", "shards":[], "generic":0},
            "card_type": {"supertypes":[], "core_types":["Land"], "subtypes":[]},
            "keywords":[], "abilities":[], "triggers":[], "static_abilities":[], "replacements":[],
            "color_identity":[], "scryfall_oracle_id": "engine-qualified"
        }
    }).to_string()).unwrap()
}

#[test]
fn engine_qualified_only_printing_is_a_loaded_normalization_fixed_point() {
    let key = QUALIFIED_ONLY_KEY;
    let db = qualified_only_database();
    assert!(db.get_face_by_name("Shifty Face").is_none());
    assert_eq!(
        db.get_face_by_name(key)
            .unwrap()
            .scryfall_oracle_id
            .as_deref(),
        Some("engine-qualified")
    );
    let seat = PlayerDeckList {
        main_deck: names(&[(key, 8)]),
        printings: vec![choice(
            DeckSection::MainDeck,
            key,
            u32::MAX,
            "qualified-art",
        )],
        ..Default::default()
    };
    let mut list = DeckList {
        player: seat.clone(),
        opponent: seat,
        ..Default::default()
    };
    let expected = choice(DeckSection::MainDeck, key, 8, "qualified-art");
    for _ in 0..2 {
        let payload = resolve_deck_list(&db, &list);
        for seat in [&payload.player, &payload.opponent] {
            assert_eq!(seat.main_deck.len(), 1);
            assert_eq!(seat.main_deck[0].count, 8);
            assert_eq!(seat.printings.as_slice(), std::slice::from_ref(&expected));
        }
        let mut state = GameState::new(FormatConfig::freeform(), 2, 4910);
        load_and_hydrate_decks(&mut state, &payload, Some(&db));
        start(&mut state);
        assert_eq!(state.objects.len(), 16);
        for owner in [PlayerId(0), PlayerId(1)] {
            let objects: Vec<_> = state
                .objects
                .values()
                .filter(|object| object.owner == owner)
                .collect();
            assert_eq!(objects.len(), 8);
            for object in objects {
                assert_eq!(object.name, "Shifty Face");
                assert_eq!(
                    object.printed_ref.as_ref().unwrap().oracle_id,
                    "engine-qualified"
                );
                assert_eq!(object.printing.get().as_deref(), Some(&expected.printing));
                assert_eq!(
                    object.base_printing.get().as_deref(),
                    Some(&expected.printing)
                );
            }
        }
        list.player.printings = payload.player.printings;
        list.opponent.printings = payload.opponent.printings;
    }
}

#[test]
fn loaded_typed_shared_piles_hide_printings_until_their_reveal_transition() {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
    use engine::types::card_type::CoreType;
    use engine::types::events::GameEvent;

    // Separate typed fixture: the pinned allocation-reference database stays unchanged.
    let db = supplementary_database();
    for section in [DeckSection::PlanarDeck, DeckSection::SchemeDeck] {
        let (shared_names, kind, owner, mut format) = match section {
            DeckSection::PlanarDeck => (
                ["Academy at Tolaria West", "Agyrem", "Akoum"],
                CoreType::Plane,
                PlayerId(0),
                FormatConfig::planechase(),
            ),
            DeckSection::SchemeDeck => (
                ["All in Good Time", "Default Scheme 1", "Default Scheme 2"],
                CoreType::Scheme,
                PlayerId(2),
                FormatConfig::archenemy(),
            ),
            _ => unreachable!(),
        };
        if section == DeckSection::SchemeDeck {
            format.archenemy_player = Some(owner);
        }
        let seat = |number: u8| {
            let mut deck = PlayerDeckList {
                main_deck: names(&[("Forest", 8)]),
                printings: shared_names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        choice(section, name, 1, &format!("p10-shared-{number}-{index}"))
                    })
                    .collect(),
                ..Default::default()
            };
            match section {
                DeckSection::PlanarDeck => {
                    deck.planar_deck = shared_names.iter().map(|name| (*name).into()).collect()
                }
                DeckSection::SchemeDeck => {
                    deck.scheme_deck = shared_names.iter().map(|name| (*name).into()).collect()
                }
                _ => unreachable!(),
            }
            deck
        };
        let list = DeckList {
            player: seat(0),
            opponent: seat(1),
            ai_decks: vec![seat(2)],
            ..Default::default()
        };
        let mut state = GameState::new(format, 3, 4910);
        load_and_hydrate_decks(&mut state, &resolve_deck_list(&db, &list), Some(&db));
        assert_eq!(state.objects.len(), 27);
        let pile = |state: &GameState| match section {
            DeckSection::PlanarDeck => state.planar_deck.iter().copied().collect::<Vec<_>>(),
            DeckSection::SchemeDeck => state.scheme_deck.iter().copied().collect::<Vec<_>>(),
            _ => unreachable!(),
        };
        let ids = pile(&state);
        assert_eq!(ids.len(), 3);
        let expected: Vec<_> = ids
            .iter()
            .map(|id| {
                let object = &state.objects[id];
                let index = shared_names
                    .iter()
                    .position(|name| *name == object.name)
                    .unwrap();
                assert!(object.card_types.core_types.contains(&kind));
                assert_eq!(object.owner, owner);
                assert!(object.face_down);
                let printing = choice(
                    section,
                    &object.name,
                    1,
                    &format!("p10-shared-{}-{index}", owner.0),
                )
                .printing;
                assert_eq!(object.printing.get().as_deref(), Some(&printing));
                assert_eq!(object.base_printing.get().as_deref(), Some(&printing));
                (*id, printing)
            })
            .collect();
        start(&mut state);
        assert_eq!(pile(&state), ids);
        let revealed = ids[0];
        let assert_exposure =
            |state: &GameState, public: Option<engine::types::identifiers::ObjectId>| {
                for viewer in [
                    Some(PlayerId(0)),
                    Some(PlayerId(1)),
                    Some(PlayerId(2)),
                    Some(PlayerId(255)),
                    None,
                ] {
                    let filtered = viewer.map_or_else(
                        || filter_state_for_unseated_viewer(state),
                        |player| filter_state_for_viewer(state, player),
                    );
                    let forms = [
                        serde_json::to_string(&filtered).unwrap(),
                        serde_json::to_string(&ClientGameStateRef::wrap_filtered(
                            state,
                            &filtered,
                            viewer.filter(|player| player.0 < 3),
                        ))
                        .unwrap(),
                    ];
                    for (id, printing) in &expected {
                        for form in &forms {
                            assert_eq!(
                                form.contains(&printing.set_code),
                                public == Some(*id),
                                "{section:?} {viewer:?} sentinel {}",
                                printing.set_code
                            );
                        }
                    }
                    for other in (0..3).filter(|seat| *seat != owner.0) {
                        for index in 0..3 {
                            let decoy = format!("p10-shared-{other}-{index}");
                            assert!(forms.iter().all(|form| !form.contains(&decoy)));
                        }
                    }
                }
            };
        assert_exposure(&state, None);
        match section {
            DeckSection::PlanarDeck => {
                assert_eq!(engine::game::planechase::active_plane(&state), None);
                engine::game::planechase::reveal_starting_plane(&mut state);
                assert_eq!(
                    engine::game::planechase::active_plane(&state),
                    Some(revealed)
                );
            }
            DeckSection::SchemeDeck => {
                assert!(engine::game::archenemy::active_schemes(&state).is_empty());
                let mut events = Vec::new();
                engine::game::archenemy::set_in_motion(&mut state, &mut events);
                assert_eq!(engine::game::archenemy::active_schemes(&state), [revealed]);
                assert!(events.iter().any(|event| matches!(event,
                    GameEvent::SchemeSetInMotion { player_id, scheme_id }
                    if *player_id == owner && *scheme_id == revealed)));
            }
            _ => unreachable!(),
        }
        assert_eq!(pile(&state), ids[1..]);
        assert!(state.command_zone.contains(&revealed));
        for (id, printing) in &expected {
            let object = &state.objects[id];
            assert_eq!(object.face_down, *id != revealed);
            assert!(object.card_types.core_types.contains(&kind));
            assert_eq!(object.printing.get().as_deref(), Some(printing));
            assert_eq!(object.base_printing.get().as_deref(), Some(printing));
        }
        assert_exposure(&state, Some(revealed));
    }
}

/// CR 203.1 + CR 206.1: objects created from a deck-pool entry AFTER load (a companion put
/// into hand, a sideboard card brought in from outside the game) carry the owner's printing.
mod post_load_creators {
    use super::*;
    use engine::database::mtgjson::{AtomicCard, AtomicIdentifiers, SetRelatedCards};
    use engine::game::deck_loading::{
        normalize_deck_printings, round_trip_name, DeckEntry, DeckPayload, PlayerDeckPayload,
    };
    use engine::game::engine::start_game_with_starting_player;
    use engine::game::scenario::{GameRunner, P0, P1};
    use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
    use engine::types::ability::{Effect, OutsideGameSourcePool};
    use engine::types::ability_visit::visit_ability_def;
    use engine::types::actions::{GameAction, MulliganChoice, OutsideGameSelection};
    use engine::types::card::CardFace;
    use engine::types::card_printing::DeckPrintingRecord;
    use engine::types::display_only::{rules_projection, DisplayOnly};
    use engine::types::events::GameEvent;
    use engine::types::game_state::{
        CastPaymentMode, CompanionChoiceSource, CompanionDeclaration, CompanionRevealChoice,
        OutsideGameCardUse, OutsideGameChoiceSource, PersistedGameState, WaitingFor,
    };
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaType, ManaUnit};
    use engine::types::match_config::{DeckCardCount, MatchPhase, MatchType};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use std::collections::BTreeMap;
    use std::ops::ControlFlow;
    use std::sync::Arc;

    const LURRUS: &str = "Lurrus of the Dream-Den";
    const LURRUS_ORACLE_ID: &str = "3bc757c1-3adb-4321-8832-8e1cc9e687f7";
    const WERNOG: &str = "Wernog, Rider's Chaplain";
    /// Living Wish's verbatim Oracle text (Scryfall).
    const LIVING_WISH_ORACLE: &str = "You may reveal a creature or land card you own from outside the game and put it into your hand. Exile Living Wish.";

    /// Living Wish, built through the production card-data face builder so the fetch
    /// vehicle's parse is the parser's, not a hand-written effect.
    fn living_wish_face() -> CardFace {
        let card = AtomicCard {
            name: "Living Wish".to_string(),
            mana_cost: Some("{1}{G}".to_string()),
            colors: vec!["G".to_string()],
            color_identity: vec!["G".to_string()],
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            text: Some(LIVING_WISH_ORACLE.to_string()),
            layout: "normal".to_string(),
            type_line: Some("Sorcery".to_string()),
            types: vec!["Sorcery".to_string()],
            subtypes: Vec::new(),
            supertypes: Vec::new(),
            keywords: None,
            side: None,
            face_name: None,
            mana_value: 2.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: AtomicIdentifiers {
                scryfall_oracle_id: Some("living-wish-oracle".to_string()),
                scryfall_id: Some("living-wish-face".to_string()),
            },
            foreign_data: Vec::new(),
            related_cards: SetRelatedCards::default(),
        };
        // The second argument is what `build_oracle_face` stores as the face's oracle id.
        let face = engine::database::synthesis::build_oracle_face(
            &card,
            Some("living-wish-oracle".to_string()),
        );
        // Reach guards: the vehicle parses cleanly to the revealing sideboard fetch.
        assert!(
            !engine::game::coverage::card_face_has_unimplemented_parts(&face),
            "Living Wish must parse without Unimplemented"
        );
        let mut fetch = None;
        for ability in &face.abilities {
            let _ = visit_ability_def(ability, &mut |effect| {
                if matches!(effect, Effect::SearchOutsideGame { .. }) {
                    fetch = Some(effect.clone());
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            });
        }
        assert!(
            matches!(
                fetch,
                Some(Effect::SearchOutsideGame {
                    reveal: true,
                    destination: Zone::Hand,
                    source_pool: OutsideGameSourcePool::Sideboard,
                    ..
                })
            ),
            "Living Wish must lower to a revealing sideboard fetch into hand, got {fetch:?}"
        );
        face
    }

    fn qualified_key() -> String {
        format!("{} [{LURRUS_ORACLE_ID}]", LURRUS.to_lowercase())
    }

    /// The committed Lurrus under its qualified key, a homonym (same printed name, another
    /// oracle id) under the bare key, plus the named fixture faces (and Living Wish).
    fn lurrus_homonym_database(extra: &[&str]) -> (CardDatabase, String) {
        let fixture =
            crate::support::shared_card_db().expect("committed integration database is required");
        let lurrus = fixture
            .get_face_by_name(LURRUS)
            .expect("fixture holds Lurrus");
        assert_eq!(lurrus.scryfall_oracle_id.as_deref(), Some(LURRUS_ORACLE_ID));
        let mut homonym = lurrus.clone();
        homonym.scryfall_oracle_id = Some("lurrus-homonym".to_string());
        let key = qualified_key();
        let mut entries = serde_json::Map::new();
        entries.insert(key.clone(), serde_json::to_value(lurrus).unwrap());
        entries.insert(
            LURRUS.to_lowercase(),
            serde_json::to_value(&homonym).unwrap(),
        );
        for name in extra {
            let face = if *name == "Living Wish" {
                living_wish_face()
            } else {
                fixture
                    .get_face_by_name(name)
                    .unwrap_or_else(|| panic!("fixture holds {name}"))
                    .clone()
            };
            entries.insert(name.to_lowercase(), serde_json::to_value(&face).unwrap());
        }
        let db = CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string())
            .expect("homonym fixture database");
        // Reach guards: the qualified face round-trips under its qualified key, and the
        // homonym owns the bare name.
        let qualified = db
            .get_face_by_name(&key)
            .expect("qualified Lurrus resolves");
        assert_eq!(
            qualified.scryfall_oracle_id.as_deref(),
            Some(LURRUS_ORACLE_ID)
        );
        assert_eq!(round_trip_name(&db, qualified), key);
        let bare = db.get_face_by_name(LURRUS).expect("bare Lurrus resolves");
        assert_eq!(bare.scryfall_oracle_id.as_deref(), Some("lurrus-homonym"));
        assert_eq!(round_trip_name(&db, bare), LURRUS);
        (db, key)
    }

    fn tag(printing: &Option<engine::types::card_printing::SharedPrinting>) -> Option<&str> {
        printing.as_ref().map(|printing| printing.set_code.as_str())
    }

    fn companion_printing(runner: &GameRunner, seat: PlayerId) -> Option<&str> {
        tag(runner.state().players[seat.0 as usize]
            .companion
            .as_ref()
            .expect("the seat revealed a companion")
            .printing
            .get())
    }

    fn hand(runner: &GameRunner, seat: PlayerId) -> Vec<ObjectId> {
        runner.state().players[seat.0 as usize]
            .hand
            .iter()
            .copied()
            .collect()
    }

    /// The single object `after` holds beyond `before`.
    fn only_new(before: &[ObjectId], after: &[ObjectId]) -> ObjectId {
        let new: Vec<ObjectId> = after
            .iter()
            .copied()
            .filter(|id| !before.contains(id))
            .collect();
        assert_eq!(new.len(), 1, "exactly one new hand object, got {new:?}");
        new[0]
    }

    fn assert_fresh_printing(runner: &GameRunner, id: ObjectId, expected: Option<&str>) {
        let object = &runner.state().objects[&id];
        assert_eq!(tag(object.printing.get()), expected, "{id:?} printing");
        assert_eq!(
            tag(object.base_printing.get()),
            expected,
            "{id:?} base printing"
        );
    }

    fn offer(name: &str, source: CompanionChoiceSource) -> CompanionRevealChoice {
        CompanionRevealChoice {
            name: name.to_string(),
            source,
        }
    }

    fn reveal(runner: &mut GameRunner, choice: CompanionRevealChoice) {
        runner
            .act(GameAction::DeclareCompanion {
                choice: CompanionDeclaration::Reveal(choice),
            })
            .expect("the offered companion is revealed");
    }

    fn assert_offer(runner: &GameRunner, seat: PlayerId, expected: &[CompanionRevealChoice]) {
        let WaitingFor::CompanionReveal {
            player,
            eligible_companions,
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected a companion reveal offer, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, seat);
        let mut offered = eligible_companions.clone();
        offered.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(offered, expected);
    }

    /// Starts the game; the first prompt is the companion reveal.
    fn start_to_reveal(mut state: GameState) -> GameRunner {
        let result = start_game_with_starting_player(&mut state, P0);
        assert!(
            matches!(result.waiting_for, WaitingFor::CompanionReveal { .. }),
            "{:?}",
            result.waiting_for
        );
        GameRunner::from_state(state)
    }

    fn play_to_main(runner: &mut GameRunner) {
        for _ in 0..4 {
            if !matches!(
                runner.state().waiting_for,
                WaitingFor::MulliganDecision { .. }
            ) {
                break;
            }
            runner
                .act(GameAction::MulliganDecision {
                    choice: MulliganChoice::Keep,
                })
                .expect("keep the opening hand");
        }
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().phase, Phase::PreCombatMain);
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
            "{:?}",
            runner.state().waiting_for
        );
    }

    /// Fixture setup only: floating mana for P0, as `GameScenario::with_mana_pool` adds it.
    fn add_pool_mana(runner: &mut GameRunner, types: &[ManaType]) {
        for mana_type in types {
            let _ = runner
                .state_mut()
                .add_mana_to_pool(P0, ManaUnit::new(*mana_type, ObjectId(0), false, vec![]));
        }
    }

    fn companion_to_hand(runner: &mut GameRunner) -> ObjectId {
        add_pool_mana(runner, &[ManaType::Colorless; 3]);
        let before = hand(runner, P0);
        runner
            .act(GameAction::CompanionToHand)
            .expect("the companion special action is paid from the pool");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ));
        only_new(&before, &hand(runner, P0))
    }

    #[test]
    fn dedicated_companion_carries_its_own_qualified_printing_into_hand() {
        let (db, key) = lurrus_homonym_database(&["Plains", "Swamp", WERNOG]);
        let decoy = choice(DeckSection::Sideboard, LURRUS, 1, "homonym-sideboard");
        let companion_row = choice(DeckSection::Companion, &key, 1, "qualified-companion");
        let list = DeckList {
            player: PlayerDeckList {
                main_deck: names(&[("Plains", 10), ("Swamp", 10)]),
                commander: names(&[(WERNOG, 1)]),
                companion: vec![key.clone()],
                // Dropped from the pool by `SideboardPolicy::Forbidden` (CR 903.5e), but its
                // row is normalized against the payload and retained.
                sideboard: vec![LURRUS.to_string()],
                printings: vec![decoy.clone(), companion_row.clone()],
                ..Default::default()
            },
            opponent: PlayerDeckList {
                main_deck: names(&[("Plains", 20)]),
                ..Default::default()
            },
            ..Default::default()
        };
        let payload = resolve_deck_list(&db, &list);
        assert_eq!(payload.player.printings, [decoy, companion_row]);

        let mut state = GameState::new(FormatConfig::commander(), 2, 4910);
        load_and_hydrate_decks(&mut state, &payload, Some(&db));
        let qualified = &payload.player.companion[0].card;
        assert_eq!(
            qualified.scryfall_oracle_id.as_deref(),
            Some(LURRUS_ORACLE_ID)
        );
        let record = state.deck_pools[0]
            .printings
            .get()
            .as_deref()
            .expect("P0 retains a printing record");
        assert_eq!(record.rows(), payload.player.printings.as_slice());
        assert_eq!(
            record.lookup_name(DeckSection::Companion, qualified),
            Some(key.as_str())
        );

        let mut runner = start_to_reveal(state);
        assert_offer(
            &runner,
            P0,
            &[offer(LURRUS, CompanionChoiceSource::Dedicated)],
        );
        reveal(&mut runner, offer(LURRUS, CompanionChoiceSource::Dedicated));
        assert_eq!(companion_printing(&runner, P0), Some("qualified-companion"));

        play_to_main(&mut runner);
        let companion = companion_to_hand(&mut runner);
        let object = &runner.state().objects[&companion];
        assert_eq!(object.name, LURRUS);
        assert_eq!(
            object.printed_ref.as_ref().expect("printed ref").oracle_id,
            LURRUS_ORACLE_ID
        );
        assert_fresh_printing(&runner, companion, Some("qualified-companion"));
    }

    /// Casts a Living Wish from P0's hand and brings sideboard slot 0 in, expecting
    /// `available` copies to be offered. Returns the new hand object (found by a hand-id
    /// diff, since every copy shares one oracle id), the Wish, and the choice's events.
    fn fetch_with_living_wish(
        runner: &mut GameRunner,
        available: u32,
        expected_printing: &str,
    ) -> (ObjectId, ObjectId, Vec<GameEvent>) {
        add_pool_mana(runner, &[ManaType::Green, ManaType::Colorless]);
        let before = hand(runner, P0);
        let wish = before
            .iter()
            .copied()
            .find(|id| runner.state().objects[id].name == "Living Wish")
            .expect("the hand holds Living Wish");
        let card_id = runner.state().objects[&wish].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: wish,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("Living Wish is castable");
        for _ in 0..8 {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve Living Wish");
            } else {
                break;
            }
        }
        let WaitingFor::OutsideGameChoice {
            player,
            choices,
            reveal,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected the outside-game choice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert!(*reveal);
        assert!(
            matches!(
                choices.as_slice(),
                [entry] if entry.count == available
                    && matches!(
                        &entry.source,
                        OutsideGameChoiceSource::Sideboard { sideboard_index: 0, .. }
                    )
            ),
            "{choices:?}"
        );
        let result = runner
            .act(GameAction::ChooseOutsideGameCards {
                selections: vec![OutsideGameSelection::Sideboard { sideboard_index: 0 }],
            })
            .expect("the sideboard copy is brought in");
        let fetched = only_new(&before, &hand(runner, P0));
        let object = &runner.state().objects[&fetched];
        assert_eq!(object.name, LURRUS);
        assert_eq!(
            object.printed_ref.as_ref().expect("printed ref").oracle_id,
            LURRUS_ORACLE_ID
        );
        assert_fresh_printing(runner, fetched, Some(expected_printing));
        (fetched, wish, result.events)
    }

    /// The state after: a sideboard companion revealed (copy 0 of a four-copy entry), three
    /// Living Wish fetches from the same entry (copies 1-3), and the companion put into hand.
    /// `fetched` is the first fetch.
    struct CompanionAndFetch {
        runner: GameRunner,
        fetched: ObjectId,
        companion_object: ObjectId,
    }

    fn companion_and_fetch() -> CompanionAndFetch {
        let (db, key) =
            lurrus_homonym_database(&["Living Wish", "Plains", "Jegantha, the Wellspring"]);
        let companion_slot = choice(DeckSection::Companion, &key, 1, "companion-slot");
        let first = choice(DeckSection::Sideboard, &key, 1, "q-first");
        let second = choice(DeckSection::Sideboard, &key, 1, "q-second");
        let third = choice(DeckSection::Sideboard, &key, 1, "q-third");
        let fourth = choice(DeckSection::Sideboard, &key, 1, "q-fourth");
        let list = DeckList {
            player: PlayerDeckList {
                main_deck: names(&[("Living Wish", 20)]),
                sideboard: vec![key.clone(); 4],
                // Dropped from the pool in Standard; its row is retained, first.
                companion: vec![key.clone()],
                printings: vec![companion_slot, first, second, third, fourth],
                ..Default::default()
            },
            opponent: PlayerDeckList {
                main_deck: names(&[("Plains", 20)]),
                sideboard: vec!["Jegantha, the Wellspring".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        let payload = resolve_deck_list(&db, &list);
        let mut state = GameState::new(FormatConfig::standard(), 2, 4910);
        state.match_config.match_type = MatchType::Bo3;
        load_and_hydrate_decks(&mut state, &payload, Some(&db));

        let mut runner = start_to_reveal(state);
        assert_offer(
            &runner,
            P0,
            &[offer(LURRUS, CompanionChoiceSource::Sideboard { index: 0 })],
        );
        reveal(
            &mut runner,
            offer(LURRUS, CompanionChoiceSource::Sideboard { index: 0 }),
        );
        assert_eq!(companion_printing(&runner, P0), Some("q-first"));
        // Reach: the removal the ordinal rule reads.
        assert_eq!(runner.state().deck_pools[0].current_sideboard[0].count, 3);

        let jegantha = "Jegantha, the Wellspring";
        assert_offer(
            &runner,
            P1,
            &[offer(
                jegantha,
                CompanionChoiceSource::Sideboard { index: 0 },
            )],
        );
        reveal(
            &mut runner,
            offer(jegantha, CompanionChoiceSource::Sideboard { index: 0 }),
        );
        let wire = serde_json::to_value(runner.state()).unwrap();
        assert!(companion_printing_is_none(&runner, P1));
        assert!(wire["players"][1]["companion"].get("printing").is_none());
        assert!(wire["players"][0]["companion"].get("printing").is_some());

        // Persisted round trip: both carriers survive, and creation works after the restore.
        let record_rows = runner.state().deck_pools[0]
            .printings
            .get()
            .as_deref()
            .expect("P0 record")
            .rows()
            .to_vec();
        let persisted =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        let restored = serde_json::from_value::<PersistedGameState>(persisted)
            .expect("the persisted wire decodes")
            .into_game_state()
            .expect("the restore contract holds");
        // Read through `get`, never `==`: the wrapper is always equal.
        let restored_record = restored.deck_pools[0]
            .printings
            .get()
            .as_deref()
            .expect("the restored pool keeps its record");
        assert_eq!(restored_record.rows(), record_rows.as_slice());
        let qualified_face = restored.deck_pools[0].registered_sideboard[0].card.clone();
        assert_eq!(
            restored_record.lookup_name(DeckSection::Sideboard, &qualified_face),
            Some(key.as_str())
        );
        assert_eq!(
            tag(restored.players[0]
                .companion
                .as_ref()
                .expect("restored companion")
                .printing
                .get()),
            Some("q-first")
        );
        let mut runner = GameRunner::from_state(restored);

        play_to_main(&mut runner);
        // Fixture setup: P1 already brought its own sideboard slot 0 in. P0's slot 0 is
        // Lurrus, so a brought-in sum that ignored the seat would count this use.
        runner
            .state_mut()
            .outside_game_cards_brought_in
            .push(OutsideGameCardUse {
                player: P1,
                sideboard_index: 0,
                count: 1,
            });

        // Three Living Wish fetches from the one entry: the sideboard holds four copies,
        // the companion took copy 0, so the fetches take copies 1, 2 and 3. The second
        // fetch sees one use of count 1, the third one use of count 2 (copies, not uses).
        let (fetched, wish, first_events) = fetch_with_living_wish(&mut runner, 3, "q-second");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ));
        assert!(runner.state().stack.is_empty());
        assert_eq!(runner.state().objects[&wish].zone, Zone::Exile);
        // Positive reveal control for the hidden-information test: the fetch revealed it.
        assert!(runner.state().revealed_cards.contains(&fetched));
        assert!(first_events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&fetched)
        )));
        let (second_fetch, _, _) = fetch_with_living_wish(&mut runner, 2, "q-third");
        let (third_fetch, _, _) = fetch_with_living_wish(&mut runner, 1, "q-fourth");
        // All four copies of the entry are taken. This asks the record alone; the fetches
        // above are what exercise the brought-in term of the ordinal.
        let record = runner.state().deck_pools[0]
            .printings
            .get()
            .as_deref()
            .expect("P0 record");
        assert!(record
            .printing_of(DeckSection::Sideboard, &qualified_face, 4)
            .is_none());

        let companion_object = companion_to_hand(&mut runner);
        assert_fresh_printing(&runner, companion_object, Some("q-first"));
        assert_fresh_printing(&runner, fetched, Some("q-second"));
        assert_fresh_printing(&runner, second_fetch, Some("q-third"));
        assert_fresh_printing(&runner, third_fetch, Some("q-fourth"));
        CompanionAndFetch {
            runner,
            fetched,
            companion_object,
        }
    }

    fn companion_printing_is_none(runner: &GameRunner, seat: PlayerId) -> bool {
        runner.state().players[seat.0 as usize]
            .companion
            .as_ref()
            .expect("the seat revealed a companion")
            .printing
            .get()
            .is_none()
    }

    #[test]
    fn sideboard_companion_and_fetch_take_distinct_printings_of_one_entry() {
        let CompanionAndFetch { runner, .. } = companion_and_fetch();
        let state = runner.state();

        // Display-only isolation: equality, the rules projection and the legal actions
        // ignore both new carriers.
        let mut stripped = state.clone();
        for pool in &mut stripped.deck_pools {
            pool.printings = DisplayOnly::new(None);
        }
        for player in &mut stripped.players {
            if let Some(companion) = player.companion.as_mut() {
                companion.printing = DisplayOnly::new(None);
            }
        }
        assert!(*state == stripped);
        assert_eq!(
            rules_projection(state).unwrap(),
            rules_projection(&stripped).unwrap()
        );
        assert_eq!(
            engine::ai_support::legal_actions(state),
            engine::ai_support::legal_actions(&stripped)
        );
        // Positive control: the wire does carry them.
        assert_ne!(
            serde_json::to_value(state).unwrap(),
            serde_json::to_value(&stripped).unwrap()
        );
    }

    #[test]
    fn same_section_homonym_sideboard_companion_keeps_its_own_row() {
        let (db, key) = lurrus_homonym_database(&["Plains"]);
        let plains = db.get_face_by_name("Plains").expect("Plains");
        let homonym = db.get_face_by_name(LURRUS).expect("homonym");
        let qualified = db.get_face_by_name(&key).expect("qualified");
        // The composition `server_core::deck_resolve::resolve_deck` performs: one entry per
        // submitted key (so homonyms stay distinct in one section), then normalization.
        let mut seat = PlayerDeckPayload {
            main_deck: vec![DeckEntry::from_resolved_face(&db, plains, 20)],
            sideboard: vec![
                DeckEntry::from_resolved_face(&db, homonym, 1),
                DeckEntry::from_resolved_face(&db, qualified, 1),
            ],
            printings: vec![
                choice(DeckSection::Sideboard, LURRUS, 1, "homonym-art"),
                choice(DeckSection::Sideboard, &key, 1, "qualified-art"),
            ],
            ..Default::default()
        };
        normalize_deck_printings(&db, &mut seat);
        assert_eq!(seat.printings.len(), 2, "both rows survive normalization");
        let bindings: Vec<(usize, &str)> = seat
            .printing_entry_names
            .iter()
            .map(|binding| (binding.entry_index, binding.name.as_str()))
            .collect();
        assert_eq!(bindings, [(0, LURRUS), (1, key.as_str())]);

        let payload = DeckPayload {
            player: seat,
            opponent: PlayerDeckPayload {
                main_deck: vec![DeckEntry::from_resolved_face(&db, plains, 20)],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::standard(), 2, 4910);
        load_and_hydrate_decks(&mut state, &payload, Some(&db));
        let mut runner = start_to_reveal(state);
        assert_offer(
            &runner,
            P0,
            &[
                offer(LURRUS, CompanionChoiceSource::Sideboard { index: 0 }),
                offer(LURRUS, CompanionChoiceSource::Sideboard { index: 1 }),
            ],
        );
        reveal(
            &mut runner,
            offer(LURRUS, CompanionChoiceSource::Sideboard { index: 1 }),
        );
        assert_eq!(companion_printing(&runner, P0), Some("qualified-art"));

        play_to_main(&mut runner);
        let companion = companion_to_hand(&mut runner);
        assert_eq!(
            runner.state().objects[&companion]
                .printed_ref
                .as_ref()
                .expect("printed ref")
                .oracle_id,
            LURRUS_ORACLE_ID
        );
        assert_fresh_printing(&runner, companion, Some("qualified-art"));
    }

    #[test]
    fn printing_record_is_redacted_with_the_pool_lists_it_annotates() {
        let CompanionAndFetch {
            mut runner,
            fetched,
            companion_object,
            ..
        } = companion_and_fetch();
        let state = runner.state();

        // The transient reveal ended: later actions from `Priority` clear `revealed_cards`.
        // (The positive control, that it WAS revealed, is in `companion_and_fetch`.) The
        // fetched card stays publicly identified through retained public identity knowledge
        // under the engine's information policy: `CardsRevealed` publishes
        // `public_revealed_cards` until the card changes zone (engine.rs ~9056-9074). That is
        // not a CR-mandated reveal duration. Its printing leaves with its identity
        // (`redact_printed_identity`). Reach guard for the paired assertions below.
        assert!(!state.revealed_cards.contains(&fetched));
        assert!(state.public_revealed_cards.contains(&fetched));
        assert!(!state.public_revealed_cards.contains(&companion_object));

        // Positives first: the unfiltered pool carries the record, and the owner's view
        // shows both objects' printings.
        let record_rows = state.deck_pools[0]
            .printings
            .get()
            .as_deref()
            .expect("the unfiltered pool carries the record")
            .rows()
            .to_vec();
        assert!(
            serde_json::to_value(&state.deck_pools[0]).unwrap()["printings"]
                .get("rows")
                .is_some()
        );
        let own = filter_state_for_viewer(state, P0);
        for (id, expected) in [(fetched, "q-second"), (companion_object, "q-first")] {
            let object = own.objects.get(&id).expect("the owner sees their hand");
            assert_eq!(tag(object.printing.get()), Some(expected));
            assert_eq!(tag(object.base_printing.get()), Some(expected));
        }
        let own_text = serde_json::to_string(&own).unwrap();
        assert!(own_text.contains("q-second"), "paired positive");

        // Negatives: no viewer outside the sideboarding prompt carries any seat's record.
        let opponent = filter_state_for_viewer(state, P1);
        let unseated = filter_state_for_unseated_viewer(state);
        for view in [&own, &opponent, &unseated] {
            for pool in &view.deck_pools {
                assert!(serde_json::to_value(pool)
                    .unwrap()
                    .get("printings")
                    .is_none());
            }
        }
        // A printing is visible to a viewer exactly when the object's identity is: the
        // publicly revealed fetched copy shows both (name and `q-second`), and the
        // never-revealed companion hand object shows neither, to the opponent and to an
        // unseated observer alike.
        let identity_visible = |view: &GameState, id: ObjectId| {
            view.objects
                .get(&id)
                .is_some_and(|object| object.name == LURRUS)
        };
        let printing_visible = |view: &GameState, id: ObjectId| {
            view.objects.get(&id).is_some_and(|object| {
                object.printing.get().is_some() || object.base_printing.get().is_some()
            })
        };
        for (view_name, view) in [("opponent", &opponent), ("unseated", &unseated)] {
            for id in [fetched, companion_object] {
                assert_eq!(
                    identity_visible(view, id),
                    printing_visible(view, id),
                    "{view_name}: {id:?} printing is visible exactly when its identity is"
                );
            }
            assert!(identity_visible(view, fetched), "{view_name}: revealed");
            assert_eq!(
                tag(view.objects[&fetched].printing.get()),
                Some("q-second"),
                "{view_name}: the revealed copy's printing"
            );
            assert_eq!(
                tag(view.objects[&fetched].base_printing.get()),
                Some("q-second")
            );
            // Strict negatives: the never-revealed hand object leaks neither.
            assert!(!identity_visible(view, companion_object), "{view_name}");
            assert!(!printing_visible(view, companion_object), "{view_name}");
        }
        // The revealed companion's printing is public (CR 702.139a).
        assert_eq!(
            tag(opponent.players[0]
                .companion
                .as_ref()
                .expect("public companion")
                .printing
                .get()),
            Some("q-first")
        );

        // During the owner's own sideboarding prompt the owner's view keeps the record.
        runner
            .act(GameAction::Concede { player_id: P1 })
            .expect("conceding game one");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::BetweenGamesSideboard { player, .. } if player == P0
            ),
            "{:?}",
            runner.state().waiting_for
        );
        let state = runner.state();
        let own = filter_state_for_viewer(state, P0);
        assert_eq!(
            own.deck_pools[0]
                .printings
                .get()
                .as_deref()
                .expect("the sideboarding owner keeps their record")
                .rows(),
            record_rows.as_slice()
        );
        let opponent = filter_state_for_viewer(state, P1);
        assert!(serde_json::to_value(&opponent.deck_pools[0])
            .unwrap()
            .get("printings")
            .is_none());
    }

    // ---- Between games (CR 100.4): the owner's printings survive the rebuild. ----

    fn concede(runner: &mut GameRunner, seat: PlayerId) {
        runner
            .act(GameAction::Concede { player_id: seat })
            .expect("the seat concedes (CR 104.3a)");
    }

    /// Submits `seat`'s between-games lists, asserting it is `seat`'s sideboard prompt.
    fn submit(
        runner: &mut GameRunner,
        seat: PlayerId,
        main: &[(&str, u32)],
        sideboard: &[(&str, u32)],
    ) {
        assert!(
            matches!(
                &runner.state().waiting_for,
                WaitingFor::BetweenGamesSideboard { player, .. } if *player == seat
            ),
            "expected the sideboard prompt for {seat:?}, got {:?}",
            runner.state().waiting_for
        );
        let counts = |rows: &[(&str, u32)]| -> Vec<DeckCardCount> {
            rows.iter()
                .map(|(name, count)| DeckCardCount {
                    name: (*name).to_string(),
                    count: *count,
                })
                .collect()
        };
        runner
            .act(GameAction::SubmitSideboard {
                main: counts(main),
                sideboard: counts(sideboard),
            })
            .expect("the submission is legal");
    }

    /// Chooses to play first as `chooser`, asserting it is `chooser`'s prompt.
    fn choose_play(runner: &mut GameRunner, chooser: PlayerId) {
        assert!(
            matches!(
                &runner.state().waiting_for,
                WaitingFor::BetweenGamesChoosePlayDraw { player, .. } if *player == chooser
            ),
            "expected the play/draw prompt for {chooser:?}, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChoosePlayDraw { play_first: true })
            .expect("the chooser plays first");
        assert!(runner.state().match_phase == MatchPhase::InGame);
    }

    fn tally(rows: &[(Option<&str>, usize)]) -> BTreeMap<Option<String>, usize> {
        rows.iter()
            .map(|(printing, count)| (printing.map(str::to_string), *count))
            .collect()
    }

    /// The multiset of printing tags over `seat`'s objects, after asserting each object's
    /// current printing is its base printing (nothing has copied or changed one).
    fn printings_of(state: &GameState, seat: PlayerId) -> BTreeMap<Option<String>, usize> {
        let mut tally = BTreeMap::new();
        for object in state.objects.values().filter(|object| object.owner == seat) {
            assert_eq!(
                object.printing.get(),
                object.base_printing.get(),
                "{:?}",
                object.id
            );
            *tally
                .entry(tag(object.printing.get()).map(str::to_string))
                .or_insert(0) += 1;
        }
        tally
    }

    fn record_of(state: &GameState, seat: PlayerId) -> Arc<DeckPrintingRecord> {
        state
            .deck_pools
            .iter()
            .find(|pool| pool.player == seat)
            .expect("the seat has a pool")
            .printings
            .get()
            .clone()
            .expect("the seat retains a printing record")
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum DatabaseHandle {
        Installed,
        Absent,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum OwnerRows {
        Chosen,
        Omitted,
    }

    fn assert_handle(runner: &GameRunner, handle: DatabaseHandle) {
        assert_eq!(
            runner.state().card_db.is_some(),
            handle == DatabaseHandle::Installed,
            "the engine's card-database handle"
        );
    }

    /// Plays game one of a Standard Bo3 (P0 reveals the qualified Lurrus as a sideboard
    /// companion, P1 concedes), submits both seats' unchanged lists, and returns the runner
    /// at the start of game two with P0's game-one record.
    fn unchanged_deck_into_game_two(
        handle: DatabaseHandle,
        rows: OwnerRows,
    ) -> (GameRunner, Option<Arc<DeckPrintingRecord>>) {
        let (db, key) = lurrus_homonym_database(&["Plains"]);
        let db = Arc::new(db);
        let chosen = |printings: Vec<DeckPrinting>| match rows {
            OwnerRows::Chosen => printings,
            OwnerRows::Omitted => Vec::new(),
        };
        let list = DeckList {
            player: PlayerDeckList {
                main_deck: names(&[("Plains", 20)]),
                sideboard: vec![key.clone(), key.clone()],
                printings: chosen(vec![
                    choice(DeckSection::MainDeck, "Plains", 15, "p-first"),
                    choice(DeckSection::MainDeck, "Plains", 5, "p-second"),
                    choice(DeckSection::Sideboard, &key, 1, "q-first"),
                    choice(DeckSection::Sideboard, &key, 1, "q-second"),
                ]),
                ..Default::default()
            },
            opponent: PlayerDeckList {
                main_deck: names(&[("Plains", 20)]),
                printings: chosen(vec![choice(DeckSection::MainDeck, "Plains", 20, "p1-art")]),
                ..Default::default()
            },
            ..Default::default()
        };
        let payload = resolve_deck_list(&db, &list);
        let mut state = GameState::new(FormatConfig::standard(), 2, 4910);
        state.match_config.match_type = MatchType::Bo3;
        if handle == DatabaseHandle::Installed {
            engine::game::printed_cards::install_card_db(&mut state, Arc::clone(&db));
        }
        load_and_hydrate_decks(&mut state, &payload, Some(&db));
        let mut runner = start_to_reveal(state);
        assert_handle(&runner, handle);

        assert_offer(
            &runner,
            P0,
            &[offer(LURRUS, CompanionChoiceSource::Sideboard { index: 0 })],
        );
        reveal(
            &mut runner,
            offer(LURRUS, CompanionChoiceSource::Sideboard { index: 0 }),
        );
        let (expected_p0, expected_p1, expected_companion) = match rows {
            OwnerRows::Chosen => (
                tally(&[(Some("p-first"), 15), (Some("p-second"), 5)]),
                tally(&[(Some("p1-art"), 20)]),
                Some("q-first"),
            ),
            OwnerRows::Omitted => (tally(&[(None, 20)]), tally(&[(None, 20)]), None),
        };
        assert_eq!(companion_printing(&runner, P0), expected_companion);
        assert_eq!(printings_of(runner.state(), P0), expected_p0);
        assert_eq!(printings_of(runner.state(), P1), expected_p1);
        let first_record = runner.state().deck_pools[0].printings.get().clone();
        assert_eq!(first_record.is_some(), rows == OwnerRows::Chosen);

        concede(&mut runner, P1);
        // Reach: the concede restored the revealed copy, so the sideboard holds both again.
        let pool = &runner.state().deck_pools[0];
        assert_eq!(pool.current_sideboard.len(), 1);
        assert_eq!(pool.current_sideboard[0].count, 2);
        assert_eq!(
            pool.current_sideboard[0].card.scryfall_oracle_id.as_deref(),
            Some(LURRUS_ORACLE_ID)
        );
        assert_handle(&runner, handle);
        submit(&mut runner, P0, &[("Plains", 20)], &[(LURRUS, 2)]);
        submit(&mut runner, P1, &[("Plains", 20)], &[]);
        choose_play(&mut runner, P1);
        (runner, first_record)
    }

    fn unchanged_deck_across_games(handle: DatabaseHandle) {
        let (mut runner, first_record) = unchanged_deck_into_game_two(handle, OwnerRows::Chosen);
        let state = runner.state();
        assert_eq!(state.game_number, 2);
        assert_handle(&runner, handle);
        // The reveal is per game (CR 103.2b + CR 702.139a): the game-one `CompanionInfo`
        // is not carried.
        assert!(state.players[0].companion.is_none());
        assert_eq!(
            printings_of(state, P0),
            tally(&[(Some("p-first"), 15), (Some("p-second"), 5)])
        );
        assert_eq!(printings_of(state, P1), tally(&[(Some("p1-art"), 20)]));
        assert!(
            Arc::ptr_eq(
                &record_of(state, P0),
                first_record.as_ref().expect("game one's record")
            ),
            "the continuation pool keeps the same record allocation"
        );

        // Hidden information: the owner's record is on the unfiltered wire and absent for
        // the opponent's view.
        assert!(serde_json::to_value(&state.deck_pools[0])
            .unwrap()
            .get("printings")
            .is_some());
        let opponent_view = filter_state_for_viewer(state, P1);
        let seen = opponent_view
            .deck_pools
            .iter()
            .find(|pool| pool.player == P0)
            .expect("P0's pool");
        assert!(serde_json::to_value(seen)
            .unwrap()
            .get("printings")
            .is_none());

        // Rules isolation (CR 203.1 + CR 206.1): the same match without printings plays
        // game two from an identical rules state; the wire differs (positive control).
        let (twin, _) = unchanged_deck_into_game_two(handle, OwnerRows::Omitted);
        assert_eq!(
            rules_projection(state).unwrap(),
            rules_projection(twin.state()).unwrap()
        );
        assert_ne!(
            serde_json::to_value(state).unwrap(),
            serde_json::to_value(twin.state()).unwrap()
        );

        // The game-two re-reveal prices copy 0 of the carried record again.
        assert_offer(
            &runner,
            P0,
            &[offer(LURRUS, CompanionChoiceSource::Sideboard { index: 0 })],
        );
        reveal(
            &mut runner,
            offer(LURRUS, CompanionChoiceSource::Sideboard { index: 0 }),
        );
        assert_eq!(companion_printing(&runner, P0), Some("q-first"));
    }

    #[test]
    fn unchanged_deck_keeps_its_printings_into_game_two_with_an_installed_database() {
        unchanged_deck_across_games(DatabaseHandle::Installed);
    }

    #[test]
    fn unchanged_deck_keeps_its_printings_into_game_two_without_a_database_handle() {
        unchanged_deck_across_games(DatabaseHandle::Absent);
    }

    #[test]
    fn qualified_only_card_keeps_its_printing_into_game_two() {
        let db = qualified_only_database();
        // Reach: the bare name resolves to nothing; only the qualified key reaches the face.
        assert!(db.get_face_by_name("Shifty Face").is_none());
        assert!(db.get_face_by_name(QUALIFIED_ONLY_KEY).is_some());
        let seat = PlayerDeckList {
            main_deck: names(&[(QUALIFIED_ONLY_KEY, 20)]),
            printings: vec![choice(
                DeckSection::MainDeck,
                QUALIFIED_ONLY_KEY,
                20,
                "qualified-art",
            )],
            ..Default::default()
        };
        let list = DeckList {
            player: seat.clone(),
            opponent: seat,
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::standard(), 2, 4910);
        state.match_config.match_type = MatchType::Bo3;
        load_and_hydrate_decks(&mut state, &resolve_deck_list(&db, &list), Some(&db));
        start(&mut state);
        let mut runner = GameRunner::from_state(state);

        let assert_all_qualified = |runner: &GameRunner| {
            let state = runner.state();
            assert_eq!(state.objects.len(), 40);
            for object in state.objects.values() {
                assert_eq!(object.name, "Shifty Face");
                assert_eq!(
                    object.printed_ref.as_ref().unwrap().oracle_id,
                    "engine-qualified"
                );
                assert_eq!(tag(object.printing.get()), Some("qualified-art"));
                assert_eq!(tag(object.base_printing.get()), Some("qualified-art"));
            }
        };
        assert_all_qualified(&runner);

        concede(&mut runner, P1);
        submit(&mut runner, P0, &[("Shifty Face", 20)], &[]);
        submit(&mut runner, P1, &[("Shifty Face", 20)], &[]);
        choose_play(&mut runner, P1);
        assert_eq!(runner.state().game_number, 2);
        assert_all_qualified(&runner);
    }

    /// CR 903.6 + CR 100.4: a command-zone section is rebuilt from the pool with its face
    /// identity, so its row rebinds in game two like a main-deck row.
    #[test]
    fn commander_keeps_its_printing_into_game_two() {
        let db = database();
        let list = DeckList {
            player: PlayerDeckList {
                main_deck: names(&[("Forest", 20)]),
                commander: names(&[("Island", 1)]),
                printings: vec![
                    choice(DeckSection::MainDeck, "Forest", 20, "forest-art"),
                    choice(DeckSection::Commander, "Island", 1, "commander-art"),
                ],
                ..Default::default()
            },
            opponent: PlayerDeckList {
                main_deck: names(&[("Forest", 20)]),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::commander(), 2, 4910);
        state.match_config.match_type = MatchType::Bo3;
        load_and_hydrate_decks(&mut state, &resolve_deck_list(&db, &list), Some(&db));
        start(&mut state);
        let mut runner = GameRunner::from_state(state);
        let expected = tally(&[(Some("forest-art"), 20), (Some("commander-art"), 1)]);
        // Reach: the commander object exists in the command zone and carries its row.
        assert_eq!(printings_of(runner.state(), P0), expected);
        assert!(runner.state().objects.values().any(|object| {
            object.owner == P0
                && object.zone == Zone::Command
                && tag(object.printing.get()) == Some("commander-art")
        }));

        concede(&mut runner, P1);
        submit(&mut runner, P0, &[("Forest", 20)], &[]);
        submit(&mut runner, P1, &[("Forest", 20)], &[]);
        choose_play(&mut runner, P1);
        assert_eq!(runner.state().game_number, 2);
        assert_eq!(printings_of(runner.state(), P0), expected);
        assert!(runner.state().objects.values().any(|object| {
            object.owner == P0
                && object.zone == Zone::Command
                && tag(object.printing.get()) == Some("commander-art")
        }));
    }

    #[test]
    fn sideboard_swaps_clamp_printings_in_game_two_and_restore_them_in_game_three() {
        let db = database();
        let list = DeckList {
            player: PlayerDeckList {
                main_deck: names(&[("Forest", 16), ("Island", 4)]),
                sideboard: names(&[("Island", 4)]),
                printings: vec![
                    choice(DeckSection::MainDeck, "Forest", 10, "f-a"),
                    choice(DeckSection::MainDeck, "Forest", 6, "f-b"),
                    choice(DeckSection::MainDeck, "Island", 4, "i-main"),
                    choice(DeckSection::Sideboard, "Island", 4, "i-side"),
                ],
                ..Default::default()
            },
            opponent: PlayerDeckList {
                main_deck: names(&[("Forest", 20)]),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::standard(), 2, 4910);
        state.match_config.match_type = MatchType::Bo3;
        load_and_hydrate_decks(&mut state, &resolve_deck_list(&db, &list), Some(&db));
        start(&mut state);
        let mut runner = GameRunner::from_state(state);

        // Game one: the rows' own multiset; P1 has no rows and no record.
        assert!(runner.state().deck_pools[1].printings.get().is_none());
        assert_eq!(
            printings_of(runner.state(), P0),
            tally(&[(Some("f-a"), 10), (Some("f-b"), 6), (Some("i-main"), 4)])
        );
        assert_eq!(printings_of(runner.state(), P1), tally(&[(None, 20)]));
        let first = record_of(runner.state(), P0);

        // Game two: four Forests move to the sideboard and four Islands come in.
        concede(&mut runner, P1);
        submit(
            &mut runner,
            P0,
            &[("Forest", 12), ("Island", 8)],
            &[("Forest", 4)],
        );
        submit(&mut runner, P1, &[("Forest", 20)], &[]);
        choose_play(&mut runner, P1);
        assert_eq!(runner.state().game_number, 2);
        // Main-deck Forest budget 12 clamps `f-b` to 2; main-deck Island budget 8 holds the
        // 4 `i-main` and 4 unmarked; the sideboard Island row has no Island left to bind.
        assert_eq!(
            printings_of(runner.state(), P0),
            tally(&[
                (Some("f-a"), 10),
                (Some("f-b"), 2),
                (Some("i-main"), 4),
                (None, 4)
            ])
        );
        assert_eq!(printings_of(runner.state(), P1), tally(&[(None, 20)]));
        assert!(Arc::ptr_eq(&record_of(runner.state(), P0), &first));

        // Game three: the original lists return, and the carried record rebinds in full.
        concede(&mut runner, P0);
        submit(
            &mut runner,
            P0,
            &[("Forest", 16), ("Island", 4)],
            &[("Island", 4)],
        );
        submit(&mut runner, P1, &[("Forest", 20)], &[]);
        choose_play(&mut runner, P0);
        assert_eq!(runner.state().game_number, 3);
        assert_eq!(
            printings_of(runner.state(), P0),
            tally(&[(Some("f-a"), 10), (Some("f-b"), 6), (Some("i-main"), 4)])
        );
        assert!(Arc::ptr_eq(&record_of(runner.state(), P0), &first));
    }

    /// Two distinct "Twin Scheme" cards (CR 904.3 allows two cards of one English name in a
    /// scheme deck; a planar deck may not, CR 901.3): the bare key is the winner, the
    /// loser is reachable only under its qualified key.
    fn scheme_database() -> CardDatabase {
        let face = |name: &str, kind: &str, oracle_id: &str| {
            json!({
                "name": name,
                "mana_cost": {"type":"Cost", "shards":[], "generic":0},
                "card_type": {"supertypes":[], "core_types":[kind], "subtypes":[]},
                "keywords":[], "abilities":[], "triggers":[], "static_abilities":[],
                "replacements":[], "color_identity":[], "scryfall_oracle_id": oracle_id
            })
        };
        CardDatabase::from_json_str(
            &json!({
                "forest": face("Forest", "Land", "oracle-forest"),
                "twin scheme": face("Twin Scheme", "Scheme", "scheme-winner"),
                "twin scheme [scheme-loser]": face("Twin Scheme", "Scheme", "scheme-loser"),
            })
            .to_string(),
        )
        .expect("scheme fixture database")
    }

    /// The printing tag of each scheme-deck object, keyed by its oracle id.
    fn scheme_printings(state: &GameState) -> BTreeMap<String, Option<String>> {
        state
            .scheme_deck
            .iter()
            .map(|id| {
                let object = &state.objects[id];
                (
                    object.printed_ref.as_ref().unwrap().oracle_id.clone(),
                    tag(object.printing.get()).map(str::to_string),
                )
            })
            .collect()
    }

    #[test]
    fn archenemy_same_named_schemes_keep_their_own_printings_into_game_two() {
        let db = scheme_database();
        let winner = db
            .get_face_by_name("Twin Scheme")
            .expect("bare Twin Scheme");
        let loser_key = "twin scheme [scheme-loser]";
        let loser = db
            .get_face_by_name(loser_key)
            .expect("qualified Twin Scheme");
        // Reach: the two cards are distinct and round-trip under distinct keys.
        assert_eq!(round_trip_name(&db, winner), "Twin Scheme");
        assert_eq!(round_trip_name(&db, loser), loser_key);

        // The composition `server_core::deck_resolve::resolve_deck` performs: one entry per
        // submitted key (the engine's own resolver would merge same-section homonyms).
        let forest = db.get_face_by_name("Forest").expect("Forest");
        let mut seat = PlayerDeckPayload {
            main_deck: vec![DeckEntry::from_resolved_face(&db, forest, 20)],
            scheme_deck: vec![
                DeckEntry::from_resolved_face(&db, winner, 1),
                DeckEntry::from_resolved_face(&db, loser, 1),
            ],
            printings: vec![
                choice(DeckSection::SchemeDeck, loser_key, 1, "loser-art"),
                choice(DeckSection::SchemeDeck, "Twin Scheme", 1, "winner-art"),
            ],
            ..Default::default()
        };
        normalize_deck_printings(&db, &mut seat);
        assert_eq!(seat.printings.len(), 2, "both rows survive normalization");
        let mut bindings: Vec<(usize, &str)> = seat
            .printing_entry_names
            .iter()
            .map(|binding| (binding.entry_index, binding.name.as_str()))
            .collect();
        bindings.sort();
        assert_eq!(bindings, [(0, "Twin Scheme"), (1, loser_key)]);

        let payload = DeckPayload {
            player: seat,
            opponent: PlayerDeckPayload {
                main_deck: vec![DeckEntry::from_resolved_face(&db, forest, 20)],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::archenemy(), 2, 4910);
        state.match_config.match_type = MatchType::Bo3;
        load_and_hydrate_decks(&mut state, &payload, Some(&db));
        let _ = start_game_with_starting_player(&mut state, P0);
        let mut runner = GameRunner::from_state(state);

        let expected: BTreeMap<String, Option<String>> = BTreeMap::from([
            ("scheme-winner".to_string(), Some("winner-art".to_string())),
            ("scheme-loser".to_string(), Some("loser-art".to_string())),
        ]);
        assert_eq!(scheme_printings(runner.state()), expected);
        let first = record_of(runner.state(), P0);

        // The archenemy wins; the match restarts directly after the last submission, with
        // no play/draw prompt.
        concede(&mut runner, P1);
        submit(&mut runner, P0, &[("Forest", 20)], &[]);
        submit(&mut runner, P1, &[("Forest", 20)], &[]);
        assert!(runner.state().match_phase == MatchPhase::InGame);
        assert_eq!(runner.state().game_number, 2);
        assert!(!matches!(
            runner.state().waiting_for,
            WaitingFor::BetweenGamesChoosePlayDraw { .. }
        ));
        assert_eq!(scheme_printings(runner.state()), expected);
        assert!(Arc::ptr_eq(&record_of(runner.state(), P0), &first));
    }
}
