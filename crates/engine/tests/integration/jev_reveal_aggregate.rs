//! Ordinary instruction-wide reveal membership. All synthetic texts are explicit
//! consumer fixtures; printed siblings retain their complete pinned Oracle text.
use engine::game::derived_views::ClientGameStateRef;
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, Effect, EffectKind, QuantityExpr, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SCALAR: &str = "Each player reveals the top card of their library. You gain life equal to the number of cards revealed this way.";
const TOP_TWO: &str = "Each player reveals the top two cards of their library. You gain life equal to the number of cards revealed this way.";
const PAYMENT: &str =
    "Each player reveals the top card of their library unless they pay 1 life. You gain 3 life.";
const SEER: &str = "Flying\nAt the beginning of your upkeep, each player reveals the top card of their library, loses life equal to that card's mana value, then puts it into their hand.";
const PARKER: &str = "At the beginning of your end step, two target players each reveal the top card of their library. They each lose life equal to the mana value of the card revealed by the other player. Then they each put the card they revealed into their hand.";
const CONFIDANT: &str = "At the beginning of your upkeep, reveal the top card of your library and put that card into your hand. You lose life equal to its mana value.";
const PRESERVE: &str = "At the beginning of your upkeep, each player reveals the top card of their library. If all cards revealed this way are creature cards, put those cards onto the battlefield under their owners' control.";
const ALL_CREATURES: &str = "Each player reveals the top card of their library. If all cards revealed this way are creature cards, put those cards onto the battlefield under their owners' control.";
const AUTOMATON: &str = "As this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nOther creatures you control of the chosen type get +1/+1.";

fn creature_top(s: &mut GameScenario, seat: PlayerId, name: &str, oracle: &str) -> ObjectId {
    s.add_creature_to_hand_from_oracle(seat, name, 2, 2, oracle)
        .id()
}

fn seat_creature_tops(r: &mut GameRunner, cards: &[ObjectId]) {
    for &card in cards {
        engine::game::zones::move_to_zone(r.state_mut(), card, Zone::Library, &mut Vec::new());
        let owner = r.state().objects[&card].owner;
        let library = r.state_mut().library_of_mut(owner);
        library.retain(|id| *id != card);
        library.push_front(card);
    }
}

fn preserve_in_hand(s: &mut GameScenario) -> ObjectId {
    use engine::types::mana::{ManaType, ManaUnit};
    s.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]); 3],
    );
    s.add_spell_to_hand_from_oracle(P0, "Game Preserve", false, PRESERVE)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![engine::types::mana::ManaCostShard::Green],
        })
        .id()
}

#[test]
fn game_preserve_cast_then_upkeep_moves_every_owner_card() {
    for canonical in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = if canonical {
            use engine::game::scenario_db::GameScenarioDbExt;
            let db = crate::support::shared_card_db()
                .expect("canonical fixture required at final verification");
            let source = s.add_real_card(P0, "Game Preserve", Zone::Hand, db);
            use engine::types::mana::{ManaType, ManaUnit};
            s.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]); 3],
            );
            source
        } else {
            preserve_in_hand(&mut s)
        };
        let deep = [
            s.add_card_to_library_top(P0, "Deep zero"),
            s.add_card_to_library_top(P1, "Deep one"),
        ];
        let tops = [
            creature_top(&mut s, P0, "Zero creature", ""),
            creature_top(&mut s, P1, "One creature", ""),
        ];
        let mut r = s.build();
        seat_creature_tops(&mut r, &tops);
        r.cast(source)
            .resolve()
            .assert_zone(&[source], Zone::Battlefield);
        upkeep(&mut r);
        assert_eq!(r.state().stack.len(), 1);
        assert_eq!(r.state().stack[0].source_id, source);
        let events = drain_triggers(&mut r);
        assert_eq!(
            reveals(&events),
            vec![(P0, vec![tops[0]]), (P1, vec![tops[1]])]
        );
        for (seat, id) in [P0, P1].into_iter().zip(tops) {
            assert_eq!(r.state().objects[&id].zone, Zone::Battlefield);
            assert_eq!(r.state().objects[&id].controller, seat);
            assert_eq!(r.state().objects[&id].owner, seat);
            assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == id)).count(), 1);
        }
        for id in deep {
            assert_eq!(r.state().objects[&id].zone, Zone::Library);
        }
    }
}

#[test]
fn game_preserve_mixed_members_block_whole_move_and_keep_independent_tail() {
    for creature_seat in [P0, P1] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let other = if creature_seat == P0 { P1 } else { P0 };
        let creature = creature_top(&mut s, creature_seat, "Creature", "");
        let noncreature = s.add_land_to_library_top(other, "Land").id();
        let source = spell(&mut s, P0, &format!("{ALL_CREATURES} You gain 7 life."));
        let mut r = s.build();
        seat_creature_tops(&mut r, &[creature]);
        let out = r.cast(source).resolve();
        assert_eq!(reveals(out.events()).len(), 2);
        out.assert_zone(&[creature, noncreature], Zone::Library);
        out.assert_life_delta(P0, 7);
        assert_eq!(gains(out.events()), vec![(P0, 7)]);
        assert_no_library_moves(out.events(), &[creature, noncreature]);
    }
}

#[test]
fn revealed_result_present_empty_and_every_empty_seat() {
    for empty in 0..4 {
        let mut s = GameScenario::new_n_player(3, 42);
        s.at_phase(Phase::PreCombatMain);
        let cards: Vec<_> = (0..3)
            .filter(|&seat| if empty == 3 { seat == 1 } else { seat != empty })
            .map(|seat| creature_top(&mut s, PlayerId(seat), "Present creature", ""))
            .collect();
        let source = spell(&mut s, P0, &format!("{ALL_CREATURES} You gain 7 life."));
        let mut r = s.build();
        seat_creature_tops(&mut r, &cards);
        let out = r.cast(source).resolve();
        out.assert_zone(&cards, Zone::Battlefield);
        out.assert_life_delta(P0, 7);
        assert_eq!(reveals(out.events()).len(), cards.len());
    }
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let prior = s.add_card_to_library_top(P0, "Earlier real result");
    let source = spell(&mut s, P0, &format!("Reveal the top card of your library. You mill a card. {ALL_CREATURES} You gain 7 life."));
    let mut r = s.build();
    let out = r.cast(source).resolve();
    out.assert_zone(&[prior], Zone::Graveyard);
    assert_eq!(reveals(out.events()), vec![(P0, vec![prior])]);
    out.assert_life_delta(P0, 7);
    assert!(
        out.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChangeZone,
                ..
            }
        )),
        "present-empty consumer executed"
    );
}

#[test]
fn revealed_result_top_two_counts_members_and_generic_predicate() {
    for (mixed, empty) in [(false, false), (true, false), (false, true)] {
        let text = "Each player reveals the top two cards of their library. If all cards revealed this way are artifact cards, put those cards onto the battlefield under their owners' control.";
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let mut cards = Vec::new();
        for seat in [P0, P1] {
            if empty && seat == P1 {
                continue;
            }
            for n in 0..2 {
                let mut card = s.add_spell_to_library_top(seat, "Member", false);
                if !(mixed && seat == P1 && n == 1) {
                    card.as_creature().as_artifact();
                }
                cards.push(card.id());
            }
        }
        let source = spell(&mut s, P0, text);
        let mut r = s.build();
        let out = r.cast(source).resolve();
        assert_eq!(
            reveals(out.events())
                .iter()
                .map(|(_, ids)| ids.len())
                .sum::<usize>(),
            if empty { 2 } else { 4 }
        );
        out.assert_zone(
            &cards,
            if mixed {
                Zone::Library
            } else {
                Zone::Battlefield
            },
        );
    }
}

fn entry_pause() -> (GameRunner, [ObjectId; 2], ObjectId) {
    entry_pause_for_text(&format!("{ALL_CREATURES} You gain 7 life."))
}

fn entry_pause_for_text(text: &str) -> (GameRunner, [ObjectId; 2], ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Private Preserve depth zero"]);
    s.with_library_top(P1, &["Private Preserve depth one"]);
    let cards = [
        creature_top(&mut s, P0, "Adaptive Automaton", AUTOMATON),
        creature_top(&mut s, P1, "Adaptive Automaton", AUTOMATON),
    ];
    let source = spell(&mut s, P0, text);
    let mut r = s.build();
    seat_creature_tops(&mut r, &cards);
    r.cast(source).resolve();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::NamedChoice { player: P0, .. }
    ));
    assert_eq!(r.life(P0), 20);
    assert_eq!(r.state().objects[&cards[1]].zone, Zone::Library);
    assert!(r.state().resolution_stack.iter().any(|frame| matches!(frame, engine::types::resolution::ResolutionFrame::BatchDelivery(batch) if batch.requests.len() == 1 && batch.requests[0].expected_origin.is_some() && batch.requests[0].performed_by == Some(P0) && batch.requests[0].controller_override.is_none() && matches!(batch.requests[0].cause, engine::types::game_state::PendingBatchZoneChangeCause::Effect { source: id } if id == source))));
    (r, cards, source)
}

fn choose_entry(r: &mut GameRunner, expected: PlayerId, name: &str) -> Vec<GameEvent> {
    use engine::ai_support::legal_actions;
    assert!(
        matches!(r.state().waiting_for, WaitingFor::NamedChoice { player, .. } if player == expected)
    );
    let action = GameAction::ChooseOption {
        choice: name.into(),
    };
    assert!(
        legal_actions(r.state()).contains(&action),
        "String action must be generated by the engine"
    );
    r.act(action).unwrap().events
}

fn finish_entry_pause(r: &mut GameRunner, cards: [ObjectId; 2]) -> Vec<GameEvent> {
    let mut events = choose_entry(r, P0, "Goblin");
    assert_eq!(
        r.life(P0),
        20,
        "tail cannot run before the final member choice"
    );
    assert_eq!(r.state().objects[&cards[0]].zone, Zone::Battlefield);
    assert!(r.state().resolution_stack.iter().any(|frame| matches!(frame, engine::types::resolution::ResolutionFrame::BatchDelivery(batch) if batch.remaining.is_empty() && batch.requests.is_empty() && batch.paused_current.is_some())));
    events.extend(choose_entry(r, P1, "Elf"));
    events.extend(drain_triggers(r));
    for (seat, card, kind) in [(P0, cards[0], "Goblin"), (P1, cards[1], "Elf")] {
        let object = &r.state().objects[&card];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(object.controller, seat);
        assert!(object.chosen_attributes.iter().any(|choice| matches!(choice, engine::types::ability::ChosenAttribute::CreatureType(value) if value == kind)));
    }
    assert_eq!(r.life(P0), 27);
    assert_eq!(gains(&events), vec![(P0, 7)]);
    for object in r.state().objects.values().filter(|object| {
        object.name == "Private Preserve depth zero" || object.name == "Private Preserve depth one"
    }) {
        assert_eq!(
            object.zone,
            Zone::Library,
            "deeper cards are never substituted"
        );
    }
    events
}

#[test]
fn revealed_original_queue_refuses_wrong_zone_and_new_birth() {
    for new_birth in [false, true] {
        let (mut r, cards, _) = entry_pause();
        let queued = cards[1];
        let pin = engine::types::identifiers::ObjectIncarnationRef::from_object(
            &r.state().objects[&queued],
        );
        engine::game::zones::move_to_zone(r.state_mut(), queued, Zone::Hand, &mut Vec::new());
        if new_birth {
            engine::game::zones::move_to_zone(
                r.state_mut(),
                queued,
                Zone::Library,
                &mut Vec::new(),
            );
        }
        assert!(!pin.is_current(r.state()));
        let mut events = choose_entry(&mut r, P0, "Goblin");
        events.extend(drain_triggers(&mut r));
        assert_eq!(r.state().objects[&cards[0]].zone, Zone::Battlefield);
        assert_eq!(
            r.state().objects[&queued].zone,
            if new_birth { Zone::Library } else { Zone::Hand }
        );
        assert_eq!(r.life(P0), 27);
        assert!(!events.iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == queued)));
    }
    let (mut positive, cards, _) = entry_pause();
    finish_entry_pause(&mut positive, cards);
}

fn current_pause_loaders(
    state: &engine::types::game_state::GameState,
) -> Vec<engine::types::game_state::GameState> {
    use engine::types::game_state::{GameState, PersistedGameState, TrustedGameStateEnvelope};
    use engine::types::resolution::ResolutionStateWire;
    let raw = serde_json::to_value(state).unwrap();
    let bytes = serde_json::to_vec(state).unwrap();
    let string = serde_json::to_string(state).unwrap();
    let wire = ResolutionStateWire::from_game_state(state.clone());
    let trusted = TrustedGameStateEnvelope::capture(state.clone());
    let persisted_raw = PersistedGameState::Raw(Box::new(state.clone()));
    let persisted_trusted = PersistedGameState::capture(state.clone());
    vec![
        serde_json::from_value::<GameState>(raw.clone()).unwrap(),
        serde_json::from_slice::<GameState>(&bytes).unwrap(),
        serde_json::from_str::<GameState>(&string).unwrap(),
        serde_json::from_value::<PersistedGameState>(raw)
            .unwrap()
            .into_game_state()
            .unwrap(),
        serde_json::from_slice::<PersistedGameState>(&bytes)
            .unwrap()
            .into_game_state()
            .unwrap(),
        serde_json::from_str::<PersistedGameState>(&string)
            .unwrap()
            .into_game_state()
            .unwrap(),
        serde_json::from_value::<ResolutionStateWire>(serde_json::to_value(&wire).unwrap())
            .unwrap()
            .into_game_state(),
        serde_json::from_slice::<ResolutionStateWire>(&serde_json::to_vec(&wire).unwrap())
            .unwrap()
            .into_game_state(),
        serde_json::from_str::<ResolutionStateWire>(&serde_json::to_string(&wire).unwrap())
            .unwrap()
            .into_game_state(),
        serde_json::from_value::<TrustedGameStateEnvelope>(serde_json::to_value(&trusted).unwrap())
            .unwrap()
            .into_game_state(),
        serde_json::from_slice::<TrustedGameStateEnvelope>(&serde_json::to_vec(&trusted).unwrap())
            .unwrap()
            .into_game_state(),
        serde_json::from_str::<TrustedGameStateEnvelope>(&serde_json::to_string(&trusted).unwrap())
            .unwrap()
            .into_game_state(),
        serde_json::from_value::<PersistedGameState>(serde_json::to_value(&persisted_raw).unwrap())
            .unwrap()
            .into_game_state()
            .unwrap(),
        serde_json::from_slice::<PersistedGameState>(&serde_json::to_vec(&persisted_raw).unwrap())
            .unwrap()
            .into_game_state()
            .unwrap(),
        serde_json::from_str::<PersistedGameState>(&serde_json::to_string(&persisted_raw).unwrap())
            .unwrap()
            .into_game_state()
            .unwrap(),
        serde_json::from_value::<PersistedGameState>(
            serde_json::to_value(&persisted_trusted).unwrap(),
        )
        .unwrap()
        .into_game_state()
        .unwrap(),
        serde_json::from_slice::<PersistedGameState>(
            &serde_json::to_vec(&persisted_trusted).unwrap(),
        )
        .unwrap()
        .into_game_state()
        .unwrap(),
        serde_json::from_str::<PersistedGameState>(
            &serde_json::to_string(&persisted_trusted).unwrap(),
        )
        .unwrap()
        .into_game_state()
        .unwrap(),
    ]
}

#[test]
fn revealed_owner_batch_current_loaders_finish_first_and_final_pauses() {
    let (r, cards, _) = entry_pause();
    for loaded in current_pause_loaders(r.state()) {
        assert_eq!(loaded.return_result_frames, r.state().return_result_frames);
        let origins = |state: &engine::types::game_state::GameState| {
            state
                .resolution_stack
                .iter()
                .filter_map(|frame| match frame {
                    engine::types::resolution::ResolutionFrame::BatchDelivery(batch) => Some(
                        batch
                            .requests
                            .iter()
                            .map(|request| {
                                (
                                    request.object_id,
                                    request.expected_origin,
                                    request.performed_by,
                                    request.controller_override,
                                )
                            })
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(origins(&loaded), origins(r.state()));
        let mut loaded = GameRunner::from_state(loaded);
        finish_entry_pause(&mut loaded, cards);
    }
    let mut r = r;
    choose_entry(&mut r, P0, "Goblin");
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::NamedChoice { player: P1, .. }
    ));
    for loaded in current_pause_loaders(r.state()) {
        let mut loaded = GameRunner::from_state(loaded);
        let mut events = choose_entry(&mut loaded, P1, "Elf");
        events.extend(drain_triggers(&mut loaded));
        assert_eq!(loaded.life(P0), 27);
        assert_eq!(gains(&events), vec![(P0, 7)]);
        for card in cards {
            assert_eq!(loaded.state().objects[&card].zone, Zone::Battlefield);
        }
    }
}

#[test]
fn revealed_pause_malformed_current_cannot_be_laundered_as_legacy() {
    use engine::types::game_state::{GameState, PersistedGameState};
    use engine::types::resolution::ResolutionStateWire;
    let (r, _, _) = entry_pause();
    for raw in [false, true] {
        let original = if raw {
            serde_json::to_value(r.state()).unwrap()
        } else {
            serde_json::to_value(ResolutionStateWire::from_game_state(r.state().clone())).unwrap()
        };
        for fault in 0..5 {
            let mut bad = original.clone();
            let carrier = if raw {
                "resolution_stack"
            } else {
                "resolution_frames"
            };
            let batch = bad[carrier]["frames"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|frame| frame["type"] == "BatchDelivery")
                .unwrap();
            match fault {
                0 => {
                    batch["data"]["requests"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("expected_origin");
                }
                1 => {
                    batch["data"].as_object_mut().unwrap().remove("requests");
                }
                2 => {
                    batch["data"]["requests"][0]["expected_origin"] =
                        serde_json::json!([null, "Library"]);
                }
                3 => {
                    batch["data"]["requests"][0]["object_id"] = serde_json::json!(999999);
                }
                4 => {
                    bad["resolution_state_version"] = serde_json::json!(6);
                }
                _ => unreachable!(),
            }
            if raw && fault != 4 {
                assert!(
                    serde_json::from_value::<GameState>(bad.clone()).is_err(),
                    "direct current fault {fault}"
                );
            }
            assert!(
                serde_json::from_value::<PersistedGameState>(bad).is_err(),
                "persisted raw={raw} fault={fault}"
            );
        }
    }
}

#[test]
fn revealed_result_pause_private_views_and_legacy_zone_readers() {
    for final_member in [false, true] {
        let (mut r, cards, _) = entry_pause();
        if final_member {
            choose_entry(&mut r, P0, "Goblin");
        }
        let mut private = r.state().clone();
        let canary = 987654321987654321u64;
        let result = private
            .return_result_frames
            .values_mut()
            .flat_map(|frame| frame.results.values_mut())
            .find_map(|result| match result {
                engine::types::game_state::InstructionResult::RevealedCards(result) => Some(result),
                _ => None,
            })
            .expect("actual populated revealed carrier");
        assert_eq!(result.revealed_cards.len(), 2);
        result.revealed_cards[0].incarnation = canary;
        assert!(serde_json::to_string(&private)
            .unwrap()
            .contains(&canary.to_string()));
        for viewer in [Some(P0), Some(P1), None] {
            let filtered = viewer.map_or_else(
                || filter_state_for_unseated_viewer(&private),
                |seat| filter_state_for_viewer(&private, seat),
            );
            assert!(filtered.return_result_frames.is_empty());
            assert!(filtered.resolution_stack.is_empty());
            assert!(matches!(
                filtered.waiting_for,
                WaitingFor::NamedChoice { .. }
            ));
            assert!(
                !filtered.revealed_cards.is_empty(),
                "legitimate public reveal survives"
            );
            assert_eq!(filtered.objects[&cards[1]].name, "Adaptive Automaton");
            for wire in [
                serde_json::to_string(&filtered).unwrap(),
                serde_json::to_string(&ClientGameStateRef::wrap(&private, viewer)).unwrap(),
                serde_json::to_string(&ClientGameStateRef::wrap_filtered(
                    &private, &filtered, viewer,
                ))
                .unwrap(),
            ] {
                assert!(!wire.contains(&canary.to_string()));
                assert!(!wire.contains("expected_origin"));
            }
        }
        assert!(serde_json::to_string(&private)
            .unwrap()
            .contains(&canary.to_string()));
    }
}

#[test]
fn revealed_exact_producers_and_latched_queue_ignore_later_mirrors() {
    for mixed in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let decoy = s.add_card_to_library_top(P0, "Earlier reveal then mill");
        let zero = creature_top(&mut s, P0, "Second exact zero", "");
        let one = if mixed {
            s.add_land_to_library_top(P1, "Second exact land").id()
        } else {
            creature_top(&mut s, P1, "Second exact one", "")
        };
        let source = spell(&mut s, P0, &format!("Reveal the top card of your library. You mill a card. {ALL_CREATURES} You gain 7 life."));
        let mut r = s.build();
        let single = [zero];
        let both = [zero, one];
        seat_creature_tops(&mut r, if mixed { &single[..] } else { &both[..] });
        let library = r.state_mut().library_of_mut(P0);
        library.retain(|id| *id != decoy);
        library.push_front(decoy);
        let out = r.cast(source).resolve();
        assert_eq!(
            reveals(out.events()),
            vec![(P0, vec![decoy]), (P0, vec![zero]), (P1, vec![one])]
        );
        out.assert_zone(&[decoy], Zone::Graveyard);
        out.assert_zone(
            &[zero, one],
            if mixed {
                Zone::Library
            } else {
                Zone::Battlefield
            },
        );
        out.assert_life_delta(P0, 7);
    }
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let cards: Vec<_> = [P0, P1]
        .into_iter()
        .flat_map(|seat| (0..2).map(move |_| seat))
        .map(|seat| creature_top(&mut s, seat, "Successive creature", ""))
        .collect();
    let source = spell(
        &mut s,
        P0,
        &format!("{ALL_CREATURES} {ALL_CREATURES} You gain 7 life."),
    );
    let mut r = s.build();
    seat_creature_tops(&mut r, &cards);
    let out = r.cast(source).resolve();
    out.assert_zone(&cards, Zone::Battlefield);
    assert_eq!(reveals(out.events()).len(), 4);
    out.assert_life_delta(P0, 7);
    let (mut r, cards, _) = entry_pause();
    let already_bound = r.state().resolution_stack.clone();
    let decoy = r
        .state()
        .objects
        .values()
        .find(|object| object.name == "Private Preserve depth zero")
        .unwrap()
        .id;
    r.state_mut().last_revealed_ids = vec![decoy];
    r.state_mut().tracked_object_sets.clear();
    let set = engine::types::identifiers::TrackedSetId(r.state().next_tracked_set_id);
    r.state_mut().next_tracked_set_id += 1;
    r.state_mut().tracked_object_sets.insert(set, vec![decoy]);
    for result in r
        .state_mut()
        .return_result_frames
        .values_mut()
        .flat_map(|frame| frame.results.values_mut())
    {
        *result = engine::types::game_state::InstructionResult::ZoneChanges(Vec::new());
    }
    assert_eq!(r.state().resolution_stack, already_bound);
    finish_entry_pause(&mut r, cards);
}

#[test]
fn reveal_result_occurrences_and_classifiers_are_scoped() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let cards = [
        creature_top(&mut s, P0, "Only zero", ""),
        creature_top(&mut s, P1, "Only one", ""),
    ];
    let source = spell(
        &mut s,
        P0,
        &format!("{ALL_CREATURES} You gain 7 life. Repeat this process one more time."),
    );
    let mut r = s.build();
    seat_creature_tops(&mut r, &cards);
    let out = r.cast(source).resolve();
    out.assert_zone(&cards, Zone::Battlefield);
    out.assert_life_delta(P0, 14);
    assert_eq!(reveals(out.events()).len(), 2);
    assert_eq!(
        out.events()
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChangeZone,
                    ..
                }
            ))
            .count(),
        2,
        "later iteration consumes a fresh present empty"
    );
    assert!(out.state().return_result_frames.is_empty());

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = preserve_in_hand(&mut s);
    let b = preserve_in_hand(&mut s);
    let response = spell(&mut s, P0, "Destroy target enchantment.");
    let cards: Vec<_> = [P0, P1]
        .into_iter()
        .flat_map(|seat| (0..2).map(move |_| seat))
        .map(|seat| creature_top(&mut s, seat, "Successive source creature", ""))
        .collect();
    let mut r = s.build();
    seat_creature_tops(&mut r, &cards);
    r.cast(a)
        .free_cast()
        .resolve()
        .assert_zone(&[a], Zone::Battlefield);
    r.cast(b)
        .free_cast()
        .resolve()
        .assert_zone(&[b], Zone::Battlefield);
    upkeep(&mut r);
    if let WaitingFor::OrderTriggers { ref triggers, .. } = r.state().waiting_for {
        assert_eq!(triggers.len(), 2);
        r.act(GameAction::OrderTriggers { order: vec![0, 1] })
            .unwrap();
    }
    assert_eq!(r.state().stack.len(), 2);
    let out = r.cast(response).target_object(a).resolve();
    out.assert_zone(&[a], Zone::Graveyard);
    out.assert_zone(&cards, Zone::Battlefield);
    assert_eq!(reveals(out.events()).len(), 4);
    assert!(out.state().return_result_frames.is_empty());
}

#[test]
fn revealed_repeat_pause_keeps_iteration_authority_and_replaces_membership() {
    let (r, cards, _) = entry_pause_for_text(&format!(
        "{ALL_CREATURES} You gain 7 life. Repeat this process one more time."
    ));
    assert!(r.state().resolution_stack.iter().any(|frame| matches!(
        frame,
        engine::types::resolution::ResolutionFrame::RepeatFor(_)
    )));
    let stamps: Vec<_> = r
        .state()
        .resolution_stack
        .iter()
        .filter_map(|frame| match frame {
            engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) => {
                frame.pending.return_result_occurrence
            }
            _ => None,
        })
        .collect();
    assert!(!stamps.is_empty());
    assert!(stamps
        .iter()
        .all(|id| r.state().return_result_frames.contains_key(id)));
    for loaded in current_pause_loaders(r.state()) {
        let mut loaded = GameRunner::from_state(loaded);
        choose_entry(&mut loaded, P0, "Goblin");
        let mut events = choose_entry(&mut loaded, P1, "Elf");
        events.extend(drain_triggers(&mut loaded));
        assert_eq!(
            loaded.life(P0),
            34,
            "both complete iterations run their own once-only tail"
        );
        for card in cards {
            assert_eq!(loaded.state().objects[&card].zone, Zone::Battlefield);
        }
        assert_eq!(gains(&events), vec![(P0, 7), (P0, 7)]);
        assert_eq!(
            reveals(&events).len(),
            2,
            "the next iteration reveals fresh noncreature deeper members"
        );
        assert!(loaded.state().return_result_frames.is_empty());
    }
}

#[test]
fn revealed_owner_entries_pause_for_first_and_final_member_then_tail_once() {
    let (mut r, cards, source) = entry_pause();
    let events = finish_entry_pause(&mut r, cards);
    for card in cards {
        assert_eq!(
            r.state().objects[&card].entered_via_ability_source,
            Some(source)
        );
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == card)).count(), 1);
    }
}

#[test]
fn revealed_owner_entry_respects_normal_cant_enter() {
    for blocked in [false, true] {
        for printed in [false, true] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            if blocked {
                s.add_artifact_from_oracle(P0, "Grafdigger's Cage", "Creature cards in graveyards and libraries can't enter the battlefield.\nPlayers can't cast spells from graveyards or libraries.");
            }
            let cards = [
                creature_top(&mut s, P0, "Zero", ""),
                creature_top(&mut s, P1, "One", ""),
            ];
            let source = if printed {
                preserve_in_hand(&mut s)
            } else {
                spell(&mut s, P0, &format!("{ALL_CREATURES} You gain 7 life."))
            };
            let mut r = s.build();
            seat_creature_tops(&mut r, &cards);
            let events = if printed {
                r.cast(source)
                    .resolve()
                    .assert_zone(&[source], Zone::Battlefield);
                upkeep(&mut r);
                drain_triggers(&mut r)
            } else {
                r.cast(source).resolve().events().to_vec()
            };
            assert_eq!(reveals(&events).len(), 2);
            for card in cards {
                assert_eq!(
                    r.state().objects[&card].zone,
                    if blocked {
                        Zone::Library
                    } else {
                        Zone::Battlefield
                    }
                );
            }
            assert_eq!(r.life(P0), if printed { 20 } else { 27 });
        }
    }
}

fn supported_chain(def: &AbilityDefinition) {
    assert!(
        !matches!(*def.effect, Effect::Unimplemented { .. }),
        "{def:#?}"
    );
    if let Some(sub) = &def.sub_ability {
        supported_chain(sub);
    }
    if let Some(other) = &def.else_ability {
        supported_chain(other);
    }
    for mode in &def.mode_abilities {
        supported_chain(mode);
    }
}

fn assert_supported(text: &str, name: &str, types: &[&str], keywords: &[&str]) {
    let parsed = parse_oracle_text(
        text,
        name,
        &keywords.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &types.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &[],
    );
    assert!(parsed.parse_warnings.is_empty(), "{parsed:#?}");
    assert!(
        !parsed.abilities.is_empty() || !parsed.triggers.is_empty(),
        "{parsed:#?}"
    );
    for def in &parsed.abilities {
        supported_chain(def);
    }
    for trigger in &parsed.triggers {
        supported_chain(trigger.execute.as_deref().expect("trigger body"));
    }
}

fn spell(scenario: &mut GameScenario, player: PlayerId, text: &str) -> ObjectId {
    assert_supported(text, "Synthetic aggregate", &["Instant"], &[]);
    scenario
        .add_spell_to_hand_from_oracle(player, "Synthetic aggregate", true, text)
        .id()
}

fn reveals(events: &[GameEvent]) -> Vec<(PlayerId, Vec<ObjectId>)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CardsRevealed {
                player, card_ids, ..
            } => Some((*player, card_ids.clone())),
            _ => None,
        })
        .collect()
}

fn gains(events: &[GameEvent]) -> Vec<(PlayerId, i32)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::LifeChanged {
                player_id, amount, ..
            } if *amount > 0 => Some((*player_id, *amount)),
            _ => None,
        })
        .collect()
}

fn assert_no_library_moves(events: &[GameEvent], ids: &[ObjectId]) {
    assert!(!events
        .iter()
        .any(|e| matches!(e, GameEvent::ZoneChanged { object_id, .. } if ids.contains(object_id))));
}

#[test]
fn ordinary_reveal_scalar_cast_counts_complete_instruction() {
    for caster in [P0, P1] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.with_library_top(P0, &["Public zero", "Secret zero"]);
        s.with_library_top(P1, &["Public one", "Secret one"]);
        let id = spell(&mut s, caster, SCALAR);
        let mut r = s.build();
        r.state_mut().active_player = caster;
        r.state_mut().priority_player = caster;
        r.state_mut().waiting_for = WaitingFor::Priority { player: caster };
        let libraries: Vec<_> = [P0, P1]
            .iter()
            .map(|&p| r.state().library_of(p).iter().copied().collect::<Vec<_>>())
            .collect();
        let out = r.cast(id).resolve();
        out.assert_life_delta(caster, 2);
        out.assert_life_delta(if caster == P0 { P1 } else { P0 }, 0);
        let actual = reveals(out.events());
        assert_eq!(actual.len(), 2);
        for (p, library) in [P0, P1].into_iter().zip(&libraries) {
            assert!(actual.contains(&(p, vec![library[0]])));
            assert_eq!(
                out.state()
                    .library_of(p)
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                *library
            );
            out.assert_zone(library, Zone::Library);
        }
        assert_eq!(gains(out.events()), vec![(caster, 2)]);
        assert_no_library_moves(out.events(), &libraries.concat());
    }
}

#[test]
fn ordinary_reveal_tail_runs_after_all_seats_once() {
    for empty_first in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        if !empty_first {
            s.with_library_top(P0, &["Zero top"]);
        }
        s.with_library_top(P1, &["One top"]);
        let id = spell(&mut s, P0, &format!("{SCALAR} You gain 3 life."));
        let mut r = s.build();
        let out = r.cast(id).resolve();
        let count = if empty_first { 1 } else { 2 };
        out.assert_life_delta(P0, count + 3);
        out.assert_life_delta(P1, 0);
        assert_eq!(reveals(out.events()).len(), count as usize);
        assert_eq!(gains(out.events()), vec![(P0, count), (P0, 3)]);
        let last_reveal = out
            .events()
            .iter()
            .rposition(|e| matches!(e, GameEvent::CardsRevealed { .. }))
            .unwrap();
        let first_gain = out
            .events()
            .iter()
            .position(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount > 0))
            .unwrap();
        assert!(last_reveal < first_gain);
    }
}

#[test]
fn ordinary_reveal_empty_seats_publish_fresh_empty() {
    for empty in 0..3 {
        let mut s = GameScenario::new_n_player(3, 42);
        s.at_phase(Phase::PreCombatMain);
        for seat in 0..3 {
            if seat != empty {
                s.with_library_top(PlayerId(seat), &["Nonempty seat"]);
            }
        }
        let id = spell(&mut s, P0, SCALAR);
        let mut r = s.build();
        let expected: Vec<_> = (0..3)
            .filter(|&seat| seat != empty)
            .map(|seat| {
                (
                    PlayerId(seat),
                    r.state()
                        .library_of(PlayerId(seat))
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let out = r.cast(id).resolve();
        out.assert_life_delta(P0, 2);
        assert_eq!(reveals(out.events()), expected);
    }
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let decoy = s.add_card_to_library_top(P0, "Earlier real reveal");
    let id = spell(
        &mut s,
        P0,
        &format!("Reveal the top card of your library. You mill a card. {SCALAR} You gain 3 life."),
    );
    let mut r = s.build();
    let out = r.cast(id).resolve();
    assert_eq!(reveals(out.events()), vec![(P0, vec![decoy])]);
    out.assert_zone(&[decoy], Zone::Graveyard);
    assert!(out
        .events()
        .iter()
        .any(|e| matches!(e, GameEvent::ZoneChanged { object_id, .. } if *object_id == decoy)));
    out.assert_life_delta(P0, 3);
    assert!(out.state().last_revealed_ids.is_empty());
    assert!(out
        .state()
        .tracked_object_sets
        .iter()
        .max_by_key(|(id, _)| id.0)
        .unwrap()
        .1
        .is_empty());
    assert_eq!(
        gains(out.events()).iter().filter(|(_, n)| *n == 3).count(),
        1
    );
}

#[test]
fn ordinary_reveal_replaces_previous_immediate_result() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Decoy", "Current zero", "Secret zero"]);
    s.with_library_top(P1, &["Current one", "Secret one"]);
    let id = spell(
        &mut s,
        P0,
        &format!("Reveal the top card of your library. You mill a card. {SCALAR}"),
    );
    let mut r = s.build();
    let a = r.state().library_of(P0).iter().copied().collect::<Vec<_>>();
    let b = r.state().library_of(P1).iter().copied().collect::<Vec<_>>();
    let out = r.cast(id).resolve();
    assert_eq!(
        reveals(out.events()),
        vec![(P0, vec![a[0]]), (P0, vec![a[1]]), (P1, vec![b[0]])]
    );
    out.assert_zone(&[a[0]], Zone::Graveyard);
    out.assert_life_delta(P0, 2);
    assert_eq!(out.state().last_revealed_ids, vec![a[1], b[0]]);
    assert_eq!(
        out.state()
            .tracked_object_sets
            .iter()
            .max_by_key(|(id, _)| id.0)
            .unwrap()
            .1,
        &vec![a[1], b[0]]
    );
}

#[test]
fn ordinary_reveal_count_is_cards_not_seats() {
    for second_count in [0, 1, 2] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.with_library_top(P0, &["Zero first", "Zero second", "Zero secret"]);
        let names = ["One first", "One second"];
        s.with_library_top(P1, &names[..second_count]);
        let id = spell(&mut s, P0, TOP_TWO);
        let mut r = s.build();
        let a = r.state().library_of(P0).iter().copied().collect::<Vec<_>>();
        let b = r.state().library_of(P1).iter().copied().collect::<Vec<_>>();
        let out = r.cast(id).resolve();
        out.assert_life_delta(P0, 2 + second_count as i32);
        assert!(reveals(out.events()).contains(&(P0, a[..2].to_vec())));
        if !b.is_empty() {
            assert!(reveals(out.events()).contains(&(P1, b.clone())));
        }
        assert_eq!(
            out.state()
                .library_of(P0)
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            a
        );
        assert_eq!(
            out.state()
                .library_of(P1)
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            b
        );
        assert!(!reveals(out.events())
            .iter()
            .any(|(_, cards)| cards.contains(&a[2])));
        assert_no_library_moves(out.events(), &[a, b].concat());
    }
    let mut s = GameScenario::new_with_format(FormatConfig::dandan(), 2, 42);
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Shared first", "Shared second", "Shared secret"]);
    let id = spell(&mut s, P0, TOP_TWO);
    let mut r = s.build();
    let top = r.state().library_of(P0).iter().copied().collect::<Vec<_>>()[..2].to_vec();
    assert_eq!(
        r.state().library_of(P1).iter().copied().collect::<Vec<_>>()[..2],
        top
    );
    let out = r.cast(id).resolve();
    assert_eq!(
        reveals(out.events()),
        vec![(P0, top.clone()), (P1, top.clone())]
    );
    out.assert_life_delta(P0, 2);
    assert_eq!(out.state().last_revealed_ids, top);
}

#[test]
fn ordinary_scoped_reveal_unless_payment_uses_normal_choices() {
    for p0_pays in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.with_library_top(P0, &["Zero top", "Zero secret"]);
        s.with_library_top(P1, &["One top", "One secret"]);
        let id = spell(&mut s, P0, PAYMENT);
        let mut r = s.build();
        let libraries: Vec<_> = [P0, P1]
            .iter()
            .map(|&p| r.state().library_of(p).iter().copied().collect::<Vec<_>>())
            .collect();
        let mut cast = r.cast(id).commit();
        assert!(!cast.state().stack.is_empty(), "committed public spell");
        let mut events = Vec::new();
        for (expected, pay) in [(P0, p0_pays), (P1, !p0_pays)] {
            for _ in 0..32 {
                if matches!(cast.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
                    break;
                }
                assert!(matches!(
                    cast.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                events.extend(cast.act(GameAction::PassPriority).unwrap().events);
            }
            let WaitingFor::UnlessPayment {
                player,
                ref cost,
                ref pending_effect,
                ..
            } = cast.state().waiting_for
            else {
                panic!("natural payment prompt: {:?}", cast.state().waiting_for)
            };
            assert_eq!(player, expected);
            assert!(matches!(
                pending_effect.effect,
                Effect::RevealTop { count: 1, .. }
            ));
            assert_eq!(pending_effect.controller, expected);
            assert_eq!(
                *cost,
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 }
                }
            );
            events.extend(cast.act(GameAction::PayUnlessCost { pay }).unwrap().events);
        }
        let out = cast.resolve();
        events.extend_from_slice(out.events());
        out.assert_life_delta(P0, if p0_pays { 2 } else { 3 });
        out.assert_life_delta(P1, if p0_pays { 0 } else { -1 });
        let decliner = if p0_pays { P1 } else { P0 };
        assert_eq!(
            reveals(&events),
            vec![(decliner, vec![libraries[decliner.0 as usize][0]])]
        );
        assert_eq!(
            gains(&events)
                .iter()
                .filter(|(_, amount)| *amount == 3)
                .count(),
            1
        );
        for p in [P0, P1] {
            assert_eq!(
                out.state()
                    .library_of(p)
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                libraries[p.0 as usize]
            );
        }
        assert_no_library_moves(&events, &libraries.concat());
        assert!(events.iter().any(|event| matches!(event, GameEvent::LifeChanged { player_id, amount: -1, .. } if *player_id == if p0_pays { P0 } else { P1 })));
        let reveal_index = events
            .iter()
            .position(|event| matches!(event, GameEvent::CardsRevealed { .. }))
            .unwrap();
        let tail_index = events
            .iter()
            .position(|event| matches!(event, GameEvent::LifeChanged { amount: 3, .. }))
            .unwrap();
        assert!(reveal_index < tail_index);
    }
}

#[test]
fn ordinary_reveal_context_authority_ignores_inherited_player_target() {
    let text = "Target player gains 1 life. Reveal the top card of your library. You gain life equal to the number of cards revealed this way.";
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Controller top", "Controller secret"]);
    s.with_library_top(P1, &["Target top", "Target secret"]);
    let id = spell(&mut s, P0, text);
    let mut r = s.build();
    let a = r.state().library_of(P0).iter().copied().collect::<Vec<_>>();
    let b = r.state().library_of(P1).iter().copied().collect::<Vec<_>>();
    let out = r.cast(id).target_player(P1).resolve();
    out.assert_life_delta(P1, 1);
    out.assert_life_delta(P0, 1);
    assert_eq!(reveals(out.events()), vec![(P0, vec![a[0]])]);
    assert_eq!(
        out.state()
            .library_of(P1)
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        b
    );
}

#[test]
fn ordinary_reveal_repetition_nested_and_roots_are_fresh() {
    let text = format!(
        "{SCALAR} Each player mills a card. You gain 3 life. Repeat this process one more time."
    );
    let parsed = parse_oracle_text(&text, "Synthetic aggregate", &[], &["Instant".into()], &[]);
    assert_eq!(
        parsed.abilities[0].repeat_for,
        Some(QuantityExpr::Fixed { value: 2 }),
        "real counted process"
    );
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["First zero"]);
    s.with_library_top(P1, &["First one"]);
    let id = spell(&mut s, P0, &text);
    let mut r = s.build();
    let tops = [r.state().library_of(P0)[0], r.state().library_of(P1)[0]];
    let out = r.cast(id).resolve();
    out.assert_life_delta(P0, 8);
    assert_eq!(
        reveals(out.events()),
        vec![(P0, vec![tops[0]]), (P1, vec![tops[1]])]
    );
    out.assert_zone(&tops, Zone::Graveyard);
    assert_eq!(
        out.events()
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Reveal,
                    ..
                }
            ))
            .count(),
        2
    );
    assert_eq!(
        gains(out.events()).iter().filter(|(_, n)| *n == 3).count(),
        2
    );
    assert!(out.state().last_revealed_ids.is_empty());

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Mode zero"]);
    s.with_library_top(P1, &["Mode one"]);
    let mode = spell(
        &mut s,
        P0,
        &format!("Choose one —\n• {SCALAR}\n• You gain 7 life."),
    );
    let second = spell(
        &mut s,
        P0,
        &format!("Each player mills a card. {SCALAR} You gain 3 life."),
    );
    let mut r = s.build();
    let out = r.cast(mode).modes(&[0]).resolve();
    out.assert_life_delta(P0, 2);
    assert_eq!(reveals(out.events()).len(), 2);
    assert_eq!(gains(out.events()), vec![(P0, 2)]);
    let out = r.cast(second).resolve();
    out.assert_life_delta(P0, 3);
    assert!(reveals(out.events()).is_empty());
    assert!(out.state().last_revealed_ids.is_empty());
}

#[test]
fn ordinary_reveal_public_events_and_private_views() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Public top zero", "Private depth zero canary"]);
    s.with_library_top(P1, &["Public top one", "Private depth one canary"]);
    let id = spell(&mut s, P0, SCALAR);
    let mut r = s.build();
    let libraries: Vec<_> = [P0, P1]
        .iter()
        .map(|&p| r.state().library_of(p).iter().copied().collect::<Vec<_>>())
        .collect();
    let out = r.cast(id).resolve();
    out.assert_life_delta(P0, 2);
    for (p, cards) in &reveals(out.events()) {
        assert_eq!(cards, &vec![libraries[p.0 as usize][0]]);
        assert!(out.events().iter().any(|e| matches!(e, GameEvent::CardsRevealed { player, card_names, .. } if player == p && card_names == &vec![format!("Public top {}", if *p == P0 { "zero" } else { "one" })])));
    }
    assert_eq!(reveals(out.events()).len(), 2);
    let before = out.state().last_revealed_ids.clone();
    for viewer in [Some(P0), Some(P1), None] {
        let projection = match viewer {
            Some(p) => filter_state_for_viewer(out.state(), p),
            None => filter_state_for_unseated_viewer(out.state()),
        };
        let wire = serde_json::to_string(&ClientGameStateRef::wrap(out.state(), viewer)).unwrap();
        for (seat, canary) in ["Private depth zero canary", "Private depth one canary"]
            .iter()
            .enumerate()
        {
            let secret = libraries[seat][1];
            assert_eq!(out.state().objects[&secret].name, *canary);
            assert_eq!(projection.objects[&secret].name, "Hidden Card");
            assert!(!wire.contains(canary));
            assert!(!projection
                .library_of(PlayerId(seat as u8))
                .contains(&secret));
        }
        assert_eq!(projection.players[0].life, 22);
    }
    assert_eq!(out.state().last_revealed_ids, before);
    assert_no_library_moves(out.events(), &libraries.concat());
}

fn printed_permanent(s: &mut GameScenario, name: &str, text: &str, mv: u32) -> ObjectId {
    let creature = name != "Parker Luck";
    let keywords: &[&str] = if name == "Duskmantle Seer" {
        &["Flying"]
    } else {
        &[]
    };
    assert_supported(
        text,
        name,
        &[if creature { "Creature" } else { "Enchantment" }],
        keywords,
    );
    let mut card = if creature {
        s.add_creature_to_hand(
            P0,
            name,
            if mv == 4 { 4 } else { 2 },
            if mv == 4 { 4 } else { 1 },
        )
    } else {
        let mut card = s.add_spell_to_hand(P0, name, false);
        card.as_enchantment();
        card
    };
    card.with_mana_cost(ManaCost::generic(mv))
        .from_oracle_text_with_keywords(keywords, text)
        .id()
}

fn upkeep(r: &mut GameRunner) {
    r.state_mut().phase = Phase::Untap;
    r.state_mut().active_player = P0;
    r.state_mut().priority_player = P0;
    r.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    r.advance_to_upkeep();
}

fn drain_triggers(r: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        let action = match &r.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return events,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected reveal sibling prompt: {other:?}"),
        };
        events.extend(r.act(action).unwrap().events);
    }
    panic!("reveal sibling did not finish before the draw step");
}

#[test]
fn reveal_sibling_authorities_remain_distinct() {
    for (name, text, source_mv) in [
        ("Duskmantle Seer", SEER, 4),
        ("Parker Luck", PARKER, 3),
        ("Dark Confidant", CONFIDANT, 2),
    ] {
        for present in [[true, true], [false, true], [true, false], [false, false]] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let source = printed_permanent(&mut s, name, text, source_mv);
            let mut tops = [None, None];
            for (seat, mv) in [(P0, if name == "Dark Confidant" { 3 } else { 1 }), (P1, 2)] {
                if present[seat.0 as usize] {
                    tops[seat.0 as usize] = Some(
                        s.add_spell_to_library_top(
                            seat,
                            if seat == P0 { "Zero MV" } else { "One MV" },
                            false,
                        )
                        .with_mana_cost(ManaCost::generic(mv))
                        .id(),
                    );
                }
            }
            let mut r = s.build();
            let out = r.cast(source).free_cast().resolve();
            out.assert_zone(&[source], Zone::Battlefield);
            if name == "Parker Luck" {
                r.advance_to_end_step();
                assert!(matches!(
                    r.state().waiting_for,
                    WaitingFor::TriggerTargetSelection { .. }
                ));
                for player in [P0, P1] {
                    r.act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(player)),
                    })
                    .unwrap();
                }
            } else {
                upkeep(&mut r);
            }
            assert!(
                !r.state().stack.is_empty(),
                "natural printed trigger reached the stack"
            );
            let events = drain_triggers(&mut r);
            let expected_life = match name {
                "Duskmantle Seer" => (20 - i32::from(present[0]), 20 - 2 * i32::from(present[1])),
                "Parker Luck" => (20 - 2 * i32::from(present[1]), 20 - i32::from(present[0])),
                "Dark Confidant" => (20 - 3 * i32::from(present[0]), 20),
                _ => unreachable!(),
            };
            assert_eq!(
                (r.life(P0), r.life(P1)),
                expected_life,
                "{name} {present:?}"
            );
            let expected_reveals: Vec<_> = [P0, P1]
                .into_iter()
                .filter(|&p| name != "Dark Confidant" || p == P0)
                .filter_map(|p| tops[p.0 as usize].map(|id| (p, vec![id])))
                .collect();
            assert_eq!(reveals(&events), expected_reveals, "{name} {present:?}");
            for (seat, top) in [P0, P1].into_iter().zip(tops) {
                if let Some(top) = top {
                    let moved = name != "Dark Confidant" || seat == P0;
                    assert_eq!(
                        r.state().objects[&top].zone,
                        if moved { Zone::Hand } else { Zone::Library }
                    );
                    if moved {
                        assert!(r.state().players[seat.0 as usize].hand.contains(&top));
                    }
                }
            }
            assert_eq!(r.state().objects[&source].zone, Zone::Battlefield);
        }
    }
}

#[test]
fn ordinary_reveal_multiple_trigger_roots_survive_source_departure() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = printed_permanent(&mut s, "Duskmantle Seer", SEER, 4);
    let b = printed_permanent(&mut s, "Duskmantle Seer", SEER, 4);
    let response = spell(&mut s, P0, "Destroy target creature.");
    let mut tops = [Vec::new(), Vec::new()];
    for (seat, values) in [(P0, [1, 3]), (P1, [2, 4])] {
        for mv in values.into_iter().rev() {
            let id = s
                .add_spell_to_library_top(seat, "Successive top", false)
                .with_mana_cost(ManaCost::generic(mv))
                .id();
            tops[seat.0 as usize].insert(0, id);
        }
    }
    let mut r = s.build();
    r.cast(a)
        .free_cast()
        .resolve()
        .assert_zone(&[a], Zone::Battlefield);
    r.cast(b)
        .free_cast()
        .resolve()
        .assert_zone(&[b], Zone::Battlefield);
    upkeep(&mut r);
    if let WaitingFor::OrderTriggers { ref triggers, .. } = r.state().waiting_for {
        let count = triggers.len();
        assert_eq!(count, 2);
        r.act(GameAction::OrderTriggers {
            order: (0..count).collect(),
        })
        .unwrap();
    }
    assert_eq!(r.state().stack.len(), 2);
    assert!(r.state().stack.iter().any(|entry| entry.source_id == a));
    assert!(r.state().stack.iter().any(|entry| entry.source_id == b));
    // The response cast resolves through the public stack; its outcome also
    // includes the already-created upkeep abilities underneath it.
    let out = r.cast(response).target_object(a).resolve();
    out.assert_zone(&[a], Zone::Graveyard);
    out.assert_zone(&[b], Zone::Battlefield);
    out.assert_life_delta(P0, -4);
    out.assert_life_delta(P1, -6);
    assert_eq!(
        reveals(out.events()),
        vec![
            (P0, vec![tops[0][0]]),
            (P1, vec![tops[1][0]]),
            (P0, vec![tops[0][1]]),
            (P1, vec![tops[1][1]])
        ]
    );
    for (seat, ids) in [P0, P1].into_iter().zip(tops) {
        out.assert_zone(&ids, Zone::Hand);
        for id in ids {
            assert!(out.state().players[seat.0 as usize].hand.contains(&id));
        }
    }
}
