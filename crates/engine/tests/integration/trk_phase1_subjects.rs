//! Phase 1 TRK production-path regressions. Full Oracle from the pinned Scryfall appendix.

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::ability::{ControllerRef, Effect, MultiTargetSpec, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, CounterCostChoice, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

fn finished(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
}

fn raffi(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand(P0, RAFFI_NAME, 3, 2)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .as_legendary()
        .with_subtypes(vec!["Human", "Officer"])
        .from_oracle_text_with_keywords(&["Investigate"], RAFFI)
        .id()
}

fn kim(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, KIM_NAME, 1, 2)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{W}"))
        .as_legendary()
        .with_subtypes(vec!["Human", "Officer"])
        .from_oracle_text(KIM)
        .id()
}

fn saga(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_creature_to_hand(P0, name, 0, 0)
        .with_mana_cost(parse_mtgjson_mana_cost(if name == SCIENTISTS_NAME {
            "{2}{U}"
        } else {
            "{3}{W}{U}"
        }))
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(text)
        .id()
}

fn defiant(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand(P0, DEFIANT_NAME, 5, 5)
        .with_mana_cost(ManaCost::generic(2))
        .as_artifact()
        .as_legendary()
        .with_subtypes(vec!["Spacecraft"])
        .from_oracle_text_with_keywords(&["Station", "Flash"], DEFIANT)
        .id()
}

// Use the existing cast/priority driver, stopping before its choice policy can
// supply an empty optional selection. Every caller asserts the expected prompt.
fn cast_to_trigger(runner: &mut GameRunner, card: ObjectId) {
    for color in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ] {
        for _ in 0..5 {
            runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
                color,
                ObjectId(0),
                false,
                vec![],
            ));
        }
    }
    runner.cast(card).commit();
    runner.advance_until_stack_empty();
}

fn optional_object_slot(runner: &GameRunner) -> Vec<TargetRef> {
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected announced trigger targets, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let slot = &target_slots[selection.current_slot];
    assert!(slot.optional);
    slot.legal_targets.clone()
}

// CR 201.5c: the printed short name identifies this entering source.
#[test]
fn raffi_modes_and_empty_exile_are_live_choices() {
    for (mode, pick) in [(1, false), (0, true), (0, false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = scenario
            .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
            .id();
        let source = raffi(&mut scenario);
        let other = scenario
            .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
            .id();
        let mut runner = scenario.build();
        cast_to_trigger(&mut runner, source);
        assert!(matches!(&runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { player, source_id, modal, .. }
            if *player == P0 && *source_id == source && modal.mode_count == 2));
        runner
            .act(GameAction::SelectModes {
                indices: vec![mode],
            })
            .unwrap();
        if mode == 0 {
            let legal = optional_object_slot(&runner);
            assert!(legal.contains(&TargetRef::Object(victim)));
            runner
                .act(GameAction::ChooseTarget {
                    target: pick.then_some(TargetRef::Object(victim)),
                })
                .unwrap();
        }
        runner.advance_until_stack_empty();
        finished(&runner);
        assert_eq!(runner.life(P0), if mode == 1 { 18 } else { 22 });
        assert_eq!(
            runner
                .state()
                .objects
                .values()
                .filter(|o| o.zone == Zone::Battlefield
                    && o.card_types.subtypes.iter().any(|s| s == "Clue"))
                .count(),
            usize::from(mode == 1)
        );
        assert_eq!(
            runner.state().objects[&victim].zone,
            if pick { Zone::Exile } else { Zone::Graveyard }
        );
        // CR 201.5: a different creature entering cannot retrigger Raffi.
        runner.cast(other).resolve();
        finished(&runner);
        assert_eq!(runner.life(P0), if mode == 1 { 18 } else { 22 });
    }
}

// CR 603.4: compare the entering creature to Kim at trigger and resolution.
#[test]
fn kim_greater_power_and_equal_power_bind_the_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = kim(&mut scenario);
    let high = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let equal = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let low = scenario
        .add_creature_to_hand(P0, "Llanowar Elves", 1, 1)
        .id();
    let mut runner = scenario.build();
    runner.cast(high).resolve();
    finished(&runner);
    assert_eq!(
        runner.state().objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    assert!(runner.state().objects[&high].counters.is_empty());
    for card in [equal, low] {
        runner.cast(card).resolve();
        finished(&runner);
        assert_eq!(
            runner.state().objects[&source]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&1)
        );
    }
}

// CR 603.4: a positively queued trigger rechecks its intervening condition.
#[test]
fn kim_intervening_if_rechecks_after_growth() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = kim(&mut scenario);
    let entrant = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Giant Growth",
            true,
            "Target creature gets +3/+3 until end of turn.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(entrant).commit();
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    assert_eq!(runner.state().objects[&entrant].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Kim's trigger must be queued"
    );
    runner.cast(growth).target_object(source).resolve();
    finished(&runner);
    assert_eq!(runner.state().objects[&source].power, Some(4));
    assert!(runner.state().objects[&source].counters.is_empty());
}

// CR 121.1: the unchanged activation draws only after its whole cost is paid.
#[test]
fn kim_activation_pays_counters_from_two_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = kim(&mut scenario);
    let donor = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_counter(source, CounterType::Plus1Plus1, 1);
    scenario.with_counter(donor, CounterType::Plus1Plus1, 1);
    let draw = scenario.add_card_to_library_top(P0, "Island");
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::RemoveCounter { .. },
            ..
        }
    ));
    runner
        .act(GameAction::ChooseRemoveCounterCostDistribution {
            distribution: [source, donor]
                .into_iter()
                .map(|object_id| CounterCostChoice {
                    object_id,
                    counter_type: CounterType::Plus1Plus1,
                    count: 1,
                })
                .collect(),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    finished(&runner);
    for id in [source, donor] {
        assert_eq!(
            runner.state().objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
    }
    assert_eq!(runner.state().objects[&draw].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

// CR 714.3a + CR 608.2c: entry produces chapter I, drawing before discarding.
#[test]
fn scientists_full_multiline_reminder_draws_then_discards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = saga(&mut scenario, SCIENTISTS_NAME, SCIENTISTS);
    let first = scenario.add_card_to_library_top(P0, "Island");
    let second = scenario.add_card_to_library_top(P0, "Mountain");
    let keep = scenario.add_card_to_hand(P0, "Plains");
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(source).resolve();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::DiscardChoice { player, count: 2, cards, .. }
        if *player == P0 && cards.contains(&first) && cards.contains(&second) && cards.contains(&keep))
    );
    assert_eq!(
        runner.state().objects[&source]
            .counters
            .get(&CounterType::Lore),
        Some(&1)
    );
    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
    // The live full-card parse must not leave the reminder as a rules ability.
    let object = &runner.state().objects[&source];
    assert!(
        !format!("{:?} {:?}", object.abilities, object.trigger_definitions)
            .contains("Unimplemented")
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![keep, second],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    finished(&runner);
    assert_eq!(runner.state().objects[&keep].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
}

// CR 400.3 + CR 608.2c: the target's owner draws first and receives the object.
#[test]
fn unification_owner_is_distinct_from_both_controllers() {
    for change_controller in [false, true] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = saga(&mut scenario, UNIFICATION_NAME, UNIFICATION);
        let target = scenario
            .add_creature(P1, "Grizzly Bears", 2, 2)
            .controlled_by(P2)
            .id();
        let other = scenario.add_creature(P2, "Runeclaw Bear", 2, 2).id();
        let prior = scenario
            .add_creature(P2, "Forest Bear", 2, 2)
            .controlled_by(P0)
            .id();
        let unsummon = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .id();
        let first = scenario.add_card_to_library_top(P1, "Island");
        let second = scenario.add_card_to_library_top(P1, "Mountain");
        scenario.with_library_top(P0, &["Plains", "Forest"]);
        scenario.with_library_top(P2, &["Swamp", "Wastes"]);
        let mut runner = scenario.build();
        runner.cast(unsummon).target_object(prior).resolve();
        finished(&runner);
        assert_eq!(runner.state().objects[&prior].zone, Zone::Hand);
        cast_to_trigger(&mut runner, source);
        let legal = optional_object_slot(&runner);
        assert!(legal.contains(&TargetRef::Object(target)));
        assert!(legal.contains(&TargetRef::Object(other)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        if change_controller {
            // Fixture mutation after announcement: owner stays P1.
            let object = runner.state_mut().objects.get_mut(&target).unwrap();
            object.controller = P0;
            object.base_controller = Some(P0);
        }
        let mut events = Vec::new();
        for _ in 0..3 {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        finished(&runner);
        // CR 701.24a + CR 608.2c: a real shuffle occurs after both owner draws,
        // even though the remaining library has only the returned creature.
        let shuffle = events.iter().position(|event| matches!(event, GameEvent::PlayerPerformedAction { player_id, action: PlayerActionKind::ShuffledLibrary, .. } if *player_id == P1)).expect("owner shuffles");
        let draws: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| {
                matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == P1)
                    .then_some(index)
            })
            .collect();
        assert_eq!(draws.len(), 2);
        assert!(draws.iter().all(|index| *index < shuffle));
        assert_eq!(shuffle_owners(&events), vec![P1]);
        for id in [first, second] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
        }
        assert_eq!(runner.state().objects[&target].zone, Zone::Library);
        assert_eq!(runner.state().objects[&target].owner, P1);
        assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
        assert!(runner.state().players[0].hand.is_empty());
        assert_eq!(
            runner.state().players[2]
                .hand
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![prior]
        );
        assert_eq!(
            runner.state().players[1]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![target]
        );
    }
}

// CR 608.2b: no selected owner is not the controller, and an illegal sole target
// prevents the entire chapter from resolving.
#[test]
fn unification_empty_and_illegal_targets_do_not_draw() {
    for (illegal, blink) in [(false, false), (true, false), (true, true)] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = saga(&mut scenario, UNIFICATION_NAME, UNIFICATION);
        let target = scenario
            .add_creature(P1, "Grizzly Bears", 2, 2)
            .controlled_by(P2)
            .id();
        for player in [P0, P1, P2] {
            scenario.with_library_top(player, &["Island", "Mountain"]);
        }
        let mut runner = scenario.build();
        cast_to_trigger(&mut runner, source);
        assert!(optional_object_slot(&runner).contains(&TargetRef::Object(target)));
        runner
            .act(GameAction::ChooseTarget {
                target: illegal.then_some(TargetRef::Object(target)),
            })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        if illegal {
            engine::game::zones::move_to_zone(runner.state_mut(), target, Zone::Exile, &mut vec![]);
            if blink {
                // CR 400.7: the same ObjectId in a new incarnation is not the target.
                engine::game::zones::move_to_zone(
                    runner.state_mut(),
                    target,
                    Zone::Battlefield,
                    &mut vec![],
                );
            }
        }
        let libraries: Vec<_> = runner
            .state()
            .players
            .iter()
            .map(|p| p.library.clone())
            .collect();
        let mut events = Vec::new();
        for _ in 0..3 {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        finished(&runner);
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::CardDrawn { .. }
                | GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                }
        )));
        for (player, library) in runner.state().players.iter().zip(libraries) {
            assert!(player.hand.is_empty());
            assert_eq!(player.library, library);
        }
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&target].zone,
            if illegal && !blink {
                Zone::Exile
            } else {
                Zone::Battlefield
            }
        );
    }
}

// CR 702.26b/d/g: the source and optional target phase as one batch, keeping
// their identities and counters; attachments follow their host.
#[test]
fn defiant_optional_target_and_attachments_phase_and_return() {
    defiant_optional_target_and_attachments_case(true);
}

#[test]
fn defiant_zero_choice_with_legal_options_phases_and_returns() {
    defiant_optional_target_and_attachments_case(false);
}

fn defiant_optional_target_and_attachments_case(pick: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"; 8]);
    scenario.with_library_top(P1, &["Mountain"; 8]);
    scenario.add_creature_from_oracle(P0, "Fountain Watch", 2, 4, "Artifacts and enchantments you control have shroud. (They can't be the targets of spells or abilities.)");
    let source = defiant(&mut scenario);
    let target = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_plus_counters(1)
        .id();
    let other = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let opponent = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let equipment = scenario
        .add_artifact_from_oracle(
            P0,
            "Bonesplitter",
            "Equipped creature gets +2/+0.\nEquip {1}",
        )
        .with_subtypes(vec!["Equipment"])
        .id();
    let aura = scenario
        .add_enchantment_from_oracle(
            P0,
            "Holy Strength",
            "Enchant creature\nEnchanted creature gets +1/+2.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .attached_to = Some(target.into());
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .attachments
        .push(aura);
    runner
        .state_mut()
        .objects
        .get_mut(&equipment)
        .unwrap()
        .attached_to = Some(target.into());
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .attachments
        .push(equipment);
    cast_to_trigger(&mut runner, source);
    // CR 702.18a: shroud does not prohibit the untargeted source leg.
    assert!(engine::game::keywords::has_keyword(
        &runner.state().objects[&source],
        &Keyword::Shroud
    ));
    let WaitingFor::TriggerTargetSelection { target_slots, .. } = &runner.state().waiting_for
    else {
        panic!("Defiant must announce its optional target");
    };
    assert_eq!(target_slots.len(), 1);
    let legal = optional_object_slot(&runner);
    assert!(legal.contains(&TargetRef::Object(target)));
    assert!(!legal.contains(&TargetRef::Object(opponent)));
    runner
        .act(GameAction::ChooseTarget {
            target: pick.then_some(TargetRef::Object(target)),
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    let engine::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } =
        &runner.state().stack[0].kind
    else {
        panic!("admitted Defiant trigger");
    };
    assert_eq!(ability.source_id, source);
    assert_eq!(
        ability.source_incarnation,
        Some(runner.state().objects[&source].incarnation)
    );
    let child = ability.sub_ability.as_ref().expect("PhaseOut child");
    assert_eq!(child.source_id, source);
    assert_eq!(child.source_incarnation, ability.source_incarnation);
    if !pick {
        assert!(ability.targets.is_empty());
        assert!(child.targets.is_empty());
    }
    let mut events = vec![];
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(events.iter().any(|event| matches!(event, GameEvent::EffectResolved { kind: engine::types::ability::EffectKind::PhaseOut, source_id, .. } if *source_id == source)));
    assert!(events
        .iter()
        .any(|event| matches!(event, GameEvent::StackResolved { .. })));
    finished(&runner);
    assert!(runner.state().objects[&source].is_phased_out());
    for id in [target, equipment, aura] {
        assert_eq!(runner.state().objects[&id].is_phased_out(), pick);
    }
    for id in [other, opponent] {
        assert!(!runner.state().objects[&id].is_phased_out());
    }
    for id in [source, target, equipment, aura] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    assert_eq!(
        runner.state().objects[&target]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    // Existing phase driver: park before the relevant untap, then advance
    // through it. No direct phasing invocation or resolved effect injection.
    runner.state_mut().phase = Phase::Cleanup;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_upkeep();
    assert_eq!(runner.state().active_player, P0);
    for id in [source, target, equipment, aura] {
        assert!(!runner.state().objects[&id].is_phased_out());
    }
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(target.into())
    );
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(target.into())
    );
    assert_eq!(
        runner.state().objects[&target]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    finished(&runner);
}

// CR 608.2b: an admitted target leaving invalidates the whole ability, including
// its untargeted source leg. Empty selection still resolves that source leg.
#[test]
fn defiant_no_creatures_and_illegal_target() {
    defiant_no_creatures_or_illegal_target_case(false);
}

#[test]
fn defiant_chosen_target_becoming_illegal_prevents_source_phasing() {
    defiant_no_creatures_or_illegal_target_case(true);
}

fn defiant_no_creatures_or_illegal_target_case(illegal: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = defiant(&mut scenario);
    let target = illegal.then(|| scenario.add_creature(P0, "Grizzly Bears", 2, 2).id());
    let mut runner = scenario.build();
    cast_to_trigger(&mut runner, source);
    if let Some(id) = target {
        let legal = optional_object_slot(&runner);
        assert!(legal.contains(&TargetRef::Object(id)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(id)),
            })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Exile, &mut vec![]);
        runner.advance_until_stack_empty();
    }
    finished(&runner);
    assert_eq!(runner.state().objects[&source].is_phased_out(), !illegal);
}

// CR 400.7: the queued source identity cannot bind to a new incarnation.
#[test]
fn defiant_zero_choice_stale_source_does_not_substitute_a_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Fountain Watch", 2, 4, "Artifacts and enchantments you control have shroud. (They can't be the targets of spells or abilities.)");
    let source = defiant(&mut scenario);
    let own = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let other = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    cast_to_trigger(&mut runner, source);
    assert!(engine::game::keywords::has_keyword(
        &runner.state().objects[&source],
        &Keyword::Shroud
    ));
    assert!(optional_object_slot(&runner).contains(&TargetRef::Object(own)));
    runner
        .act(GameAction::ChooseTarget { target: None })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    let engine::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } =
        &runner.state().stack[0].kind
    else {
        panic!("admitted Defiant trigger");
    };
    let old_incarnation = runner.state().objects[&source].incarnation;
    assert_eq!(ability.source_id, source);
    assert_eq!(ability.source_incarnation, Some(old_incarnation));
    assert!(ability.targets.is_empty());
    let child = ability.sub_ability.as_ref().expect("PhaseOut child");
    assert!(child.targets.is_empty());
    assert_eq!(child.source_incarnation, Some(old_incarnation));
    // Fixture-only return isolates the already admitted trigger from a new ETB.
    engine::game::zones::move_to_zone(runner.state_mut(), source, Zone::Exile, &mut vec![]);
    engine::game::zones::move_to_zone(runner.state_mut(), source, Zone::Battlefield, &mut vec![]);
    assert_ne!(runner.state().objects[&source].incarnation, old_incarnation);
    let mut events = vec![];
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    finished(&runner);
    assert!(events.iter().any(|event| matches!(event, GameEvent::EffectResolved { kind: engine::types::ability::EffectKind::PhaseOut, source_id, .. } if *source_id == source)));
    assert!(events
        .iter()
        .any(|event| matches!(event, GameEvent::StackResolved { .. })));
    for id in [source, own, other] {
        assert!(!runner.state().objects[&id].is_phased_out());
    }
}

// CR 400.7 + CR 201.5: a source that left and returned is not the old source.
#[test]
fn defiant_returned_source_does_not_replace_queued_source_identity() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = defiant(&mut scenario);
    let target = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    cast_to_trigger(&mut runner, source);
    assert!(optional_object_slot(&runner).contains(&TargetRef::Object(target)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    engine::game::zones::move_to_zone(runner.state_mut(), source, Zone::Exile, &mut vec![]);
    // Fixture return without delivering its separate ETB event isolates the
    // already-announced ability's incarnation test.
    engine::game::zones::move_to_zone(runner.state_mut(), source, Zone::Battlefield, &mut vec![]);
    runner.advance_until_stack_empty();
    finished(&runner);
    assert!(runner.state().objects[&target].is_phased_out());
    assert!(!runner.state().objects[&source].is_phased_out());
}

// CR 205.1b + CR 611.2a/c: the resolved chosen population gains an additive
// canonical subtype, and every temporary part expires at cleanup.
#[test]
fn worf_two_opponents_keep_original_types_and_expire() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_hand(P0, WORF_NAME, 3, 2)
        .as_legendary()
        .with_subtypes(vec!["Klingon", "Warrior", "Spy"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{R}"))
        .from_oracle_text(WORF)
        .id();
    let first = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    let second = scenario
        .add_creature(P2, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    let unchosen = scenario
        .add_creature(P1, "Forest Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    let ineligible = scenario
        .add_creature(P2, "Hill Giant", 3, 3)
        .with_subtypes(vec!["Giant"])
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{R}"))
        .id();
    let mut runner = scenario.build();
    for id in [first, second] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    cast_to_trigger(&mut runner, source);
    let legal = optional_object_slot(&runner);
    assert!(legal.contains(&TargetRef::Object(first)));
    assert!(legal.contains(&TargetRef::Object(unchosen)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(first)),
        })
        .unwrap();
    let legal = optional_object_slot(&runner);
    assert!(legal.contains(&TargetRef::Object(second)));
    assert!(!legal.contains(&TargetRef::Object(ineligible)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(second)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    finished(&runner);
    for id in [first, second] {
        let object = &runner.state().objects[&id];
        assert_eq!(object.controller, P0);
        assert!(!object.tapped);
        assert!(object.card_types.subtypes.iter().any(|s| s == "Warrior"));
        assert!(object.card_types.subtypes.iter().any(|s| s == "Bear"));
        assert!(engine::game::keywords::has_keyword(object, &Keyword::Haste));
    }
    for id in [unchosen, ineligible] {
        assert!(!runner.state().objects[&id]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Warrior"));
        assert!(!engine::game::keywords::has_keyword(
            &runner.state().objects[&id],
            &Keyword::Haste
        ));
    }
    runner.state_mut().phase = Phase::End;
    runner.advance_to_upkeep();
    finished(&runner);
    for (id, owner) in [(first, P1), (second, P2)] {
        let object = &runner.state().objects[&id];
        assert_eq!(object.controller, owner);
        assert_eq!(object.card_types.subtypes, vec!["Bear".to_string()]);
        assert!(!engine::game::keywords::has_keyword(
            object,
            &Keyword::Haste
        ));
    }
}

// CR 201.5: same printed name never makes another source share the trigger.
#[test]
fn two_raffi_sources_do_not_cross_bind() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Mirror Gallery", "The \"legend rule\" doesn't apply.");
    let first = raffi(&mut scenario);
    let second = raffi(&mut scenario);
    let mut runner = scenario.build();
    for source in [first, second] {
        cast_to_trigger(&mut runner, source);
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::AbilityModeChoice { source_id, .. } if source_id == source)
        );
        runner
            .act(GameAction::SelectModes { indices: vec![1] })
            .unwrap();
        runner.advance_until_stack_empty();
        finished(&runner);
    }
    assert_eq!(runner.life(P0), 16);
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield
                && o.card_types.subtypes.iter().any(|s| s == "Clue"))
            .count(),
        2
    );
}

// CR 603.4 + CR 201.5: each Kim compares the entrant against its own power.
#[test]
fn two_kims_compare_against_their_own_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Mirror Gallery", "The \"legend rule\" doesn't apply.");
    let first = kim(&mut scenario);
    let second = kim(&mut scenario);
    scenario.with_counter(second, CounterType::Plus1Plus1, 2);
    let entrant = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut runner = scenario.build();
    runner.cast(entrant).resolve();
    finished(&runner);
    assert_eq!(
        runner.state().objects[&first]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    assert_eq!(
        runner.state().objects[&second]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&2)
    );
    assert!(runner.state().objects[&entrant].counters.is_empty());
}

// CR 201.5: two same-name Defiants keep independent source identities.
#[test]
fn two_defiants_keep_separate_source_and_target_sets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = defiant(&mut scenario);
    let second = defiant(&mut scenario);
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    cast_to_trigger(&mut runner, first);
    assert!(optional_object_slot(&runner).contains(&TargetRef::Object(creature)));
    runner
        .act(GameAction::ChooseTarget { target: None })
        .unwrap();
    runner.advance_until_stack_empty();
    finished(&runner);
    assert!(runner.state().objects[&first].is_phased_out());
    assert!(!runner.state().objects[&creature].is_phased_out());
    cast_to_trigger(&mut runner, second);
    assert!(optional_object_slot(&runner).contains(&TargetRef::Object(creature)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(creature)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    finished(&runner);
    for id in [first, second, creature] {
        assert!(runner.state().objects[&id].is_phased_out());
    }
}

// CR 702.26d: selecting an animated source as its own optional target does
// not create a second object or lose the source in the composed collector.
#[test]
fn defiant_animated_source_deduplicates_self_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "March of the Machines", "Each noncreature artifact is an artifact creature with power and toughness each equal to its mana value. (Equipment that's a creature can't equip a creature.)");
    let source = defiant(&mut scenario);
    let mut runner = scenario.build();
    cast_to_trigger(&mut runner, source);
    assert!(optional_object_slot(&runner).contains(&TargetRef::Object(source)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(source)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    finished(&runner);
    assert!(runner.state().objects[&source].is_phased_out());
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
}
// SHAPE / slot seam supplement to the full-card runtime regressions above.
#[test]
fn defiant_full_oracle_has_one_optional_slot_and_no_phase_out_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Fountain Watch", 2, 4, "Artifacts and enchantments you control have shroud. (They can't be the targets of spells or abilities.)");
    let source = defiant(&mut scenario);
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let opponent = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    cast_to_trigger(&mut runner, source);
    assert!(engine::game::keywords::has_keyword(
        &runner.state().objects[&source],
        &Keyword::Shroud
    ));
    let object = &runner.state().objects[&source];
    let def = object
        .trigger_definitions
        .iter_all()
        .filter_map(|trigger| trigger.definition.execute.as_deref())
        .find(|def| matches!(def.effect.as_ref(), Effect::TargetOnly { .. }))
        .expect("full Defiant ETB starts with a target declaration");
    assert_eq!(
        def.effect.as_ref(),
        &Effect::TargetOnly {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        }
    );
    assert_eq!(def.multi_target, Some(MultiTargetSpec::fixed(0, 1)));
    let sub = def.sub_ability.as_ref().expect("compound PhaseOut");
    assert_eq!(
        sub.effect.as_ref(),
        &Effect::PhaseOut {
            target: TargetFilter::Or {
                filters: vec![TargetFilter::SelfRef, TargetFilter::ParentTarget]
            },
        }
    );
    let slots =
        build_target_slots(runner.state(), &build_resolved_from_def(def, source, P0)).unwrap();
    assert_eq!(slots.len(), 1);
    assert!(slots[0].optional);
    assert!(slots[0]
        .legal_targets
        .contains(&TargetRef::Object(creature)));
    assert!(!slots[0]
        .legal_targets
        .contains(&TargetRef::Object(opponent)));
    assert!(
        build_target_slots(runner.state(), &build_resolved_from_def(sub, source, P0))
            .unwrap()
            .is_empty()
    );
}

const REALITY_RIPPLE: &str = "Target artifact, creature, or land phases out. (While it's phased out, it's treated as though it doesn't exist. It phases in before its controller untaps during their next untap step.)";
const TEFERIS_PROTECTION: &str = "Until your next turn, your life total can't change and you gain protection from everything. All permanents you control phase out. (While they're phased out, they're treated as though they don't exist. They phase in before you untap during your untap step.)\nExile Teferi's Protection.";

// CR 608.2b + CR 702.18a: a genuine targeted phase-out requires a legal
// chosen object at announcement and rechecks that object at resolution.
#[test]
fn reality_ripple_preserves_target_selection_and_legality() {
    for illegal in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Reality Ripple", true, REALITY_RIPPLE)
            .id();
        let selected = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let other = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
        let shrouded = scenario
            .add_creature(P1, "Deadly Insect", 6, 1)
            .from_oracle_text_with_keywords(
                &["Shroud"],
                "Shroud (This creature can't be the target of spells or abilities.)",
            )
            .id();
        let enchantment = scenario
            .add_enchantment_from_oracle(P1, "Glorious Anthem", "Creatures you control get +1/+1.")
            .id();
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Reality Ripple must ask for a target");
        };
        assert_eq!(*player, P0);
        assert_eq!(target_slots.len(), 1);
        assert!(!target_slots[0].optional);
        assert!(engine::game::keywords::has_keyword(
            &runner.state().objects[&shrouded],
            &Keyword::Shroud
        ));
        let legal = &target_slots[0].legal_targets;
        for id in [selected, other] {
            assert!(legal.contains(&TargetRef::Object(id)));
        }
        for id in [shrouded, enchantment] {
            assert!(!legal.contains(&TargetRef::Object(id)));
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(selected)),
            })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        if illegal {
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                selected,
                Zone::Exile,
                &mut vec![],
            );
        }
        runner.advance_until_stack_empty();
        finished(&runner);
        assert_eq!(runner.state().objects[&selected].is_phased_out(), !illegal);
        for id in [other, shrouded, enchantment] {
            assert!(!runner.state().objects[&id].is_phased_out());
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    }

    // Same full spell as the positive board, with only prohibited candidates.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Reality Ripple", true, REALITY_RIPPLE)
        .id();
    scenario
        .add_creature(P1, "Deadly Insect", 6, 1)
        .from_oracle_text_with_keywords(
            &["Shroud"],
            "Shroud (This creature can't be the target of spells or abilities.)",
        );
    scenario.add_enchantment_from_oracle(P1, "Glorious Anthem", "Creatures you control get +1/+1.");
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    assert!(runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto
        })
        .is_err());
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
    finished(&runner);
}

// CR 115.10a + CR 702.26a/g: mass phasing affects shrouded permanents and
// attached Equipment without targeting, then restores them on the next turn.
#[test]
fn teferis_protection_preserves_mass_phasing_and_empty_population() {
    for populated in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Island"; 8]);
        scenario.with_library_top(P1, &["Mountain"; 8]);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Teferi's Protection", true, TEFERIS_PROTECTION)
            .id();
        let opponent = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let own = populated.then(|| {
            let creature = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
            let shrouded = scenario
                .add_creature(P0, "Deadly Insect", 6, 1)
                .from_oracle_text_with_keywords(
                    &["Shroud"],
                    "Shroud (This creature can't be the target of spells or abilities.)",
                )
                .id();
            let equipment = scenario
                .add_artifact_from_oracle(
                    P0,
                    "Bonesplitter",
                    "Equipped creature gets +2/+0.\nEquip {1}",
                )
                .with_subtypes(vec!["Equipment"])
                .id();
            (creature, shrouded, equipment)
        });
        let mut runner = scenario.build();
        if let Some((creature, shrouded, equipment)) = own {
            runner
                .state_mut()
                .objects
                .get_mut(&equipment)
                .unwrap()
                .attached_to = Some(creature.into());
            runner
                .state_mut()
                .objects
                .get_mut(&creature)
                .unwrap()
                .attachments
                .push(equipment);
            assert!(engine::game::keywords::has_keyword(
                &runner.state().objects[&shrouded],
                &Keyword::Shroud
            ));
        }
        runner.cast(spell).commit();
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.advance_until_stack_empty();
        finished(&runner);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
        assert!(!runner.state().objects[&opponent].is_phased_out());
        if let Some((creature, shrouded, equipment)) = own {
            for id in [creature, shrouded, equipment] {
                assert!(runner.state().objects[&id].is_phased_out());
                assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
            }
            runner.state_mut().phase = Phase::Cleanup;
            runner.state_mut().active_player = P1;
            runner.state_mut().priority_player = P1;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
            runner.advance_to_upkeep();
            assert_eq!(runner.state().active_player, P0);
            for id in [creature, shrouded, equipment] {
                assert!(!runner.state().objects[&id].is_phased_out());
            }
            assert_eq!(
                runner.state().objects[&equipment].attached_to,
                Some(creature.into())
            );
            assert!(runner.state().objects[&creature]
                .attachments
                .contains(&equipment));
            finished(&runner);
        }
    }
}

// CR 714.3c: fixture a prior chapter's counter, then let turn progression
// produce the next chapter; never inject its trigger or ability.
fn next_chapter_scenario(name: &str, text: &str, previous: u32) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Upkeep);
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Island"; 8]);
    }
    let source = scenario
        .add_creature(P0, name, 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(text)
        .id();
    scenario.with_counter(source, CounterType::Lore, previous);
    (scenario, source)
}

// CR 714.3c: later Scientists chapters preserve their original selected cards.
#[test]
fn scientists_second_chapter_returns_the_selected_card() {
    let (mut scenario, source) = next_chapter_scenario(SCIENTISTS_NAME, SCIENTISTS, 1);
    let card = scenario
        .add_spell_to_graveyard(P0, "Opt", true)
        .from_oracle_text("Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.")
        .id();
    let wrong = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().objects[&source]
            .counters
            .get(&CounterType::Lore),
        Some(&2)
    );
    let legal = optional_object_slot(&runner);
    assert!(legal.contains(&TargetRef::Object(card)));
    assert!(!legal.contains(&TargetRef::Object(wrong)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(card)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    finished(&runner);
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&wrong].zone, Zone::Graveyard);
}

// CR 400.3: chapter III routes each selected card to its respective owner.
#[test]
fn scientists_third_chapter_distinct_owners_and_no_picks() {
    for (pick, second_owner) in [(false, P2), (true, P2), (true, P1)] {
        let (mut scenario, source) = next_chapter_scenario(SCIENTISTS_NAME, SCIENTISTS, 2);
        let first = scenario
            .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
            .id();
        let second = scenario
            .add_creature_to_graveyard(second_owner, "Runeclaw Bear", 2, 2)
            .id();
        let mut runner = scenario.build();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(
            runner.state().objects[&source]
                .counters
                .get(&CounterType::Lore),
            Some(&3)
        );
        assert!(optional_object_slot(&runner).contains(&TargetRef::Object(first)));
        runner
            .act(GameAction::ChooseTarget {
                target: pick.then_some(TargetRef::Object(first)),
            })
            .unwrap();
        if pick {
            assert!(optional_object_slot(&runner).contains(&TargetRef::Object(second)));
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(second)),
                })
                .unwrap();
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().stack.len(), 1);
        let mut events = Vec::new();
        for _ in 0..3 {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        finished(&runner);
        // CR 701.24a: each selected card's owner shuffles, and no other player does.
        for player in [P0, P1, P2] {
            assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::PlayerPerformedAction { player_id, action: PlayerActionKind::ShuffledLibrary, .. } if *player_id == player)).count(), usize::from(pick && (player == P1 || player == second_owner)));
        }
        for (id, owner) in [(first, P1), (second, second_owner)] {
            assert_eq!(
                runner.state().objects[&id].zone,
                if pick { Zone::Library } else { Zone::Graveyard }
            );
            assert_eq!(
                runner.state().players[owner.0 as usize]
                    .library
                    .contains(&id),
                pick
            );
        }
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    }
}

fn shuffle_owners(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect()
}

// CR 608.2b: only still-legal selected cards contribute owners at resolution.
#[test]
fn scientists_third_chapter_filters_illegal_members_without_owner_fallback() {
    for all_illegal in [false, true] {
        let (mut scenario, _) = next_chapter_scenario(SCIENTISTS_NAME, SCIENTISTS, 2);
        let first = scenario
            .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
            .id();
        let second = scenario
            .add_creature_to_graveyard(P2, "Runeclaw Bear", 2, 2)
            .id();
        let mut runner = scenario.build();
        runner.advance_to_phase(Phase::PreCombatMain);
        for id in [first, second] {
            assert!(optional_object_slot(&runner).contains(&TargetRef::Object(id)));
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(id)),
                })
                .unwrap();
        }
        assert_eq!(runner.state().stack.len(), 1);
        engine::game::zones::move_to_zone(runner.state_mut(), first, Zone::Exile, &mut vec![]);
        if all_illegal {
            engine::game::zones::move_to_zone(runner.state_mut(), second, Zone::Exile, &mut vec![]);
        }
        let mut events = vec![];
        for _ in 0..3 {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        finished(&runner);
        assert_eq!(
            shuffle_owners(&events),
            if all_illegal { vec![] } else { vec![P2] }
        );
        assert_eq!(runner.state().objects[&first].zone, Zone::Exile);
        assert_eq!(
            runner.state().objects[&second].zone,
            if all_illegal {
                Zone::Exile
            } else {
                Zone::Library
            }
        );
    }
}

fn commander_library_prompt(runner: &GameRunner, owner: PlayerId) {
    assert!(
        matches!(runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player, candidate_count: 2, kind: engine::types::game_state::ReplacementChoiceKind::OptionalBranch, .. } if player == owner),
        "commander owner must receive optional replacement: {:?}",
        runner.state().waiting_for
    );
    let actions = engine::ai_support::legal_actions_for_viewer(runner.state(), owner).0;
    for index in [0, 1] {
        assert!(actions.iter().any(|action| matches!(action, GameAction::ChooseReplacement { index: offered } if *offered == index)));
    }
    assert!(
        engine::ai_support::legal_actions_for_viewer(runner.state(), P0)
            .0
            .is_empty()
    );
}

// CR 903.9b + CR 701.24c: both prospective owners shuffle once even when
// consecutive commander replacements redirect one or both library deliveries.
#[test]
fn scientists_third_chapter_commander_pauses_keep_distinct_owner_ledger() {
    for redirects in [[true, false], [false, true], [true, true], [false, false]] {
        let (mut scenario, _) = next_chapter_scenario(SCIENTISTS_NAME, SCIENTISTS, 2);
        let commanders: Vec<_> = [P1, P2]
            .into_iter()
            .map(|owner| {
                scenario
                    .add_creature_to_graveyard(owner, "Isamaru, Hound of Konda", 2, 2)
                    .as_legendary()
                    .with_subtypes(vec!["Dog"])
                    .with_mana_cost(parse_mtgjson_mana_cost("{W}"))
                    .from_oracle_text("")
                    .commander()
                    .id()
            })
            .collect();
        let mut runner = scenario.build();
        runner.state_mut().format_config.command_zone = true;
        runner.advance_to_phase(Phase::PreCombatMain);
        // CR 903.9a: decline each initial graveyard return before announcing
        // chapter targets; the later library replacements remain independent.
        let mut declined = vec![];
        for _ in 0..commanders.len() {
            let WaitingFor::CommanderZoneChoice {
                player,
                commander_id,
                current_zone: Zone::Graveyard,
            } = runner.state().waiting_for
            else {
                panic!(
                    "expected initial graveyard commander choice: {:?}",
                    runner.state().waiting_for
                );
            };
            assert!(commanders.contains(&commander_id));
            assert_eq!(runner.state().objects[&commander_id].owner, player);
            assert!([P1, P2].contains(&player));
            assert!(!declined.contains(&commander_id));
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .unwrap();
            declined.push(commander_id);
            assert_eq!(runner.state().objects[&commander_id].zone, Zone::Graveyard);
            runner.advance_to_phase(Phase::PreCombatMain);
        }
        for id in &commanders {
            assert_eq!(runner.state().objects[id].zone, Zone::Graveyard);
        }
        for id in &commanders {
            assert!(optional_object_slot(&runner).contains(&TargetRef::Object(*id)));
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(*id)),
                })
                .unwrap();
        }
        let mut events = vec![];
        for _ in 0..3 {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        for (index, owner) in [P1, P2].into_iter().enumerate() {
            commander_library_prompt(&runner, owner);
            assert!(
                shuffle_owners(&events).is_empty(),
                "terminal shuffle waits for every delivery"
            );
            events.extend(
                runner
                    .act(GameAction::ChooseReplacement {
                        index: usize::from(!redirects[index]),
                    })
                    .unwrap()
                    .events,
            );
        }
        finished(&runner);
        let owners = shuffle_owners(&events);
        for owner in [P1, P2] {
            assert_eq!(owners.iter().filter(|p| **p == owner).count(), 1);
        }
        assert_eq!(owners.len(), 2);
        for (index, id) in commanders.into_iter().enumerate() {
            assert_eq!(
                runner.state().objects[&id].zone,
                if redirects[index] {
                    Zone::Command
                } else {
                    Zone::Library
                }
            );
        }
    }
}

// CR 608.2c + CR 903.9b + CR 701.24c: owner draws precede the paused move,
// and redirecting the creature never removes the required terminal shuffle.
#[test]
fn unification_owner_draws_before_commander_replacement_and_shuffles_once() {
    for redirect in [false, true] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = saga(&mut scenario, UNIFICATION_NAME, UNIFICATION);
        let target = scenario
            .add_creature(P1, "Isamaru, Hound of Konda", 2, 2)
            .as_legendary()
            .with_subtypes(vec!["Dog"])
            .with_mana_cost(parse_mtgjson_mana_cost("{W}"))
            .from_oracle_text("")
            .commander()
            .controlled_by(P2)
            .id();
        scenario.with_library_top(P1, &["Island", "Mountain"]);
        let mut runner = scenario.build();
        runner.state_mut().format_config.command_zone = true;
        cast_to_trigger(&mut runner, source);
        assert!(optional_object_slot(&runner).contains(&TargetRef::Object(target)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
        let mut events = vec![];
        for _ in 0..3 {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        commander_library_prompt(&runner, P1);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::CardDrawn { player_id, .. } if *player_id == P1))
                .count(),
            2
        );
        assert_eq!(runner.state().players[1].hand.len(), 2);
        assert!(shuffle_owners(&events).is_empty());
        events.extend(
            runner
                .act(GameAction::ChooseReplacement {
                    index: usize::from(!redirect),
                })
                .unwrap()
                .events,
        );
        finished(&runner);
        assert_eq!(shuffle_owners(&events), vec![P1]);
        assert_eq!(
            runner.state().objects[&target].zone,
            if redirect {
                Zone::Command
            } else {
                Zone::Library
            }
        );
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    }
}

// CR 121.1: chapter III draws for the controller and selected opponent only.
#[test]
fn unification_third_chapter_draws_for_only_two_players() {
    let (scenario, source) = next_chapter_scenario(UNIFICATION_NAME, UNIFICATION, 2);
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().objects[&source]
            .counters
            .get(&CounterType::Lore),
        Some(&3)
    );
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("chapter III must ask for an opponent");
    };
    assert!(target_slots[selection.current_slot]
        .legal_targets
        .contains(&TargetRef::Player(P2)));
    assert!(!target_slots[selection.current_slot]
        .legal_targets
        .contains(&TargetRef::Player(P0)));
    let before: Vec<_> = runner
        .state()
        .players
        .iter()
        .map(|p| p.hand.len())
        .collect();
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P2)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    finished(&runner);
    for (index, delta) in [2, 0, 2].into_iter().enumerate() {
        assert_eq!(
            runner.state().players[index].hand.len(),
            before[index] + delta
        );
    }
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
}

// CR 608.2d: the controller may deploy a Spacecraft from either printed zone,
// or decline without moving either of the offered cards.
#[test]
fn unification_second_chapter_accepts_each_zone_and_declines() {
    for choice in [None, Some(Zone::Hand), Some(Zone::Graveyard)] {
        let (mut scenario, source) = next_chapter_scenario(UNIFICATION_NAME, UNIFICATION, 1);
        let hand = defiant(&mut scenario);
        let grave = defiant(&mut scenario);
        let opponent = scenario
            .add_creature_to_hand(P1, DEFIANT_NAME, 5, 5)
            .as_artifact()
            .as_legendary()
            .with_subtypes(vec!["Spacecraft"])
            .with_mana_cost(ManaCost::generic(2))
            .from_oracle_text_with_keywords(&["Station", "Flash"], DEFIANT)
            .id();
        let mut runner = scenario.build();
        engine::game::zones::move_to_zone(runner.state_mut(), grave, Zone::Graveyard, &mut vec![]);
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(
            runner.state().objects[&source]
                .counters
                .get(&CounterType::Lore),
            Some(&2)
        );
        runner.advance_until_stack_empty();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player, source_id, .. } if player == P0 && source_id == source)
        );
        runner
            .act(GameAction::DecideOptionalEffect {
                accept: choice.is_some(),
            })
            .unwrap();
        if let Some(zone) = choice {
            let WaitingFor::EffectZoneChoice { player, cards, .. } = &runner.state().waiting_for
            else {
                panic!(
                    "accepted deployment must offer a card choice: {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(*player, P0);
            assert!(cards.contains(&hand) && cards.contains(&grave));
            assert_eq!(cards.len(), 2);
            assert!(!cards.contains(&opponent));
            // CR 400.2: inspect actual viewer projections, including ordinary
            // own-hand access and public graveyard visibility.
            let own_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P0);
            let opponent_view =
                engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
            assert_eq!(own_view.objects[&hand].name, DEFIANT_NAME);
            assert_eq!(
                own_view.objects[&hand].card_id,
                runner.state().objects[&hand].card_id
            );
            assert_eq!(opponent_view.objects[&hand].name, "Hidden Card");
            assert_eq!(
                opponent_view.objects[&hand].card_id,
                engine::types::identifiers::CardId(0)
            );
            for view in [&own_view, &opponent_view] {
                assert_eq!(view.objects[&grave].name, DEFIANT_NAME);
                assert_eq!(
                    view.objects[&grave].card_id,
                    runner.state().objects[&grave].card_id
                );
            }
            assert!(
                engine::ai_support::legal_actions_for_viewer(runner.state(), P1)
                    .0
                    .is_empty()
            );
            let selected = if zone == Zone::Hand { hand } else { grave };
            runner
                .act(GameAction::SelectCards {
                    cards: vec![selected],
                })
                .unwrap();
            runner.advance_until_stack_empty();
            // The deployed full Defiant emits its own real ETB, proving the
            // chapter continuation finished and handed control to that trigger.
            assert_eq!(runner.state().objects[&selected].zone, Zone::Battlefield);
            assert!(runner.state().objects[&selected].is_phased_out());
            let untouched = if zone == Zone::Hand { grave } else { hand };
            assert_eq!(
                runner.state().objects[&untouched].zone,
                if zone == Zone::Hand {
                    Zone::Graveyard
                } else {
                    Zone::Hand
                }
            );
        } else {
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().objects[&hand].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&grave].zone, Zone::Graveyard);
        }
        finished(&runner);
        assert_eq!(runner.state().objects[&opponent].zone, Zone::Hand);
        let own_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P0);
        let opponent_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
        assert_eq!(own_view.objects[&hand].name, DEFIANT_NAME);
        assert_eq!(
            own_view.objects[&hand].card_id,
            runner.state().objects[&hand].card_id
        );
        for view in [&own_view, &opponent_view] {
            assert_eq!(view.objects[&grave].name, DEFIANT_NAME);
            assert_eq!(
                view.objects[&grave].card_id,
                runner.state().objects[&grave].card_id
            );
        }
        if choice != Some(Zone::Hand) {
            assert_eq!(opponent_view.objects[&hand].name, "Hidden Card");
            assert_eq!(
                opponent_view.objects[&hand].card_id,
                engine::types::identifiers::CardId(0)
            );
        } else {
            assert_eq!(opponent_view.objects[&hand].name, DEFIANT_NAME);
            assert_eq!(
                opponent_view.objects[&hand].card_id,
                runner.state().objects[&hand].card_id
            );
        }
    }
}

// CR 608.2d: accepting a singleton union choice uses that object's actual
// origin; accepting an empty population finishes without inventing a target.
#[test]
fn unification_second_chapter_singletons_and_empty_population() {
    for origin in [Some(Zone::Hand), Some(Zone::Graveyard), None] {
        let (mut scenario, source) = next_chapter_scenario(UNIFICATION_NAME, UNIFICATION, 1);
        let selected = origin.map(|_| defiant(&mut scenario));
        let mut runner = scenario.build();
        if origin == Some(Zone::Graveyard) {
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                selected.unwrap(),
                Zone::Graveyard,
                &mut vec![],
            );
        }
        runner.advance_to_phase(Phase::PreCombatMain);
        assert!(!matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ));
        runner.advance_until_stack_empty();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == source)
        );
        let result = runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        if let Some(id) = selected {
            assert!(result.events.iter().any(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, from, to: Zone::Battlefield, .. }
                if *object_id == id && *from == origin)));
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        }
        runner.advance_until_stack_empty();
        finished(&runner);
        if let Some(id) = selected {
            assert!(runner.state().objects[&id].is_phased_out());
        }
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    }
}

// CR 608.2d: only a card in the offered Hand/Graveyard population may be chosen.
#[test]
fn unification_second_chapter_rejects_non_offered_exile_card() {
    for origin in [Zone::Hand, Zone::Graveyard] {
        let (mut scenario, _) = next_chapter_scenario(UNIFICATION_NAME, UNIFICATION, 1);
        let hand = defiant(&mut scenario);
        let grave = defiant(&mut scenario);
        let exiled = defiant(&mut scenario);
        let mut runner = scenario.build();
        engine::game::zones::move_to_zone(runner.state_mut(), grave, Zone::Graveyard, &mut vec![]);
        engine::game::zones::move_to_zone(runner.state_mut(), exiled, Zone::Exile, &mut vec![]);
        runner.advance_to_phase(Phase::PreCombatMain);
        runner.advance_until_stack_empty();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ));
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        let WaitingFor::EffectZoneChoice { player, cards, .. } = &runner.state().waiting_for else {
            panic!(
                "expected deployment choice: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(cards.len(), 2);
        assert!(cards.contains(&hand) && cards.contains(&grave));
        assert!(!cards.contains(&exiled));
        let waiting = runner.state().waiting_for.clone();
        let stack_len = runner.state().stack.len();
        assert!(runner
            .act(GameAction::SelectCards {
                cards: vec![exiled]
            })
            .is_err());
        assert_eq!(runner.state().waiting_for, waiting);
        assert_eq!(runner.state().stack.len(), stack_len);
        assert_eq!(runner.state().objects[&hand].zone, Zone::Hand);
        assert_eq!(runner.state().objects[&grave].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&exiled].zone, Zone::Exile);
        let selected = if origin == Zone::Hand { hand } else { grave };
        let result = runner
            .act(GameAction::SelectCards {
                cards: vec![selected],
            })
            .unwrap();
        assert!(result.events.iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, from, to: Zone::Battlefield, .. }
        if *object_id == selected && *from == Some(origin))));
        runner.advance_until_stack_empty();
        finished(&runner);
        assert_eq!(runner.state().objects[&exiled].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&selected].zone, Zone::Battlefield);
        assert!(runner.state().objects[&selected].is_phased_out());
    }
}

// CR 611.2c: declining one opponent's legal choice cannot broaden the tracked
// population, and an opponent with no eligible creature supplies no substitute.
#[test]
fn worf_decline_and_empty_opponent_keep_unselected_creatures() {
    for empty in [false, true] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_to_hand(P0, WORF_NAME, 3, 2)
            .as_legendary()
            .with_subtypes(vec!["Klingon", "Warrior", "Spy"])
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{R}"))
            .from_oracle_text(WORF)
            .id();
        let selected = scenario
            .add_creature(P1, "Grizzly Bears", 2, 2)
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
            .id();
        let power = if empty { 3 } else { 2 };
        let unselected = scenario
            .add_creature(
                P2,
                if empty { "Hill Giant" } else { "Runeclaw Bear" },
                power,
                power,
            )
            .with_subtypes(vec![if empty { "Giant" } else { "Bear" }])
            .with_mana_cost(parse_mtgjson_mana_cost(if empty {
                "{3}{R}"
            } else {
                "{1}{G}"
            }))
            .id();
        let mut runner = scenario.build();
        cast_to_trigger(&mut runner, source);
        assert!(optional_object_slot(&runner).contains(&TargetRef::Object(selected)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(selected)),
            })
            .unwrap();
        if !empty {
            let legal = optional_object_slot(&runner);
            assert!(legal.contains(&TargetRef::Object(unselected)));
            runner
                .act(GameAction::ChooseTarget { target: None })
                .unwrap();
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().stack.len(), 1);
        runner.advance_until_stack_empty();
        finished(&runner);
        assert_eq!(runner.state().objects[&selected].controller, P0);
        assert!(runner.state().objects[&selected]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Warrior"));
        assert_eq!(runner.state().objects[&unselected].controller, P2);
        assert!(!runner.state().objects[&unselected]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Warrior"));
        assert!(!engine::game::keywords::has_keyword(
            &runner.state().objects[&unselected],
            &Keyword::Haste
        ));
    }
}

// CR 611.2c + CR 201.5: a second source selects a fresh set; neither source
// can include an unchosen creature via a previous tracked-object population.
#[test]
fn two_worfs_do_not_broaden_the_selected_population() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Mirror Gallery", "The \"legend rule\" doesn't apply.");
    let sources: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_creature_to_hand(P0, WORF_NAME, 3, 2)
                .as_legendary()
                .with_subtypes(vec!["Klingon", "Warrior", "Spy"])
                .with_mana_cost(parse_mtgjson_mana_cost("{2}{R}"))
                .from_oracle_text(WORF)
                .id()
        })
        .collect();
    let selected: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_creature(P1, "Grizzly Bears", 2, 2)
                .with_subtypes(vec!["Bear"])
                .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
                .id()
        })
        .collect();
    let unchosen = scenario
        .add_creature(P1, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    let mut runner = scenario.build();
    for (index, source) in sources.into_iter().enumerate() {
        cast_to_trigger(&mut runner, source);
        let legal = optional_object_slot(&runner);
        assert!(legal.contains(&TargetRef::Object(selected[index])));
        assert!(legal.contains(&TargetRef::Object(unchosen)));
        if index == 1 {
            assert!(!legal.contains(&TargetRef::Object(selected[0])));
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(selected[index])),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        finished(&runner);
        assert_eq!(runner.state().objects[&selected[index]].controller, P0);
        assert!(runner.state().objects[&selected[index]]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Warrior"));
        assert_eq!(runner.state().objects[&unchosen].controller, P1);
        assert!(!runner.state().objects[&unchosen]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Warrior"));
    }
}

const RAFFI_NAME: &str = "Commander Raffi Musiker";
const RAFFI: &str = r#"When Commander Musiker enters, choose one —
• Exile up to one target card from a graveyard. You gain 2 life.
• Investigate. You lose 2 life. (Create a Clue token. It's an artifact with "{2}, Sacrifice this token: Draw a card.")"#;

const KIM_NAME: &str = "Ensign Harry Kim, Operations";
const KIM: &str = r#"Whenever another creature you control enters, if it has greater power than Ensign Kim, put a +1/+1 counter on Ensign Kim.
{2}, Remove two +1/+1 counters from among creatures you control: Draw a card."#;

const SCIENTISTS_NAME: &str = "Those Old Scientists";
const SCIENTISTS: &str = r#"(As this Saga enters and after your draw
step, add a lore counter. Sacrifice after III.)
I — Draw two cards, then discard two cards.
II — Return up to one target artifact, instant, or sorcery card from your graveyard to your hand.
III — Choose up to two target cards in graveyards. The owners of those cards shuffle them into their libraries."#;

const UNIFICATION_NAME: &str = "Unification";
const UNIFICATION: &str = r#"(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)
I — The owner of up to one target creature draws two cards, then shuffles that creature into their library.
II — You may put a Spacecraft card from your hand or graveyard onto the battlefield.
III — You and target opponent each draw two cards."#;

const DEFIANT_NAME: &str = "U.S.S. Defiant, Mirror Migrant";
const DEFIANT: &str = r#"Flash
When U.S.S. Defiant enters, it and up to one target creature you control phase out. (Treat them and anything attached to them as though they don't exist until your next turn.)
Station
8+ | Flying"#;

const WORF_NAME: &str = "Worf, Intelligence Contractor";
const WORF: &str = r#"When Worf enters, for each opponent, gain control of up to one target creature that player controls with mana value 3 or less until end of turn. Untap those creatures. Until end of turn, they become Warriors in addition to their other types and gain haste."#;
