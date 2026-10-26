//! Exposure parity of the owner's printing: a viewer receives a card's printing only in a
//! carrier that also discloses that card's name to the viewer.
//!
//! CR 203.1 + CR 206.1: a printing is display data with no rules meaning. CR 400.2, CR 401.2,
//! CR 402.3, CR 406.3, CR 708.2, CR 708.5 and CR 708.9 decide which viewer may identify which
//! card, so the printing is redacted wherever the name is: every projection leaf, side table
//! and the raw event channel, and the two derived rows (stack, debug library) derive it from
//! the authority their displayed name comes from.
//!
//! Concealment: a card that can hide its name (turned face down before it enters, CR 708.3 and
//! CR 708.4, or merged into a mutate pile, CR 702.140c, CR 730.2a, CR 730.2e, CR 730.2f)
//! forgets its printing on the authoritative object, and no later face write puts one back, so a
//! concealed printing is gone for the rest of the game. The one exception is a reversal: the
//! cast reversal (CR 733.1) and the action-rejection snapshot restore replay the exact recorded
//! values, which returns the pre-cast printing to a card that is back in hand face up.
//!
//! Sentinels: these historical privacy fixtures stamp each card by hand with its
//! own sentinel (set code `S<TAG>`), and a census walks authoritative state,
//! events, filtered state, client wire and filtered events. The production
//! resolver/loader seam is exercised separately in owner_printing_stamping.
//!
//! Recorded observations that these rows pin as parity, not as fixes: a face-down-redacted id's
//! cached earlier-incarnation last-known information keeps its name and printing together; the
//! `back_face` of a face-up survivor over a buried face-down component and the turn-scoped
//! journal record of a card played face down still carry a name the opponent should not see,
//! but no printing joins either.

use std::collections::HashMap;

use engine::game::combat::AttackTarget;
use engine::game::derived_views::ClientGameStateRef;
use engine::game::effects::create_emblem;
use engine::game::game_object::BackFaceData;
use engine::game::merge::MergeSide;
use engine::game::printed_cards::{
    rehydrate_game_from_card_db_with_finalization, CardDbRehydrationFinalization,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::specialize::SpecializeFaceMap;
use engine::game::visibility::{
    filter_events_for_viewer, filter_state_for_unseated_viewer, filter_state_for_viewer,
};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CopyRecipient, CopyRetargetPermission, Duration,
    Effect, ExileConcealment, FaceDownProfile, FilterProp, QuantityExpr, ResolvedAbility,
    SearchSelectionConstraint, StaticDefinition, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card::LayoutKind;
use engine::types::card_printing::CardPrinting;
use engine::types::card_type::{CardType, CoreType};
use engine::types::display_only::DisplayOnly;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, ManaSpentSourceSnapshot, PersistedGameState, StackEntry,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{AdditionalCostTaxAction, StaticMode};
use engine::types::zones::Zone;
use serde_json::Value;

use crate::support::shared_card_db as load_db;

const HIDDEN: &str = "Hidden Card";
const SPECTATOR: PlayerId = PlayerId(u8::MAX);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Who is looking: a seat, a spectator (a seat id no player holds), or the unseated audience.
#[derive(Clone, Copy, Debug)]
enum Viewer {
    Seat(PlayerId),
    Spectator,
    Unseated,
}

const VIEWERS: [Viewer; 4] = [
    Viewer::Seat(P0),
    Viewer::Seat(P1),
    Viewer::Spectator,
    Viewer::Unseated,
];

fn sentinel(tag: &str) -> CardPrinting {
    CardPrinting {
        set_code: format!("S{tag}"),
        collector_number: "1".to_string(),
    }
}

fn carried(tag: &str) -> DisplayOnly<Option<CardPrinting>> {
    DisplayOnly::new(Some(sentinel(tag)))
}

/// A running game plus every event its actions and primitives produced, and which card name
/// each sentinel was stamped beside.
struct Board {
    runner: GameRunner,
    log: Vec<GameEvent>,
    names: HashMap<String, String>,
}

impl Board {
    fn new(scenario: GameScenario) -> Self {
        Self {
            runner: scenario.build(),
            log: Vec::new(),
            names: HashMap::new(),
        }
    }

    fn state(&self) -> &GameState {
        self.runner.state()
    }

    fn state_mut(&mut self) -> &mut GameState {
        self.runner.state_mut()
    }

    /// Stamp both fields beside the current name for this focused privacy fixture.
    /// Production creation stamping is covered by owner_printing_stamping.
    fn stamp(&mut self, id: ObjectId, tag: &str) {
        let object = self
            .runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("the stamped object exists");
        object.printing = carried(tag);
        object.base_printing = carried(tag);
        let name = object.name.clone();
        self.names.insert(sentinel(tag).set_code, name);
    }

    /// Submit an engine action and keep its events.
    fn act(&mut self, action: GameAction) {
        let result = self
            .runner
            .act(action)
            .expect("the production action is accepted");
        self.log.extend(result.events);
    }

    /// Run a production primitive that writes events into the log.
    fn primitive<T>(&mut self, run: impl FnOnce(&mut GameState, &mut Vec<GameEvent>) -> T) -> T {
        let mut events = Vec::new();
        let out = run(self.runner.state_mut(), &mut events);
        self.log.extend(events);
        out
    }

    /// Move an object through the production zone pipeline.
    fn move_to(&mut self, id: ObjectId, to: Zone) {
        self.primitive(|state, events| {
            move_object_for_test(state, ZoneMoveRequest::effect(id, to, id), events)
        });
    }

    fn play_face_down(&mut self, player: PlayerId, id: ObjectId) {
        self.primitive(|state, events| {
            engine::game::morph::play_face_down(state, player, id, events)
                .expect("the card is played face down");
        });
    }

    /// Every serialized carrier a viewer receives, labelled: the filtered state, the client
    /// wire (filtered state plus derived rows) and the filtered raw events. The unseated
    /// audience has no event filter.
    fn forms(&self, viewer: Viewer) -> Vec<(String, Value)> {
        let raw = self.state();
        let (filtered, wire, events) = match viewer {
            Viewer::Seat(seat) => {
                let filtered = filter_state_for_viewer(raw, seat);
                let wire = ClientGameStateRef::wrap_filtered(raw, &filtered, Some(seat));
                let wire = json(&wire);
                let events = filter_events_for_viewer(&self.log, raw, seat);
                (json(&filtered), wire, Some(json(&events)))
            }
            Viewer::Spectator => {
                let filtered = filter_state_for_viewer(raw, SPECTATOR);
                let wire = ClientGameStateRef::wrap_filtered(raw, &filtered, None);
                let wire = json(&wire);
                let events = filter_events_for_viewer(&self.log, raw, SPECTATOR);
                (json(&filtered), wire, Some(json(&events)))
            }
            Viewer::Unseated => {
                let filtered = filter_state_for_unseated_viewer(raw);
                let wire = json(&ClientGameStateRef::wrap(raw, None));
                (json(&filtered), wire, None)
            }
        };
        let mut forms = vec![
            (format!("{viewer:?} state"), filtered),
            (format!("{viewer:?} wire"), wire),
        ];
        if let Some(events) = events {
            forms.push((format!("{viewer:?} events"), events));
        }
        forms
    }

    /// The authoritative state and events, serialized.
    fn authoritative(&self) -> Vec<(String, Value)> {
        vec![
            (
                "authoritative state".to_string(),
                serde_json::to_value(self.state()).expect("state serializes"),
            ),
            (
                "authoritative events".to_string(),
                serde_json::to_value(&self.log).expect("events serialize"),
            ),
        ]
    }

    /// Every form of every audience plus the authoritative ones.
    fn all_forms(&self) -> Vec<(String, Value)> {
        let mut forms = self.authoritative();
        for viewer in VIEWERS {
            forms.extend(self.forms(viewer));
        }
        forms
    }

    /// The exposure rule on every form (authoritative ones included): a printing node exposes
    /// its card's name. Also asserts the rule was exercised: every state and wire form held at
    /// least one stamped printing, so a vacuous pass cannot hide a broken harness.
    fn assert_exposure_rule(&self, step: &str) {
        for (label, seen) in self.assert_follows_name(step) {
            if label.ends_with("state") || label.ends_with("wire") {
                assert!(seen > 0, "{step}: reach: {label} held no stamped printing");
            }
        }
    }

    /// The exposure rule alone, returning the number of stamped printings each form held.
    fn assert_follows_name(&self, step: &str) -> Vec<(String, usize)> {
        self.all_forms()
            .into_iter()
            .map(|(label, form)| {
                let seen =
                    assert_printing_follows_name(&form, &self.names, &format!("{step}: {label}"));
                (label, seen)
            })
            .collect()
    }

    /// Nodes holding `tag` in each form, labelled.
    fn census(&self, tag: &str) -> HashMap<String, usize> {
        let set_code = sentinel(tag).set_code;
        self.all_forms()
            .into_iter()
            .map(|(label, form)| (label, count_set_code(&form, &set_code)))
            .collect()
    }

    /// `tag` occurs in no node of any form (authoritative included).
    fn assert_nowhere(&self, tag: &str, step: &str) {
        let census = self.census(tag);
        let leaking: Vec<_> = census.iter().filter(|(_, count)| **count > 0).collect();
        assert!(
            leaking.is_empty(),
            "{step}: sentinel {tag} reached {leaking:?}"
        );
    }
}

fn json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("carrier serializes")
}

/// Number of nodes whose `set_code` is `set_code`.
fn count_set_code(value: &Value, set_code: &str) -> usize {
    match value {
        Value::Object(map) => {
            usize::from(map.get("set_code").and_then(Value::as_str) == Some(set_code))
                + map
                    .values()
                    .map(|child| count_set_code(child, set_code))
                    .sum::<usize>()
        }
        Value::Array(items) => items
            .iter()
            .map(|child| count_set_code(child, set_code))
            .sum(),
        _ => 0,
    }
}

/// The exposure rule itself: at every object node holding a `printing` or `base_printing`
/// whose set code is a known sentinel, the same node exposes that sentinel's card name in
/// `name`, `source_name`, `current_face.name`, `front_face.name` or `back_face.name`. Returns
/// the number of sentinel nodes seen.
fn assert_printing_follows_name(
    value: &Value,
    names: &HashMap<String, String>,
    context: &str,
) -> usize {
    let mut seen = 0;
    match value {
        Value::Object(map) => {
            for key in ["printing", "base_printing"] {
                let Some(set_code) = map
                    .get(key)
                    .and_then(|printing| printing.get("set_code"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let Some(expected) = names.get(set_code) else {
                    continue;
                };
                seen += 1;
                let exposed = ["name", "source_name"]
                    .iter()
                    .filter_map(|field| map.get(*field).and_then(Value::as_str))
                    .chain(
                        ["current_face", "front_face", "back_face"]
                            .iter()
                            .filter_map(|face| map.get(*face))
                            .filter_map(|face| face.get("name").and_then(Value::as_str)),
                    )
                    .any(|name| name == expected);
                assert!(
                    exposed,
                    "{context}: {key} {set_code} of {expected:?} sits in a node that does not \
                     expose that name: {}",
                    node_summary(map)
                );
            }
            for child in map.values() {
                seen += assert_printing_follows_name(child, names, context);
            }
        }
        Value::Array(items) => {
            for child in items {
                seen += assert_printing_follows_name(child, names, context);
            }
        }
        _ => {}
    }
    seen
}

fn node_summary(map: &serde_json::Map<String, Value>) -> String {
    ["name", "source_name", "printing", "base_printing"]
        .iter()
        .filter_map(|key| map.get(*key).map(|value| format!("{key}={value}")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// The printing a live object currently holds, as a set code.
fn live_set_code(state: &GameState, id: ObjectId) -> Option<String> {
    state.objects[&id]
        .printing
        .get()
        .as_ref()
        .map(|printing| printing.set_code.clone())
}

fn base_set_code(state: &GameState, id: ObjectId) -> Option<String> {
    state.objects[&id]
        .base_printing
        .get()
        .as_ref()
        .map(|printing| printing.set_code.clone())
}

/// The projected object `id` inside a serialized filtered state.
fn projected_object(state_form: &Value, id: ObjectId) -> &Value {
    &state_form["objects"][id.0.to_string()]
}

// ---------------------------------------------------------------------------
// captures read the live printing
// ---------------------------------------------------------------------------

/// Every capture route copies the live object's printing: the public-characteristics
/// snapshot family, the zone-exit last-known information, the emblem source and the library
/// search view. The same routes give nothing for the card once it is face down.
#[test]
fn captures_take_the_live_printing() {
    let mut s = scenario();
    let bear = s.add_creature(P0, "Public Bear", 2, 2).id();
    let walker = s
        .add_creature(P0, "Source Walker", 0, 0)
        .as_planeswalker_with_loyalty("Walker", 3)
        .id();
    let searched = s.add_card_to_library_top(P0, "Searched Card");
    let morph = s.add_card_to_hand(P0, "Hand Secret");
    let mut board = Board::new(s);
    board.stamp(bear, "BEAR");
    board.stamp(walker, "WALK");
    board.stamp(searched, "SRCH");
    board.stamp(morph, "MRPH");

    // The snapshot family reads the live field.
    let object = &board.state().objects[&bear];
    let expected = Some(sentinel("BEAR"));
    assert_eq!(
        object.snapshot_public_characteristics().printing.get(),
        &expected
    );
    assert_eq!(object.snapshot_for_mana_spent().printing.get(), &expected);
    assert_eq!(
        object
            .snapshot_for_attack_declaration(bear)
            .lki
            .printing
            .get(),
        &expected
    );
    let record = object.snapshot_for_zone_change(bear, Some(Zone::Battlefield), Zone::Graveyard);
    assert_eq!(
        record
            .trigger_source_context
            .as_ref()
            .expect("a real record carries its source context")
            .lki
            .printing
            .get(),
        &expected
    );

    // The zone-exit last-known information of a real departure.
    board.move_to(bear, Zone::Graveyard);
    assert_eq!(
        board.state().lki_cache[&bear].printing.get(),
        &expected,
        "the zone-exit snapshot takes the live printing"
    );

    // The emblem source of a face-up planeswalker.
    let ability = ResolvedAbility::new(
        Effect::CreateEmblem {
            statics: vec![],
            triggers: vec![],
        },
        vec![],
        walker,
        P0,
    );
    board.primitive(|state, events| {
        create_emblem::resolve(state, &ability, events).expect("the emblem is created");
    });
    let emblem_printings: Vec<_> = board
        .state()
        .objects
        .values()
        .filter_map(|object| object.emblem_source.as_ref())
        .map(|source| source.printing.get().clone())
        .collect();
    assert_eq!(emblem_printings, vec![Some(sentinel("WALK"))]);

    // The library search view of a searched card.
    let search = ResolvedAbility::new(
        Effect::SearchLibrary {
            filter: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
            source_zones: vec![Zone::Library],
        },
        vec![],
        walker,
        P0,
    );
    board.primitive(|state, events| {
        engine::game::effects::resolve_ability_chain(state, &search, events, 0)
            .expect("the search resolves");
    });
    assert!(matches!(
        board.state().waiting_for,
        WaitingFor::SearchChoice { .. }
    ));
    let viewed: Vec<_> = board
        .log
        .iter()
        .filter_map(|event| match event {
            GameEvent::HiddenSearchViewed { cards, .. } => Some(cards),
            _ => None,
        })
        .flatten()
        .map(|view| view.printing.get().clone())
        .collect();
    assert_eq!(viewed, vec![Some(sentinel("SRCH"))]);

    // The same card played face down: the object forgot its printing at the entry, so every
    // capture of it carries none, beside the empty name.
    assert_eq!(
        live_set_code(board.state(), morph).as_deref(),
        Some("SMRPH"),
        "reach: the hand card holds its sentinel before it is played face down"
    );
    board.play_face_down(P0, morph);
    let object = &board.state().objects[&morph];
    assert!(object.face_down);
    assert_eq!(
        object.back_face.as_ref().map(|face| face.name.as_str()),
        Some("Hand Secret"),
        "reach: the real face is stashed"
    );
    assert_eq!(object.name, "");
    assert!(object
        .snapshot_public_characteristics()
        .printing
        .get()
        .is_none());
    assert!(object.snapshot_for_mana_spent().printing.get().is_none());
    let departed_from = board.state().objects[&morph].zone;
    assert_eq!(departed_from, Zone::Battlefield);
    board.move_to(morph, Zone::Graveyard);
    let lki = &board.state().lki_cache[&morph];
    assert_eq!(lki.name, "", "the face-down permanent departs nameless");
    assert!(lki.printing.get().is_none());
    // CR 708.9: the graveyard object is face up with its real name and no printing.
    assert_eq!(board.state().objects[&morph].name, "Hand Secret");
    assert!(live_set_code(board.state(), morph).is_none());
    board.assert_nowhere("MRPH", "face-down departure");
}

// ---------------------------------------------------------------------------
// projection clears the printing with the name
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sees {
    Everyone,
    OnlyP0,
    OnlyP1,
    Nobody,
}

impl Sees {
    fn includes(self, viewer: Viewer) -> bool {
        match (self, viewer) {
            (Sees::Everyone, _) => true,
            (Sees::OnlyP0, Viewer::Seat(seat)) => seat == P0,
            (Sees::OnlyP1, Viewer::Seat(seat)) => seat == P1,
            (Sees::OnlyP0 | Sees::OnlyP1 | Sees::Nobody, _) => false,
        }
    }
}

#[test]
fn projection_clears_printing_with_the_name() {
    let mut s = scenario();
    let hand0 = s.add_card_to_hand(P0, "Hand Zero");
    let hand_revealed = s.add_card_to_hand(P0, "Hand Revealed");
    let hand1 = s.add_card_to_hand(P1, "Hand One");
    let lib0 = s.add_card_to_library_top(P0, "Library Zero");
    let lib1 = s.add_card_to_library_top(P1, "Library One");
    let public = s.add_creature(P0, "Public Bear", 2, 2).id();
    let stolen = s
        .add_creature(P0, "Stolen Bear", 2, 2)
        .controlled_by(P1)
        .id();
    let twin_a = s.add_creature(P0, "Twin", 1, 1).id();
    let twin_b = s.add_creature(P1, "Twin", 1, 1).id();
    let mut board = Board::new(s);
    let (exile_down, foretold, planar) = {
        let state = board.state_mut();
        let exile_down = create_object(
            state,
            CardId(9001),
            P0,
            "Exiled Face Down".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exile_down).unwrap().face_down = true;
        let foretold = create_object(
            state,
            CardId(9002),
            P0,
            "Foretold Card".to_string(),
            Zone::Exile,
        );
        {
            let object = state.objects.get_mut(&foretold).unwrap();
            object.face_down = true;
            object.foretold = true;
        }
        let planar = create_object(
            state,
            CardId(9003),
            P0,
            "Planar Card".to_string(),
            Zone::Command,
        );
        // The planar deck is a face-down pile, as the deck loader builds it.
        state.objects.get_mut(&planar).unwrap().face_down = true;
        state.planar_deck.push_back(planar);
        state.revealed_cards.insert(hand_revealed);
        (exile_down, foretold, planar)
    };

    let table = [
        (hand0, "HAND0", Sees::OnlyP0),
        (hand_revealed, "HREV", Sees::Everyone),
        (hand1, "HAND1", Sees::OnlyP1),
        (lib0, "LIB0", Sees::Nobody),
        (lib1, "LIB1", Sees::Nobody),
        (public, "PUB", Sees::Everyone),
        (stolen, "STOLEN", Sees::Everyone),
        (twin_a, "TWINA", Sees::Everyone),
        (twin_b, "TWINB", Sees::Everyone),
        (exile_down, "EXDN", Sees::Nobody),
        (foretold, "FORE", Sees::OnlyP0),
        (planar, "PLAN", Sees::Nobody),
    ];
    for (id, tag, _) in table {
        board.stamp(id, tag);
    }

    for (id, tag, sees) in table {
        let name = board.names[&sentinel(tag).set_code].clone();
        // Reach: the authoritative object keeps its sentinel, live and base.
        assert_eq!(live_set_code(board.state(), id), Some(format!("S{tag}")));
        assert_eq!(base_set_code(board.state(), id), Some(format!("S{tag}")));
        for viewer in VIEWERS {
            let forms = board.forms(viewer);
            let object = projected_object(&forms[0].1, id);
            let set_code = format!("S{tag}");
            let count = |form: &Value| count_set_code(form, &set_code);
            if sees.includes(viewer) {
                assert_eq!(object["name"], name, "{tag} {viewer:?}: the name is shown");
                assert_eq!(
                    object["printing"]["set_code"], set_code,
                    "{tag} {viewer:?}: the printing is shown beside the name"
                );
                assert_eq!(object["base_printing"]["set_code"], set_code);
            } else {
                assert_eq!(
                    object["name"], HIDDEN,
                    "{tag} {viewer:?}: the name is hidden"
                );
                assert!(
                    object.get("printing").is_none() && object.get("base_printing").is_none(),
                    "{tag} {viewer:?}: no printing beside the hidden name: {object}"
                );
                for (label, form) in &forms {
                    assert_eq!(
                        count(form),
                        0,
                        "{tag} {viewer:?}: {label} carries the sentinel"
                    );
                }
            }
        }
    }
    // The twin cards keep their own printings (a name is not an identity).
    let forms = board.forms(Viewer::Seat(P0));
    assert_eq!(
        projected_object(&forms[0].1, twin_a)["printing"]["set_code"],
        "STWINA"
    );
    assert_eq!(
        projected_object(&forms[0].1, twin_b)["printing"]["set_code"],
        "STWINB"
    );
    board.assert_exposure_rule("projection");
}

/// CR 723.4: a player who controls another player's turn sees what that player sees, so
/// the controlling player receives the controlled player's hand printings; the opponent
/// still does not.
#[test]
fn a_turn_controller_sees_the_controlled_hand_printing() {
    let mut s = scenario();
    let hand0 = s.add_card_to_hand(P0, "Controlled Hand");
    let mut board = Board::new(s);
    board.stamp(hand0, "CTRL");
    board.state_mut().turn_decision_controller = Some(P1);
    let forms = board.forms(Viewer::Seat(P1));
    let object = projected_object(&forms[0].1, hand0);
    assert_eq!(object["name"], "Controlled Hand");
    assert_eq!(object["printing"]["set_code"], "SCTRL");
    // Positive reach: without the control effect the same viewer is shut out.
    board.state_mut().turn_decision_controller = None;
    let forms = board.forms(Viewer::Seat(P1));
    assert_eq!(projected_object(&forms[0].1, hand0)["name"], HIDDEN);
    assert_eq!(count_set_code(&forms[0].1, "SCTRL"), 0);
}

// ---------------------------------------------------------------------------
// last-known information and side tables
// ---------------------------------------------------------------------------

/// The journal entries of `key` that belong to object `id`.
fn records_of<'a>(state_form: &'a Value, key: &str, id: ObjectId) -> Vec<&'a Value> {
    state_form[key]
        .as_array()
        .map(|records| {
            records
                .iter()
                .filter(|record| record["object_id"] == serde_json::json!(id.0))
                .collect()
        })
        .unwrap_or_default()
}

/// A payment snapshot, an attack declaration and a zone-change record of a card that is later
/// hidden lose the printing with the name for the viewer who cannot see the card, and keep
/// both for the viewer who can; a still-public source keeps both for everyone.
#[test]
fn lki_and_side_tables_follow_the_name() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::BeginCombat);
    let rock = s.add_creature(P0, "Mana Rock", 1, 1).id();
    let forest = s.add_creature(P0, "Public Forest", 1, 1).id();
    let paid = s.add_creature(P0, "Paid Permanent", 2, 2).id();
    let attacker = s.add_creature(P0, "Hidden Attacker", 3, 3).id();
    let leaver = s.add_creature(P0, "Leaver", 2, 2).id();
    let mut board = Board::new(s);
    for (id, tag) in [
        (rock, "ROCK"),
        (forest, "FRST"),
        (paid, "PAID"),
        (attacker, "ATKR"),
        (leaver, "LEAV"),
    ] {
        board.stamp(id, tag);
    }

    // Payment snapshots of two sources, taken while both were public.
    {
        let state = board.state_mut();
        let snapshots = [rock, forest]
            .map(|source| ManaSpentSourceSnapshot {
                source_id: source,
                lki: state.objects[&source].snapshot_for_mana_spent(),
            })
            .to_vec();
        state
            .objects
            .get_mut(&paid)
            .unwrap()
            .mana_spent_source_snapshots = snapshots;
    }
    // A real attack declaration, then the attacker and the rock return to P0's hand, and a
    // creature leaves to the hand outside combat.
    let result = board
        .runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P1))])
        .expect("the attack is declared");
    board.log.extend(result.events);
    board.move_to(attacker, Zone::Hand);
    board.move_to(rock, Zone::Hand);
    board.move_to(leaver, Zone::Hand);

    // Reach: the records exist with their printings on the authoritative state.
    let authoritative = json(board.state());
    let declared = records_of(&authoritative, "attacker_declarations_this_turn", attacker);
    assert_eq!(declared.len(), 1, "reach: the attack was recorded");
    assert_eq!(declared[0]["lki"]["printing"]["set_code"], "SATKR");
    assert_eq!(
        authoritative["objects"][paid.0.to_string()]["mana_spent_source_snapshots"][0]["lki"]
            ["printing"]["set_code"],
        "SROCK"
    );

    // P1 cannot see the cards now in P0's hand.
    let p1 = board.forms(Viewer::Seat(P1));
    let state_form = &p1[0].1;
    let paid_snapshots = &state_form["objects"][paid.0.to_string()]["mana_spent_source_snapshots"];
    assert_eq!(paid_snapshots[0]["lki"]["name"], HIDDEN);
    assert!(paid_snapshots[0]["lki"].get("printing").is_none());
    assert_eq!(paid_snapshots[1]["lki"]["name"], "Public Forest");
    assert_eq!(paid_snapshots[1]["lki"]["printing"]["set_code"], "SFRST");
    let declared = records_of(state_form, "attacker_declarations_this_turn", attacker);
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0]["lki"]["name"], HIDDEN);
    assert!(declared[0]["lki"].get("printing").is_none());
    for hidden in [attacker, rock, leaver] {
        assert!(
            state_form["lki_cache"][hidden.0.to_string()].is_null(),
            "no last-known information of a hidden id"
        );
    }
    let changes = records_of(state_form, "zone_changes_this_turn", leaver);
    assert!(!changes.is_empty(), "reach: the departure was journaled");
    assert!(changes.iter().all(|record| record
        .get("trigger_source_context")
        .is_none_or(Value::is_null)));
    // The raw events of the departures are not asserted absent: a card leaving the
    // battlefield is a public move that names it, and the printing travels beside that name
    // (checked by the exposure rule below).
    for tag in ["ROCK", "ATKR", "LEAV"] {
        for (label, form) in p1
            .iter()
            .filter(|(label, _)| label.ends_with("state") || label.ends_with("wire"))
        {
            assert_eq!(
                count_set_code(form, &format!("S{tag}")),
                0,
                "{tag} in {label}"
            );
        }
    }

    // P0 can see its own hand: every record keeps name and printing together.
    let p0 = board.forms(Viewer::Seat(P0));
    let state_form = &p0[0].1;
    let paid_snapshots = &state_form["objects"][paid.0.to_string()]["mana_spent_source_snapshots"];
    assert_eq!(paid_snapshots[0]["lki"]["name"], "Mana Rock");
    assert_eq!(paid_snapshots[0]["lki"]["printing"]["set_code"], "SROCK");
    let declared = records_of(state_form, "attacker_declarations_this_turn", attacker);
    assert_eq!(declared[0]["lki"]["name"], "Hidden Attacker");
    assert_eq!(declared[0]["lki"]["printing"]["set_code"], "SATKR");

    board.assert_exposure_rule("side tables");
}

// ---------------------------------------------------------------------------
// the stack row derives its printing from the filtered state
// ---------------------------------------------------------------------------

/// A creature card with morph {4} and mana value 5 in `player`'s hand (the face-down cast
/// costs {3}).
fn add_morph(s: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    s.add_creature_to_hand(player, name, 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id()
}

fn fund(board: &mut Board, player: PlayerId, amount: usize) {
    for _ in 0..amount {
        let unit = ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]);
        board
            .state_mut()
            .add_mana_to_pool(player, unit)
            .expect("the player has a mana pool");
    }
}

fn cast(board: &mut Board, id: ObjectId, payment_mode: CastPaymentMode) {
    let card_id = board.state().objects[&id].card_id;
    board.act(GameAction::CastSpell {
        object_id: id,
        card_id,
        targets: vec![],
        payment_mode,
    });
}

/// Cast `id` face down through the production alternative-cost choice.
fn cast_face_down(board: &mut Board, id: ObjectId) {
    cast(board, id, CastPaymentMode::Auto);
    board.act(GameAction::ChooseAlternativeCast {
        choice: AlternativeCastDecision::Alternative,
    });
}

/// The typed state `viewer` receives.
fn filtered_state(board: &Board, viewer: Viewer) -> GameState {
    match viewer {
        Viewer::Seat(seat) => filter_state_for_viewer(board.state(), seat),
        Viewer::Spectator => filter_state_for_viewer(board.state(), SPECTATOR),
        Viewer::Unseated => filter_state_for_unseated_viewer(board.state()),
    }
}

/// The derived stack row for stack entry `entry` on `viewer`'s client wire.
fn stack_row(board: &Board, viewer: Viewer, entry: ObjectId) -> Value {
    let wire = &board.forms(viewer)[1].1;
    let row = &wire["derived"]["stack_entry_details"][entry.0.to_string()];
    assert!(row.is_object(), "{viewer:?}: no stack row for {entry:?}");
    row.clone()
}

fn row_set_code(row: &Value) -> Option<String> {
    row.get("printing")
        .and_then(|printing| printing["set_code"].as_str())
        .map(str::to_owned)
}

fn draw_effect() -> Effect {
    Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    }
}

/// A public spell cast face up carries its printing on every viewer's row.
#[test]
fn stack_row_of_a_public_spell_carries_its_printing() {
    let mut s = scenario();
    let spell = s
        .add_creature_to_hand(P0, "Public Spell", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut board = Board::new(s);
    board.stamp(spell, "PSPL");
    fund(&mut board, P0, 2);
    cast(&mut board, spell, CastPaymentMode::Auto);
    assert_eq!(
        board.state().stack.len(),
        1,
        "reach: the spell is on the stack"
    );
    for viewer in VIEWERS {
        let row = stack_row(&board, viewer, spell);
        assert_eq!(row["source_name"], "Public Spell", "{viewer:?}");
        assert_eq!(row_set_code(&row).as_deref(), Some("SPSPL"), "{viewer:?}");
    }
    board.assert_exposure_rule("public spell row");
}

/// A face-down cast shows the controller its real name and nobody a printing; the
/// card's earlier face-up incarnation is still cached (the trap), and a row that fell back to
/// that last-known information over the present, redacted object would show it.
#[test]
fn stack_row_of_a_face_down_cast_has_no_printing_and_ignores_cached_lki() {
    let mut s = scenario();
    let trap = add_morph(&mut s, P0, "Trap Beast");
    let mut board = Board::new(s);
    board.stamp(trap, "TRAP");
    // A face-up incarnation on the battlefield first, so its last-known information is cached
    // with the name and the printing.
    board.move_to(trap, Zone::Battlefield);
    board.move_to(trap, Zone::Hand);
    assert_eq!(
        board.state().lki_cache[&trap].printing.get(),
        &Some(sentinel("TRAP")),
        "reach: the earlier incarnation's cached last-known information holds the printing"
    );
    fund(&mut board, P0, 6);
    cast_face_down(&mut board, trap);
    let object = &board.state().objects[&trap];
    assert_eq!(
        object.zone,
        Zone::Stack,
        "reach: the face-down spell is on the stack"
    );
    assert!(object.face_down);

    let p0 = stack_row(&board, Viewer::Seat(P0), trap);
    assert_eq!(
        p0["source_name"], "Trap Beast",
        "the controller sees the real name"
    );
    assert_eq!(row_set_code(&p0), None);
    for viewer in [Viewer::Seat(P1), Viewer::Spectator, Viewer::Unseated] {
        let row = stack_row(&board, viewer, trap);
        assert_eq!(row["source_name"], HIDDEN, "{viewer:?}");
        assert_eq!(
            row_set_code(&row),
            None,
            "{viewer:?}: no printing from the cached earlier incarnation: {row}"
        );
    }
    // The trap is armed: the opponent's filtered cache still holds the earlier incarnation's
    // name and printing together (parity), which a LKI fallback would have surfaced.
    let p1 = filter_state_for_viewer(board.state(), P1);
    assert_eq!(p1.lki_cache[&trap].printing.get(), &Some(sentinel("TRAP")));
    assert_eq!(p1.lki_cache[&trap].name, "Trap Beast");
    board.assert_exposure_rule("face-down cast row");
}

/// A triggered ability whose name is latched: with a trigger-source context the row
/// carries the context's printing; a context-free (delayed) entry reads the live, filtered
/// source object, so the opponent loses the printing once that object is hidden.
/// (Hand-built stack entries: the context and the context-free shapes are pushed directly.)
#[test]
fn triggered_stack_rows_follow_their_latched_authority() {
    let mut s = scenario();
    let source = s.add_creature(P0, "Trigger Source", 1, 1).id();
    let delayed = s.add_creature(P0, "Delayed Source", 1, 1).id();
    let mut board = Board::new(s);
    board.stamp(source, "TRIG");
    board.stamp(delayed, "DLYD");
    let context = board.state().objects[&source]
        .snapshot_for_zone_change(source, Some(Zone::Battlefield), Zone::Hand)
        .trigger_source_context
        .expect("a real record carries its source context");
    let triggered = |entry: u64, source_id: ObjectId, name: &str, context| {
        let mut ability = ResolvedAbility::new(Effect::NoOp, vec![], source_id, P0);
        ability.trigger_source = context;
        StackEntry {
            id: ObjectId(entry),
            source_id,
            controller: P0,
            kind: StackEntryKind::TriggeredAbility {
                source_id,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: name.to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        }
    };
    board
        .state_mut()
        .stack
        .push_back(triggered(900, source, "Trigger Source", Some(context)));
    board
        .state_mut()
        .stack
        .push_back(triggered(901, delayed, "Delayed Source", None));

    // Public sources: both rows carry their printings.
    for viewer in VIEWERS {
        assert_eq!(
            row_set_code(&stack_row(&board, viewer, ObjectId(900))).as_deref(),
            Some("STRIG"),
            "{viewer:?}"
        );
        assert_eq!(
            row_set_code(&stack_row(&board, viewer, ObjectId(901))).as_deref(),
            Some("SDLYD"),
            "{viewer:?}"
        );
    }

    // Both sources return to P0's hand; the opponent cannot see them.
    board.move_to(source, Zone::Hand);
    board.move_to(delayed, Zone::Hand);
    let p0_delayed = stack_row(&board, Viewer::Seat(P0), ObjectId(901));
    assert_eq!(row_set_code(&p0_delayed).as_deref(), Some("SDLYD"));
    for viewer in [Viewer::Seat(P1), Viewer::Spectator, Viewer::Unseated] {
        let delayed_row = stack_row(&board, viewer, ObjectId(901));
        assert_eq!(delayed_row["source_name"], "Delayed Source");
        assert_eq!(
            row_set_code(&delayed_row),
            None,
            "{viewer:?}: the hidden live source discloses no printing beside the latched name"
        );
    }
    // The context-carrying row equals the context in the viewer's own filtered state.
    for viewer in VIEWERS {
        let filtered = filtered_state(&board, viewer);
        let entry = filtered
            .stack
            .iter()
            .find(|entry| entry.id == ObjectId(900))
            .expect("the trigger is on the stack");
        let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
            panic!("the entry is a triggered ability");
        };
        let context_printing = ability
            .trigger_source
            .as_ref()
            .and_then(|context| context.lki.printing.get().clone())
            .map(|printing| printing.set_code);
        assert_eq!(
            row_set_code(&stack_row(&board, viewer, ObjectId(900))),
            context_printing,
            "{viewer:?}: the row's printing is the filtered context's"
        );
    }
    board.assert_exposure_rule("triggered rows");
}

/// A stamped battlefield source creates a delayed ability through the production resolver
/// (the ability stores the source's context as it is now), the source is optionally merged
/// under a mutating card, and the delayed ability then fires through the production check.
/// Returns the board and the fired stack entry.
fn fire_delayed_ability_of_host(merge_first: bool) -> (Board, ObjectId) {
    let mut s = scenario();
    let host = s.add_creature(P0, "Delayed Host", 2, 2).id();
    let gem = add_mutator(&mut s, "Gemrazer");
    let mut board = Board::new(s);
    board.stamp(host, "HOST");
    board.stamp(gem, "GEMZ");
    fund(&mut board, P0, 12);
    let creator = ResolvedAbility::new(
        Effect::CreateDelayedTrigger {
            condition: engine::types::ability::DelayedTriggerCondition::AtNextPhase {
                phase: Phase::End,
            },
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )),
            uses_tracked_set: false,
        },
        vec![],
        host,
        P0,
    );
    board.primitive(|state, events| {
        engine::game::effects::delayed_trigger::resolve(state, &creator, events)
            .expect("the delayed trigger is created")
    });
    assert_eq!(
        board.state().delayed_triggers.len(),
        1,
        "reach: the delayed ability is waiting"
    );
    let stored_context = board.state().delayed_triggers[0]
        .ability
        .trigger_source
        .as_ref()
        .expect("the delayed ability carries its creation-time source context");
    assert_eq!(stored_context.lki.name, "Delayed Host");
    assert_eq!(
        stored_context.lki.printing.get(),
        &Some(sentinel("HOST")),
        "reach: the stored context holds the source's printing"
    );
    if merge_first {
        mutate_onto(&mut board, gem, MergeSide::Top);
        assert_eq!(
            board.state().objects[&host].name,
            "Gemrazer",
            "reach: the merged source now carries the top card's name"
        );
    }
    let produced = engine::game::triggers::check_delayed_triggers(
        board.state_mut(),
        &[GameEvent::PhaseChanged { phase: Phase::End }],
    );
    board.log.extend(produced);
    assert_eq!(
        board.state().stack.len(),
        1,
        "reach: the delayed ability fired onto the stack"
    );
    let entry = board.state().stack[0].id;
    (board, entry)
}

/// A delayed ability keeps the source context stored when it was created (CR 603.7d), while
/// its row's name is latched when it fires. After the source is merged under a mutating card
/// the row names the merged permanent, so it must not carry the printing the older context
/// holds beside the name of the card that was there before.
#[test]
fn delayed_row_pairs_its_name_and_printing_from_one_authority() {
    // Control: unmerged, the row names the source and carries the source's printing.
    let (board, entry) = fire_delayed_ability_of_host(false);
    for viewer in VIEWERS {
        let row = stack_row(&board, viewer, entry);
        assert_eq!(row["source_name"], "Delayed Host", "{viewer:?}");
        assert_eq!(row_set_code(&row).as_deref(), Some("SHOST"), "{viewer:?}");
    }

    // After the merge the latched name is the top card's, and no viewer's row pairs it with
    // the survivor's earlier printing (nor with any other printing: the merge cleared both).
    let (board, entry) = fire_delayed_ability_of_host(true);
    for viewer in VIEWERS {
        let row = stack_row(&board, viewer, entry);
        assert_eq!(row["source_name"], "Gemrazer", "{viewer:?}");
        assert_eq!(row_set_code(&row), None, "{viewer:?}: {row}");
    }
}

/// An activated ability of a public permanent carries the live printing, put on the stack
/// through the production activation.
#[test]
fn stack_row_of_an_activated_ability_carries_the_live_printing() {
    let mut s = scenario();
    let source = s
        .add_creature(P0, "Ability Source", 1, 1)
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Activated,
            draw_effect(),
        ))
        .id();
    let mut board = Board::new(s);
    board.stamp(source, "ACTV");
    board.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    let entry = board
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == source)
        .map(|entry| entry.id)
        .expect("reach: the ability is on the stack");
    for viewer in VIEWERS {
        let row = stack_row(&board, viewer, entry);
        assert_eq!(row["source_name"], "Ability Source");
        assert_eq!(row_set_code(&row).as_deref(), Some("SACTV"), "{viewer:?}");
    }
    board.assert_exposure_rule("activated ability row");
}

/// An ability entry whose source no longer exists names "Unknown" and carries no printing,
/// even when the filtered last-known information holds one (hand-built board: the source
/// object is removed and its cached snapshot kept, the state a ceased copy leaves behind).
#[test]
fn stack_row_of_an_absent_source_discloses_no_printing() {
    let mut s = scenario();
    let source = s.add_creature(P0, "Ceased Copy", 1, 1).id();
    let mut board = Board::new(s);
    board.stamp(source, "CEAS");
    let snapshot = board.state().objects[&source].snapshot_public_characteristics();
    {
        let state = board.state_mut();
        state.lki_cache.insert(source, snapshot);
        state.battlefield.retain(|id| *id != source);
        state.objects.remove(&source);
        let ability = ResolvedAbility::new(draw_effect(), vec![], source, P0);
        state.stack.push_back(StackEntry {
            id: ObjectId(950),
            source_id: source,
            controller: P0,
            kind: StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: Box::new(ability),
            },
        });
    }
    for viewer in VIEWERS {
        let row = stack_row(&board, viewer, ObjectId(950));
        assert_eq!(row["source_name"], "Unknown", "{viewer:?}");
        assert_eq!(row_set_code(&row), None, "{viewer:?}: {row}");
    }
    // Reach: the cached snapshot still holds the sentinel in every viewer's filtered state.
    for viewer in VIEWERS {
        let state_form = &board.forms(viewer)[0].1;
        assert_eq!(
            state_form["lki_cache"][source.0.to_string()]["printing"]["set_code"],
            "SCEAS",
            "{viewer:?}: the trap is armed"
        );
    }
}

/// The production route to an absent source with an inherited printing: a copy of a
/// permanent spell resolves as a token that carries the original's printing (CR 707.10f), its
/// ability is activated, the token leaves and ceases to exist (CR 704.5d), and the ability waits
/// on the stack (CR 113.7a). Its row names "Unknown" and discloses no printing, although every
/// viewer's filtered last-known information of the token still holds one.
#[test]
fn stack_row_of_a_ceased_spell_copy_token_discloses_no_printing() {
    let mut s = scenario();
    let copier = s.add_creature(P0, "Copier", 1, 1).id();
    let spell = s
        .add_creature_to_hand(P0, "Copied Beast", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Activated,
            draw_effect(),
        ))
        .id();
    let mut board = Board::new(s);
    board.stamp(spell, "CPYB");
    fund(&mut board, P0, 3);
    cast(&mut board, spell, CastPaymentMode::Auto);
    assert_eq!(
        board.state().stack.len(),
        1,
        "reach: the creature spell is on the stack"
    );

    let copy = ResolvedAbility::new(
        Effect::CopySpell {
            target: TargetFilter::Any,
            retarget: CopyRetargetPermission::KeepOriginalTargets,
            copier: None,
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
        },
        vec![],
        copier,
        P0,
    );
    board.primitive(|state, events| {
        engine::game::effects::copy_spell::resolve(state, &copy, events)
            .expect("the spell is copied");
    });
    assert_eq!(
        board.state().stack.len(),
        2,
        "reach: the copy is on the stack"
    );
    // The copy resolves first (as a token), then the original spell.
    pass_priority_twice(&mut board);
    pass_priority_twice(&mut board);
    let token = board
        .state()
        .objects
        .values()
        .find(|object| object.is_token && object.name == "Copied Beast")
        .map(|object| object.id)
        .expect("reach: the permanent-spell copy resolved as a token");
    assert_eq!(
        live_set_code(board.state(), token).as_deref(),
        Some("SCPYB"),
        "reach: the token inherited the original's printing"
    );

    board.act(GameAction::ActivateAbility {
        source_id: token,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    let entry = board
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == token)
        .map(|entry| entry.id)
        .expect("reach: the token's ability is on the stack");
    for viewer in VIEWERS {
        let row = stack_row(&board, viewer, entry);
        assert_eq!(
            row["source_name"], "Copied Beast",
            "{viewer:?}: while it exists"
        );
        assert_eq!(row_set_code(&row).as_deref(), Some("SCPYB"), "{viewer:?}");
    }

    board.move_to(token, Zone::Graveyard);
    board.primitive(|state, events| {
        engine::game::sba::check_state_based_actions(state, events);
    });
    assert!(
        !board.state().objects.contains_key(&token),
        "reach: the token ceased to exist"
    );
    for viewer in VIEWERS {
        let row = stack_row(&board, viewer, entry);
        assert_eq!(row["source_name"], "Unknown", "{viewer:?}");
        assert_eq!(row_set_code(&row), None, "{viewer:?}: {row}");
        // The trap is armed: the viewer's filtered cache still holds the token's printing.
        assert_eq!(
            board.forms(viewer)[0].1["lki_cache"][token.0.to_string()]["printing"]["set_code"],
            "SCPYB",
            "{viewer:?}: the cached last-known information holds the inherited printing"
        );
    }
}

/// A public source whose rules name is empty while it holds its printing: an uncast Room
/// enters fully locked (CR 709.5d), so it has no name while its physical identity is public.
/// The row discloses no printing beside the empty name. Production route: the Room enters the
/// battlefield through the zone pipeline and the layers derive its empty name; the ability
/// entry is built on top of it (both doors are locked, so none of its text functions to
/// activate). No concealment clears a Room's printing, correctly: its artwork is public. Public
/// nameless sources (Rooms, copies of a face-down permanent) pair an empty rules name with a
/// public printing, and the derived row's empty-name guard is what keeps them apart.
#[test]
fn stack_row_of_a_nameless_public_room_has_no_printing() {
    let mut s = scenario();
    let room = s
        .add_creature_to_hand(P0, "Test Room", 0, 0)
        .with_subtypes(vec!["Room"])
        .id();
    let mut board = Board::new(s);
    board.stamp(room, "ROOM");
    board.move_to(room, Zone::Battlefield);
    engine::game::layers::evaluate_layers(board.state_mut());
    let object = &board.state().objects[&room];
    assert_eq!(object.zone, Zone::Battlefield, "reach: the Room entered");
    assert_eq!(object.name, "", "reach: an uncast Room has no rules name");
    assert_eq!(
        live_set_code(board.state(), room).as_deref(),
        Some("SROOM"),
        "reach: its public printing is retained"
    );
    {
        let ability = ResolvedAbility::new(draw_effect(), vec![], room, P0);
        board.state_mut().stack.push_back(StackEntry {
            id: ObjectId(960),
            source_id: room,
            controller: P0,
            kind: StackEntryKind::ActivatedAbility {
                source_id: room,
                ability: Box::new(ability),
            },
        });
    }
    for viewer in VIEWERS {
        let row = stack_row(&board, viewer, ObjectId(960));
        assert_eq!(row["source_name"], "", "{viewer:?}");
        assert_eq!(
            row_set_code(&row),
            None,
            "{viewer:?}: no printing beside an empty displayed name: {row}"
        );
    }
}

// ---------------------------------------------------------------------------
// the debug library row
// ---------------------------------------------------------------------------

fn debug_rows(board: &Board, viewer: Viewer) -> Vec<Value> {
    board.forms(viewer)[1].1["derived"]["debug_library_cards"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The debug library row carries each own library card's printing beside its name, only for
/// a permitted viewer while debug mode is on; the ordinary projection of the same cards
/// carries neither.
#[test]
fn debug_library_row_only_inside_its_gate() {
    let mut s = scenario();
    let own = s.add_card_to_library_top(P0, "Own Library Card");
    let other = s.add_card_to_library_top(P1, "Other Library Card");
    let mut board = Board::new(s);
    board.stamp(own, "LIBA");
    board.stamp(other, "LIBB");
    {
        let state = board.state_mut();
        state.debug_mode = true;
        state.debug_permitted.insert(P0);
    }

    // The permitted viewer: the own library card's row, with its printing.
    let rows = debug_rows(&board, Viewer::Seat(P0));
    assert_eq!(
        rows.len(),
        1,
        "reach: the permitted viewer receives its library"
    );
    assert_eq!(rows[0]["name"], "Own Library Card");
    assert_eq!(rows[0]["printing"]["set_code"], "SLIBA");
    // The ordinary projection of the same card carries neither the name nor the printing.
    let p0 = board.forms(Viewer::Seat(P0));
    assert_eq!(projected_object(&p0[0].1, own)["name"], HIDDEN);
    assert_eq!(count_set_code(&p0[0].1, "SLIBA"), 0);
    // The other player's library is never in the row set.
    assert_eq!(count_set_code(&p0[1].1, "SLIBB"), 0);

    // Everyone else, and the same viewer with debug off: no rows, no library sentinel at all.
    let mut gated_out: Vec<(String, Vec<(String, Value)>)> = vec![
        ("not permitted".to_string(), board.forms(Viewer::Seat(P1))),
        ("spectator".to_string(), board.forms(Viewer::Spectator)),
        ("unseated".to_string(), board.forms(Viewer::Unseated)),
    ];
    board.state_mut().debug_mode = false;
    gated_out.push(("debug off".to_string(), board.forms(Viewer::Seat(P0))));
    for (label, forms) in gated_out {
        for (form_label, form) in forms {
            for tag in ["SLIBA", "SLIBB"] {
                assert_eq!(
                    count_set_code(&form, tag),
                    0,
                    "{label}: {form_label} holds {tag}"
                );
            }
        }
    }
    board.state_mut().debug_mode = true;
    board.assert_follows_name("debug row");
}

// ---------------------------------------------------------------------------
// the raw event channel
// ---------------------------------------------------------------------------

fn events_for(board: &Board, viewer: PlayerId) -> Vec<GameEvent> {
    filter_events_for_viewer(&board.log, board.state(), viewer)
}

fn events_text(events: &[GameEvent]) -> Value {
    json(&events)
}

/// Every printing-bearing event the raw channel forwards carries its card's name beside it,
/// and a dropped event drops the printing with it.
#[test]
fn raw_event_channel_forwards_printing_only_beside_its_name() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::BeginCombat);
    let searched = s.add_card_to_library_top(P0, "Searched Card");
    let drawn = s.add_card_to_library_top(P0, "Drawn Card");
    let foretold = s.add_card_to_hand(P0, "Foretold Card");
    let face_up_attacker = s.add_creature(P0, "Face Up Attacker", 2, 2).id();
    let morph_attacker = add_morph(&mut s, P0, "Face Down Attacker");
    let entering = add_morph(&mut s, P0, "Entering Secret");
    let walker = s.add_creature(P0, "Searcher", 1, 1).id();
    let mut board = Board::new(s);
    for (id, tag) in [
        (searched, "SRCH"),
        (drawn, "DRWN"),
        (foretold, "FORE"),
        (face_up_attacker, "FUAT"),
        (morph_attacker, "FDAT"),
        (entering, "ENTR"),
    ] {
        board.stamp(id, tag);
    }

    // A search of P0's library by P0: the viewed cards go to the searcher only.
    let search = ResolvedAbility::new(
        Effect::SearchLibrary {
            filter: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
            source_zones: vec![Zone::Library],
        },
        vec![],
        walker,
        P0,
    );
    board.primitive(|state, events| {
        engine::game::effects::resolve_ability_chain(state, &search, events, 0)
            .expect("the search resolves");
    });
    let has_search_view = |events: &[GameEvent]| {
        events
            .iter()
            .any(|event| matches!(event, GameEvent::HiddenSearchViewed { .. }))
    };
    assert!(
        has_search_view(&board.log),
        "reach: the search viewed cards"
    );
    assert!(
        has_search_view(&events_for(&board, P0)),
        "the searcher receives the view"
    );
    for viewer in [P1, SPECTATOR] {
        let events = events_for(&board, viewer);
        assert!(
            !has_search_view(&events),
            "{viewer:?} receives no search view"
        );
        assert_eq!(count_set_code(&events_text(&events), "SSRCH"), 0);
        assert_eq!(count_set_code(&events_text(&events), "SDRWN"), 0);
    }
    // The searcher's view carries each viewed card's printing beside its face names.
    let p0_events = events_text(&events_for(&board, P0));
    assert_eq!(count_set_code(&p0_events, "SSRCH"), 1);
    // Finish the search through the production choice so the game moves on.
    board.act(GameAction::SelectCards {
        cards: vec![searched],
    });

    // A draw (library -> hand) and a hand -> exile face-down move (foretell-shaped).
    board.move_to(drawn, Zone::Hand);
    board.primitive(|state, events| {
        let mut request = ZoneMoveRequest::effect(foretold, Zone::Exile, foretold);
        request.face_down_in_exile = ExileConcealment::FaceDown;
        move_object_for_test(state, request, events);
    });
    assert!(
        board.state().objects[&foretold].face_down,
        "reach: exiled face down"
    );
    for viewer in [P1, SPECTATOR] {
        let text = events_text(&events_for(&board, viewer));
        assert_eq!(
            count_set_code(&text, "SDRWN"),
            0,
            "{viewer:?}: the drawn card"
        );
        assert_eq!(
            count_set_code(&text, "SFORE"),
            0,
            "{viewer:?}: the foretold card"
        );
    }
    let p0_text = events_text(&events_for(&board, P0));
    assert!(
        count_set_code(&p0_text, "SDRWN") > 0,
        "the owner receives the library -> hand record with name and printing"
    );

    // A card played face down: its entry record has the real name, is flagged face down and
    // carries no printing; the opponent and the spectator never receive that event.
    board.play_face_down(P0, entering);
    let record = board
        .state()
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == entering)
        .expect("the entry was journaled");
    assert_eq!(record.name, "Entering Secret");
    let context = record
        .trigger_source_context
        .as_ref()
        .expect("a real record carries its source context");
    assert!(context.face_down);
    assert!(context.lki.printing.get().is_none());
    let entry_events = |events: &[GameEvent]| {
        events
            .iter()
            .filter(|event| {
                matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. }
                    if *object_id == entering)
            })
            .count()
    };
    assert_eq!(
        entry_events(&board.log),
        1,
        "reach: the raw entry event exists"
    );
    assert_eq!(entry_events(&events_for(&board, P0)), 1);
    assert_eq!(entry_events(&events_for(&board, P1)), 0);
    assert_eq!(entry_events(&events_for(&board, SPECTATOR)), 0);

    // A face-up and a face-down attacker declared together: the face-down attacker's record
    // carries no printing for anyone, the face-up one carries its own for everyone.
    board.play_face_down(P0, morph_attacker);
    board
        .state_mut()
        .objects
        .get_mut(&morph_attacker)
        .unwrap()
        .summoning_sick = false;
    let result = board
        .runner
        .declare_attackers(&[
            (face_up_attacker, AttackTarget::Player(P1)),
            (morph_attacker, AttackTarget::Player(P1)),
        ])
        .expect("both attackers are declared");
    board.log.extend(result.events);
    for viewer in [P0, P1, SPECTATOR] {
        let text = events_text(&events_for(&board, viewer));
        assert!(
            count_set_code(&text, "SFUAT") > 0,
            "{viewer:?}: the face-up attacker's record carries its printing"
        );
        assert_eq!(
            count_set_code(&text, "SFDAT"),
            0,
            "{viewer:?}: the face-down attacker's record carries none"
        );
    }

    // A face-down permanent leaving the battlefield carries no printing either.
    board.move_to(morph_attacker, Zone::Graveyard);
    board.assert_nowhere("FDAT", "event channel, face-down departure");
    board.assert_nowhere("ENTR", "event channel, face-down entry");

    // Every filtered event list passes the exposure rule.
    for viewer in [P0, P1, SPECTATOR] {
        let text = events_text(&events_for(&board, viewer));
        assert_printing_follows_name(
            &text,
            &board.names,
            &format!("event channel, {viewer:?} events"),
        );
    }
}

// ---------------------------------------------------------------------------
// whole-projection census
// ---------------------------------------------------------------------------

/// One board with a card in every hidden category and every viewer: a printing occurs in a
/// carrier only where the card's name is shown in the same carrier, and the face-down
/// permanent and face-down spell hold no printing anywhere, authoritative state included.
#[test]
fn whole_projection_sentinel_census() {
    let mut s = scenario();
    let public = s.add_creature(P0, "Public Card", 2, 2).id();
    let hand = s.add_card_to_hand(P0, "Hand Card");
    let library = s.add_card_to_library_top(P0, "Library Card");
    let down_permanent = add_morph(&mut s, P0, "Face Down Permanent");
    let down_spell = add_morph(&mut s, P0, "Face Down Spell");
    let bounced = s.add_creature(P0, "Bounced Card", 1, 1).id();
    let twin_a = s.add_creature(P0, "Twin", 1, 1).id();
    let twin_b = s.add_creature(P1, "Twin", 1, 1).id();
    let exiled = s.add_card_to_hand(P0, "Exiled Card");
    let mut board = Board::new(s);
    for (id, tag) in [
        (public, "PUBL"),
        (hand, "HAND"),
        (library, "LIBR"),
        (down_permanent, "FDPM"),
        (down_spell, "FDSP"),
        (bounced, "BNCD"),
        (twin_a, "TWNA"),
        (twin_b, "TWNB"),
        (exiled, "EXLD"),
    ] {
        board.stamp(id, tag);
    }
    fund(&mut board, P0, 6);
    board.play_face_down(P0, down_permanent);
    cast_face_down(&mut board, down_spell);
    board.move_to(bounced, Zone::Hand);
    board.primitive(|state, events| {
        let mut request = ZoneMoveRequest::effect(exiled, Zone::Exile, exiled);
        request.face_down_in_exile = ExileConcealment::FaceDown;
        move_object_for_test(state, request, events);
    });
    let before = json(board.state());

    // Reach: the face-down permanent and spell exist as such, and the hidden-category cards
    // still hold their sentinels on the authoritative objects.
    assert!(board.state().objects[&down_permanent].face_down);
    assert_eq!(board.state().objects[&down_spell].zone, Zone::Stack);
    for (id, tag) in [
        (hand, "HAND"),
        (library, "LIBR"),
        (bounced, "BNCD"),
        (exiled, "EXLD"),
    ] {
        assert_eq!(live_set_code(board.state(), id), Some(format!("S{tag}")));
    }

    // The face-down cards forgot their printings at the moment they became face down.
    board.assert_nowhere("FDPM", "census");
    board.assert_nowhere("FDSP", "census");

    for viewer in [Viewer::Seat(P1), Viewer::Spectator, Viewer::Unseated] {
        let forms = board.forms(viewer);
        let screens: Vec<_> = forms
            .iter()
            .filter(|(label, _)| label.ends_with("state") || label.ends_with("wire"))
            .collect();
        for tag in ["HAND", "LIBR", "BNCD", "EXLD"] {
            for (label, form) in &screens {
                assert_eq!(
                    count_set_code(form, &format!("S{tag}")),
                    0,
                    "{viewer:?}: {tag} reached {label}"
                );
            }
        }
        // The hidden hand and library cards also never reach the raw events.
        for (label, form) in forms.iter().filter(|(label, _)| label.ends_with("events")) {
            for tag in ["HAND", "LIBR", "EXLD"] {
                assert_eq!(
                    count_set_code(form, &format!("S{tag}")),
                    0,
                    "{viewer:?}: {tag} in {label}"
                );
            }
        }
        // Positive controls: the public card and both twins keep their own printings.
        for (label, form) in &screens {
            for tag in ["PUBL", "TWNA", "TWNB"] {
                assert!(
                    count_set_code(form, &format!("S{tag}")) > 0,
                    "{viewer:?}: {tag} missing from {label}"
                );
            }
        }
    }
    // The owner's own hand card reaches the owner's forms.
    for (label, form) in board
        .forms(Viewer::Seat(P0))
        .iter()
        .filter(|(label, _)| label.ends_with("state") || label.ends_with("wire"))
    {
        assert!(
            count_set_code(form, "SHAND") > 0,
            "P0: hand card missing from {label}"
        );
        assert_eq!(
            count_set_code(form, "SLIBR"),
            0,
            "P0: the library stays hidden in {label}"
        );
    }
    board.assert_exposure_rule("census");
    assert_eq!(
        json(board.state()),
        before,
        "the projections leave the authoritative state untouched"
    );
}

// ---------------------------------------------------------------------------
// the concealment writers
// ---------------------------------------------------------------------------

fn pass_priority_twice(board: &mut Board) {
    board.act(GameAction::PassPriority);
    board.act(GameAction::PassPriority);
}

fn assert_face_down_and_stamped_before(board: &Board, id: ObjectId, tag: &str) {
    assert_eq!(
        live_set_code(board.state(), id),
        Some(format!("S{tag}")),
        "reach: the card holds its sentinel before the writer runs"
    );
}

/// `play_face_down`: the entry preflight and the face-down profile forget the printing
/// before the hand-exit record is captured; it stays gone after the departure.
#[test]
fn play_face_down_conceals_the_printing() {
    let mut s = scenario();
    let hand = add_morph(&mut s, P0, "Hand Secret");
    let public = s.add_creature(P0, "Public Bear", 2, 2).id();
    let mut board = Board::new(s);
    board.stamp(hand, "HSEC");
    board.stamp(public, "PUBB");
    assert_face_down_and_stamped_before(&board, hand, "HSEC");
    board.play_face_down(P0, hand);
    assert!(board.state().objects[&hand].face_down, "reach: face down");
    assert_eq!(
        board.state().objects[&hand]
            .back_face
            .as_ref()
            .map(|face| face.name.as_str()),
        Some("Hand Secret"),
        "reach: the real face is stashed"
    );
    board.assert_nowhere("HSEC", "after play_face_down");
    board.move_to(hand, Zone::Graveyard);
    assert_eq!(board.state().objects[&hand].zone, Zone::Graveyard);
    board.assert_nowhere("HSEC", "after the departure");
    // Positive control: a public card's sentinel is present for every viewer throughout.
    board.assert_exposure_rule("play face down");
}

/// A face-down cast through the engine: on the stack, resolved face down, turned up by
/// the special action, then departed.
#[test]
fn face_down_cast_conceals_the_printing_through_turn_up_and_departure() {
    let mut s = scenario();
    let morph = add_morph(&mut s, P0, "Secret Beast");
    let public = s.add_creature(P0, "Public Bear", 2, 2).id();
    let mut board = Board::new(s);
    board.stamp(morph, "BEAST");
    board.stamp(public, "PUBB");
    fund(&mut board, P0, 9);
    assert_face_down_and_stamped_before(&board, morph, "BEAST");
    cast_face_down(&mut board, morph);
    assert_eq!(board.state().objects[&morph].zone, Zone::Stack);
    assert!(
        board.state().objects[&morph].face_down,
        "reach: cast face down"
    );
    board.assert_nowhere("BEAST", "on the stack");
    pass_priority_twice(&mut board);
    assert_eq!(
        board.state().objects[&morph].zone,
        Zone::Battlefield,
        "reach: resolved"
    );
    assert!(board.state().objects[&morph].face_down);
    board.assert_nowhere("BEAST", "resolved face down");
    board.act(GameAction::TurnFaceUp {
        object_id: morph,
        x: 0,
    });
    let object = &board.state().objects[&morph];
    assert!(!object.face_down, "reach: turned face up");
    assert_eq!(object.name, "Secret Beast", "turned up with its real name");
    assert!(live_set_code(board.state(), morph).is_none());
    board.assert_nowhere("BEAST", "after the turn-up");
    board.move_to(morph, Zone::Graveyard);
    board.assert_nowhere("BEAST", "after the departure");
    board.assert_exposure_rule("face-down cast");
}

/// Restoration authorities. A face-down cast cancelled during mana payment, before any cost is
/// paid, takes the plain face-down rollback: the card returns to hand face up with its real
/// name, and that rollback assigns no printing, so the cancelled cast's card stays without one.
/// (A cancel after a cost was paid restores through the recorded cast undo edits instead; see
/// `cancelled_face_down_cast_that_paid_a_cost_with_the_turn_down_restores_the_recorded_printing`.)
#[test]
fn cancelled_face_down_cast_returns_the_card_to_hand_without_resurrecting_a_printing() {
    let mut s = scenario();
    let morph = add_morph(&mut s, P0, "Secret Beast");
    let mut board = Board::new(s);
    board.stamp(morph, "CNCL");
    fund(&mut board, P0, 5);
    assert_face_down_and_stamped_before(&board, morph, "CNCL");
    cast(&mut board, morph, CastPaymentMode::Manual);
    board.act(GameAction::ChooseAlternativeCast {
        choice: AlternativeCastDecision::Alternative,
    });
    assert!(
        matches!(board.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the cast waits at mana payment, got {:?}",
        board.state().waiting_for
    );
    assert!(
        board.state().objects[&morph].face_down,
        "reach: blanked before payment"
    );
    board.act(GameAction::CancelCast);
    let object = &board.state().objects[&morph];
    assert_eq!(object.zone, Zone::Hand, "reach: back in hand");
    assert!(!object.face_down, "reach: face up again");
    assert_eq!(
        object.name, "Secret Beast",
        "reach: the real name is restored"
    );
    // Recorded outcome: the cancel restored name and face, and the printing stays gone (the
    // reversal records no printing edit, and the concealment happened inside the cast).
    assert_eq!(
        live_set_code(board.state(), morph),
        None,
        "the cancelled cast's card holds no printing"
    );
    assert_eq!(base_set_code(board.state(), morph), None);
    board.assert_nowhere("CNCL", "after the cancel");
}

/// The printing `tag` is held by the owner's forms (the authoritative state and the owner's
/// state and wire) and by no other viewer's state or wire, as for any card in a hand.
fn assert_printing_in_owner_forms_only(board: &Board, tag: &str, step: &str) {
    let census = board.census(tag);
    for label in [
        "authoritative state",
        "Seat(PlayerId(0)) state",
        "Seat(PlayerId(0)) wire",
    ] {
        assert!(census[label] > 0, "{step}: {label} lost {tag}: {census:?}");
    }
    for label in [
        "Seat(PlayerId(1)) state",
        "Seat(PlayerId(1)) wire",
        "Spectator state",
        "Spectator wire",
        "Unseated state",
        "Unseated wire",
    ] {
        assert_eq!(census[label], 0, "{step}: {label} shows {tag}: {census:?}");
    }
}

/// A morph card in P0's hand and an opposing source of an unconditional "cast a spell: pay 2
/// life" tax, whose payment is made by the very action that turns the card face down.
fn face_down_cast_under_a_life_tax(tag: &str, life: i32) -> (Board, ObjectId) {
    let mut s = scenario();
    let morph = add_morph(&mut s, P0, "Secret Beast");
    let tax = s.add_creature(P1, "Tax Source", 1, 1).id();
    let mut board = Board::new(s);
    board.stamp(morph, tag);
    {
        let object = board
            .state_mut()
            .objects
            .get_mut(&tax)
            .expect("the tax source exists");
        let definition = StaticDefinition::new(StaticMode::ImposeAdditionalCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            spell_filter: None,
            action: AdditionalCostTaxAction::Cast,
        });
        object.static_definitions.push(definition.clone());
        std::sync::Arc::make_mut(&mut object.base_static_definitions).push(definition);
    }
    engine::game::layers::flush_layers(board.state_mut());
    fund(&mut board, P0, 5);
    board.state_mut().players[0].life = life;
    cast(&mut board, morph, CastPaymentMode::Manual);
    assert!(
        matches!(
            board.state().waiting_for,
            WaitingFor::AlternativeCastChoice { .. }
        ),
        "reach: the face-down choice is pending, got {:?}",
        board.state().waiting_for
    );
    (board, morph)
}

/// Restoration authorities: a cast cancelled after a cost was paid is reversed through the
/// recorded cast undo edits, which replay a diff of the states around the action that paid. Here
/// the tax's life payment is made by the same action that turns the card face down, so that
/// diff holds the card's printing from before the action and the cancel writes it back: the
/// card is in hand face up with its real name and the printing it held before the cast. (When
/// the paying action comes after the turn-down, the diff holds the cleared printing instead.)
#[test]
fn cancelled_face_down_cast_that_paid_a_cost_with_the_turn_down_restores_the_recorded_printing() {
    let (mut board, morph) = face_down_cast_under_a_life_tax("PAID", 20);
    board.act(GameAction::ChooseAlternativeCast {
        choice: AlternativeCastDecision::Alternative,
    });
    // Reach: the cost was paid, the reversal is populated with the printing, the card is blank.
    assert!(
        matches!(board.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the cast waits at mana payment, got {:?}",
        board.state().waiting_for
    );
    assert_eq!(board.state().players[0].life, 18, "reach: the tax was paid");
    let reversal = serde_json::to_value(board.state()).expect("state serializes")
        ["spell_cast_reversal"]
        .clone();
    assert_eq!(
        reversal["cost_mutations"], true,
        "reach: a cost mutation was recorded: {reversal}"
    );
    let edited_paths: Vec<&Vec<Value>> = reversal["undo"]
        .as_array()
        .expect("the reversal holds its undo edits")
        .iter()
        .filter_map(|edit| edit.get("Value"))
        .filter_map(|edit| edit["path"].as_array())
        .collect();
    let morph_key = Value::String(morph.0.to_string());
    for field in ["printing", "base_printing"] {
        assert!(
            edited_paths.iter().any(|path| {
                path.len() == 3 && path[0] == "objects" && path[1] == morph_key && path[2] == field
            }),
            "reach: the undo edits restore {field}: {edited_paths:?}"
        );
    }
    assert!(board.state().objects[&morph].face_down, "reach: blanked");
    assert_eq!(live_set_code(board.state(), morph), None);
    assert_eq!(base_set_code(board.state(), morph), None);
    // The recorded printing lives only inside the reversal of the authoritative state; no
    // viewer's state, wire or event form carries it while the card is blank.
    assert_eq!(count_set_code(&reversal, "SPAID"), 2, "{reversal}");
    for (label, count) in board.census("PAID") {
        let expected = if label == "authoritative state" { 2 } else { 0 };
        assert_eq!(count, expected, "while the cast is face down: {label}");
    }

    board.act(GameAction::CancelCast);
    let object = &board.state().objects[&morph];
    assert_eq!(object.zone, Zone::Hand, "reach: back in hand");
    assert!(!object.face_down, "reach: face up again");
    assert_eq!(object.name, "Secret Beast");
    assert_eq!(
        board.state().players[0].life,
        20,
        "reach: the payment was reversed"
    );
    // Recorded outcome: the undo edits wrote the pre-cast printing back, beside the real name.
    assert_eq!(
        live_set_code(board.state(), morph).as_deref(),
        Some("SPAID")
    );
    assert_eq!(
        base_set_code(board.state(), morph).as_deref(),
        Some("SPAID")
    );
    assert_printing_in_owner_forms_only(&board, "PAID", "after the paid cancel");
    board.assert_follows_name("after the paid cancel");
}

/// Restoration authorities: an action the engine rejects restores the state it began from, so
/// the snapshot taken before the action returns whatever it held. Rejection after the face-down
/// writer ran is reachable: choosing the face-down cost builds the blanked card, takes it as the
/// live state, and only then declares the tax, which a player at 1 life cannot pay, so the
/// choice is refused (`continue_with_prepared_casting_variant`, then
/// `Cannot pay imposed additional cost`). The card is back as it was before the choice, face up
/// in hand, still holding its printing, and the cast still waits for the choice.
#[test]
fn rejected_face_down_choice_after_the_writer_ran_restores_the_card_as_it_was() {
    let (mut board, morph) = face_down_cast_under_a_life_tax("LATE", 1);
    let rejected = board.runner.act(GameAction::ChooseAlternativeCast {
        choice: AlternativeCastDecision::Alternative,
    });
    let error = rejected.expect_err("reach: 1 life cannot pay the 2 life tax");
    assert!(
        format!("{error:?}").contains("Cannot pay imposed additional cost"),
        "reach: refused by the declaration that follows the writer, got {error:?}"
    );
    assert!(
        matches!(
            board.state().waiting_for,
            WaitingFor::AlternativeCastChoice { .. }
        ),
        "the choice is still pending, got {:?}",
        board.state().waiting_for
    );
    let object = &board.state().objects[&morph];
    assert_eq!(object.zone, Zone::Hand);
    assert!(!object.face_down);
    assert_eq!(object.name, "Secret Beast");
    assert_eq!(
        live_set_code(board.state(), morph).as_deref(),
        Some("SLATE"),
        "the snapshot taken before the action returned the printing"
    );
    assert_eq!(
        base_set_code(board.state(), morph).as_deref(),
        Some("SLATE")
    );
    assert_printing_in_owner_forms_only(&board, "LATE", "after the refused choice");
    board.assert_follows_name("after the refused choice");
}

/// Restoration authorities: an action the engine rejects restores the state it began from. A
/// cast of a morph card the player cannot pay for at all is rejected by the cast action itself,
/// before any face-down writer runs, so the card keeps the printing it held and every viewer's
/// carriers still follow the name. (The rejection after the writer is the previous test's.)
#[test]
fn rejected_face_down_cast_leaves_the_card_as_it_was() {
    let mut s = scenario();
    let morph = add_morph(&mut s, P0, "Secret Beast");
    let mut board = Board::new(s);
    board.stamp(morph, "RJCT");
    fund(&mut board, P0, 2);
    let card_id = board.state().objects[&morph].card_id;
    let rejected = board.runner.act(GameAction::CastSpell {
        object_id: morph,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        rejected.is_err(),
        "reach: two mana pay neither cost, so the cast is rejected"
    );
    let object = &board.state().objects[&morph];
    assert_eq!(object.zone, Zone::Hand);
    assert!(!object.face_down);
    assert_eq!(object.name, "Secret Beast");
    assert_eq!(
        live_set_code(board.state(), morph).as_deref(),
        Some("SRJCT"),
        "nothing concealed the card, so it keeps its printing"
    );
    assert_eq!(
        base_set_code(board.state(), morph).as_deref(),
        Some("SRJCT")
    );
    assert_printing_in_owner_forms_only(&board, "RJCT", "after the rejected cast");
    board.assert_follows_name("after the rejected cast");
}

/// A face-down entry the zone-change pipeline refuses (CR 101.2: the "can't" effect wins; Grafdigger's Cage class)
/// rolls back the flag only; the card stays in its zone and has already forgotten its printing.
#[test]
fn prevented_face_down_entry_leaves_the_card_without_a_printing() {
    let mut s = scenario();
    let source = s.add_creature(P0, "Manifest Source", 1, 1).id();
    let cage = s.add_creature(P0, "Cage", 0, 0).id();
    let caged = s.add_card_to_library_top(P0, "Caged Creature");
    let mut board = Board::new(s);
    board.stamp(caged, "CAGD");
    {
        let state = board.state_mut();
        let object = state.objects.get_mut(&caged).unwrap();
        object.card_types.core_types = vec![CoreType::Creature];
        object.base_card_types = object.card_types.clone();
        state
            .objects
            .get_mut(&cage)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantEnterBattlefieldFrom).affected(
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Creature)
                            .properties(vec![FilterProp::InAnyZone {
                                zones: vec![Zone::Graveyard, Zone::Library],
                            }]),
                    ),
                ),
            );
    }
    board.primitive(|state, events| {
        move_object_for_test(
            state,
            ZoneMoveRequest::effect(caged, Zone::Battlefield, source)
                .face_down(FaceDownProfile::vanilla_2_2()),
            events,
        );
    });
    let object = &board.state().objects[&caged];
    assert_eq!(object.zone, Zone::Library, "reach: the entry was refused");
    assert!(!object.face_down, "reach: the flag was rolled back");
    assert!(
        object.back_face.is_none(),
        "reach: the profile was not applied"
    );
    // Recorded outcome: only the flag rolls back; the preflight's concealment is kept.
    assert_eq!(live_set_code(board.state(), caged), None);
    assert_eq!(base_set_code(board.state(), caged), None);
}

/// A face-up battlefield creature turned face down and up again by the resolving effects.
#[test]
fn turn_face_down_effect_conceals_the_printing() {
    let mut s = scenario();
    let bear = s.add_creature(P0, "Ixidron Victim", 3, 3).id();
    let mut board = Board::new(s);
    board.stamp(bear, "VICT");
    // Positive control: the face-up creature's sentinel reaches every viewer.
    for viewer in VIEWERS {
        let forms = board.forms(viewer);
        assert!(count_set_code(&forms[0].1, "SVICT") > 0, "{viewer:?}");
    }
    let ability = ResolvedAbility::new(
        Effect::TurnFaceDown {
            target: TargetFilter::SelfRef,
            profile: None,
        },
        vec![],
        bear,
        P0,
    );
    board.primitive(|state, events| {
        engine::game::effects::turn_face_down::resolve(state, &ability, events)
            .expect("the effect resolves");
    });
    assert!(
        board.state().objects[&bear].face_down,
        "reach: turned face down"
    );
    board.assert_nowhere("VICT", "after turning face down");
    let up = ResolvedAbility::new(
        Effect::TurnFaceUp {
            target: TargetFilter::SelfRef,
        },
        vec![],
        bear,
        P0,
    );
    board.primitive(|state, events| {
        engine::game::effects::turn_face_up::resolve(state, &up, events)
            .expect("the effect resolves");
    });
    let object = &board.state().objects[&bear];
    assert!(!object.face_down, "reach: turned face up");
    assert_eq!(object.name, "Ixidron Victim");
    board.assert_nowhere("VICT", "after turning face up");
    board.move_to(bear, Zone::Graveyard);
    board.assert_nowhere("VICT", "after the departure");
}

/// A face-down permanent owned by P0 and controlled by P1: its controller sees the real
/// name, its owner and the others see none, and nobody (owner included) receives a printing.
#[test]
fn face_down_permanent_owned_and_controlled_by_different_players() {
    let mut s = scenario();
    let stolen = add_morph(&mut s, P0, "Stolen Secret");
    let mut board = Board::new(s);
    board.stamp(stolen, "STLN");
    board.play_face_down(P0, stolen);
    {
        let state = board.state_mut();
        let object = state.objects.get_mut(&stolen).unwrap();
        object.controller = P1;
        object.base_controller = Some(P1);
    }
    assert!(board.state().objects[&stolen].face_down, "reach: face down");
    let p1 = board.forms(Viewer::Seat(P1));
    assert_eq!(
        projected_object(&p1[0].1, stolen)["name"],
        "Stolen Secret",
        "the controller sees the real name (CR 708.5)"
    );
    let p0 = board.forms(Viewer::Seat(P0));
    assert_eq!(
        projected_object(&p0[0].1, stolen)["name"],
        HIDDEN,
        "the owner who does not control it does not"
    );
    board.assert_nowhere("STLN", "owner and controller differ");
}

// ---------------------------------------------------------------------------
// the mutate merge conceals every component
// ---------------------------------------------------------------------------

/// A mutating creature card in hand: mutate {2}, printed cost {4}, one costless activated
/// ability, so the merged permanent has an ability to activate.
fn add_mutator(s: &mut GameScenario, name: &str) -> ObjectId {
    s.add_creature_to_hand(P0, name, 4, 4)
        .with_keyword(Keyword::Mutate(ManaCost::generic(2)))
        .with_mana_cost(ManaCost::generic(4))
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Activated,
            draw_effect(),
        ))
        .id()
}

/// Cast `id` as a mutating creature spell onto its single legal target, resolve it and merge
/// it on `side`, all through the production choices.
fn mutate_onto(board: &mut Board, id: ObjectId, side: MergeSide) {
    cast(board, id, CastPaymentMode::Auto);
    board.act(GameAction::ChooseAlternativeCast {
        choice: AlternativeCastDecision::Alternative,
    });
    assert_eq!(
        board.state().stack.len(),
        1,
        "reach: the mutating spell is on the stack, waiting_for {:?}",
        board.state().waiting_for
    );
    pass_priority_twice(board);
    assert!(
        matches!(
            board.state().waiting_for,
            WaitingFor::MutateMergeChoice { .. }
        ),
        "reach: the merge choice is pending, got {:?}",
        board.state().waiting_for
    );
    board.act(GameAction::ChooseMutateMergeSide { side });
}

/// Neither stamped sentinel occurs in `printing` or `base_printing` of any live component of
/// the merged permanent whose survivor is `survivor`; returns the component ids.
fn assert_every_component_concealed(
    board: &Board,
    survivor: ObjectId,
    step: &str,
) -> Vec<ObjectId> {
    let components = board.state().objects[&survivor].merged_components.clone();
    assert!(
        components.len() >= 2,
        "{step}: reach: the pile has several components, got {components:?}"
    );
    for id in &components {
        assert_eq!(
            live_set_code(board.state(), *id),
            None,
            "{step}: component {id:?} keeps a live printing"
        );
        assert_eq!(
            base_set_code(board.state(), *id),
            None,
            "{step}: component {id:?} keeps a base printing"
        );
    }
    components
}

/// A sentinel occurs in no state or wire form, authoritative state included (the event
/// forms are left out: the card's public history may carry it beside its name).
fn assert_absent_from_state_forms(board: &Board, tag: &str, step: &str) {
    let set_code = sentinel(tag).set_code;
    for (label, form) in board.all_forms() {
        if label.ends_with("events") {
            continue;
        }
        assert_eq!(
            count_set_code(&form, &set_code),
            0,
            "{step}: {tag} reached {label}"
        );
    }
}

/// A mutating card merged over a face-down creature: the buried card's sentinel never
/// reaches any form, the pile's components hold no printing, an ability of the merged
/// permanent can be put on the stack, and the pile's departure leaks nothing.
#[test]
fn mutate_over_a_face_down_creature_conceals_every_component() {
    let mut s = scenario();
    let buried = add_morph(&mut s, P0, "Buried Secret");
    let gem = add_mutator(&mut s, "Gemrazer");
    // A public non-creature card: a positive control that cannot be a mutate target.
    let forest = s.add_basic_land(P0, ManaColor::Green);
    let mut board = Board::new(s);
    board.stamp(buried, "BRIED");
    board.stamp(gem, "GEMZ");
    board.stamp(forest, "FRST");
    fund(&mut board, P0, 12);
    board.play_face_down(P0, buried);
    assert!(
        board.state().objects[&buried].face_down,
        "reach: the root is face down"
    );

    // The mutating spell is public while it waits on the stack.
    cast(&mut board, gem, CastPaymentMode::Auto);
    board.act(GameAction::ChooseAlternativeCast {
        choice: AlternativeCastDecision::Alternative,
    });
    assert_eq!(
        board.state().stack.len(),
        1,
        "reach: the mutating spell is on the stack"
    );
    for viewer in VIEWERS {
        let row = stack_row(&board, viewer, gem);
        assert_eq!(
            row_set_code(&row).as_deref(),
            Some("SGEMZ"),
            "{viewer:?}: the public spell shows its printing: {row}"
        );
    }
    pass_priority_twice(&mut board);
    assert!(
        matches!(
            board.state().waiting_for,
            WaitingFor::MutateMergeChoice { .. }
        ),
        "reach: the merge choice is pending"
    );
    board.act(GameAction::ChooseMutateMergeSide {
        side: MergeSide::Top,
    });
    assert_eq!(
        assert_every_component_concealed(&board, buried, "after the merge"),
        vec![gem, buried],
        "reach: the mutating card is on top"
    );
    board.assert_nowhere("BRIED", "after the merge");
    board.assert_follows_name("after the merge");

    // The merged permanent's ability goes on the stack through the production activation.
    board.act(GameAction::ActivateAbility {
        source_id: buried,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        board
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == buried),
        "reach: the survivor's ability is on the stack"
    );
    board.assert_nowhere("BRIED", "with the ability on the stack");
    board.assert_follows_name("with the ability on the stack");

    // The pile leaves: both components change zone while the ability waits (CR 113.7a).
    board.move_to(buried, Zone::Graveyard);
    assert_eq!(board.state().objects[&buried].zone, Zone::Graveyard);
    assert_eq!(board.state().objects[&gem].zone, Zone::Graveyard);
    assert!(
        board
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == buried),
        "reach: the ability is still on the stack after the departure"
    );
    board.assert_nowhere("BRIED", "after the departure");
    for id in [buried, gem] {
        assert_eq!(live_set_code(board.state(), id), None, "{id:?}");
        assert_eq!(base_set_code(board.state(), id), None, "{id:?}");
    }
    board.assert_follows_name("after the departure");
    // Positive control: the public land's printing stayed present for every viewer.
    for viewer in VIEWERS {
        assert!(
            count_set_code(&board.forms(viewer)[0].1, "SFRST") > 0,
            "{viewer:?}"
        );
    }
}

/// A mutating card merged over a face-up root, then a second one underneath: every component
/// is concealed as it merges, the survivor's sentinel included (the survivor's own card was
/// public and never face down, so only the merge can clear it).
#[test]
fn mutate_over_a_face_up_root_conceals_every_component_across_merges() {
    let mut s = scenario();
    let root = s.add_creature(P0, "Root Mutant", 2, 2).id();
    let gem = add_mutator(&mut s, "Gemrazer");
    let third = add_mutator(&mut s, "Third Mutant");
    let mut board = Board::new(s);
    board.stamp(root, "ROOT");
    board.stamp(gem, "GEMZ");
    board.stamp(third, "THRD");
    fund(&mut board, P0, 12);
    // Positive control: before the merge the root's printing reaches every viewer.
    for viewer in VIEWERS {
        let forms = board.forms(viewer);
        assert!(count_set_code(&forms[0].1, "SROOT") > 0, "{viewer:?}");
    }
    assert_eq!(live_set_code(board.state(), root).as_deref(), Some("SROOT"));

    mutate_onto(&mut board, gem, MergeSide::Top);
    assert_eq!(
        assert_every_component_concealed(&board, root, "after the first merge"),
        vec![gem, root]
    );
    assert_absent_from_state_forms(&board, "ROOT", "after the first merge");
    board.assert_follows_name("after the first merge");

    // A second merge underneath the existing pile conceals the new component too.
    mutate_onto(&mut board, third, MergeSide::Bottom);
    assert_eq!(
        assert_every_component_concealed(&board, root, "after the bottom merge"),
        vec![gem, root, third]
    );
    assert_absent_from_state_forms(&board, "ROOT", "after the bottom merge");
    board.assert_follows_name("after the bottom merge");

    board.act(GameAction::ActivateAbility {
        source_id: root,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    board.move_to(root, Zone::Graveyard);
    for id in [root, gem, third] {
        assert_eq!(
            board.state().objects[&id].zone,
            Zone::Graveyard,
            "reach: {id:?} left"
        );
        assert_eq!(live_set_code(board.state(), id), None, "{id:?}");
        assert_eq!(base_set_code(board.state(), id), None, "{id:?}");
    }
    assert_absent_from_state_forms(&board, "ROOT", "after the departure");
    board.assert_follows_name("after the departure");
}

// ---------------------------------------------------------------------------
// later authorities cannot write a concealed printing back
// ---------------------------------------------------------------------------

/// A board with two cards that can hide their names: a mutate pile (a face-down card with a
/// card merged on top of it by the production merge) and a lone face-down permanent, plus a
/// public card as a positive control.
struct HiddenBoard {
    board: Board,
    buried: ObjectId,
    top: ObjectId,
    lone: ObjectId,
}

fn hidden_board() -> HiddenBoard {
    let mut s = scenario();
    let buried = add_morph(&mut s, P0, "Buried Secret");
    let top = s.add_creature_to_hand(P0, "Mutant", 4, 4).id();
    let lone = add_morph(&mut s, P0, "Lone Secret");
    let public = s.add_creature(P0, "Public Bear", 2, 2).id();
    let mut board = Board::new(s);
    board.stamp(buried, "BRID");
    board.stamp(top, "MUTT");
    board.stamp(lone, "LONE");
    board.stamp(public, "PUBB");
    board.play_face_down(P0, buried);
    board.play_face_down(P0, lone);
    board.primitive(|state, events| {
        engine::game::merge::merge_object_onto(state, top, buried, MergeSide::Top, events);
    });
    engine::game::layers::evaluate_layers(board.state_mut());
    assert_eq!(
        assert_every_component_concealed(&board, buried, "setup"),
        vec![top, buried],
        "reach: the pile is merged"
    );
    assert!(
        board.state().objects[&lone].face_down,
        "reach: the lone card is face down"
    );
    HiddenBoard {
        board,
        buried,
        top,
        lone,
    }
}

impl HiddenBoard {
    /// After an authority ran: the hidden cards' sentinels occur nowhere, every printing
    /// follows its name with no exception, and the public control keeps its printing for every
    /// viewer. Then the pile and the lone card depart and the same holds.
    fn assert_hidden_cards_stay_hidden(&mut self, step: &str) {
        engine::game::layers::evaluate_layers(self.board.state_mut());
        self.check(step);
        let (buried, lone) = (self.buried, self.lone);
        self.board.move_to(buried, Zone::Graveyard);
        self.board.move_to(lone, Zone::Graveyard);
        self.check(&format!("{step}, after the departures"));
        for id in [self.buried, self.top, self.lone] {
            assert_eq!(
                live_set_code(self.board.state(), id),
                None,
                "{step}: {id:?}"
            );
            assert_eq!(
                base_set_code(self.board.state(), id),
                None,
                "{step}: {id:?}"
            );
        }
    }

    fn check(&self, step: &str) {
        for tag in ["BRID", "MUTT", "LONE"] {
            self.board.assert_nowhere(tag, step);
        }
        self.board.assert_follows_name(step);
        for viewer in VIEWERS {
            assert!(
                count_set_code(&self.board.forms(viewer)[0].1, "SPUBB") > 0,
                "{step}: {viewer:?}: the public control lost its printing"
            );
        }
    }
}

/// `GameAction::Transform` is accepted on the merged survivor and on an unmerged face-down
/// permanent (a pre-existing rules defect, recorded elsewhere); the stored face it applies
/// carries no printing. A never-hidden double-faced card transformed the same way keeps its
/// own printing, beside its back face's name.
#[test]
fn transform_cannot_write_a_concealed_printing_back() {
    let mut h = hidden_board();
    let (buried, lone) = (h.buried, h.lone);
    h.board.act(GameAction::Transform { object_id: buried });
    h.board.act(GameAction::Transform { object_id: lone });
    assert!(
        h.board.state().objects[&buried].transformed,
        "reach: the survivor transformed"
    );
    assert!(
        h.board.state().objects[&lone].transformed,
        "reach: the lone card transformed"
    );
    h.assert_hidden_cards_stay_hidden("after Transform");

    // Control: a never-hidden double-faced card keeps its sentinel through a transform.
    let mut s = scenario();
    let dfc = s.add_creature(P0, "Day Face", 2, 2).id();
    let mut board = Board::new(s);
    board.stamp(dfc, "DFCC");
    board.state_mut().objects.get_mut(&dfc).unwrap().back_face = Some(BackFaceData {
        name: "Night Face".to_string(),
        power: Some(3),
        toughness: Some(3),
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        },
        layout_kind: Some(LayoutKind::Transform),
        ..Default::default()
    });
    board.act(GameAction::Transform { object_id: dfc });
    engine::game::layers::evaluate_layers(board.state_mut());
    assert_eq!(
        board.state().objects[&dfc].name,
        "Night Face",
        "reach: transformed"
    );
    board
        .names
        .insert(sentinel("DFCC").set_code, "Night Face".to_string());
    for viewer in VIEWERS {
        assert!(
            count_set_code(&board.forms(viewer)[0].1, "SDFCC") > 0,
            "{viewer:?}: the transformed card keeps its printing"
        );
    }
    board.assert_exposure_rule("transform control");
}

/// The survivor becomes a copy of a flip card and then flips into its stashed face; the
/// never-hidden flip card flips and keeps its own printing.
#[test]
fn become_copy_then_flip_cannot_write_a_concealed_printing_back() {
    let mut h = hidden_board();
    let buried = h.buried;
    let flip_card = {
        let state = h.board.state_mut();
        let id = create_object(
            state,
            CardId(8800),
            P0,
            "Flip Card".to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types = vec![CoreType::Creature];
        object.base_card_types = object.card_types.clone();
        object.back_face = Some(BackFaceData {
            name: "Flipped Face".to_string(),
            power: Some(3),
            toughness: Some(4),
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            },
            ..Default::default()
        });
        id
    };
    h.board.stamp(flip_card, "FLIP");
    let copy = ResolvedAbility::new(
        Effect::BecomeCopy {
            target: TargetFilter::Any,
            recipient: CopyRecipient::Source,
            duration: Some(Duration::Permanent),
            mana_value_limit: None,
            additional_modifications: Vec::new(),
        },
        vec![TargetRef::Object(flip_card)],
        buried,
        P0,
    );
    h.board.primitive(|state, events| {
        engine::game::effects::become_copy::resolve(state, &copy, events)
            .expect("the copy effect resolves");
    });
    engine::game::layers::flush_layers(h.board.state_mut());
    assert_eq!(
        h.board.state().objects[&buried].name,
        "Flip Card",
        "reach: the survivor is a copy of the flip card"
    );
    h.board.primitive(|state, events| {
        engine::game::flip::flip_permanent(state, buried, events).expect("the flip applies");
        engine::game::flip::flip_permanent(state, flip_card, events).expect("the flip applies");
    });
    assert!(
        h.board.state().objects[&buried].flipped,
        "reach: the survivor flipped"
    );
    assert!(
        h.board.state().objects[&flip_card].flipped,
        "reach: the control flipped"
    );
    h.board
        .names
        .insert(sentinel("FLIP").set_code, "Flipped Face".to_string());
    h.assert_hidden_cards_stay_hidden("after become_copy and flip");
    // Control: the never-hidden flip card keeps its printing for every viewer.
    // (The pile has departed by now; the flip card is still on the battlefield.)
    for viewer in VIEWERS {
        assert!(
            count_set_code(&h.board.forms(viewer)[0].1, "SFLIP") > 0,
            "{viewer:?}: the flipped card keeps its printing"
        );
    }
}

/// The survivor specializes (its specialize faces set by hand) into a stored face.
#[test]
fn specialize_cannot_write_a_concealed_printing_back() {
    let mut h = hidden_board();
    let buried = h.buried;
    h.board
        .state_mut()
        .objects
        .get_mut(&buried)
        .unwrap()
        .specialize_faces = Some(SpecializeFaceMap::from([(
        ManaColor::Green,
        BackFaceData {
            name: "Specialized Face".to_string(),
            power: Some(5),
            toughness: Some(5),
            ..Default::default()
        },
    )]));
    h.board.primitive(|state, events| {
        engine::game::specialize::specialize_permanent(state, buried, ManaColor::Green, events)
            .expect("the survivor specializes");
    });
    assert_eq!(
        h.board.state().objects[&buried].specialized_color,
        Some(ManaColor::Green),
        "reach: specialized"
    );
    h.assert_hidden_cards_stay_hidden("after specialize");
}

/// A persisted-state round trip and a card-database reload rewrite the survivor's faces from
/// printed data; real cards are used so the reload has data to apply. A public real card keeps
/// its printing through the reload.
#[test]
fn reload_cannot_write_a_concealed_printing_back() {
    let db = load_db().expect("integration fixture");
    let mut s = scenario();
    let buried = s.add_real_card(P0, "Gemrazer", Zone::Hand, db);
    let top = s.add_real_card(P0, "Gemrazer", Zone::Hand, db);
    let public = s.add_real_card(P0, "Gemrazer", Zone::Battlefield, db);
    let mut board = Board::new(s);
    board.stamp(buried, "RBUR");
    board.stamp(top, "RTOP");
    board.stamp(public, "RPUB");
    board.play_face_down(P0, buried);
    board.primitive(|state, events| {
        engine::game::merge::merge_object_onto(state, top, buried, MergeSide::Top, events);
    });
    engine::game::layers::evaluate_layers(board.state_mut());
    assert_eq!(
        assert_every_component_concealed(&board, buried, "before the reload"),
        vec![top, buried]
    );

    let wire = serde_json::to_value(board.state()).expect("the state serializes");
    let mut restored = serde_json::from_value::<PersistedGameState>(wire)
        .expect("the persisted wire decodes")
        .into_game_state()
        .expect("the restore contract holds");
    rehydrate_game_from_card_db_with_finalization(
        &mut restored,
        db,
        CardDbRehydrationFinalization::Immediate,
    );
    board.runner = GameRunner::from_state(restored);
    assert_eq!(
        assert_every_component_concealed(&board, buried, "after the reload"),
        vec![top, buried],
        "reach: the pile survived the reload"
    );
    for tag in ["RBUR", "RTOP"] {
        board.assert_nowhere(tag, "after the reload");
    }
    board.assert_follows_name("after the reload");
    board.move_to(buried, Zone::Graveyard);
    for tag in ["RBUR", "RTOP"] {
        board.assert_nowhere(tag, "after the departure");
    }
    board.assert_follows_name("after the departure");
    // Control: the public real card keeps its printing for every viewer through the reload.
    for viewer in VIEWERS {
        assert!(
            count_set_code(&board.forms(viewer)[0].1, "SRPUB") > 0,
            "{viewer:?}: the public card lost its printing"
        );
    }
}
