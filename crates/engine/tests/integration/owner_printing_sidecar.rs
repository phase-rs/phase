//! The printings sidecar: an optional, display-only list of the owner's explicit
//! printing choices that rides on every deck wire and resolved type.
//!
//! CR 203.1 + CR 206.1: a card's illustration and expansion symbol have no effect
//! on game play. These rows pin the wire contract (byte identity of name-only
//! payloads, lenient decoding, the codec boundary) and conversion carry. Resolver
//! fixtures use valid normalized fixed points, whose sidecars remain unchanged.

use std::collections::BTreeSet;

use engine::database::CardDatabase;
use engine::game::bracket_estimate::CommanderBracketTier;
use engine::game::deck_loading::{
    resolve_deck_list, resolve_player_deck_list, DeckList, DeckPayload, PlayerDeckList,
    PlayerDeckPayload,
};
use engine::starter_decks::DeckData;
use engine::types::card_printing::{
    deserialize_deck_printings, CardPrinting, DeckPrinting, DeckSection,
};
use serde::de::value::{BytesDeserializer, Error as ValueError, MapDeserializer, SeqDeserializer};
use serde_json::{json, Value};

/// The mixed annotation array: fifteen elements of which exactly three decode. Kept, in
/// order: a valid `main_deck` annotation, one whose section this build does not
/// know (`Unknown`), and a `commander` annotation with an extra key. Dropped:
/// count -1, 1.5 and 4294967296; a numeric section; a missing `printing`; a string
/// `printing`; and scalar, array and nested-map elements. Copied verbatim into the
/// server-core test `deck_resolve.rs` (test code cannot be shared across crates).
const MIXED_PRINTINGS: &str = r#"[
    {"section":"main_deck","name":"Forest","count":2,"printing":{"set_code":"lea","collector_number":"294"}},
    {"section":"main_deck","name":"Forest","count":-1,"printing":{"set_code":"lea","collector_number":"294"}},
    {"section":"main_deck","name":"Forest","count":1.5,"printing":{"set_code":"lea","collector_number":"294"}},
    {"section":"main_deck","name":"Forest","count":4294967296,"printing":{"set_code":"lea","collector_number":"294"}},
    {"section":7,"name":"Forest","count":1,"printing":{"set_code":"lea","collector_number":"294"}},
    {"section":"main_deck","name":"Forest","count":1},
    {"section":"main_deck","name":"Forest","count":1,"printing":"lea/294"},
    7,
    "text",
    null,
    true,
    [1,2,[3]],
    {"a":{"b":[{"c":1}]}},
    {"section":"sticker_sheets","name":"Sheet","count":1,"printing":{"set_code":"lea","collector_number":"294"}},
    {"section":"commander","name":"Forest","count":1,"printing":{"set_code":"lea","collector_number":"294"},"extra":{"k":[1]}}
]"#;

/// A valid annotation element as JSON text, for building frames.
const VALID_ELEMENT: &str = r#"{"section":"main_deck","name":"Forest","count":1,"printing":{"set_code":"lea","collector_number":"294"}}"#;

fn lea_294() -> CardPrinting {
    CardPrinting {
        set_code: "lea".to_string(),
        collector_number: "294".to_string(),
    }
}

fn printing(section: DeckSection, name: &str, count: u32, set: &str, number: &str) -> DeckPrinting {
    DeckPrinting {
        section,
        name: name.to_string(),
        count,
        printing: CardPrinting {
            set_code: set.to_string(),
            collector_number: number.to_string(),
        },
    }
}

fn mixed_printings_kept() -> Vec<DeckPrinting> {
    vec![
        printing(DeckSection::MainDeck, "Forest", 2, "lea", "294"),
        printing(DeckSection::Unknown, "Sheet", 1, "lea", "294"),
        printing(DeckSection::Commander, "Forest", 1, "lea", "294"),
    ]
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

fn card(name: &str) -> Value {
    json!({
        "name": name,
        "mana_cost": { "type": "Cost", "shards": [], "generic": 0 },
        "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
        "power": null,
        "toughness": null,
        "loyalty": null,
        "defense": null,
        "oracle_text": null,
        "non_ability_text": null,
        "flavor_name": null,
        "keywords": [],
        "abilities": [],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "color_override": null,
        "color_identity": [],
        "scryfall_oracle_id": format!("oracle-{name}"),
    })
}

/// A minimal in-memory database, so the rows never depend on a card-data export.
fn db() -> CardDatabase {
    let entries: serde_json::Map<String, Value> = ["Forest", "Island", "Lightning Bolt"]
        .iter()
        .map(|name| (name.to_lowercase(), card(name)))
        .collect();
    CardDatabase::from_json_str(&Value::Object(entries).to_string()).expect("fixture db parses")
}

fn frame_with(printings: &str) -> String {
    format!(r#"{{"main_deck":["Forest"],"printings":{printings},"sticker_sheets":["Sheet"]}}"#)
}

/// A name-only list serializes without a `printings` key on all three types, so
/// every existing payload keeps its bytes. Reverting `skip_serializing_if` makes
/// `"printings":[]` appear and fails the exact-string and key-set assertions.
/// `sidecar_round_trips_and_an_unknown_section_stays_unknown` is the positive control: a
/// non-empty sidecar IS serialized.
#[test]
fn name_only_deck_lists_serialize_without_a_printings_key() {
    let wire = DeckData {
        main_deck: names(&["Forest"]),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_string(&wire).unwrap(),
        r#"{"main_deck":["Forest"],"sideboard":[],"commander":[],"companion":[],"attraction_deck":[],"planar_deck":[],"scheme_deck":[],"contraption_deck":[],"sticker_sheets":[],"signature_spell":[],"bracket_tier":"core"}"#
    );

    let base_keys: BTreeSet<&str> = [
        "main_deck",
        "sideboard",
        "commander",
        "companion",
        "attraction_deck",
        "planar_deck",
        "scheme_deck",
        "contraption_deck",
        "sticker_sheets",
        "signature_spell",
        "bracket_tier",
    ]
    .into_iter()
    .collect();
    let keys_of = |value: Value| -> BTreeSet<String> {
        value
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect()
    };
    let expected: BTreeSet<String> = base_keys.iter().map(|key| key.to_string()).collect();
    assert_eq!(
        keys_of(serde_json::to_value(PlayerDeckList::default()).unwrap()),
        expected
    );
    assert_eq!(
        keys_of(serde_json::to_value(PlayerDeckPayload::default()).unwrap()),
        expected
    );
    let seats = serde_json::to_value(DeckList::default()).unwrap();
    assert_eq!(keys_of(seats["player"].clone()), expected);
    assert_eq!(keys_of(seats["opponent"].clone()), expected);
}

/// Legacy frames that predate the field decode with an empty sidecar. Reverting
/// `default` makes the missing key a decode error.
#[test]
fn legacy_deck_frames_decode_with_an_empty_sidecar() {
    let wire: DeckData =
        serde_json::from_str(r#"{"main_deck":["Forest"],"sideboard":["Island"]}"#).unwrap();
    assert_eq!(wire.main_deck, names(&["Forest"]));
    assert!(wire.printings.is_empty());

    let list: PlayerDeckList =
        serde_json::from_str(r#"{"main_deck":["Forest"],"sideboard":["Island"]}"#).unwrap();
    assert_eq!(list.sideboard, names(&["Island"]));
    assert!(list.printings.is_empty());

    let seats: DeckList = serde_json::from_str(
        r#"{"player":{"main_deck":["Forest"]},"opponent":{"main_deck":["Island"]}}"#,
    )
    .unwrap();
    assert_eq!(seats.player.main_deck, names(&["Forest"]));
    assert!(seats.player.printings.is_empty());
    assert!(seats.opponent.printings.is_empty());

    let payload: PlayerDeckPayload =
        serde_json::from_str(r#"{"sticker_sheets":["Sheet"]}"#).unwrap();
    assert_eq!(payload.sticker_sheets, names(&["Sheet"]));
    assert!(payload.printings.is_empty());
}

/// Malformed elements are dropped, valid and unknown-section ones are kept in
/// order, and the list, the next field and the next seat still decode. Reverting
/// `deserialize_with` makes the first malformed element fail the whole decode.
#[test]
fn malformed_sidecar_elements_are_dropped_without_failing_the_deck() {
    let frame = frame_with(MIXED_PRINTINGS);

    let wire: DeckData = serde_json::from_str(&frame).expect("DeckData decodes");
    assert_eq!(wire.printings, mixed_printings_kept());
    assert_eq!(wire.sticker_sheets, names(&["Sheet"]));

    let list: PlayerDeckList = serde_json::from_str(&frame).expect("PlayerDeckList decodes");
    assert_eq!(list.printings, mixed_printings_kept());
    assert_eq!(list.sticker_sheets, names(&["Sheet"]));

    // `PlayerDeckPayload`'s card sections hold resolved entries, so they stay empty.
    let payload_frame = format!(r#"{{"printings":{MIXED_PRINTINGS},"sticker_sheets":["Sheet"]}}"#);
    let payload: PlayerDeckPayload =
        serde_json::from_str(&payload_frame).expect("PlayerDeckPayload decodes");
    assert_eq!(payload.printings, mixed_printings_kept());
    assert_eq!(payload.sticker_sheets, names(&["Sheet"]));

    // The seat that follows a seat holding the mixed array still decodes.
    let seats_frame = format!(
        r#"{{"player":{{"main_deck":["Forest"],"printings":{MIXED_PRINTINGS}}},"opponent":{{"main_deck":["Island"],"sticker_sheets":["Sheet"]}}}}"#
    );
    let seats: DeckList = serde_json::from_str(&seats_frame).expect("DeckList decodes");
    assert_eq!(seats.player.printings, mixed_printings_kept());
    assert_eq!(seats.opponent.main_deck, names(&["Island"]));
    assert_eq!(seats.opponent.sticker_sheets, names(&["Sheet"]));
}

/// The boundary of "never fatal for the sidecar". An element the codec cannot
/// materialise at all (a JSON number outside the finite f64 range) fails the frame
/// exactly as the same value in any other field does, with the same
/// `Category::Syntax`; everything the codec can read is isolated. A decoder that
/// swallowed the stream error would make the first block decode; restoring
/// `serde_json::Value`'s own `Deserialize` or dropping the depth cap fails the 200-deep
/// row with `recursion limit exceeded`.
#[test]
fn an_element_the_codec_cannot_read_fails_the_frame_like_any_other_field() {
    use serde_json::error::Category;

    // Codec-level failures: Err, Syntax, and the same as the control field.
    let control =
        serde_json::from_str::<DeckData>(r#"{"main_deck":["Forest"],"sticker_sheets":[1e400]}"#)
            .expect_err("1e400 fails any field of the frame");
    assert_eq!(control.classify(), Category::Syntax);
    for printings in ["[1e400]", "[-1e400]"] {
        let err = serde_json::from_str::<DeckData>(&frame_with(printings))
            .expect_err("an unreadable number in the sidecar fails the frame");
        assert_eq!(err.classify(), Category::Syntax, "{printings}");
    }
    // Within the depth cap an element is materialised whole, extra keys included.
    let extra = format!(
        r#"[{}]"#,
        VALID_ELEMENT.replace("}}", r#"},"extra":[1e400]}"#)
    );
    let err = serde_json::from_str::<DeckData>(&frame_with(&extra))
        .expect_err("an unreadable number in a valid element's extra key fails the frame");
    assert_eq!(err.classify(), Category::Syntax);

    // Everything the codec can read is isolated: Ok, and the next field decoded.
    let ok_cases: Vec<(&str, String, Vec<DeckPrinting>)> = vec![
        ("finite 1e300", "[1e300]".to_string(), vec![]),
        (
            "a type error after materialisation",
            r#"[{"section":"main_deck","name":"Forest","count":"x","printing":{"set_code":"lea","collector_number":"294"}}]"#
                .to_string(),
            vec![],
        ),
        (
            "a 200-deep nested element",
            format!("[{}{}]", "[".repeat(200), "]".repeat(200)),
            vec![],
        ),
        (
            "a valid element whose extra key nests 30 deep",
            format!(
                "[{}]",
                VALID_ELEMENT.replacen(
                    r#""count":1,"#,
                    &format!(r#""count":1,"extra":{}{},"#, "[".repeat(30), "]".repeat(30)),
                    1
                )
            ),
            vec![printing(DeckSection::MainDeck, "Forest", 1, "lea", "294")],
        ),
        (
            "an unreadable number nested beyond the depth cap",
            format!("[{}1e400{}]", "[".repeat(21), "]".repeat(21)),
            vec![],
        ),
    ];
    for (label, printings, expected) in ok_cases {
        let wire: DeckData = serde_json::from_str(&frame_with(&printings))
            .unwrap_or_else(|err| panic!("{label}: must decode: {err}"));
        assert_eq!(wire.printings, expected, "{label}");
        assert_eq!(wire.sticker_sheets, names(&["Sheet"]), "{label}");
    }
}

/// The two non-JSON visitor shapes `serde-wasm-bindgen` hands a visitor inside an
/// element (a `Uint8Array`/`ArrayBuffer` as bytes, a `Map` with number keys) never
/// fail the deck. They are modelled natively with serde's own value deserializers.
/// Buffering the element through `serde_json::Value`'s `Deserialize` instead returns
/// `Err` for both.
#[test]
fn non_json_visitor_shapes_inside_an_element_never_fail_the_deck() {
    let bytes = SeqDeserializer::<_, ValueError>::new(
        vec![BytesDeserializer::<ValueError>::new(b"ab")].into_iter(),
    );
    assert_eq!(deserialize_deck_printings(bytes), Ok(vec![]));

    let number_keyed = SeqDeserializer::<_, ValueError>::new(
        vec![MapDeserializer::<_, ValueError>::new(
            vec![(1u32, 2u32), (3u32, 4u32)].into_iter(),
        )]
        .into_iter(),
    );
    assert_eq!(deserialize_deck_printings(number_keyed), Ok(vec![]));
}

/// A non-sequence `printings` value is an empty sidecar, not a frame error, and the
/// field after it still decodes. Reverting the visitor to `visit_seq` only makes each
/// of these a decode error.
#[test]
fn non_sequence_sidecar_values_decode_as_empty() {
    for value in ["5", r#""x""#, "true", r#"{"a":[1]}"#, "null", "[]"] {
        let frame = frame_with(value);
        let wire: DeckData = serde_json::from_str(&frame)
            .unwrap_or_else(|err| panic!("{value}: DeckData must decode: {err}"));
        assert!(wire.printings.is_empty(), "{value}");
        assert_eq!(wire.sticker_sheets, names(&["Sheet"]), "{value}");

        let list: PlayerDeckList = serde_json::from_str(&frame)
            .unwrap_or_else(|err| panic!("{value}: PlayerDeckList must decode: {err}"));
        assert!(list.printings.is_empty(), "{value}");
        assert_eq!(list.sticker_sheets, names(&["Sheet"]), "{value}");
    }
}

/// A non-empty sidecar round-trips, and a section a newer peer names stays
/// `Unknown` through re-serialization. This is also the positive control for
/// `name_only_deck_lists_serialize_without_a_printings_key`: the `printings` key IS present
/// when the sidecar is not empty.
#[test]
fn sidecar_round_trips_and_an_unknown_section_stays_unknown() {
    let wire = DeckData {
        main_deck: names(&["Forest", "Island"]),
        printings: vec![
            printing(DeckSection::MainDeck, "Forest", 1, "lea", "294"),
            printing(DeckSection::Sideboard, "Island", 1, "lea", "295"),
        ],
        ..Default::default()
    };
    let text = serde_json::to_string(&wire).unwrap();
    assert!(text.contains(r#""printings""#), "{text}");
    assert_eq!(serde_json::from_str::<DeckData>(&text).unwrap(), wire);

    let list = PlayerDeckList::from(wire.clone());
    let list_back: PlayerDeckList =
        serde_json::from_str(&serde_json::to_string(&list).unwrap()).unwrap();
    assert_eq!(list_back.printings, wire.printings);

    let payload = PlayerDeckPayload {
        printings: wire.printings.clone(),
        ..Default::default()
    };
    let payload_back: PlayerDeckPayload =
        serde_json::from_str(&serde_json::to_string(&payload).unwrap()).unwrap();
    assert_eq!(payload_back.printings, wire.printings);

    let unknown: DeckSection = serde_json::from_str(r#""future_deck""#).unwrap();
    assert_eq!(unknown, DeckSection::Unknown);
    let text = serde_json::to_string(&unknown).unwrap();
    assert_eq!(text, r#""unknown""#);
    assert_eq!(
        serde_json::from_str::<DeckSection>(&text).unwrap(),
        DeckSection::Unknown
    );
}

/// The deck-list field a known section names, or `None` for `Unknown`. Exhaustive
/// with no wildcard on purpose: a new `DeckSection` variant is a build error here,
/// forcing the totality pin below to be updated with it.
fn section_list(deck: &DeckData, section: DeckSection) -> Option<&Vec<String>> {
    match section {
        DeckSection::MainDeck => Some(&deck.main_deck),
        DeckSection::Sideboard => Some(&deck.sideboard),
        DeckSection::Commander => Some(&deck.commander),
        DeckSection::Companion => Some(&deck.companion),
        DeckSection::AttractionDeck => Some(&deck.attraction_deck),
        DeckSection::PlanarDeck => Some(&deck.planar_deck),
        DeckSection::SchemeDeck => Some(&deck.scheme_deck),
        DeckSection::ContraptionDeck => Some(&deck.contraption_deck),
        DeckSection::SignatureSpell => Some(&deck.signature_spell),
        DeckSection::Unknown => None,
    }
}

/// `DeckSection`'s serialized names are exactly `DeckData`'s card-section field
/// names, and sticker sheets are not a section. A new `DeckData` section without a
/// variant breaks the key-set equality; a new variant breaks `section_list`.
#[test]
fn deck_section_names_are_exactly_the_card_section_fields_of_deck_data() {
    let known = [
        DeckSection::MainDeck,
        DeckSection::Sideboard,
        DeckSection::Commander,
        DeckSection::Companion,
        DeckSection::AttractionDeck,
        DeckSection::PlanarDeck,
        DeckSection::SchemeDeck,
        DeckSection::ContraptionDeck,
        DeckSection::SignatureSpell,
    ];
    let serde_name = |section: DeckSection| -> String {
        serde_json::to_value(section)
            .unwrap()
            .as_str()
            .expect("a unit variant serializes as a string")
            .to_string()
    };

    let deck = DeckData {
        main_deck: names(&["m-main"]),
        sideboard: names(&["m-side"]),
        commander: names(&["m-cmdr"]),
        companion: names(&["m-comp"]),
        attraction_deck: names(&["m-attr"]),
        planar_deck: names(&["m-plan"]),
        scheme_deck: names(&["m-sche"]),
        contraption_deck: names(&["m-cont"]),
        signature_spell: names(&["m-sig"]),
        ..Default::default()
    };
    let value = serde_json::to_value(&deck).unwrap();

    for section in known {
        let list = section_list(&deck, section).expect("a known section names a field");
        assert_eq!(list.len(), 1, "{section:?} marker present");
        assert_eq!(
            value[serde_name(section)],
            json!(list),
            "{section:?} must serialize under its own field name"
        );
    }

    let field_keys: BTreeSet<String> = value
        .as_object()
        .unwrap()
        .keys()
        .filter(|key| !matches!(key.as_str(), "sticker_sheets" | "bracket_tier"))
        .cloned()
        .collect();
    let section_names: BTreeSet<String> =
        known.iter().map(|section| serde_name(*section)).collect();
    assert_eq!(field_keys, section_names);

    assert_eq!(
        serde_json::from_str::<DeckSection>(r#""sticker_sheets""#).unwrap(),
        DeckSection::Unknown,
        "sticker sheets are not a card section"
    );
}

/// `DeckData` and `PlayerDeckList` convert both ways carrying every field, the
/// sidecar included. Every field holds its own distinct marker, so a symmetric swap
/// in both impls (which the round trip alone would miss) fails the per-field
/// assertions against independent literals.
#[test]
fn wire_deck_and_engine_seat_list_convert_with_every_field() {
    let two_printings = || {
        vec![
            printing(DeckSection::MainDeck, "Main", 1, "lea", "1"),
            printing(DeckSection::SignatureSpell, "Sig", 1, "lea", "2"),
        ]
    };
    let deck = DeckData {
        main_deck: names(&["Main"]),
        sideboard: names(&["Side"]),
        commander: names(&["Cmdr"]),
        companion: names(&["Comp"]),
        attraction_deck: names(&["Attr"]),
        planar_deck: names(&["Plan"]),
        scheme_deck: names(&["Sche"]),
        contraption_deck: names(&["Cont"]),
        sticker_sheets: names(&["Sheet"]),
        signature_spell: names(&["Sig"]),
        bracket_tier: CommanderBracketTier::Cedh,
        printings: two_printings(),
    };

    // Forward.
    let list = PlayerDeckList::from(deck.clone());
    assert_eq!(list.main_deck, names(&["Main"]));
    assert_eq!(list.sideboard, names(&["Side"]));
    assert_eq!(list.commander, names(&["Cmdr"]));
    assert_eq!(list.companion, names(&["Comp"]));
    assert_eq!(list.attraction_deck, names(&["Attr"]));
    assert_eq!(list.planar_deck, names(&["Plan"]));
    assert_eq!(list.scheme_deck, names(&["Sche"]));
    assert_eq!(list.contraption_deck, names(&["Cont"]));
    assert_eq!(list.sticker_sheets, names(&["Sheet"]));
    assert_eq!(list.signature_spell, names(&["Sig"]));
    assert_eq!(list.bracket_tier, CommanderBracketTier::Cedh);
    assert_eq!(list.printings, two_printings());

    // Reverse, from a second, disjoint set of markers.
    let other_printings = || vec![printing(DeckSection::Companion, "Comp2", 1, "lea", "9")];
    let reverse_source = PlayerDeckList {
        main_deck: names(&["Main2"]),
        sideboard: names(&["Side2"]),
        commander: names(&["Cmdr2"]),
        companion: names(&["Comp2"]),
        attraction_deck: names(&["Attr2"]),
        planar_deck: names(&["Plan2"]),
        scheme_deck: names(&["Sche2"]),
        contraption_deck: names(&["Cont2"]),
        sticker_sheets: names(&["Sheet2"]),
        signature_spell: names(&["Sig2"]),
        bracket_tier: CommanderBracketTier::Optimized,
        printings: other_printings(),
    };
    let back = DeckData::from(reverse_source);
    assert_eq!(back.main_deck, names(&["Main2"]));
    assert_eq!(back.sideboard, names(&["Side2"]));
    assert_eq!(back.commander, names(&["Cmdr2"]));
    assert_eq!(back.companion, names(&["Comp2"]));
    assert_eq!(back.attraction_deck, names(&["Attr2"]));
    assert_eq!(back.planar_deck, names(&["Plan2"]));
    assert_eq!(back.scheme_deck, names(&["Sche2"]));
    assert_eq!(back.contraption_deck, names(&["Cont2"]));
    assert_eq!(back.sticker_sheets, names(&["Sheet2"]));
    assert_eq!(back.signature_spell, names(&["Sig2"]));
    assert_eq!(back.bracket_tier, CommanderBracketTier::Optimized);
    assert_eq!(back.printings, other_printings());

    // Invariant.
    assert_eq!(DeckData::from(PlayerDeckList::from(deck.clone())), deck);
}

fn seat(main: &str, printings: Vec<DeckPrinting>) -> PlayerDeckList {
    PlayerDeckList {
        main_deck: names(&[main]),
        printings,
        ..Default::default()
    }
}

fn main_names(payload: &PlayerDeckPayload) -> Vec<String> {
    payload
        .main_deck
        .iter()
        .map(|entry| entry.card.name.clone())
        .collect()
}

/// Both engine resolvers preserve these normalized fixed points and keep seats apart.
/// Four seats each hold a distinct valid sidecar (the last holds none), so a cross-seat
/// swap or a dropped field fails. Invalidity, caps and lookup-name normalization
/// are exercised through actual loaded cards in owner_printing_stamping.
#[test]
fn engine_resolvers_carry_each_seats_sidecar_unchanged() {
    let db = db();
    let list = DeckList {
        player: seat(
            "Forest",
            vec![printing(DeckSection::MainDeck, "Forest", 1, "lea", "294")],
        ),
        opponent: seat(
            "Island",
            vec![printing(DeckSection::MainDeck, "Island", 1, "lea", "295")],
        ),
        ai_decks: vec![
            seat(
                "Lightning Bolt",
                vec![printing(
                    DeckSection::MainDeck,
                    "Lightning Bolt",
                    1,
                    "lea",
                    "161",
                )],
            ),
            seat("Forest", vec![]),
        ],
        ..Default::default()
    };

    let payload: DeckPayload = resolve_deck_list(&db, &list);

    // Reach guard: each seat's names resolved, so the resolver ran on every seat.
    assert_eq!(main_names(&payload.player), names(&["Forest"]));
    assert_eq!(main_names(&payload.opponent), names(&["Island"]));
    assert_eq!(main_names(&payload.ai_decks[0]), names(&["Lightning Bolt"]));
    assert_eq!(main_names(&payload.ai_decks[1]), names(&["Forest"]));

    assert_eq!(payload.player.printings, list.player.printings);
    assert_eq!(payload.opponent.printings, list.opponent.printings);
    assert_eq!(payload.ai_decks[0].printings, list.ai_decks[0].printings);
    assert!(
        payload.ai_decks[1].printings.is_empty(),
        "an empty-sidecar seat stays empty beside non-empty neighbours"
    );

    // The single-seat authority.
    let single = resolve_player_deck_list(&db, &list.player);
    assert_eq!(main_names(&single), names(&["Forest"]));
    assert_eq!(single.printings, list.player.printings);
}

/// Reach guard for the frames built from `VALID_ELEMENT`: the text really is a valid
/// annotation, so a "kept" assertion over it measures the decoder and not a fixture typo.
#[test]
fn the_valid_element_const_matches_the_typed_fixture() {
    let decoded: DeckPrinting = serde_json::from_str(VALID_ELEMENT).unwrap();
    assert_eq!(decoded.printing, lea_294());
    assert_eq!(
        decoded,
        printing(DeckSection::MainDeck, "Forest", 1, "lea", "294")
    );
}
