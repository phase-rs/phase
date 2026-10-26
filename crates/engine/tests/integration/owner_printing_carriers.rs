//! The printing carriers: the display-only `printing` fields on the live object, every
//! last-known-information snapshot, the stack row, the library-search view, the emblem
//! source and the debug library row.
//!
//! CR 203.1 + CR 206.1: a card's illustration and expansion symbol have no effect on game
//! play, so no rules or AI path may read a printing. These rows pin the three transparency
//! contracts the carriers rely on: typed equality (including loop detection, CR 104.4b),
//! `Debug` text, and the wire shape (absent when empty, wire-transparent when present, omitted
//! by the rules projection), plus that a restored game keeps every printing it was given.

use std::fmt::Debug;

use engine::analysis::resource::loop_states_equal_modulo_resources;
use engine::game::derived_views::{derive_views, DebugLibraryCardView, StackEntryDisplay};
use engine::game::game_object::{EmblemSource, GameObject};
use engine::game::zones::create_object;
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::card_printing::CardPrinting;
use engine::types::display_only::{rules_projection, DisplayOnly};
use engine::types::events::{GameEvent, LibrarySearchCardFaceView, LibrarySearchCardView};
use engine::types::game_state::{
    GameState, LKISnapshot, PersistedGameState, StackEntry, StackEntryKind, TriggerSourceContext,
    ZoneChangeRecord,
};
use engine::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

fn s1() -> CardPrinting {
    CardPrinting {
        set_code: "SNTA".to_string(),
        collector_number: "1".to_string(),
    }
}

fn s2() -> CardPrinting {
    CardPrinting {
        set_code: "SNTB".to_string(),
        collector_number: "2".to_string(),
    }
}

/// The three printing states every contract is checked across: none, S1, S2.
fn variants() -> [Option<CardPrinting>; 3] {
    [None, Some(s1()), Some(s2())]
}

fn carried(printing: &Option<CardPrinting>) -> DisplayOnly<Option<CardPrinting>> {
    DisplayOnly::new(printing.clone())
}

/// A two-player state holding one battlefield creature.
fn base_state() -> (GameState, ObjectId) {
    let mut state = GameState::new_two_player(42);
    let id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Carrier Bear".to_string(),
        Zone::Battlefield,
    );
    (state, id)
}

fn object_in(state: &GameState, id: ObjectId) -> &GameObject {
    state.objects.get(&id).expect("the fixture object exists")
}

/// The live object with both of its printing fields set.
fn object_with(printing: &Option<CardPrinting>) -> GameObject {
    let (state, id) = base_state();
    let mut object = object_in(&state, id).clone();
    object.printing = carried(printing);
    object.base_printing = carried(printing);
    object
}

fn lki_with(printing: &Option<CardPrinting>) -> LKISnapshot {
    let (state, id) = base_state();
    let mut lki = object_in(&state, id).snapshot_public_characteristics();
    lki.printing = carried(printing);
    lki
}

/// A real zone-change record whose source context carries `printing`.
fn record_with(
    state: &GameState,
    id: ObjectId,
    printing: &Option<CardPrinting>,
) -> ZoneChangeRecord {
    let mut record =
        object_in(state, id).snapshot_for_zone_change(id, Some(Zone::Battlefield), Zone::Graveyard);
    let context = record
        .trigger_source_context
        .as_mut()
        .expect("a real record carries its source context");
    context.lki.printing = carried(printing);
    record
}

fn context_with(
    state: &GameState,
    id: ObjectId,
    printing: &Option<CardPrinting>,
    power: i32,
) -> TriggerSourceContext {
    let mut context = record_with(state, id, printing)
        .trigger_source_context
        .expect("a real record carries its source context");
    context.lki.power = Some(power);
    context
}

fn zone_changed_with(printing: &Option<CardPrinting>) -> GameEvent {
    let (state, id) = base_state();
    GameEvent::ZoneChanged {
        object_id: id,
        from: Some(Zone::Battlefield),
        to: Zone::Graveyard,
        record: Box::new(record_with(&state, id, printing)),
    }
}

fn emblem_with(printing: &Option<CardPrinting>) -> EmblemSource {
    EmblemSource {
        name: "Source Walker".to_string(),
        printed_ref: None,
        printing: carried(printing),
    }
}

fn search_view_with(printing: &Option<CardPrinting>) -> LibrarySearchCardView {
    let (state, id) = base_state();
    let object = object_in(&state, id);
    let face = LibrarySearchCardFaceView {
        name: object.name.clone(),
        mana_cost: object.mana_cost.clone(),
        mana_value: object.mana_cost.mana_value(),
        colors: object.color.clone(),
        card_type: object.card_types.clone(),
        keywords: object.keywords.clone(),
        power: object.power,
        toughness: object.toughness,
        loyalty: object.loyalty,
        printed_ref: None,
    };
    LibrarySearchCardView {
        owner: object.owner,
        zone: object.zone,
        identity: ObjectIncarnationRef::from_object(object),
        card_id: object.card_id,
        printing: carried(printing),
        current_face: face.clone(),
        front_face: face,
        back_face: None,
    }
}

fn debug_row_with(printing: &Option<CardPrinting>) -> DebugLibraryCardView {
    DebugLibraryCardView {
        object_id: ObjectId(9),
        name: "Library Card".to_string(),
        printed_ref: None,
        printing: carried(printing),
    }
}

/// A stack trigger whose ability carries `context` as its source authority.
fn triggered_entry(source: ObjectId, context: TriggerSourceContext) -> StackEntry {
    let mut ability = ResolvedAbility::new(Effect::NoOp, vec![], source, PlayerId(0));
    ability.trigger_source = Some(context);
    StackEntry {
        id: ObjectId(600),
        source_id: source,
        controller: PlayerId(0),
        kind: StackEntryKind::TriggeredAbility {
            source_id: source,
            ability: Box::new(ability),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        },
    }
}

/// A state with one stack trigger whose context carries `printing` and `power`.
fn state_with_stack_trigger(printing: &Option<CardPrinting>, power: i32) -> GameState {
    let (mut state, id) = base_state();
    let context = context_with(&state, id, printing, power);
    state.stack.push_back(triggered_entry(id, context));
    state
}

/// The real derived stack row for the fixture trigger, with `printing` set on it.
fn stack_row_with(printing: &Option<CardPrinting>) -> StackEntryDisplay {
    let state = state_with_stack_trigger(&None, 2);
    let mut row = derive_views(&state, None)
        .stack_entry_details
        .get(&ObjectId(600))
        .expect("the stack trigger has a derived row")
        .clone();
    row.printing = carried(printing);
    row
}

/// `make(none) == make(S1) == make(S2)`, a rules difference still separates, and the printing
/// is really on the value (the wire text carries it).
fn assert_transparent<T: PartialEq + Debug + Serialize>(
    label: &str,
    make: impl Fn(&Option<CardPrinting>) -> T,
    rename: impl Fn(&mut T),
) {
    let [none, first, second] = variants().map(|printing| make(&printing));
    assert!(
        serde_json::to_string(&first).unwrap().contains("SNTA"),
        "{label}: the printing is really on the value"
    );
    assert_eq!(none, first, "{label}: none == S1");
    assert_eq!(first, second, "{label}: S1 == S2");
    let mut renamed = make(&Some(s1()));
    rename(&mut renamed);
    assert_ne!(
        first, renamed,
        "{label}: a rules difference still separates"
    );
}

#[test]
fn carriers_are_equality_transparent() {
    assert_transparent("LKISnapshot", lki_with, |lki| lki.name = "Elf".to_string());
    assert_transparent("EmblemSource", emblem_with, |emblem| {
        emblem.name = "Other Walker".to_string()
    });
    assert_transparent("LibrarySearchCardView", search_view_with, |view| {
        view.current_face.name = "Elf".to_string()
    });
    assert_transparent("StackEntryDisplay", stack_row_with, |row| {
        row.source_name = "Elf".to_string()
    });
    assert_transparent("DebugLibraryCardView", debug_row_with, |row| {
        row.name = "Other Card".to_string()
    });

    // Embedders: an event holding a record holding a context holding a snapshot.
    assert_transparent("GameEvent::ZoneChanged", zone_changed_with, |event| {
        if let GameEvent::ZoneChanged { record, .. } = event {
            record.name = "Elf".to_string();
        } else {
            panic!("the fixture is a zone-change event");
        }
    });

    // The game state compares its stack by derived equality, so a snapshot printing nested
    // under a stack trigger is invisible and a rules fact at the same position is not.
    let none = state_with_stack_trigger(&None, 2);
    let first = state_with_stack_trigger(&Some(s1()), 2);
    let second = state_with_stack_trigger(&Some(s2()), 2);
    assert!(
        serde_json::to_string(&first).unwrap().contains("SNTA"),
        "the stack trigger really carries the printing"
    );
    assert!(
        none == first && first == second,
        "GameState ignores a printing"
    );
    assert!(
        first != state_with_stack_trigger(&Some(s1()), 3),
        "GameState still sees a rules difference at the same position"
    );

    // Loop detection compares objects field by field; the live object's printing is not
    // among them.
    let (base, id) = base_state();
    let mut printed = base.clone();
    let object = printed.objects.get_mut(&id).unwrap();
    object.printing = carried(&Some(s1()));
    object.base_printing = carried(&Some(s1()));
    let mut reprinted = base.clone();
    let object = reprinted.objects.get_mut(&id).unwrap();
    object.printing = carried(&Some(s2()));
    object.base_printing = carried(&Some(s2()));
    assert!(serde_json::to_string(&printed).unwrap().contains("SNTA"));
    assert!(loop_states_equal_modulo_resources(&base, &printed));
    assert!(loop_states_equal_modulo_resources(&printed, &reprinted));
    let mut tapped = printed.clone();
    tapped.objects.get_mut(&id).unwrap().tapped = true;
    assert!(
        !loop_states_equal_modulo_resources(&printed, &tapped),
        "the comparator does read objects"
    );
}

#[test]
fn carrier_debug_text_is_printing_independent() {
    fn texts<T: Debug>(make: impl Fn(&Option<CardPrinting>) -> T) -> [(String, String); 3] {
        variants().map(|printing| {
            let value = make(&printing);
            (format!("{value:?}"), format!("{value:#?}"))
        })
    }

    // The snapshot omits its printing from the text unconditionally, so every Debug-keyed
    // reading of a value embedding one is what it was before the field existed.
    let omitting: [(&str, [(String, String); 3]); 3] = [
        ("LKISnapshot", texts(lki_with)),
        (
            "TriggerSourceContext",
            texts(|printing| {
                let (state, id) = base_state();
                context_with(&state, id, printing, 2)
            }),
        ),
        ("GameEvent::ZoneChanged", texts(zone_changed_with)),
    ];
    for (label, [none, first, second]) in omitting {
        assert_eq!(none, first, "{label}: none == S1");
        assert_eq!(first, second, "{label}: S1 == S2");
        for text in [&first.0, &first.1] {
            assert!(!text.contains("printing"), "{label}: no printing in {text}");
            assert!(!text.contains("SNTA"), "{label}: no sentinel in {text}");
            assert!(
                text.contains("token_image_ref"),
                "{label}: text is the real one"
            );
        }
    }
    // Reach: the value really held a printing while its text did not show it.
    assert_eq!(lki_with(&Some(s1())).printing.get(), &Some(s1()));

    // The other carriers print the wrapper's constant text.
    let constant: [(&str, [(String, String); 3]); 5] = [
        ("GameObject", texts(object_with)),
        ("EmblemSource", texts(emblem_with)),
        ("LibrarySearchCardView", texts(search_view_with)),
        ("StackEntryDisplay", texts(stack_row_with)),
        ("DebugLibraryCardView", texts(debug_row_with)),
    ];
    for (label, [none, first, second]) in constant {
        assert_eq!(none, first, "{label}: none == S1");
        assert_eq!(first, second, "{label}: S1 == S2");
        for text in [&first.0, &first.1] {
            assert!(
                text.contains("printing: DisplayOnly(..)"),
                "{label}: constant wrapper text in {text}"
            );
            assert!(!text.contains("SNTA"), "{label}: no sentinel in {text}");
        }
    }
}

/// The wire contract of one carrier: no key when empty, the bare `CardPrinting` object when
/// present, the same decode back, `null` and absent both empty, and the rules projection
/// omits the field.
fn assert_wire_contract<T: Serialize + DeserializeOwned>(
    label: &str,
    make: impl Fn(&Option<CardPrinting>) -> T,
    rename: impl Fn(&mut T),
    printings_of: impl Fn(&T) -> Vec<Option<CardPrinting>>,
    keys: &[&str],
) {
    let empty = make(&None);
    let empty_text = serde_json::to_string(&empty).unwrap();
    for key in keys {
        assert!(
            !empty_text.contains(&format!("\"{key}\"")),
            "{label}: an empty {key} is absent from the wire"
        );
    }

    let present = make(&Some(s1()));
    let present_value = serde_json::to_value(&present).unwrap();
    let present_text = serde_json::to_string(&present).unwrap();
    for key in keys {
        assert_eq!(
            present_value[*key],
            json!({"set_code": "SNTA", "collector_number": "1"}),
            "{label}: a present {key} is the bare printing object"
        );
    }
    assert!(
        !present_text.contains("$phase_engine"),
        "{label}: marker leaked"
    );

    let decoded: T = serde_json::from_value(present_value.clone()).unwrap();
    assert_eq!(
        printings_of(&decoded),
        vec![Some(s1()); keys.len()],
        "{label}: decode keeps the printing"
    );
    let from_empty: T = serde_json::from_str(&empty_text).unwrap();
    assert_eq!(printings_of(&from_empty), vec![None; keys.len()]);
    let mut with_null: Value = serde_json::from_str(&empty_text).unwrap();
    for key in keys {
        with_null[*key] = Value::Null;
    }
    let from_null: T = serde_json::from_value(with_null).unwrap();
    assert_eq!(printings_of(&from_null), vec![None; keys.len()]);

    // The projection the rules and AI may read leaves no trace of the printing.
    let projected = rules_projection(&present).unwrap();
    assert_eq!(projected, rules_projection(&empty).unwrap(), "{label}");
    assert!(!projected.to_string().contains("SNTA"), "{label}");
    let mut renamed = make(&Some(s1()));
    rename(&mut renamed);
    assert_ne!(
        rules_projection(&renamed).unwrap(),
        projected,
        "{label}: the projection still sees a rules difference"
    );
}

#[test]
fn carrier_wire_shape_is_absent_when_empty_and_transparent_when_present() {
    assert_wire_contract(
        "GameObject",
        object_with,
        |object| object.name = "Elf".to_string(),
        |object| {
            vec![
                object.printing.get().clone(),
                object.base_printing.get().clone(),
            ]
        },
        &["printing", "base_printing"],
    );
    assert_wire_contract(
        "LKISnapshot",
        lki_with,
        |lki| lki.name = "Elf".to_string(),
        |lki| vec![lki.printing.get().clone()],
        &["printing"],
    );
    assert_wire_contract(
        "EmblemSource",
        emblem_with,
        |emblem| emblem.name = "Other Walker".to_string(),
        |emblem| vec![emblem.printing.get().clone()],
        &["printing"],
    );
    assert_wire_contract(
        "LibrarySearchCardView",
        search_view_with,
        |view| view.current_face.name = "Elf".to_string(),
        |view| vec![view.printing.get().clone()],
        &["printing"],
    );
    assert_wire_contract(
        "StackEntryDisplay",
        stack_row_with,
        |row| row.source_name = "Elf".to_string(),
        |row| vec![row.printing.get().clone()],
        &["printing"],
    );
    assert_wire_contract(
        "DebugLibraryCardView",
        debug_row_with,
        |row| row.name = "Other Card".to_string(),
        |row| vec![row.printing.get().clone()],
        &["printing"],
    );
}

#[test]
fn persisted_state_keeps_every_printing() {
    let (mut state, id) = base_state();
    let object = state.objects.get_mut(&id).unwrap();
    object.printing = carried(&Some(s1()));
    object.base_printing = carried(&Some(s1()));
    let plain = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Plain Bear".to_string(),
        Zone::Battlefield,
    );

    let mut lki = object_in(&state, id).snapshot_public_characteristics();
    lki.printing = carried(&Some(s2()));
    state.lki_cache.insert(id, lki);

    let emblem = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Emblem".to_string(),
        Zone::Command,
    );
    state.objects.get_mut(&emblem).unwrap().emblem_source = Some(emblem_with(&Some(s1())));

    let context = context_with(&state, id, &Some(s2()), 2);
    let mut ability = ResolvedAbility::new(Effect::NoOp, vec![], id, PlayerId(0));
    ability.trigger_source = Some(context);
    state.stack.push_back(StackEntry {
        id: ObjectId(600),
        source_id: id,
        controller: PlayerId(0),
        kind: StackEntryKind::ActivatedAbility {
            source_id: id,
            ability: Box::new(ability),
        },
    });

    let wire = serde_json::to_value(&state).unwrap();
    let text = wire.to_string();
    assert!(
        text.contains("SNTA") && text.contains("SNTB"),
        "the wire carries both"
    );

    let restored = serde_json::from_value::<PersistedGameState>(wire)
        .expect("the persisted wire decodes")
        .into_game_state()
        .expect("the restore contract holds for the fixture");

    // Read through `get`, never `==`: the wrapper is always equal, so `==` would pass vacuously.
    let object = object_in(&restored, id);
    assert_eq!(object.printing.get(), &Some(s1()));
    assert_eq!(object.base_printing.get(), &Some(s1()));
    assert_eq!(
        restored.lki_cache.get(&id).unwrap().printing.get(),
        &Some(s2())
    );
    let emblem_source = object_in(&restored, emblem).emblem_source.as_ref().unwrap();
    assert_eq!(emblem_source.printing.get(), &Some(s1()));
    let StackEntryKind::ActivatedAbility { ability, .. } = &restored.stack[0].kind else {
        panic!("the stack entry is the fixture ability");
    };
    let restored_context = ability.trigger_source.as_ref().unwrap();
    assert_eq!(restored_context.lki.printing.get(), &Some(s2()));

    // Sibling: an object that never had a printing restores without one.
    let plain = object_in(&restored, plain);
    assert!(plain.printing.get().is_none() && plain.base_printing.get().is_none());
}
