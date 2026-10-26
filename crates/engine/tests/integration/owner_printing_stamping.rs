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
    assert_eq!(state.deck_pools[0].current_companion.len(), 0);
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
                object.printing.get().as_ref().map(|p| p.set_code.clone()),
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
                o.printing.get().clone(),
                o.base_printing.get().clone(),
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

#[test]
fn engine_qualified_only_printing_is_a_loaded_normalization_fixed_point() {
    let key = "shifty face [engine-qualified]";
    let db = CardDatabase::from_json_str(&json!({
        (key): {
            "name": "Shifty Face",
            "mana_cost": {"type":"Cost", "shards":[], "generic":0},
            "card_type": {"supertypes":[], "core_types":["Land"], "subtypes":[]},
            "keywords":[], "abilities":[], "triggers":[], "static_abilities":[], "replacements":[],
            "color_identity":[], "scryfall_oracle_id": "engine-qualified"
        }
    }).to_string()).unwrap();
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
                assert_eq!(object.printing.get().as_ref(), Some(&expected.printing));
                assert_eq!(
                    object.base_printing.get().as_ref(),
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
                assert_eq!(object.printing.get().as_ref(), Some(&printing));
                assert_eq!(object.base_printing.get().as_ref(), Some(&printing));
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
            assert_eq!(object.printing.get().as_ref(), Some(printing));
            assert_eq!(object.base_printing.get().as_ref(), Some(printing));
        }
        assert_exposure(&state, Some(revealed));
    }
}
