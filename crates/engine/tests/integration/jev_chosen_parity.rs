//! U14: a complete synthetic ordinary permanent with a durable parity reader.
//! Full Ashling's external complement-entry consumer belongs to phase 15.

use engine::game::derived_views::ClientGameStateRef;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::{
    ChoiceType, ChoiceValue, ChosenAttribute, ContinuousModification, Effect, FilterProp, Parity,
    ParitySource, TargetFilter,
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

fn pause(r: &mut GameRunner, source: ObjectId) {
    let object = &r.state().objects[&source];
    assert_eq!(object.zone, Zone::Hand);
    assert!(object
        .replacement_definitions
        .iter_all()
        .any(|d| d.execute.as_deref().is_some_and(|a| matches!(
            a.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::OddOrEven,
                persist: true,
                ..
            }
        ))));
    assert!(object
        .static_definitions
        .iter_all()
        .any(|d| d
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste
            })
            && d.affected.as_ref().is_some_and(|f| source_parity(f))));
    assert!(object
        .abilities
        .iter()
        .all(|a| !matches!(a.effect.as_ref(), Effect::Unimplemented { .. })));
    assert!(object.static_definitions.iter_all().all(|s| s.modifications.iter().all(|m| !matches!(m,
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
fn u14_public_entry_stores_parity_and_grants_haste() {
    // CR 614.12a + CR 607.2d + CR 613.1f: choose before entry; the linked
    // static grants haste in layer 6 to the live population of either player.
    for choice in ["Odd", "Even"] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let cards = population(&mut s);
        let a = standard(&mut s, P0, STANDARD);
        let mut r = s.build();
        pause(&mut r, a);
        answer(&mut r, a, choice);
        assert_table(&r, &cards, choice == "Odd", choice == "Even");
    }
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
        pause(&mut r, a);
        answer(&mut r, a, "Odd");
        next_turn(&mut r, P1);
        pause(&mut r, b);
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
    pause(&mut r, a);
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
    pause(&mut r, b);
    answer(&mut r, b, "Even");
    next_turn(&mut r, P1);
    pause(&mut r, a);
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
    pause(&mut r, a);
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
    pause(&mut r, a);
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
            pause(&mut r, a);
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
            assert_eq!(wire["resolution_state_version"], 8);
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
    assert_eq!(RESOLUTION_STATE_WIRE_VERSION, 8);
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let cards = population(&mut s);
    let a = standard(&mut s, P0, STANDARD);
    let mut r = s.build();
    pause(&mut r, a);
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
    pause(&mut r, a);
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
        pause(&mut r, a);
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
        pause(&mut r, source);
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
                choice_type: ChoiceType::Color,
                ..
            }
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
