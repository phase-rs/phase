use engine::analysis::deck_signals::{deck_signals, resolve_bracket_sections, DeckSignalKind};
use engine::database::CardDatabase;
use engine::game::deck_loading::{DeckEntry, PlayerDeckList};
use serde_json::Value;
use strum::IntoEnumIterator;

fn shared_db() -> Option<&'static CardDatabase> {
    let Some(db) = crate::support::shared_card_db() else {
        eprintln!("skipping: full card-data export not generated");
        return None;
    };
    Some(db)
}

#[test]
fn resolve_bracket_sections_folds_duplicates_and_preserves_first_seen_order() {
    let Some(db) = shared_db() else {
        return;
    };
    let deck = PlayerDeckList {
        commander: vec!["Counterspell".into()],
        main_deck: vec!["Island".into(), "COUNTERSPELL".into()],
        ..Default::default()
    };

    let resolved = resolve_bracket_sections(&deck, db);

    assert_eq!(resolved.cards.len(), 2);
    assert_eq!(resolved.cards[0].face.name, "Counterspell");
    assert_eq!(resolved.cards[0].count, 2);
    assert_eq!(resolved.cards[1].face.name, "Island");
    assert_eq!(resolved.cards[1].count, 1);
}

#[test]
fn resolve_bracket_sections_reads_the_same_sections_as_estimate_bracket() {
    let Some(db) = shared_db() else {
        return;
    };
    let deck = PlayerDeckList {
        commander: vec!["Counterspell".into()],
        main_deck: vec!["Counterspell".into()],
        companion: vec!["Counterspell".into()],
        signature_spell: vec!["Counterspell".into()],
        sideboard: vec!["Island".into()],
        planar_deck: vec!["Island".into()],
        ..Default::default()
    };

    let resolved = resolve_bracket_sections(&deck, db);

    assert_eq!(resolved.cards.len(), 1);
    assert_eq!(resolved.cards[0].face.name, "Counterspell");
    assert_eq!(resolved.cards[0].count, 4);
}

#[test]
fn deck_card_mana_value_uses_off_stack_value_for_split_cards() {
    let Some(db) = shared_db() else {
        return;
    };
    let Some(face) = db.get_face_by_name("Fire") else {
        eprintln!("skipping: Fire // Ice is not in the committed fixture");
        return;
    };
    let entry = DeckEntry::from_resolved_face(db, face, 1);
    let deck = PlayerDeckList {
        commander: vec!["Fire".into()],
        ..Default::default()
    };

    let resolved = resolve_bracket_sections(&deck, db);

    assert_eq!(face.mana_cost.mana_value(), 2);
    assert_eq!(entry.off_stack_mana_value(), 4);
    assert_eq!(resolved.cards[0].mana_value, entry.off_stack_mana_value());
}

#[test]
fn deck_signals_emits_every_kind_even_at_zero() {
    let Some(db) = shared_db() else {
        return;
    };
    let deck = PlayerDeckList {
        commander: vec!["Island".into()],
        ..Default::default()
    };

    let signals = deck_signals(&deck, db).expect("commander produces signals");

    for kind in DeckSignalKind::iter() {
        let reading = signals.readings.get(&kind).expect("kind always present");
        assert_eq!(reading.count, 0);
        assert!(reading.contributing.is_empty());
    }
}

#[test]
fn deck_signals_carries_no_float_and_no_bracket_axis() {
    let Some(db) = shared_db() else {
        return;
    };
    let deck = PlayerDeckList {
        commander: vec!["Island".into()],
        ..Default::default()
    };
    let signals = deck_signals(&deck, db).expect("commander produces signals");
    let value = serde_json::to_value(signals).expect("signals serialize");

    fn assert_no_float(value: &Value) {
        match value {
            Value::Number(number) => assert!(!number.is_f64(), "float found: {number}"),
            Value::Array(values) => values.iter().for_each(assert_no_float),
            Value::Object(values) => values.values().for_each(assert_no_float),
            Value::Null | Value::Bool(_) | Value::String(_) => {}
        }
    }
    assert_no_float(&value);

    let forbidden = [
        "game_changers",
        "mass_land_denial",
        "extra_turns",
        "efficient_tutors",
    ];
    let top = value.as_object().expect("signals object");
    let readings = top["readings"].as_object().expect("readings object");
    assert!(forbidden
        .iter()
        .all(|key| !top.contains_key(*key) && !readings.contains_key(*key)));
}

#[test]
fn deck_signals_returns_none_without_commander() {
    assert!(deck_signals(&PlayerDeckList::default(), &CardDatabase::default()).is_none());
}

#[test]
fn deck_signals_over_the_shared_fixture_reads_a_known_counterspell() {
    let Some(db) = shared_db() else {
        return;
    };
    if db.get_face_by_name("Counterspell").is_none() {
        eprintln!("skipping: Counterspell is not in the committed fixture");
        return;
    }
    let deck = PlayerDeckList {
        commander: vec!["Island".into()],
        main_deck: vec!["Counterspell".into()],
        ..Default::default()
    };

    let signals = deck_signals(&deck, db).expect("commander produces signals");
    let counterspells = &signals.readings[&DeckSignalKind::Counterspells];

    assert_eq!(counterspells.count, 1);
    assert_eq!(counterspells.contributing, ["Counterspell"]);
}
