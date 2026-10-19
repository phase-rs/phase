//! U14: a complete synthetic ordinary permanent with a durable parity reader.
//! Full Ashling's external complement-entry consumer belongs to phase 15.

use engine::game::derived_views::ClientGameStateRef;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::{
    ChoiceType, ChoiceValue, ChosenAttribute, ContinuousModification, Effect, FilterProp, Parity,
    ParitySource, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, NamedChoiceSourceBinding, PersistedGameState, PersistedRestoreFinalization,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::{ResolutionStateWire, RESOLUTION_STATE_WIRE_VERSION};

const STANDARD: &str = "As this enchantment enters, choose odd or even. (Zero is even.)\nEach creature with mana value of the chosen quality has haste.";
const ASHLING: &str = "As this enchantment enters, choose odd or even. (Zero is even.)\nEach creature with mana value of the chosen quality has haste.\nEach creature without mana value of the chosen quality enters tapped.";
const HASTE_MODIFICATIONS: &[ContinuousModification] = &[ContinuousModification::AddKeyword {
    keyword: Keyword::Haste,
}];
const ADAPTIVE: &str = "As this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nOther creatures you control of the chosen type get +1/+1.";

fn standard(s: &mut GameScenario, player: PlayerId, text: &str) -> ObjectId {
    s.add_spell_to_hand(player, "Parity Standard", false)
        .as_enchantment()
        .from_oracle_text(text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn population(s: &mut GameScenario) -> Vec<(ObjectId, u32)> {
    let mut cards = Vec::new();
    for player in [P0, P1] {
        s.with_library_top(
            player,
            &[
                "Draw reach 1",
                "Draw reach 2",
                "Draw reach 3",
                "Draw reach 4",
                "Draw reach 5",
                "Draw reach 6",
            ],
        );
        for mv in [0, 2, 3] {
            cards.push((
                s.add_creature(player, "Parity candidate", 1, 2)
                    .with_mana_cost(ManaCost::generic(mv))
                    .id(),
                mv,
            ));
        }
        // CR 202.3a: no mana cost and the explicit zero cost above both have MV0.
        cards.push((
            s.add_creature(player, "No mana cost", 1, 2)
                .with_mana_cost(ManaCost::NoCost)
                .id(),
            0,
        ));
    }
    cards
}

fn assert_table(r: &GameRunner, cards: &[(ObjectId, u32)], odd: bool, even: bool) {
    for (id, mv) in cards {
        let object = &r.state().objects[id];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(
            object.keywords.contains(&Keyword::Haste),
            if mv % 2 == 0 { even } else { odd },
            "MV{mv}, controller {:?}",
            object.controller
        );
    }
}

fn pause(r: &mut GameRunner, source: ObjectId, expected_modifications: &[ContinuousModification]) {
    let object = &r.state().objects[&source];
    assert_eq!(object.zone, Zone::Hand);
    assert!(object.replacement_definitions.iter_unchecked().any(|d| d
        .execute
        .as_deref()
        .is_some_and(|a| matches!(
            a.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::OddOrEven,
                persist: true,
                ..
            }
        ))));
    assert!(object
        .static_definitions
        .iter_unchecked()
        .any(|d| d.modifications.as_slice() == expected_modifications
            && d.affected.as_ref().is_some_and(|f| source_parity(f))));
    assert!(object
        .abilities
        .iter()
        .all(|a| !matches!(a.effect.as_ref(), Effect::Unimplemented { .. })));
    assert!(object.static_definitions.iter_unchecked().all(|s| s.modifications.iter().all(|m| !matches!(m,
        ContinuousModification::GrantAbility { definition } if matches!(definition.effect.as_ref(), Effect::Unimplemented { .. })))));
    r.cast(source).resolve();
    assert_ne!(r.state().objects[&source].zone, Zone::Battlefield);
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        source: Some(binding),
        ..
    } = &r.state().waiting_for
    else {
        panic!("natural pre-entry NamedChoice: {:?}", r.state().waiting_for)
    };
    assert_eq!(choice_type, &ChoiceType::OddOrEven);
    assert_eq!(options, &["Odd", "Even"]);
    assert_eq!(binding.binding, NamedChoiceSourceBinding::PendingEntry);
    assert!(binding.context.is_some());
    let actions = engine::ai_support::legal_actions(r.state());
    for choice in ["Odd", "Even"] {
        assert!(actions
            .iter()
            .any(|a| matches!(a, GameAction::ChooseOption { choice: c } if c == choice)));
    }
    assert!(r.state().pending_replacement.is_some());
}

fn source_parity(filter: &TargetFilter) -> bool {
    // Fixture shape inspection only; production matching is exercised by the
    // real layer pass after the public entry answer.
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(|p| {
            matches!(
                p,
                FilterProp::ManaValueParity {
                    parity: ParitySource::SourceChosen
                }
            )
        }),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(source_parity)
        }
        TargetFilter::Not { filter } => source_parity(filter),
        _ => false,
    }
}

fn answer(r: &mut GameRunner, source: ObjectId, choice: &str) {
    r.act(GameAction::ChooseOption {
        choice: choice.into(),
    })
    .expect("answer real NamedChoice");
    r.advance_until_stack_empty();
    assert_eq!(r.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(
        r.state().objects[&source].chosen_attributes,
        vec![ChosenAttribute::OddOrEven(if choice == "Odd" {
            Parity::Odd
        } else {
            Parity::Even
        })]
    );
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
    assert!(r.state().pending_replacement.is_none());
}

fn next_turn(r: &mut GameRunner, expected: PlayerId) {
    r.advance_to_end_step();
    r.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(r.state().active_player, expected);
}

#[test]
fn u15_full_ashling_public_entry_both_seats_and_answers() {
    // CR 607.2d + CR 614.12: the registry source's choice selects the external
    // creature population; SelfRef in the modifier names the entering creature.
    for player in [P0, P1] {
        for choice in ["Odd", "Even"] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let source = standard(&mut s, player, ASHLING);
            let candidates: Vec<_> = [0, 2, 3]
                .into_iter()
                .map(|mv| {
                    (
                        s.add_creature_to_hand(player, "Incoming creature", 1, 2)
                            .with_mana_cost(ManaCost::generic(mv))
                            .id(),
                        mv,
                    )
                })
                .collect();
            s.with_library_top(player, &["Draw reach", "Second draw reach"]);
            s.with_mana_pool(
                player,
                (0..30)
                    .map(|_| {
                        engine::types::mana::ManaUnit::new(
                            engine::types::mana::ManaType::Colorless,
                            source,
                            false,
                            vec![],
                        )
                    })
                    .collect(),
            );
            let mut r = s.build();
            r.state_mut().active_player = player;
            r.state_mut().priority_player = player;
            r.state_mut().waiting_for = WaitingFor::Priority { player };
            pause(&mut r, source, HASTE_MODIFICATIONS);
            answer(&mut r, source, choice);
            assert_eq!(
                r.state().objects[&source]
                    .replacement_definitions
                    .iter_unchecked()
                    .count(),
                2
            );
            for (candidate, mv) in candidates {
                r.cast(candidate)
                    .resolve()
                    .assert_zone(&[candidate], Zone::Battlefield);
                let object = &r.state().objects[&candidate];
                let matching = (mv % 2 == 0) == (choice == "Even");
                assert_eq!(object.tapped, !matching, "{choice} MV{mv} {player:?}");
                assert_eq!(object.keywords.contains(&Keyword::Haste), matching);
                assert!(!r.state().objects[&source].tapped);
            }
        }
    }
}

#[test]
fn u15_full_ashling_normal_and_copied_token_population() {
    // CR 111.1 + CR 202.3a + CR 707.2: ordinary tokens have MV0, whereas
    // a copy token keeps the captured mana cost. The entry answer is public.
    for player in [P0, P1] {
        for choice in ["Odd", "Even"] {
            for (text, copied, counter_copy) in [
                ("Create a 1/1 black Spirit creature token.", false, false),
                (
                    "Create a token that's a copy of target creature.",
                    true,
                    false,
                ),
                (
                    "Create a token that's a copy of target creature.",
                    true,
                    true,
                ),
            ] {
                let mut s = GameScenario::new();
                s.at_phase(Phase::PreCombatMain);
                let source = standard(&mut s, player, ASHLING);
                let target = s
                    .add_creature_to_hand(player, "MV3 copy source", 1, 2)
                    .with_mana_cost(ManaCost::generic(3))
                    .from_oracle_text(if counter_copy {
                        "This creature enters with a +1/+1 counter on it."
                    } else {
                        ""
                    })
                    .id();
                let producer = s
                    .add_spell_to_hand_from_oracle(player, "Synthetic token producer", true, text)
                    .id();
                let mut r = s.build();
                r.state_mut().active_player = player;
                r.state_mut().priority_player = player;
                r.state_mut().waiting_for = WaitingFor::Priority { player };
                pause(&mut r, source, HASTE_MODIFICATIONS);
                answer(&mut r, source, choice);
                assert!(!r.state().objects[&producer].abilities.is_empty());
                if copied {
                    for _ in 0..3 {
                        r.state_mut().add_mana_to_pool(
                            player,
                            engine::types::mana::ManaUnit::new(
                                engine::types::mana::ManaType::Colorless,
                                source,
                                false,
                                vec![],
                            ),
                        );
                    }
                    r.cast(target).resolve();
                    assert_eq!(r.state().objects[&target].zone, Zone::Battlefield);
                    assert_eq!(r.state().objects[&target].effective_mana_value(), 3);
                }
                if copied {
                    r.cast(producer).target_object(target).resolve();
                } else {
                    r.cast(producer).resolve();
                }
                let ids = &r.state().last_created_token_ids;
                assert_eq!(ids.len(), 1, "{text}: {:?}", r.state().waiting_for);
                let token = &r.state().objects[&ids[0]];
                assert!(token.is_token);
                assert_eq!(token.zone, Zone::Battlefield);
                assert_eq!(token.owner, player);
                assert_eq!(token.controller, player);
                assert_eq!(token.effective_mana_value(), if copied { 3 } else { 0 });
                let matching = if copied {
                    choice == "Odd"
                } else {
                    choice == "Even"
                };
                assert_eq!(token.tapped, !matching);
                assert_eq!(token.keywords.contains(&Keyword::Haste), matching);
                if counter_copy {
                    assert_eq!(
                        token
                            .counters
                            .get(&engine::types::counter::CounterType::Plus1Plus1),
                        Some(&1)
                    );
                }
                assert!(!r.state().objects[&source].tapped);
                assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
                assert!(r.state().liminal_entries.is_empty());
            }
        }
    }
}

#[test]
fn u15_sources_survivor_control_and_new_incarnation() {
    // CR 400.7: the source incarnation owns its answer. Control changes retain
    // it; leaving ends its future effect without undoing earlier tapped entry.
    for remove_odd in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let cards = population(&mut s);
        let a = standard(
            &mut s,
            P0,
            &format!("{ASHLING}\n{{0}}: Choose odd or even."),
        );
        let b = standard(&mut s, P1, ASHLING);
        let steal = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Control fixture",
                true,
                "Gain control of target permanent.",
            )
            .id();
        let bounce = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Bounce fixture",
                true,
                "Return target enchantment to its owner's hand.",
            )
            .id();
        let local = s
            .add_spell_to_hand_from_oracle(P0, "Local fixture", true, "Choose odd or even.")
            .id();
        let color = s
            .add_spell_to_hand_from_oracle(P0, "Color fixture", true, "Choose a color.")
            .id();
        let early = s
            .add_creature_to_hand(P0, "Earlier opposite-source entry", 1, 1)
            .id();
        let entering: Vec<_> = [P0, P1]
            .into_iter()
            .flat_map(|seat| [2, 3].into_iter().map(move |mv| (seat, mv)))
            .map(|(seat, mv)| {
                (
                    s.add_creature_to_hand(seat, "New lifetime entrant", 1, 2)
                        .with_mana_cost(ManaCost::generic(mv))
                        .id(),
                    seat,
                    mv,
                )
            })
            .collect();
        for seat in [P0, P1] {
            s.with_mana_pool(
                seat,
                vec![
                    engine::types::mana::ManaUnit::new(
                        engine::types::mana::ManaType::Colorless,
                        a,
                        false,
                        vec![]
                    );
                    30
                ],
            );
        }
        let mut r = s.build();
        pause(&mut r, a, HASTE_MODIFICATIONS);
        answer(&mut r, a, "Odd");
        next_turn(&mut r, P1);
        pause(&mut r, b, HASTE_MODIFICATIONS);
        answer(&mut r, b, "Even");
        assert_table(&r, &cards, true, true);
        next_turn(&mut r, P0);
        for (spell, value) in [(local, "Even"), (color, "Blue")] {
            r.cast(spell).resolve();
            r.act(GameAction::ChooseOption {
                choice: value.into(),
            })
            .unwrap();
            r.advance_until_stack_empty();
        }
        r.activate(a, 0).resolve();
        r.act(GameAction::ChooseOption {
            choice: "Even".into(),
        })
        .unwrap();
        r.advance_until_stack_empty();
        assert_eq!(
            r.state().objects[&a].chosen_attributes,
            vec![ChosenAttribute::OddOrEven(Parity::Odd)]
        );
        let victim = if remove_odd { a } else { b };
        let survivor = if remove_odd { b } else { a };
        let epoch = r.state().objects[&b].incarnation;
        r.cast(steal).target_object(b).resolve();
        assert_eq!(
            (
                r.state().objects[&b].owner,
                r.state().objects[&b].controller
            ),
            (P1, P0)
        );
        assert_eq!(r.state().objects[&b].incarnation, epoch);
        assert_eq!(
            r.state().objects[&b].chosen_attributes,
            vec![ChosenAttribute::OddOrEven(Parity::Even)]
        );
        assert_table(&r, &cards, true, true);
        r.cast(early).resolve();
        assert!(r.state().objects[&early].tapped);
        assert!(r.state().objects[&early].keywords.contains(&Keyword::Haste));
        r.cast(bounce).target_object(victim).resolve();
        assert_eq!(r.state().objects[&victim].zone, Zone::Hand);
        assert_eq!(r.state().objects[&survivor].zone, Zone::Battlefield);
        assert_table(&r, &cards, !remove_odd, remove_odd);
        assert!(
            r.state().objects[&early].tapped,
            "source departure never reverses a completed entry event"
        );
        assert_eq!(
            r.state().objects[&early].keywords.contains(&Keyword::Haste),
            remove_odd
        );
        for (id, seat, mv) in entering {
            if r.state().active_player != seat {
                next_turn(&mut r, seat);
            }
            // Mana is a turn resource; replenish only this fixture's payment pool.
            for _ in 0..10 {
                r.state_mut().add_mana_to_pool(
                    seat,
                    engine::types::mana::ManaUnit::new(
                        engine::types::mana::ManaType::Colorless,
                        survivor,
                        false,
                        vec![],
                    ),
                );
            }
            r.cast(id).resolve();
            let object = &r.state().objects[&id];
            let matches_survivor = (mv % 2 == 0) == remove_odd;
            assert_eq!(object.tapped, !matches_survivor);
            assert_eq!(object.keywords.contains(&Keyword::Haste), matches_survivor);
        }
        let owner = r.state().objects[&victim].owner;
        if r.state().active_player != owner {
            next_turn(&mut r, owner);
        }
        let old_epoch = r.state().objects[&victim].incarnation;
        pause(&mut r, victim, HASTE_MODIFICATIONS);
        answer(&mut r, victim, if remove_odd { "Even" } else { "Odd" });
        assert_ne!(r.state().objects[&victim].incarnation, old_epoch);
        assert_eq!(
            r.state().objects[&survivor].chosen_attributes,
            vec![ChosenAttribute::OddOrEven(if remove_odd {
                Parity::Even
            } else {
                Parity::Odd
            })]
        );
    }
}

#[test]
fn u15_missing_source_and_first_parity_normal_entry() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let source = standard(&mut s, P0, ASHLING);
    let entrant = s.add_creature_to_hand(P0, "Zero entrant", 1, 1).id();
    let mut r = s.build();
    pause(&mut r, source, HASTE_MODIFICATIONS);
    let paused = r.state().pending_replacement.clone();
    for invalid in ["", "Neither"] {
        assert!(r
            .act(GameAction::ChooseOption {
                choice: invalid.into()
            })
            .is_err());
        assert_eq!(r.state().pending_replacement, paused);
        assert_ne!(r.state().objects[&source].zone, Zone::Battlefield);
    }
    answer(&mut r, source, "Odd");
    for (attrs, tapped, haste) in [
        (vec![], false, false),
        (
            vec![ChosenAttribute::Color(engine::types::mana::ManaColor::Blue)],
            false,
            false,
        ),
        (
            vec![
                ChosenAttribute::Color(engine::types::mana::ManaColor::Blue),
                ChosenAttribute::OddOrEven(Parity::Odd),
                ChosenAttribute::OddOrEven(Parity::Even),
            ],
            true,
            false,
        ),
        (vec![ChosenAttribute::OddOrEven(Parity::Even)], false, true),
    ] {
        let mut value = serde_json::to_value(r.state()).unwrap();
        value["objects"][source.0.to_string()]["chosen_attributes"] =
            serde_json::to_value(attrs).unwrap();
        let mut state: GameState = serde_json::from_value(value).unwrap();
        state.last_named_choice = Some(ChoiceValue::OddOrEven(Parity::Even));
        state.objects.get_mut(&entrant).unwrap().chosen_attributes =
            vec![ChosenAttribute::OddOrEven(Parity::Even)];
        engine::game::layers::mark_layers_full(&mut state);
        let mut loaded = GameRunner::from_state(state);
        loaded.cast(entrant).resolve();
        assert_eq!(loaded.state().objects[&entrant].zone, Zone::Battlefield);
        assert_eq!(loaded.state().objects[&entrant].tapped, tapped);
        assert_eq!(
            loaded.state().objects[&entrant]
                .keywords
                .contains(&Keyword::Haste),
            haste
        );
    }
}

#[test]
fn u15_external_type_population_and_controller_restriction() {
    for restricted in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let text = if restricted {
            ASHLING.replace("Each creature without", "Each creature you control without")
        } else {
            ASHLING.into()
        };
        let source = standard(&mut s, P0, &text);
        let mut entries = Vec::new();
        for seat in [P0, P1] {
            s.with_library_top(seat, &["Reach one", "Reach two", "Reach three"]);
            let creature = s
                .add_creature_to_hand(seat, "Artifact creature", 1, 1)
                .as_artifact_creature()
                .id();
            let artifact = s
                .add_spell_to_hand(seat, "Noncreature artifact", false)
                .as_artifact()
                .id();
            let land = s.add_land_to_hand(seat, "Noncreature land").id();
            entries.push((seat, creature, artifact, land));
        }
        let mut r = s.build();
        pause(&mut r, source, HASTE_MODIFICATIONS);
        answer(&mut r, source, "Odd");
        assert_eq!(
            r.state().objects[&source]
                .replacement_definitions
                .iter_unchecked()
                .count(),
            2
        );
        for (seat, creature, artifact, land) in entries {
            if r.state().active_player != seat {
                next_turn(&mut r, seat);
            }
            r.cast(creature).resolve();
            r.cast(artifact).resolve();
            r.act(GameAction::PlayLand {
                object_id: land,
                card_id: r.state().objects[&land].card_id,
            })
            .unwrap();
            assert_eq!(
                r.state().objects[&creature].tapped,
                !restricted || seat == P0
            );
            assert!(!r.state().objects[&creature]
                .keywords
                .contains(&Keyword::Haste));
            for id in [artifact, land] {
                assert_eq!(r.state().objects[&id].zone, Zone::Battlefield);
                assert!(!r.state().objects[&id].tapped);
                assert!(!r.state().objects[&id].keywords.contains(&Keyword::Haste));
            }
        }
        assert!(!r.state().objects[&source].tapped);
    }
}

#[test]
fn u15_queued_owner_entries_reload_and_hostile_origin() {
    use engine::types::events::GameEvent;
    use engine::types::resolution::ResolutionFrame;
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let source = standard(&mut s, P0, ASHLING);
    let first = s
        .add_creature_to_hand(P0, "Adaptive Automaton", 2, 2)
        .from_oracle_text(ADAPTIVE)
        .id();
    let second = s
        .add_creature_to_hand(P1, "Opposite queued creature", 2, 2)
        .id();
    let producer = s.add_spell_to_hand_from_oracle(P0, "Owner entry fixture", true,
        "Each player reveals the top card of their library. If all cards revealed this way are creature cards, put those cards onto the battlefield under their owners' control. You gain 7 life.").id();
    let mut r = s.build();
    pause(&mut r, source, HASTE_MODIFICATIONS);
    answer(&mut r, source, "Odd");
    for id in [first, second] {
        engine::game::zones::move_to_zone(r.state_mut(), id, Zone::Library, &mut Vec::new());
        let owner = r.state().objects[&id].owner;
        r.state_mut()
            .library_of_mut(owner)
            .retain(|card| *card != id);
        r.state_mut().library_of_mut(owner).push_front(id);
    }
    r.cast(producer).resolve();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::NamedChoice { player: P0, .. }
    ));
    assert_ne!(r.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(r.state().objects[&second].zone, Zone::Library);
    assert_eq!(r.life(P0), 20);
    let saved_queue = r
        .state()
        .resolution_stack
        .iter()
        .find_map(|frame| match frame {
            ResolutionFrame::BatchDelivery(batch) => Some(batch.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(saved_queue.requests.len(), 1);
    assert!(saved_queue.requests[0].expected_origin.is_some());
    assert_eq!(saved_queue.requests[0].performed_by, Some(P0));
    assert!(saved_queue.requests[0].controller_override.is_none());
    assert!(!r.state().return_result_frames.is_empty());
    for viewer in [Some(P0), Some(P1), None] {
        let public = viewer.map_or_else(
            || filter_state_for_unseated_viewer(r.state()),
            |seat| filter_state_for_viewer(r.state(), seat),
        );
        assert!(public.return_result_frames.is_empty() && public.resolution_stack.is_empty());
        assert!(matches!(public.waiting_for, WaitingFor::NamedChoice { .. }));
        assert!(!public.revealed_cards.is_empty());
        let wire = serde_json::to_string(&ClientGameStateRef::wrap(r.state(), viewer)).unwrap();
        assert!(!wire.contains("expected_origin") && !wire.contains("forwarded_result_context"));
        assert_eq!(
            public.objects[&source].chosen_attributes,
            vec![ChosenAttribute::OddOrEven(Parity::Odd)]
        );
    }
    let current =
        serde_json::to_value(ResolutionStateWire::from_game_state(r.state().clone())).unwrap();
    assert!(serde_json::from_value::<ResolutionStateWire>(current.clone()).is_ok());
    for fault in 0..3 {
        let mut bad = current.clone();
        let batch = bad["resolution_frames"]["frames"]
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
                batch["data"]["requests"][0]["object_id"] = serde_json::json!(999999);
            }
            _ => unreachable!(),
        }
        assert!(serde_json::from_value::<ResolutionStateWire>(bad.clone()).is_err());
        assert!(serde_json::from_value::<PersistedGameState>(bad).is_err());
    }
    // Historical ordinary queues have no exact origin pins. They retain that
    // explicit compatibility status after a current save, without acquiring
    // authority from the new source's parity or a latest-result mirror.
    let mut legacy = current.clone();
    legacy["resolution_state_version"] = serde_json::json!(7);
    let batch = legacy["resolution_frames"]["frames"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|frame| frame["type"] == "BatchDelivery")
        .unwrap();
    for request in batch["data"]["requests"].as_array_mut().unwrap() {
        request.as_object_mut().unwrap().remove("expected_origin");
    }
    let historical = serde_json::from_value::<ResolutionStateWire>(legacy)
        .unwrap()
        .into_game_state();
    assert!(historical
        .resolution_stack
        .iter()
        .any(|frame| matches!(frame,
        ResolutionFrame::BatchDelivery(batch) if !batch.requests.is_empty()
            && batch.requests.iter().all(|request| request.expected_origin.is_none()))));
    let mut historical = GameRunner::from_state(historical);
    historical
        .act(GameAction::ChooseOption {
            choice: "Elf".into(),
        })
        .unwrap();
    historical.advance_until_stack_empty();
    assert_eq!(historical.life(P0), 27);
    assert_eq!(historical.state().objects[&second].zone, Zone::Battlefield);
    assert!(historical.state().objects[&second].tapped);
    let original_pin =
        engine::types::identifiers::ObjectIncarnationRef::from_object(&r.state().objects[&second]);
    for route in 0..4 {
        for hostile in 0..3 {
            let mut loaded = restored(&r, route);
            assert!(loaded
                .state()
                .resolution_stack
                .iter()
                .any(|frame| matches!(frame,
                ResolutionFrame::BatchDelivery(batch) if batch == &saved_queue)));
            if hostile != 0 {
                engine::game::zones::move_to_zone(
                    loaded.state_mut(),
                    second,
                    Zone::Hand,
                    &mut Vec::new(),
                );
                if hostile == 2 {
                    engine::game::zones::move_to_zone(
                        loaded.state_mut(),
                        second,
                        Zone::Library,
                        &mut Vec::new(),
                    );
                }
                assert!(!original_pin.is_current(loaded.state()));
            }
            let mut events = loaded
                .act(GameAction::ChooseOption {
                    choice: "Elf".into(),
                })
                .unwrap()
                .events;
            for _ in 0..40 {
                if loaded.state().stack.is_empty() && loaded.state().resolution_stack.is_empty() {
                    break;
                }
                assert!(matches!(
                    loaded.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                events.extend(loaded.act(GameAction::PassPriority).unwrap().events);
            }
            assert_eq!(loaded.state().objects[&first].zone, Zone::Battlefield);
            assert!(loaded.state().objects[&first].tapped);
            assert!(!loaded.state().objects[&first]
                .keywords
                .contains(&Keyword::Haste));
            assert_eq!(loaded.life(P0), 27);
            if hostile == 0 {
                let object = &loaded.state().objects[&second];
                assert_eq!(
                    (object.zone, object.owner, object.controller),
                    (Zone::Battlefield, P1, P1)
                );
                assert!(object.tapped);
                assert!(!object.keywords.contains(&Keyword::Haste));
                assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == second)).count(), 1);
            } else {
                assert_eq!(
                    loaded.state().objects[&second].zone,
                    if hostile == 1 {
                        Zone::Hand
                    } else {
                        Zone::Library
                    }
                );
                assert!(!events.iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == second)));
            }
            assert!(loaded.state().pending_replacement.is_none());
            assert!(matches!(
                loaded.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }
}

#[test]
fn u15_projection_public_keyword_competitor() {
    for choice in ["Odd", "Even"] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let competitor = standard(&mut s, P0, "Creatures with haste enter tapped.");
        let source = s.add_creature_to_hand(P0, "Synthetic parity creature", 2, 2)
            .from_oracle_text("As this creature enters, choose odd or even.\nEach creature with mana value of the chosen quality has haste.")
            .with_mana_cost(ManaCost::generic(3)).id();
        s.with_mana_pool(
            P0,
            vec![
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    source,
                    false,
                    vec![]
                );
                3
            ],
        );
        let mut r = s.build();
        r.cast(competitor).resolve();
        let def = r.state().objects[&competitor]
            .replacement_definitions
            .iter_unchecked()
            .next()
            .unwrap();
        assert!(
            matches!(&def.valid_card, Some(TargetFilter::Typed(tf)) if tf.properties.iter().any(|p| matches!(p, FilterProp::WithKeyword { value: Keyword::Haste })))
        );
        pause(&mut r, source, HASTE_MODIFICATIONS);
        answer(&mut r, source, choice);
        let creature = &r.state().objects[&source];
        assert_eq!(creature.effective_mana_value(), 3);
        assert_eq!(creature.keywords.contains(&Keyword::Haste), choice == "Odd");
        assert_eq!(
            creature.tapped,
            choice == "Odd",
            "competitor reads proposed own Haste before delivery"
        );
        assert_eq!(creature.zone, Zone::Battlefield);
    }
}

#[test]
fn u15_external_siblings_optional_and_trigger_order() {
    use engine::types::events::GameEvent;
    for sibling in ["Authority", "Root Maze", "Amulet"] {
        for choice in ["Odd", "Even"] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let source = standard(&mut s, P0, ASHLING);
            let text = match sibling {
                "Authority" => "Creatures your opponents control enter tapped.\nWhenever a creature an opponent controls enters, you gain 1 life.",
                "Root Maze" => "Artifacts and lands enter tapped.",
                _ => "Whenever a permanent you control enters tapped, untap it.",
            };
            let other = standard(&mut s, P0, text);
            let own = s.add_creature_to_hand(P0, "Own zero creature", 1, 1).id();
            let opposing = s
                .add_creature_to_hand(P1, "Opposing zero creature", 1, 1)
                .id();
            let artifact = s
                .add_spell_to_hand(P0, "Noncreature artifact", false)
                .as_artifact()
                .id();
            let land = s
                .add_land_to_hand(P0, "Optional land")
                .from_oracle_text(
                    "As this land enters, you may pay 2 life. If you don't, it enters tapped.",
                )
                .id();
            s.with_library_top(P0, &["Draw one", "Draw two"]);
            s.with_library_top(P1, &["Draw one", "Draw two"]);
            let mut r = s.build();
            pause(&mut r, source, HASTE_MODIFICATIONS);
            answer(&mut r, source, choice);
            r.cast(other).resolve();
            assert!(if sibling == "Amulet" {
                !r.state().objects[&other].trigger_definitions.is_empty()
            } else {
                !r.state().objects[&other].replacement_definitions.is_empty()
            });
            let events = r.cast(own).resolve().events().to_vec();
            assert_eq!(
                r.state().objects[&own].tapped,
                choice == "Odd" && sibling != "Amulet"
            );
            assert_eq!(
                r.state().objects[&own].keywords.contains(&Keyword::Haste),
                choice == "Even"
            );
            if sibling == "Amulet" && choice == "Odd" {
                assert!(events.iter().any(|e| matches!(e, GameEvent::ReplacementApplied { source_id, .. } if *source_id == source)));
                assert!(events.iter().any(|e| matches!(e, GameEvent::PermanentUntapped { object_id, .. } if *object_id == own)));
            }
            r.cast(artifact).resolve();
            assert_eq!(r.state().objects[&artifact].tapped, sibling == "Root Maze");
            let before = r.life(P0);
            next_turn(&mut r, P1);
            r.cast(opposing).resolve();
            assert_eq!(
                r.state().objects[&opposing].tapped,
                choice == "Odd" || sibling == "Authority"
            );
            assert_eq!(r.life(P0), before + i32::from(sibling == "Authority"));
            next_turn(&mut r, P0);
            // Branch a real optional entry pause into accepted and declined saves.
            let card_id = r.state().objects[&land].card_id;
            r.act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .unwrap();
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            for accept in [false, true] {
                let mut loaded = restored(&r, 1);
                let initial_life = loaded.life(P0);
                for _ in 0..6 {
                    let WaitingFor::ReplacementChoice { candidates, .. } =
                        &loaded.state().waiting_for
                    else {
                        break;
                    };
                    let index = candidates
                        .iter()
                        .position(|c| {
                            if accept {
                                c.description != "Decline"
                            } else {
                                c.description == "Decline"
                            }
                        })
                        .unwrap_or(0);
                    loaded.act(GameAction::ChooseReplacement { index }).unwrap();
                }
                loaded.advance_until_stack_empty();
                assert_eq!(loaded.state().objects[&land].zone, Zone::Battlefield);
                assert_eq!(loaded.life(P0), initial_life - if accept { 2 } else { 0 });
                assert_eq!(
                    loaded.state().objects[&land].tapped,
                    sibling != "Amulet" && (!accept || sibling == "Root Maze")
                );
                assert!(!loaded.state().objects[&source].tapped);
            }
        }
    }
}

#[test]
fn u15_token_controls_substitution_and_multiple_result_members() {
    use engine::types::events::GameEvent;
    for source_seat in [P0, P1] {
        for creator in [P0, P1] {
            for choice in ["Odd", "Even"] {
                for (text, creature, substituted) in [
                    ("Create a Treasure token.", false, false),
                    (
                        "Create two 1/1 colorless Servo artifact creature tokens.",
                        true,
                        false,
                    ),
                    (
                        "Create a token that's a copy of target creature.",
                        true,
                        true,
                    ),
                ] {
                    let mut s = GameScenario::new();
                    s.at_phase(Phase::PreCombatMain);
                    let source = standard(&mut s, source_seat, ASHLING);
                    let target = s
                        .add_creature_to_hand(creator, "Plain real MV3 target", 2, 3)
                        .with_mana_cost(ManaCost::generic(3))
                        .id();
                    if substituted {
                        s.add_enchantment_from_oracle(creator, "Divine Visitation",
                            "If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.");
                    }
                    let producer = s
                        .add_spell_to_hand_from_oracle(
                            creator,
                            "Complete token control",
                            true,
                            text,
                        )
                        .id();
                    s.with_library_top(P0, &["Reach one", "Reach two", "Reach three"]);
                    s.with_library_top(P1, &["Reach one", "Reach two", "Reach three"]);
                    let mut r = s.build();
                    r.state_mut().active_player = source_seat;
                    r.state_mut().priority_player = source_seat;
                    r.state_mut().waiting_for = WaitingFor::Priority {
                        player: source_seat,
                    };
                    pause(&mut r, source, HASTE_MODIFICATIONS);
                    answer(&mut r, source, choice);
                    if creator != source_seat {
                        next_turn(&mut r, creator);
                    }
                    if substituted {
                        for _ in 0..3 {
                            r.state_mut().add_mana_to_pool(
                                creator,
                                engine::types::mana::ManaUnit::new(
                                    engine::types::mana::ManaType::Colorless,
                                    source,
                                    false,
                                    vec![],
                                ),
                            );
                        }
                        r.cast(target).resolve();
                        assert_eq!(r.state().objects[&target].effective_mana_value(), 3);
                    }
                    let ability = &r.state().objects[&producer].abilities[0];
                    assert!(if substituted {
                        matches!(ability.effect.as_ref(), Effect::CopyTokenOf { .. })
                    } else {
                        matches!(ability.effect.as_ref(), Effect::Token { .. })
                    });
                    let events = if substituted {
                        r.cast(producer)
                            .target_object(target)
                            .resolve()
                            .events()
                            .to_vec()
                    } else {
                        r.cast(producer).resolve().events().to_vec()
                    };
                    let ids = r.state().last_created_token_ids.clone();
                    assert_eq!(
                        ids.len(),
                        if text.starts_with("Create two") { 2 } else { 1 }
                    );
                    assert_eq!(
                        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
                        ids.len()
                    );
                    for id in ids {
                        let token = &r.state().objects[&id];
                        assert_eq!(
                            (token.owner, token.controller, token.zone),
                            (creator, creator, Zone::Battlefield)
                        );
                        assert_eq!(token.effective_mana_value(), 0);
                        assert_eq!(token.tapped, creature && choice == "Odd");
                        assert_eq!(
                            token.keywords.contains(&Keyword::Haste),
                            creature && choice == "Even"
                        );
                        if substituted {
                            assert_eq!(
                                (token.base_power, token.base_toughness),
                                (Some(4), Some(4))
                            );
                            assert!(token.card_types.subtypes.contains(&"Angel".into()));
                            assert!(
                                token.keywords.contains(&Keyword::Flying)
                                    && token.keywords.contains(&Keyword::Vigilance)
                            );
                        }
                        assert_eq!(events.iter().filter(|e| matches!(e, GameEvent::TokenCreated { object_id, .. } if *object_id == id)).count(), 1);
                        assert_eq!(events.iter().filter(|e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == id)).count(), 1);
                    }
                    assert_eq!(
                        r.state().objects[&source].chosen_attributes,
                        vec![ChosenAttribute::OddOrEven(if choice == "Odd" {
                            Parity::Odd
                        } else {
                            Parity::Even
                        })]
                    );
                    assert!(!r.state().objects[&source].tapped);
                    assert!(
                        r.state().pending_replacement.is_none()
                            && r.state().liminal_entries.is_empty()
                    );
                }
            }
        }
    }
}

#[test]
fn u15_outer_normal_and_counter_copy_material_orders() {
    use engine::types::events::GameEvent;
    use engine::types::proposed_event::ProposedEvent;
    for counter_copy in [false, true] {
        for ashling in [false, true] {
            for tap_first in [false, true] {
                let mut s = GameScenario::new();
                s.at_phase(Phase::PreCombatMain);
                let source = standard(
                    &mut s,
                    P0,
                    if ashling {
                        ASHLING
                    } else {
                        "Creatures enter tapped."
                    },
                );
                let untapper = standard(&mut s, P0, "Creatures enter untapped.");
                let target = s
                    .add_creature_to_hand(P0, "Counter copy source", 2, 2)
                    .with_mana_cost(ManaCost::generic(3))
                    .from_oracle_text("This creature enters with a +1/+1 counter on it.")
                    .id();
                let producer = s
                    .add_spell_to_hand_from_oracle(
                        P0,
                        "Outer token producer",
                        true,
                        if counter_copy {
                            "Create a token that's a copy of target creature. You gain 1 life."
                        } else {
                            "Create a 1/1 black Spirit creature token. You gain 1 life."
                        },
                    )
                    .id();
                s.with_mana_pool(
                    P0,
                    vec![
                        engine::types::mana::ManaUnit::new(
                            engine::types::mana::ManaType::Colorless,
                            source,
                            false,
                            vec![]
                        );
                        3
                    ],
                );
                let mut r = s.build();
                if ashling {
                    pause(&mut r, source, HASTE_MODIFICATIONS);
                    answer(&mut r, source, if counter_copy { "Even" } else { "Odd" });
                } else {
                    r.cast(source).resolve();
                }
                if counter_copy {
                    r.cast(target).resolve();
                }
                r.cast(untapper).resolve();
                let mut events = if counter_copy {
                    r.cast(producer)
                        .target_object(target)
                        .resolve()
                        .events()
                        .to_vec()
                } else {
                    r.cast(producer).resolve().events().to_vec()
                };
                let pending = r
                    .state()
                    .pending_replacement
                    .as_ref()
                    .expect("material outer entry ordering");
                assert!(
                    matches!(&pending.proposed, ProposedEvent::CreateToken { applied, copy, .. }
                    if applied.is_empty() && copy.is_some() == counter_copy)
                );
                assert_eq!(pending.candidates.len(), 2);
                assert!(r.state().liminal_entries.is_empty());
                assert_eq!(r.life(P0), 20);
                let first = if tap_first { source } else { untapper };
                let index = pending
                    .candidates
                    .iter()
                    .position(|id| id.source == first)
                    .unwrap();
                for route in 0..4 {
                    let mut loaded = restored(&r, route);
                    let mut resumed_events = events.clone();
                    resumed_events.extend(
                        loaded
                            .act(GameAction::ChooseReplacement { index })
                            .unwrap()
                            .events,
                    );
                    loaded.advance_until_stack_empty();
                    assert_eq!(loaded.life(P0), 21);
                    let ids = &loaded.state().last_created_token_ids;
                    assert_eq!(ids.len(), 1);
                    let id = ids[0];
                    let token = &loaded.state().objects[&id];
                    assert_eq!(token.zone, Zone::Battlefield);
                    assert_eq!(
                        token.effective_mana_value(),
                        if counter_copy { 3 } else { 0 }
                    );
                    assert_eq!(token.tapped, !tap_first);
                    assert!(!token.keywords.contains(&Keyword::Haste));
                    if counter_copy {
                        assert_eq!(
                            token
                                .counters
                                .get(&engine::types::counter::CounterType::Plus1Plus1),
                            Some(&1)
                        );
                    }
                    for external in [source, untapper] {
                        assert_eq!(resumed_events.iter().filter(|e| matches!(e, GameEvent::ReplacementApplied { source_id, .. } if *source_id == external)).count(), 1);
                    }
                    assert_eq!(resumed_events.iter().filter(|e| matches!(e, GameEvent::TokenCreated { object_id, .. } if *object_id == id)).count(), 1);
                    assert_eq!(resumed_events.iter().filter(|e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == id)).count(), 1);
                    assert!(
                        loaded.state().pending_replacement.is_none()
                            && loaded.state().liminal_entries.is_empty()
                    );
                }
                events.clear();
            }
        }
    }
}

#[test]
fn u15_full_source_choice_roundtrips_recompute_and_token_attack() {
    for choice in ["Odd", "Even"] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = standard(&mut s, P0, ASHLING);
        let hidden = standard(&mut s, P0, STANDARD);
        let producer = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Fresh token attack",
                true,
                "Create a 1/1 black Spirit creature token.",
            )
            .id();
        let sick = s
            .add_creature_to_hand(P0, "Fresh opposite non-token", 2, 2)
            .with_mana_cost(ManaCost::generic(3))
            .id();
        let untap = s
            .add_spell_to_hand_from_oracle(P0, "Untap sick control", true, "Untap target creature.")
            .id();
        s.with_mana_pool(
            P0,
            vec![
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    source,
                    false,
                    vec![]
                );
                3
            ],
        );
        let mut r = s.build();
        r.state_mut()
            .objects
            .get_mut(&hidden)
            .unwrap()
            .chosen_attributes = vec![ChosenAttribute::Color(engine::types::mana::ManaColor::Blue)];
        pause(&mut r, source, HASTE_MODIFICATIONS);
        let pending = r.state().pending_replacement.as_ref().unwrap().clone();
        for viewer in [Some(P0), Some(P1), None] {
            let public = viewer.map_or_else(
                || filter_state_for_unseated_viewer(r.state()),
                |seat| filter_state_for_viewer(r.state(), seat),
            );
            assert!(
                matches!(&public.waiting_for, WaitingFor::NamedChoice { source: Some(binding), options, .. }
                if binding.context.is_none() && options == &["Odd", "Even"])
            );
            assert!(public.resolution_stack.is_empty());
            if viewer != Some(P0) {
                assert!(public.objects[&hidden].chosen_attributes.is_empty());
            }
        }
        for route in 0..4 {
            let mut loaded = restored(&r, route);
            assert_eq!(loaded.state().pending_replacement.as_ref(), Some(&pending));
            assert_eq!(
                loaded.state().objects[&hidden].chosen_attributes,
                vec![ChosenAttribute::Color(engine::types::mana::ManaColor::Blue)]
            );
            answer(&mut loaded, source, choice);
            loaded.cast(producer).resolve();
            let token = loaded.state().last_created_token_ids[0];
            assert_eq!(loaded.state().objects[&token].tapped, choice == "Odd");
            assert_eq!(
                loaded.state().objects[&token]
                    .keywords
                    .contains(&Keyword::Haste),
                choice == "Even"
            );
            let mut reloaded = restored(&loaded, route);
            reloaded
                .state_mut()
                .objects
                .get_mut(&token)
                .unwrap()
                .keywords
                .clear();
            reloaded.state_mut().last_named_choice = Some(ChoiceValue::OddOrEven(Parity::Odd));
            engine::game::layers::mark_layers_full(reloaded.state_mut());
            engine::game::layers::evaluate_layers(reloaded.state_mut());
            assert_eq!(
                reloaded.state().objects[&token]
                    .keywords
                    .contains(&Keyword::Haste),
                choice == "Even"
            );
            reloaded.cast(sick).resolve();
            if choice == "Even" {
                assert!(reloaded.state().objects[&sick].tapped);
                assert!(!reloaded.state().objects[&sick]
                    .keywords
                    .contains(&Keyword::Haste));
                reloaded.cast(untap).target_object(sick).resolve();
                assert!(
                    !reloaded.state().objects[&sick].tapped
                        && reloaded.state().objects[&sick].summoning_sick
                );
            }
            reloaded.advance_to_combat();
            if choice == "Even" {
                assert!(reloaded
                    .declare_attackers(&[(sick, engine::game::combat::AttackTarget::Player(P1))])
                    .is_err());
                assert!(reloaded.state().objects[&token].summoning_sick);
                reloaded
                    .declare_attackers(&[(token, engine::game::combat::AttackTarget::Player(P1))])
                    .expect("fresh matching token attacks");
            } else {
                assert!(reloaded
                    .declare_attackers(&[(token, engine::game::combat::AttackTarget::Player(P1))])
                    .is_err());
                assert!(reloaded.state().objects[&sick]
                    .keywords
                    .contains(&Keyword::Haste));
                reloaded
                    .declare_attackers(&[(sick, engine::game::combat::AttackTarget::Player(P1))])
                    .expect("same-board opposite attack control");
            }
        }
    }
}

#[test]
fn u15_noncreature_copy_and_relative_token_controller() {
    for restricted in [false, true] {
        for creator in [P0, P1] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let text = if restricted {
                ASHLING.replace("Each creature without", "Each creature you control without")
            } else {
                ASHLING.into()
            };
            let source = standard(&mut s, P0, &text);
            let artifact = s
                .add_spell_to_hand(creator, "Real MV3 artifact source", false)
                .as_artifact()
                .with_mana_cost(ManaCost::generic(3))
                .id();
            let copy = s
                .add_spell_to_hand_from_oracle(
                    creator,
                    "Noncreature copy control",
                    true,
                    "Create a token that's a copy of target artifact.",
                )
                .id();
            let spirit = s
                .add_spell_to_hand_from_oracle(
                    creator,
                    "Qualifying token control",
                    true,
                    "Create a 1/1 black Spirit creature token.",
                )
                .id();
            s.with_library_top(P0, &["Reach one", "Reach two"]);
            s.with_library_top(P1, &["Reach one", "Reach two"]);
            let mut r = s.build();
            pause(&mut r, source, HASTE_MODIFICATIONS);
            answer(&mut r, source, "Odd");
            if creator != P0 {
                next_turn(&mut r, creator);
            }
            for _ in 0..3 {
                r.state_mut().add_mana_to_pool(
                    creator,
                    engine::types::mana::ManaUnit::new(
                        engine::types::mana::ManaType::Colorless,
                        source,
                        false,
                        vec![],
                    ),
                );
            }
            r.cast(artifact).resolve();
            assert!(matches!(
                r.state().objects[&copy].abilities[0].effect.as_ref(),
                Effect::CopyTokenOf { .. }
            ));
            r.cast(copy).target_object(artifact).resolve();
            let result = r.state().last_created_token_ids.clone();
            assert_eq!(result.len(), 1);
            let object = &r.state().objects[&result[0]];
            assert_eq!(object.effective_mana_value(), 3);
            assert!(object
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Artifact));
            assert!(!object
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Creature));
            assert_eq!(
                (object.owner, object.controller, object.zone),
                (creator, creator, Zone::Battlefield)
            );
            assert!(!object.tapped && !object.keywords.contains(&Keyword::Haste));
            r.cast(spirit).resolve();
            let token = &r.state().objects[&r.state().last_created_token_ids[0]];
            assert!(token
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Creature));
            assert_eq!(token.tapped, !restricted || creator == P0);
            assert!(!token.keywords.contains(&Keyword::Haste));
            assert!(!r.state().objects[&source].tapped);
        }
    }
}

#[test]
fn u15_optional_creation_settles_before_external_entry() {
    for branch in [None, Some(0), Some(1)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = standard(&mut s, P0, ASHLING);
        let jinnie = s.add_creature_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 3,
            "If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.").id();
        let producer = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Optional creation fixture",
                true,
                "Create a 1/1 black Spirit creature token. You gain 1 life.",
            )
            .id();
        let mut r = s.build();
        pause(&mut r, source, HASTE_MODIFICATIONS);
        answer(&mut r, source, "Odd");
        r.cast(producer).resolve();
        assert_eq!(r.life(P0), 20);
        let pending = r.state().pending_replacement.as_ref().unwrap();
        assert_eq!(
            pending.candidates.len(),
            1,
            "creation family is the first choosable subset"
        );
        assert_eq!(pending.candidates[0].source, jinnie);
        assert!(
            matches!(&pending.proposed, engine::types::proposed_event::ProposedEvent::CreateToken { applied, .. } if applied.is_empty())
        );
        for route in 0..4 {
            let mut loaded = restored(&r, route);
            loaded
                .act(GameAction::ChooseReplacement {
                    index: if branch.is_some() { 0 } else { 1 },
                })
                .unwrap();
            if let Some(index) = branch {
                assert!(matches!(
                    loaded.state().waiting_for,
                    WaitingFor::ChooseOneOfBranch { .. }
                ));
                loaded.act(GameAction::ChooseBranch { index }).unwrap();
            }
            loaded.advance_until_stack_empty();
            assert_eq!(loaded.life(P0), 21);
            let ids = &loaded.state().last_created_token_ids;
            assert_eq!(ids.len(), 1);
            let object = &loaded.state().objects[&ids[0]];
            assert_eq!(object.zone, Zone::Battlefield);
            assert_eq!(object.effective_mana_value(), 0);
            assert!(object.tapped);
            let (subtype, power, toughness) = match branch {
                None => ("Spirit", 1, 1),
                Some(0) => ("Cat", 2, 2),
                Some(1) => ("Dog", 3, 1),
                _ => unreachable!(),
            };
            assert!(object.card_types.subtypes.contains(&subtype.into()));
            assert_eq!(
                (object.base_power, object.base_toughness),
                (Some(power), Some(toughness))
            );
            assert_eq!(object.keywords.contains(&Keyword::Haste), branch == Some(0));
            assert_eq!(
                object.keywords.contains(&Keyword::Vigilance),
                branch == Some(1)
            );
            assert!(
                loaded.state().pending_replacement.is_none()
                    && loaded.state().liminal_entries.is_empty()
            );
        }
    }
}

#[test]
fn u15_substituted_mv3_body_keeps_both_inner_orders() {
    use engine::types::ability::{CopyTargetPurpose, TargetRef};
    use engine::types::events::GameEvent;
    use engine::types::proposed_event::{AppliedReplacementKey, ProposedEvent};
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let source = standard(&mut s, P0, ASHLING);
    let untapper = standard(&mut s, P0, "Creatures enter untapped.");
    let esix = s.add_creature_from_oracle(P0, "Esix, Fractal Bloom", 4, 4,
        "Flying\nThe first time you would create one or more tokens during each of your turns, you may instead choose a creature other than Esix and create that many tokens that are copies of that creature.").id();
    let target = s
        .add_creature_to_hand(P0, "Substituted real MV3 source", 2, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let producer = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Complete substituted creation",
            true,
            "Create a 1/1 black Spirit creature token. You gain 1 life.",
        )
        .id();
    s.with_mana_pool(
        P0,
        vec![
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                source,
                false,
                vec![]
            );
            3
        ],
    );
    let mut r = s.build();
    pause(&mut r, source, HASTE_MODIFICATIONS);
    answer(&mut r, source, "Even");
    r.cast(target).resolve();
    r.cast(untapper).resolve();
    let mut events = r.cast(producer).resolve().events().to_vec();
    let pending = r.state().pending_replacement.as_ref().unwrap();
    assert_eq!(pending.candidates.len(), 1);
    assert_eq!(pending.candidates[0].source, esix);
    events.extend(
        r.act(GameAction::ChooseReplacement { index: 0 })
            .unwrap()
            .events,
    );
    let WaitingFor::CopyTargetChoice {
        purpose:
            CopyTargetPurpose::CopyTokenSource {
                continuation: Some(tail),
            },
        ..
    } = &r.state().waiting_for
    else {
        panic!("actual saved copy-source choice")
    };
    assert_eq!(
        tail.context
            .token_creation_parent
            .as_ref()
            .unwrap()
            .substitution_count,
        1
    );
    assert_eq!(r.life(P0), 20);
    events.extend(
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .unwrap()
        .events,
    );
    assert!(events.iter().all(|event| !matches!(event, GameEvent::ReplacementApplied { source_id, .. } if *source_id == source || *source_id == untapper)), "empty superseded outer body owns no entry IDs");
    let pending = r
        .state()
        .pending_replacement
        .as_ref()
        .expect("substituted actual liminal MV3 ordering");
    let ProposedEvent::TokenEntry {
        entry_ref, applied, ..
    } = &pending.proposed
    else {
        panic!("inner event")
    };
    assert!(!applied.contains(&AppliedReplacementKey::object(source, 1)));
    assert!(!applied.contains(&AppliedReplacementKey::object(untapper, 0)));
    assert_eq!(
        r.state().liminal_entries[entry_ref]
            .object
            .projected()
            .effective_mana_value(),
        3
    );
    assert_eq!(pending.candidates.len(), 2);
    for route in 0..4 {
        for first in [source, untapper] {
            let mut loaded = restored(&r, route);
            let index = loaded
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .candidates
                .iter()
                .position(|id| id.source == first)
                .unwrap();
            let mut resumed = events.clone();
            resumed.extend(
                loaded
                    .act(GameAction::ChooseReplacement { index })
                    .unwrap()
                    .events,
            );
            loaded.advance_until_stack_empty();
            assert_eq!(loaded.life(P0), 21);
            assert_eq!(loaded.state().last_created_token_ids.len(), 1);
            let id = loaded.state().last_created_token_ids[0];
            let token = &loaded.state().objects[&id];
            assert_eq!(token.effective_mana_value(), 3);
            assert_eq!((token.base_power, token.base_toughness), (Some(2), Some(3)));
            assert_eq!(token.tapped, first == untapper);
            assert!(!token.keywords.contains(&Keyword::Haste));
            for external in [source, untapper] {
                assert_eq!(resumed.iter().filter(|event| matches!(event, GameEvent::ReplacementApplied { source_id, .. } if *source_id == external)).count(), 1);
            }
            assert_eq!(resumed.iter().filter(|event| matches!(event, GameEvent::TokenCreated { object_id, .. } if *object_id == id)).count(), 1);
            assert!(
                loaded.state().liminal_entries.is_empty()
                    && loaded.state().pending_replacement.is_none()
            );
        }
    }
}

#[test]
fn u14_public_entry_stores_parity_and_grants_haste() {
    // CR 614.12a + CR 607.2d + CR 613.1f: choose before entry; the linked
    // static grants haste in layer 6 to the live population of either player.
    for choice in ["Odd", "Even"] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let cards = population(&mut s);
        let a = standard(&mut s, P0, STANDARD);
        let mut r = s.build();
        pause(&mut r, a, HASTE_MODIFICATIONS);
        answer(&mut r, a, choice);
        assert_table(&r, &cards, choice == "Odd", choice == "Even");
    }
}

#[test]
fn u15_copy_entry_both_material_orders_reload_private_and_once_only_tail() {
    use engine::types::events::GameEvent;
    use engine::types::proposed_event::{AppliedReplacementKey, ProposedEvent};
    // CR 614.5 + CR 616.1g: neither external entry writer may apply to the
    // outer liminal copy creation. Both orders are available at each entry.
    for player in [P0, P1] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = standard(&mut s, player, ASHLING);
        let untapper = standard(&mut s, player, "Creatures enter untapped.");
        let target = s
            .add_creature_to_hand(player, "Plain MV3 copied creature", 2, 3)
            .with_mana_cost(ManaCost::generic(3))
            .id();
        let producer = s
            .add_spell_to_hand_from_oracle(
                player,
                "Synthetic two-copy producer",
                true,
                "Create two tokens that are copies of target creature. You gain 1 life.",
            )
            .id();
        let hidden = standard(&mut s, player, STANDARD);
        let mut r = s.build();
        r.state_mut().active_player = player;
        r.state_mut().priority_player = player;
        r.state_mut().waiting_for = WaitingFor::Priority { player };
        r.state_mut()
            .objects
            .get_mut(&hidden)
            .unwrap()
            .chosen_attributes = vec![ChosenAttribute::Color(engine::types::mana::ManaColor::Blue)];
        pause(&mut r, source, HASTE_MODIFICATIONS);
        answer(&mut r, source, "Even");
        for _ in 0..3 {
            r.state_mut().add_mana_to_pool(
                player,
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    source,
                    false,
                    vec![],
                ),
            );
        }
        r.cast(target).resolve();
        assert_eq!(r.state().objects[&target].zone, Zone::Battlefield);
        assert_eq!(r.state().objects[&target].effective_mana_value(), 3);
        assert!(r.state().objects[&target].tapped);
        let untap_definition = r.state().objects[&untapper]
            .replacement_definitions
            .iter_unchecked()
            .next()
            .unwrap();
        assert_eq!(
            untap_definition.event,
            engine::types::replacements::ReplacementEvent::ChangeZone
        );
        assert_eq!(untap_definition.destination_zone, Some(Zone::Battlefield));
        r.cast(untapper).resolve();
        let outcome = r.cast(producer).target_object(target).resolve();
        assert!(outcome.events().iter().all(|event| !matches!(event, GameEvent::ReplacementApplied { source_id, .. } if *source_id == source || *source_id == untapper)));
        assert_eq!(r.state().players[player.0 as usize].life, 20);
        let pending = r
            .state()
            .pending_replacement
            .as_ref()
            .expect("natural material TokenEntry pause");
        assert!(
            matches!(r.state().waiting_for, WaitingFor::ReplacementChoice { player: acting, .. } if acting == player)
        );
        let ProposedEvent::TokenEntry {
            entry_ref, applied, ..
        } = &pending.proposed
        else {
            panic!("{pending:#?}")
        };
        assert!(applied.is_empty());
        let entry = &r.state().liminal_entries[entry_ref];
        assert!(entry.creation_frame.is_some() && entry.copy_resume.is_some());
        assert_eq!(entry.remaining_count, 1);
        assert_eq!(entry.object.projected().effective_mana_value(), 3);
        assert_eq!(pending.candidates.len(), 2);
        for external in [source, untapper] {
            assert_eq!(
                pending
                    .candidates
                    .iter()
                    .filter(|id| id.source == external)
                    .count(),
                1
            );
        }
        let opponent = if player == P0 { P1 } else { P0 };
        for public in [
            filter_state_for_viewer(r.state(), opponent),
            filter_state_for_unseated_viewer(r.state()),
        ] {
            assert!(public.pending_replacement.is_none());
            assert!(public.liminal_entries.is_empty());
            assert!(public.objects[&hidden].chosen_attributes.is_empty());
            let wire = serde_json::to_value(ClientGameStateRef::wrap(&public, None)).unwrap();
            assert!(!wire.to_string().contains("copy_resume"));
        }
        for route in 0..4 {
            for first_source in [source, untapper] {
                let mut loaded = restored(&r, route);
                assert_eq!(loaded.state().pending_replacement.as_ref(), Some(pending));
                assert!(loaded
                    .act(GameAction::ChooseReplacement { index: usize::MAX })
                    .is_err());
                assert_eq!(loaded.state().pending_replacement.as_ref(), Some(pending));
                let mut events = Vec::new();
                let mut entries = Vec::new();
                for _ in 0..2 {
                    let current = loaded
                        .state()
                        .pending_replacement
                        .as_ref()
                        .expect("each token has its own ordinary material choice");
                    let ProposedEvent::TokenEntry {
                        entry_ref, applied, ..
                    } = &current.proposed
                    else {
                        panic!("token entry")
                    };
                    assert!(applied.is_empty());
                    assert!(!applied.contains(&AppliedReplacementKey::object(source, 1)));
                    entries.push(*entry_ref);
                    let index = current
                        .candidates
                        .iter()
                        .position(|id| id.source == first_source)
                        .unwrap();
                    events.extend(
                        loaded
                            .act(GameAction::ChooseReplacement { index })
                            .unwrap()
                            .events,
                    );
                }
                loaded.advance_until_stack_empty();
                assert_ne!(entries[0], entries[1]);
                assert_eq!(loaded.state().last_created_token_ids, entries);
                assert_eq!(
                    loaded.state().players[player.0 as usize].life,
                    21,
                    "independent tail once"
                );
                assert!(loaded.state().pending_replacement.is_none());
                assert!(loaded.state().liminal_entries.is_empty());
                assert!(matches!(
                    loaded.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                for id in &entries {
                    let token = &loaded.state().objects[id];
                    assert_eq!(token.zone, Zone::Battlefield);
                    assert_eq!(token.owner, player);
                    assert_eq!(token.controller, player);
                    assert_eq!(token.effective_mana_value(), 3);
                    assert_eq!(token.tapped, first_source == untapper);
                    assert!(!token.keywords.contains(&Keyword::Haste));
                    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::TokenCreated { object_id, .. } if object_id == id)).count(), 1);
                    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if object_id == id)).count(), 1);
                }
                for external in [source, untapper] {
                    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ReplacementApplied { source_id, .. } if *source_id == external)).count(), 2, "each external ID once per entrant");
                }
            }
        }
    }
}

#[test]
fn u14_complete_where_x_has_public_static_consumers() {
    // CR 611.3a + CR 613.4c: the quoted grant reads the recipient's
    // controller's graveyard; the complete outer grant also supplies menace.
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let commander = s.add_creature(P0, "Own commander", 1, 2).commander().id();
    let ordinary = s.add_creature(P0, "Noncommander sibling", 1, 2).id();
    let opposing = s
        .add_creature(P1, "Opposing commander", 1, 2)
        .commander()
        .id();
    for name in ["Creature in graveyard one", "Creature in graveyard two"] {
        s.add_creature_to_graveyard(P0, name, 1, 1);
    }
    s.add_land_to_graveyard(P0, "Noncreature in graveyard");
    let source = s.add_spell_to_hand(P0, "Criminal Past", false)
        .as_enchantment()
        .from_oracle_text("Commander creatures you own have menace and \"This creature gets +X/+0, where X is the number of creature cards in your graveyard.\" (A creature with menace can't be blocked except by two or more creatures.)")
        .with_mana_cost(ManaCost::zero()).id();
    let mut r = s.build();
    r.cast(source).resolve();
    assert_eq!(r.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(r.state().objects[&commander].power, Some(3));
    assert_eq!(r.state().objects[&commander].toughness, Some(2));
    assert!(r.state().objects[&commander]
        .keywords
        .contains(&Keyword::Menace));
    for id in [ordinary, opposing] {
        assert_eq!(r.state().objects[&id].power, Some(1));
        assert!(!r.state().objects[&id].keywords.contains(&Keyword::Menace));
    }
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let own = s.add_creature(P0, "Dynamic own recipient", 3, 4).id();
    let opposing = s.add_creature(P1, "Dynamic opposing recipient", 1, 2).id();
    let dynamic = s
        .add_spell_to_hand(P0, "Synthetic dynamic keyword grant", false)
        .as_enchantment()
        .from_oracle_text("Creatures have mobilize X, where X is its power.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(dynamic).resolve();
    for (id, power) in [(own, 3), (opposing, 1)] {
        assert!(r.state().objects[&id]
            .keywords
            .contains(&Keyword::Mobilize(QuantityExpr::Fixed { value: power })));
    }
}

#[test]
fn u14_complete_noncontinuous_companion_has_public_consumer() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let host = s.add_creature(P0, "Companion host", 1, 1).id();
    let sibling = s.add_creature(P0, "Unaffected sibling", 1, 1).id();
    let aura = s
        .add_spell_to_hand(P0, "Synthetic complete companion Aura", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(
            "Enchant creature\nEnchanted creature can't be blocked and has haste and gets +1/+1.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(aura).target_object(host).resolve();
    assert_eq!(r.state().objects[&aura].zone, Zone::Battlefield);
    let object = &r.state().objects[&host];
    assert_eq!((object.power, object.toughness), (Some(2), Some(2)));
    assert!(object.keywords.contains(&Keyword::Haste));
    assert!(
        engine::game::derived_views::derive_views(r.state(), Some(P0))
            .cant_be_blocked
            .contains(&host)
    );
    let object = &r.state().objects[&sibling];
    assert_eq!((object.power, object.toughness), (Some(1), Some(1)));
    assert!(!object.keywords.contains(&Keyword::Haste));
}

#[test]
fn u14_divine_sacrament_full_oracle_threshold_consumer() {
    use engine::types::mana::ManaColor;
    // CR 611.3a + CR 613.4c: threshold adds its second bonus to the
    // unconditional bonus, and only the printed white population is modified.
    for graveyard_size in [6, 7] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        for _ in 0..graveyard_size {
            s.add_land_to_graveyard(P0, "Threshold card");
        }
        let white = s
            .add_creature(P0, "White creature", 1, 1)
            .with_color(vec![ManaColor::White])
            .id();
        let opposing_white = s
            .add_creature(P1, "Opposing white creature", 1, 1)
            .with_color(vec![ManaColor::White])
            .id();
        let nonwhite = s
            .add_creature(P0, "Blue creature", 1, 1)
            .with_color(vec![ManaColor::Blue])
            .id();
        let source = s.add_spell_to_hand(P0, "Divine Sacrament", false).as_enchantment()
            .from_oracle_text("White creatures get +1/+1.\nThreshold — White creatures get an additional +1/+1 as long as there are seven or more cards in your graveyard.")
            .with_mana_cost(ManaCost::zero()).id();
        let mut r = s.build();
        r.cast(source).resolve();
        assert_eq!(r.state().objects[&source].zone, Zone::Battlefield);
        let expected = if graveyard_size == 7 { 3 } else { 2 };
        for id in [white, opposing_white] {
            let object = &r.state().objects[&id];
            assert_eq!(
                (object.power, object.toughness),
                (Some(expected), Some(expected))
            );
        }
        let object = &r.state().objects[&nonwhite];
        assert_eq!((object.power, object.toughness), (Some(1), Some(1)));
    }
}

#[test]
fn u14_maha_full_oracle_sets_only_opposing_toughness() {
    // CR 613.4b + CR 611.3a + CR 611.3b: a live base-toughness setter
    // preserves power and ends when its source leaves the battlefield.
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let own = s.add_creature(P0, "Own base-stat control", 4, 5).id();
    let opposing = s
        .add_creature(P1, "Opposing base-stat recipient", 4, 5)
        .id();
    let source = s.add_creature_to_hand(P0, "Maha, Its Feathers Night", 6, 5)
        .from_oracle_text_with_keywords(&["Flying", "Trample"],
            "Flying, trample\nWard—Discard a card.\nCreatures your opponents control have base toughness 1.")
        .with_mana_cost(ManaCost::zero()).id();
    let removal = s
        .add_spell_to_hand(P0, "Synthetic source removal", true)
        .from_oracle_text("Destroy target creature.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(source).resolve();
    assert_eq!(r.state().objects[&source].zone, Zone::Battlefield);
    for (id, toughness) in [(own, 5), (opposing, 1)] {
        let object = &r.state().objects[&id];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!((object.power, object.toughness), (Some(4), Some(toughness)));
    }
    // Its own controller's removal does not invoke the opponent-only ward.
    r.cast(removal).target_object(source).resolve();
    assert_eq!(r.state().objects[&source].zone, Zone::Graveyard);
    for id in [own, opposing] {
        let object = &r.state().objects[&id];
        assert_eq!((object.power, object.toughness), (Some(4), Some(5)));
    }
}

#[test]
fn u14_removal_led_parity_grants_haste_and_enables_attack() {
    // Explicitly synthetic complete Oracle contract, not printed card wording.
    // CR 613.1f + CR 611.3a: the matching creature loses Flying and gains
    // Haste; CR 302.6 + CR 702.10b then permit its actual new-creature attack.
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    fund(&mut s, P0);
    let source = standard(&mut s, P0,
        "As this enchantment enters, choose odd or even.\nEach creature with mana value of the chosen quality loses flying and has haste.");
    let odd = s
        .add_creature_to_hand(P0, "Synthetic MV3 flyer", 2, 3)
        .with_keyword(Keyword::Flying)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let even = s
        .add_creature_to_hand(P0, "Synthetic MV2 flyer", 2, 3)
        .with_keyword(Keyword::Flying)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut r = s.build();
    pause(
        &mut r,
        source,
        &[
            ContinuousModification::RemoveKeyword {
                keyword: Keyword::Flying,
            },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            },
        ],
    );
    assert!(r.state().objects[&source]
        .static_definitions
        .iter_unchecked()
        .any(|d| d.modifications
            == vec![
                ContinuousModification::RemoveKeyword {
                    keyword: Keyword::Flying
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste
                },
            ]));
    answer(&mut r, source, "Odd");
    assert_eq!(
        r.state().objects[&source].chosen_attributes,
        vec![ChosenAttribute::OddOrEven(Parity::Odd)]
    );
    r.cast(odd).resolve();
    r.cast(even).resolve();
    for (id, matching) in [(odd, true), (even, false)] {
        let object = &r.state().objects[&id];
        assert_eq!(object.zone, Zone::Battlefield);
        assert!(object.summoning_sick && !object.tapped);
        assert_eq!(object.keywords.contains(&Keyword::Flying), !matching);
        assert_eq!(object.keywords.contains(&Keyword::Haste), matching);
    }
    r.advance_to_combat();
    assert!(r
        .declare_attackers(&[(even, engine::game::combat::AttackTarget::Player(P1))])
        .is_err());
    r.declare_attackers(&[(odd, engine::game::combat::AttackTarget::Player(P1))])
        .expect("matching newly cast creature can attack on the same rejecting board");
}

#[test]
fn u14_inherited_removal_keeps_matching_creature_sick() {
    // Explicitly synthetic complete two-line contract, not printed card text.
    // CR 613.1f + CR 611.3a: Odd creatures lose both Flying and Haste,
    // then gain Vigilance. CR 302.6 + CR 702.10b distinguish the two attacks.
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    fund(&mut s, P0);
    let source = standard(&mut s, P0,
        "As this enchantment enters, choose odd or even.\nEach creature with mana value of the chosen quality loses flying and haste and has vigilance.");
    let odd = s
        .add_creature_to_hand(P0, "Synthetic MV3 hasty flyer", 2, 3)
        .with_keyword(Keyword::Flying)
        .with_keyword(Keyword::Haste)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let even = s
        .add_creature_to_hand(P0, "Synthetic MV2 hasty flyer", 2, 3)
        .with_keyword(Keyword::Flying)
        .with_keyword(Keyword::Haste)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut r = s.build();
    pause(
        &mut r,
        source,
        &[
            ContinuousModification::RemoveKeyword {
                keyword: Keyword::Flying,
            },
            ContinuousModification::RemoveKeyword {
                keyword: Keyword::Haste,
            },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Vigilance,
            },
        ],
    );
    assert!(r.state().objects[&source]
        .static_definitions
        .iter_unchecked()
        .any(|d| d.modifications
            == vec![
                ContinuousModification::RemoveKeyword {
                    keyword: Keyword::Flying
                },
                ContinuousModification::RemoveKeyword {
                    keyword: Keyword::Haste
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Vigilance
                },
            ]));
    answer(&mut r, source, "Odd");
    assert_eq!(
        r.state().objects[&source].chosen_attributes,
        vec![ChosenAttribute::OddOrEven(Parity::Odd)]
    );
    r.cast(odd).resolve();
    r.cast(even).resolve();
    for (id, matching) in [(odd, true), (even, false)] {
        let object = &r.state().objects[&id];
        assert_eq!(object.zone, Zone::Battlefield);
        assert!(object.summoning_sick && !object.tapped);
        assert_eq!(object.keywords.contains(&Keyword::Flying), !matching);
        assert_eq!(object.keywords.contains(&Keyword::Haste), !matching);
        assert_eq!(object.keywords.contains(&Keyword::Vigilance), matching);
    }
    r.advance_to_combat();
    assert!(r
        .declare_attackers(&[(odd, engine::game::combat::AttackTarget::Player(P1))])
        .is_err());
    r.declare_attackers(&[(even, engine::game::combat::AttackTarget::Player(P1))])
        .expect("opposite-parity Haste creature attacks on the same rejecting board");
}

#[test]
fn u14_opposite_sources_are_independent() {
    for remove_odd in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let cards = population(&mut s);
        let a = standard(&mut s, P0, STANDARD);
        let b = standard(&mut s, P1, STANDARD);
        let removal = s
            .add_spell_to_hand_from_oracle(
                P1,
                "Synthetic removal",
                true,
                "Exile target enchantment.",
            )
            .id();
        let mut r = s.build();
        pause(&mut r, a, HASTE_MODIFICATIONS);
        answer(&mut r, a, "Odd");
        next_turn(&mut r, P1);
        pause(&mut r, b, HASTE_MODIFICATIONS);
        answer(&mut r, b, "Even");
        assert_table(&r, &cards, true, true);
        let victim = if remove_odd { a } else { b };
        r.cast(removal)
            .target_object(victim)
            .resolve()
            .assert_zone(&[victim], Zone::Exile);
        let survivor = if remove_odd { b } else { a };
        assert_eq!(r.state().objects[&survivor].zone, Zone::Battlefield);
        assert_table(&r, &cards, !remove_odd, remove_odd);
    }
}

#[test]
fn u14_unrelated_choices_do_not_retarget_static() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let cards = population(&mut s);
    let a = standard(
        &mut s,
        P0,
        &format!("{STANDARD}\n{{0}}: Choose odd or even."),
    );
    let local = s
        .add_spell_to_hand_from_oracle(P0, "Local parity", true, "Choose odd or even.")
        .id();
    let color = s
        .add_spell_to_hand_from_oracle(P0, "Local color", true, "Choose a color.")
        .id();
    let mut r = s.build();
    pause(&mut r, a, HASTE_MODIFICATIONS);
    answer(&mut r, a, "Odd");
    for (spell, choice) in [(local, "Even"), (color, "Blue")] {
        r.cast(spell).resolve();
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::NamedChoice { .. }
        ));
        r.act(GameAction::ChooseOption {
            choice: choice.into(),
        })
        .unwrap();
        r.advance_until_stack_empty();
        assert_table(&r, &cards, true, false);
    }
    r.activate(a, 0).resolve();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    r.act(GameAction::ChooseOption {
        choice: "Even".into(),
    })
    .unwrap();
    r.advance_until_stack_empty();
    assert_eq!(
        r.state().objects[&a].chosen_attributes,
        vec![ChosenAttribute::OddOrEven(Parity::Odd)]
    );
    assert_table(&r, &cards, true, false);
}

#[test]
fn u14_control_and_new_incarnation() {
    // CR 400.7: control preserves the choice; a later incarnation chooses anew.
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let cards = population(&mut s);
    let b = standard(&mut s, P0, STANDARD);
    let a = standard(&mut s, P1, STANDARD);
    let steal = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic control",
            true,
            "Gain control of target permanent.",
        )
        .id();
    let bounce = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic bounce",
            true,
            "Return target enchantment to its owner's hand.",
        )
        .id();
    let mut r = s.build();
    pause(&mut r, b, HASTE_MODIFICATIONS);
    answer(&mut r, b, "Even");
    next_turn(&mut r, P1);
    pause(&mut r, a, HASTE_MODIFICATIONS);
    answer(&mut r, a, "Odd");
    let incarnation = r.state().objects[&a].incarnation;
    next_turn(&mut r, P0);
    r.cast(steal).target_object(a).resolve();
    assert_eq!(r.state().objects[&a].controller, P0);
    assert_eq!(r.state().objects[&a].owner, P1);
    assert_eq!(r.state().objects[&a].incarnation, incarnation);
    assert_table(&r, &cards, true, true);
    r.cast(bounce)
        .target_object(a)
        .resolve()
        .assert_zone(&[a], Zone::Hand);
    assert_table(&r, &cards, false, true);
    next_turn(&mut r, P1);
    pause(&mut r, a, HASTE_MODIFICATIONS);
    assert_table(&r, &cards, false, true);
    answer(&mut r, a, "Even");
    assert_ne!(r.state().objects[&a].incarnation, incarnation);
    assert_eq!(
        r.state().objects[&b].chosen_attributes,
        vec![ChosenAttribute::OddOrEven(Parity::Even)]
    );
    assert_table(&r, &cards, false, true);
}

#[test]
fn u14_absent_source_choice_refuses_global_fallback() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let cards = population(&mut s);
    let a = standard(&mut s, P0, STANDARD);
    let mut r = s.build();
    pause(&mut r, a, HASTE_MODIFICATIONS);
    for invalid in ["", "Neither"] {
        assert!(r
            .act(GameAction::ChooseOption {
                choice: invalid.into()
            })
            .is_err());
        assert_ne!(r.state().objects[&a].zone, Zone::Battlefield);
        assert!(r.state().pending_replacement.is_some());
    }
    answer(&mut r, a, "Odd");
    assert_table(&r, &cards, true, false);
    // Successful mandatory entry cannot omit parity. These are compatibility
    // load fixtures copied from the real reached state, with the same statics.
    for attrs in [
        vec![],
        vec![ChosenAttribute::Color(engine::types::mana::ManaColor::Blue)],
    ] {
        let mut value = serde_json::to_value(r.state()).unwrap();
        value["objects"][a.0.to_string()]["chosen_attributes"] =
            serde_json::to_value(attrs).unwrap();
        let mut state: GameState = serde_json::from_value(value).unwrap();
        state.last_named_choice = Some(ChoiceValue::OddOrEven(Parity::Even));
        engine::game::layers::mark_layers_full(&mut state);
        engine::game::layers::evaluate_layers(&mut state);
        assert_table(&GameRunner::from_state(state), &cards, false, false);
    }
}

#[test]
fn u14_haste_enables_new_creature_attack() {
    // CR 302.6 + CR 702.10b: a newly controlled matching creature can attack.
    for player in [P0, P1] {
        for matches in [true, false] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let a = standard(&mut s, player, STANDARD);
            let creature = s
                .add_creature(player, "New creature", 2, 2)
                .with_summoning_sickness()
                .with_mana_cost(ManaCost::generic(if matches { 3 } else { 2 }))
                .id();
            let guard = s
                .add_creature(player, "Matching attack reach", 2, 2)
                .with_summoning_sickness()
                .with_mana_cost(ManaCost::generic(3))
                .id();
            let mut r = s.build();
            // Initial phase-correct board for this seat, before any action.
            r.state_mut().active_player = player;
            r.state_mut().priority_player = player;
            r.state_mut().waiting_for = WaitingFor::Priority { player };
            pause(&mut r, a, HASTE_MODIFICATIONS);
            answer(&mut r, a, "Odd");
            assert_eq!(
                r.state().objects[&creature]
                    .keywords
                    .contains(&Keyword::Haste),
                matches
            );
            assert!(r.state().objects[&creature].summoning_sick);
            r.advance_to_combat();
            let result = r.declare_attackers(&[(
                creature,
                engine::game::combat::AttackTarget::Player(if player == P0 { P1 } else { P0 }),
            )]);
            assert_eq!(result.is_ok(), matches);
            if !matches {
                assert!(r.state().objects[&guard].keywords.contains(&Keyword::Haste));
                assert!(r.state().objects[&guard].summoning_sick);
                r.declare_attackers(&[(
                    guard,
                    engine::game::combat::AttackTarget::Player(if player == P0 { P1 } else { P0 }),
                )])
                .expect("matching positive attacker in the rejected board");
            }
        }
    }
}

#[test]
fn u14_existing_stored_type_static_consumer() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let elf = s
        .add_creature(P0, "Elf", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let other = s
        .add_creature(P0, "Human", 1, 1)
        .with_subtypes(vec!["Human"])
        .id();
    let opposing = s
        .add_creature(P1, "Opposing Elf", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let a = s
        .add_creature_to_hand(P0, "Adaptive Automaton", 2, 2)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .from_oracle_text(ADAPTIVE)
        .id();
    let mut r = s.build();
    r.state_mut().all_creature_types = vec!["Elf".into(), "Human".into(), "Construct".into()];
    r.cast(a).resolve();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    r.act(GameAction::ChooseOption {
        choice: "Elf".into(),
    })
    .unwrap();
    r.advance_until_stack_empty();
    let object = &r.state().objects[&a];
    assert_eq!(object.chosen_creature_type(), Some("Elf"));
    assert!(
        object.card_types.subtypes.contains(&"Construct".into())
            && object.card_types.subtypes.contains(&"Elf".into())
    );
    for (id, expected) in [(a, 2), (elf, 2), (other, 1), (opposing, 1)] {
        let object = &r.state().objects[&id];
        assert_eq!(
            (object.power, object.toughness),
            (Some(expected), Some(expected))
        );
    }
}

fn restored(r: &GameRunner, route: usize) -> GameRunner {
    let state = match route {
        0 => serde_json::from_value(serde_json::to_value(r.state()).unwrap()).unwrap(),
        1 => serde_json::from_str(&serde_json::to_string(r.state()).unwrap()).unwrap(),
        2 => {
            let saved =
                serde_json::to_string(&PersistedGameState::capture(r.state().clone())).unwrap();
            serde_json::from_str::<PersistedGameState>(&saved)
                .unwrap()
                .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
                .unwrap()
                .finalize_after_rehydration(|_| Ok(()))
                .unwrap()
        }
        3 => {
            let wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(r.state().clone()))
                    .unwrap();
            assert_eq!(wire["resolution_state_version"], 9);
            serde_json::from_value::<ResolutionStateWire>(wire)
                .unwrap()
                .into_game_state()
        }
        _ => unreachable!(),
    };
    GameRunner::from_state(state)
}

#[test]
fn u14_choice_entry_and_static_roundtrips() {
    assert_eq!(RESOLUTION_STATE_WIRE_VERSION, 9);
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let cards = population(&mut s);
    let a = standard(&mut s, P0, STANDARD);
    let mut r = s.build();
    pause(&mut r, a, HASTE_MODIFICATIONS);
    let pending = r.state().pending_replacement.as_ref().unwrap().clone();
    for route in 0..4 {
        let mut loaded = restored(&r, route);
        assert_eq!(loaded.state().pending_replacement.as_ref(), Some(&pending));
        assert!(
            matches!(&loaded.state().waiting_for, WaitingFor::NamedChoice { source: Some(source), .. } if source.binding == NamedChoiceSourceBinding::PendingEntry && source.context.is_some())
        );
        assert_ne!(loaded.state().objects[&a].zone, Zone::Battlefield);
        answer(&mut loaded, a, "Odd");
        assert_table(&loaded, &cards, true, false);
        loaded.state_mut().last_named_choice = Some(ChoiceValue::OddOrEven(Parity::Even));
        let mut reloaded = restored(&loaded, route);
        // Recompute from base characteristics, never from serialized derived haste.
        for (id, _) in &cards {
            reloaded
                .state_mut()
                .objects
                .get_mut(id)
                .unwrap()
                .keywords
                .clear();
        }
        engine::game::layers::mark_layers_full(reloaded.state_mut());
        engine::game::layers::evaluate_layers(reloaded.state_mut());
        assert_table(&reloaded, &cards, true, false);
        assert_eq!(
            reloaded.state().objects[&a].chosen_attributes,
            vec![ChosenAttribute::OddOrEven(Parity::Odd)]
        );
    }
}

#[test]
fn u14_public_choice_and_private_resume_views() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let cards = population(&mut s);
    let a = standard(&mut s, P0, STANDARD);
    let hidden = standard(&mut s, P0, STANDARD);
    let mut r = s.build();
    r.state_mut()
        .objects
        .get_mut(&hidden)
        .unwrap()
        .chosen_attributes = vec![ChosenAttribute::Color(engine::types::mana::ManaColor::Blue)];
    pause(&mut r, a, HASTE_MODIFICATIONS);
    assert_eq!(
        restored(&r, 2).state().objects[&hidden].chosen_attributes,
        vec![ChosenAttribute::Color(engine::types::mana::ManaColor::Blue)]
    );
    for public in [
        filter_state_for_viewer(r.state(), P0),
        filter_state_for_viewer(r.state(), P1),
        filter_state_for_unseated_viewer(r.state()),
    ] {
        let WaitingFor::NamedChoice {
            source: Some(source),
            options,
            ..
        } = &public.waiting_for
        else {
            panic!("visible prompt")
        };
        assert_eq!(options, &["Odd", "Even"]);
        assert!(source.context.is_none());
        assert!(public.resolution_stack.is_empty());
        let wire = serde_json::to_value(ClientGameStateRef::wrap(&public, None)).unwrap();
        assert!(wire.to_string().contains("Odd"));
    }
    for public in [
        filter_state_for_viewer(r.state(), P1),
        filter_state_for_unseated_viewer(r.state()),
    ] {
        assert!(public.pending_replacement.is_none());
        assert!(public.objects[&hidden].chosen_attributes.is_empty());
        let wire = serde_json::to_value(ClientGameStateRef::wrap(&public, None)).unwrap();
        let attrs = &wire["objects"][hidden.0.to_string()]["chosen_attributes"];
        assert!(attrs.is_null() || attrs.as_array().is_some_and(Vec::is_empty));
        assert_ne!(public.objects[&hidden].name, "Parity Standard");
    }
    let mut trusted = restored(&r, 2);
    answer(&mut trusted, a, "Odd");
    assert_table(&trusted, &cards, true, false);
    for public in [
        filter_state_for_viewer(trusted.state(), P0),
        filter_state_for_viewer(trusted.state(), P1),
        filter_state_for_unseated_viewer(trusted.state()),
    ] {
        assert_eq!(
            public.objects[&a].chosen_attributes,
            vec![ChosenAttribute::OddOrEven(Parity::Odd)]
        );
        let wire = serde_json::to_value(ClientGameStateRef::wrap(&public, None)).unwrap();
        assert!(wire.to_string().contains("OddOrEven"));
    }
}

#[test]
fn u14_turn_and_controller_subjects_have_public_consumers() {
    for conditional in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let cards = population(&mut s);
        let text = if conditional {
            "As this enchantment enters, choose odd or even.\nDuring your turn, creatures that have mana value of the chosen quality have haste."
        } else {
            STANDARD
        };
        let a = standard(&mut s, P0, text);
        let mut r = s.build();
        pause(&mut r, a, HASTE_MODIFICATIONS);
        answer(&mut r, a, "Odd");
        assert_table(&r, &cards, true, false);
        next_turn(&mut r, P1);
        assert_table(&r, &cards, !conditional, false);
        assert_eq!(
            r.state().objects[&a].chosen_attributes,
            vec![ChosenAttribute::OddOrEven(Parity::Odd)]
        );
    }
    for other in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let own = s
            .add_creature(P0, "Own matching", 1, 2)
            .with_mana_cost(ManaCost::generic(3))
            .id();
        let opponent = s
            .add_creature(P1, "Opponent matching", 1, 2)
            .with_mana_cost(ManaCost::generic(3))
            .id();
        let text = format!("As this creature enters, choose odd or even.\n{}creatures you control that each have mana value of that quality have haste.", if other { "Other " } else { "" });
        let source = s
            .add_creature_to_hand(P0, "Synthetic creature parity", 1, 2)
            .from_oracle_text(&text)
            .with_mana_cost(ManaCost::generic(3))
            .id();
        // Fund the printed source's actual cost; choice remains a natural entry pause.
        s.with_mana_pool(
            P0,
            (0..3)
                .map(|_| {
                    engine::types::mana::ManaUnit::new(
                        engine::types::mana::ManaType::Colorless,
                        source,
                        false,
                        vec![],
                    )
                })
                .collect(),
        );
        let mut r = s.build();
        pause(&mut r, source, HASTE_MODIFICATIONS);
        answer(&mut r, source, "Odd");
        assert!(r.state().objects[&own].keywords.contains(&Keyword::Haste));
        assert!(!r.state().objects[&opponent]
            .keywords
            .contains(&Keyword::Haste));
        assert_eq!(
            r.state().objects[&source]
                .keywords
                .contains(&Keyword::Haste),
            !other
        );
    }
}

const GRUUL: &str = "Riot (This creature enters with your choice of a +1/+1 counter or haste.)\nTrample\nDuring your turn, you and this creature have hexproof.";
const CLOUD: &str = "During your turn, as long as Cloud is equipped, it has double strike and indestructible. (This creature deals both first-strike and regular combat damage. Damage and effects that say \"destroy\" don't destroy this creature.)\nEquip abilities you activate that target Cloud cost {2} less to activate.";
const RAGGADRAGGA: &str = "Each creature you control with a mana ability gets +2/+2.\nWhenever a creature you control with a mana ability attacks, untap it.\nWhenever you cast a spell, if at least seven mana was spent to cast it, untap target creature. It gets +7/+7 and gains trample until end of turn.";
const WARD: &str = "Enchant creature\nAs this Aura enters, choose a color.\nWhen this Aura enters, draw a card.\nEnchanted creature has protection from the chosen color. This effect doesn't remove this Aura.";
const BLESSING: &str = "Flash\nEnchant creature\nAs this Aura enters, choose a color.\nEnchanted creature has protection from the chosen color. This effect doesn't remove Auras and Equipment you control that are already attached to it.";
const WHIP: &str = "Equipped creature has reach, \"{2}, {T}: Tap target artifact or creature,\" and \"{8}, {T}: Discover 10.\" (Exile cards from the top of your library until you exile a nonland card with mana value 10 or less. Cast it without paying its mana cost or put it into your hand. Put the rest on the bottom in a random order.)\nEquip {1}";

fn fund(s: &mut GameScenario, player: PlayerId) {
    s.with_mana_pool(
        player,
        (0..20)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(99_999),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
}

#[test]
fn u14_continuous_grammar_reference_preservation_gruul() {
    for player_target in [true, false] {
        for own_turn in [true, false] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            population(&mut s);
            let source = s
                .add_creature_to_hand(P0, "Gruul Spellbreaker", 3, 3)
                .from_oracle_text_with_keywords(&["Riot", "Trample"], GRUUL)
                .with_mana_cost(ManaCost::zero())
                .id();
            let legal = s.add_creature(P0, "Legal target sibling", 2, 3).id();
            let damage = s
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic target",
                    true,
                    "Deal 1 damage to target creature.",
                )
                .id();
            let player_spell = s
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic player target",
                    true,
                    if player_target {
                        "Target player gains 1 life."
                    } else {
                        "Deal 1 damage to target creature."
                    },
                )
                .id();
            let mut r = s.build();
            r.cast(source).resolve();
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            r.act(GameAction::ChooseReplacement { index: 0 }).unwrap();
            r.advance_until_stack_empty();
            assert_eq!(
                r.state().objects[&source]
                    .counters
                    .get(&engine::types::counter::CounterType::Plus1Plus1),
                Some(&1)
            );
            if !own_turn {
                next_turn(&mut r, P1);
            } else {
                r.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(
                r.state().objects[&source]
                    .keywords
                    .contains(&Keyword::Hexproof),
                own_turn
            );
            // The legal sibling proves ordinary target selection and resolution are reached.
            r.cast(damage).target_object(legal).resolve();
            assert_eq!(r.state().objects[&legal].damage_marked, 1);
            if own_turn {
                r.act(GameAction::PassPriority).unwrap();
            }
            // The fluent driver intentionally refuses illegal declared intents
            // before submitting them. Exercise the reducer's actual rejection.
            let card_id = r.state().objects[&player_spell].card_id;
            r.act(GameAction::CastSpell {
                object_id: player_spell,
                card_id,
                targets: vec![],
                payment_mode: Default::default(),
            })
            .unwrap();
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ));
            let target = if player_target {
                engine::types::ability::TargetRef::Player(P0)
            } else {
                engine::types::ability::TargetRef::Object(source)
            };
            let result = r.act(GameAction::ChooseTarget {
                target: Some(target),
            });
            assert_eq!(result.is_err(), own_turn);
            if own_turn {
                let legal_target = if player_target {
                    engine::types::ability::TargetRef::Player(P1)
                } else {
                    engine::types::ability::TargetRef::Object(legal)
                };
                r.act(GameAction::ChooseTarget {
                    target: Some(legal_target),
                })
                .expect("legal target after protected target refusal");
            }
            if matches!(r.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                r.act(GameAction::PassPriority).unwrap();
            }
            r.advance_until_stack_empty();
        }
    }
}

#[test]
fn u14_continuous_grammar_reference_preservation_cloud() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    population(&mut s);
    fund(&mut s, P0);
    let cloud = s
        .add_creature_to_hand(P0, "Cloud, Planet's Champion", 3, 3)
        .from_oracle_text(CLOUD)
        .with_mana_cost(ManaCost::zero())
        .id();
    let sword = s
        .add_spell_to_hand(P0, "Synthetic Equipment", false)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text("Equip {1}")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(cloud).resolve();
    r.cast(sword).resolve();
    for kw in [Keyword::DoubleStrike, Keyword::Indestructible] {
        assert!(!r.state().objects[&cloud].keywords.contains(&kw));
    }
    r.activate(sword, 0).target_object(cloud).resolve();
    for kw in [Keyword::DoubleStrike, Keyword::Indestructible] {
        assert!(r.state().objects[&cloud].keywords.contains(&kw));
    }
    next_turn(&mut r, P1);
    for kw in [Keyword::DoubleStrike, Keyword::Indestructible] {
        assert!(!r.state().objects[&cloud].keywords.contains(&kw));
    }
    assert_eq!(
        r.state().objects[&sword].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(cloud))
    );
}

#[test]
fn u14_continuous_grammar_reference_preservation_anthem_populations() {
    for mana_anthem in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let own_mana = s
            .add_creature(P0, "Mana creature", 1, 1)
            .from_oracle_text("{T}: Add {G}.")
            .id();
        let opponent_mana = s
            .add_creature(P1, "Opponent mana creature", 1, 1)
            .from_oracle_text("{T}: Add {G}.")
            .id();
        let own_vanilla = s.add_creature(P0, "Vanilla", 1, 1).id();
        let opponent_vanilla = s.add_creature(P1, "Opponent vanilla", 1, 1).id();
        let source = if mana_anthem {
            s.add_creature_to_hand(P0, "Raggadragga, Goreguts Boss", 4, 4)
                .from_oracle_text(RAGGADRAGGA)
                .with_mana_cost(ManaCost::zero())
                .id()
        } else {
            s.add_spell_to_hand(P0, "Muraganda Petroglyphs", false)
                .as_enchantment()
                .from_oracle_text("Creatures with no abilities get +2/+2.")
                .with_mana_cost(ManaCost::zero())
                .id()
        };
        let mut r = s.build();
        r.cast(source).resolve();
        for (id, boosted) in [
            (own_mana, mana_anthem),
            (opponent_mana, false),
            (own_vanilla, !mana_anthem),
            (opponent_vanilla, !mana_anthem),
        ] {
            let obj = &r.state().objects[&id];
            let expected = if boosted { 3 } else { 1 };
            assert_eq!((obj.power, obj.toughness), (Some(expected), Some(expected)));
        }
    }
}

#[test]
fn u14_continuous_grammar_reference_preservation_protection_riders() {
    use engine::game::game_object::AttachTarget;
    use engine::types::mana::ManaColor;
    for blessing in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        population(&mut s);
        let host = s.add_creature(P0, "Aura host", 2, 2).id();
        let own = s
            .add_enchantment_from_oracle(
                P0,
                "Existing own Aura",
                "Enchant creature\nEnchanted creature gets +0/+1.",
            )
            .with_subtypes(vec!["Aura"])
            .with_color(vec![ManaColor::White])
            .id();
        let opposing = s
            .add_enchantment_from_oracle(
                P1,
                "Existing opposing Aura",
                "Enchant creature\nEnchanted creature gets +0/+1.",
            )
            .with_subtypes(vec!["Aura"])
            .with_color(vec![ManaColor::White])
            .id();
        let source = s
            .add_spell_to_hand(
                P0,
                if blessing {
                    "Benevolent Blessing"
                } else {
                    "Pentarch Ward"
                },
                false,
            )
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .from_oracle_text_with_keywords(
                if blessing { &["Flash"] } else { &[] },
                if blessing { BLESSING } else { WARD },
            )
            .with_mana_cost(ManaCost::zero())
            .with_color(vec![ManaColor::White])
            .id();
        let mut r = s.build();
        for id in [own, opposing] {
            r.state_mut().objects.get_mut(&id).unwrap().attached_to =
                Some(AttachTarget::Object(host));
        }
        r.cast(source).target_object(host).resolve();
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::Color { ref excluded },
                ..
            } if excluded.is_empty()
        ));
        r.act(GameAction::ChooseOption {
            choice: "White".into(),
        })
        .unwrap();
        r.advance_until_stack_empty();
        assert_eq!(r.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(
            r.state().objects[&source].attached_to,
            Some(AttachTarget::Object(host))
        );
        assert!(r.state().objects[&host]
            .keywords
            .contains(&Keyword::Protection(
                engine::types::keywords::ProtectionTarget::Color(ManaColor::White)
            )));
        assert_eq!(
            r.state().objects[&own].zone,
            if blessing {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(r.state().objects[&opposing].zone, Zone::Graveyard);
    }
}

#[test]
fn u14_continuous_grammar_reference_preservation_whip() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    fund(&mut s, P0);
    let host = s.add_creature(P0, "Equipped host", 2, 2).id();
    let next_host = s.add_creature(P0, "Second equipped host", 2, 2).id();
    let target = s
        .add_creature(P1, "Artifact target", 1, 3)
        .as_artifact()
        .id();
    let illegal = s.add_basic_land(P1, engine::types::mana::ManaColor::Blue);
    let whip = s
        .add_spell_to_hand(P0, "Swashbuckler's Whip", false)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(WHIP)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(whip).resolve();
    r.activate(whip, 0).target_object(host).resolve();
    assert!(r.state().objects[&host].keywords.contains(&Keyword::Reach));
    let tap_index = r.state().objects[&host]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::SetTapState { .. }))
        .expect("granted tap body");
    let discover = r.state().objects[&host]
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::Discover { .. }))
        .expect("second quoted ability");
    assert!(matches!(
        discover.effect.as_ref(),
        Effect::Discover {
            mana_value_limit: engine::types::ability::QuantityExpr::Fixed { value: 10 },
            ..
        }
    ));
    r.activate(host, tap_index).target_object(target).resolve();
    assert!(r.state().objects[&target].tapped && r.state().objects[&host].tapped);
    r.activate(whip, 0).target_object(next_host).resolve();
    let index = r.state().objects[&next_host]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::SetTapState { .. }))
        .unwrap();
    r.act(GameAction::ActivateAbility {
        source_id: next_host,
        ability_index: index,
    })
    .unwrap();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    assert!(r
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(illegal))
        })
        .is_err());
    assert!(r
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(target))
        })
        .is_ok());
    if matches!(r.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        r.act(GameAction::PassPriority).unwrap();
    }
    r.advance_until_stack_empty();
}
