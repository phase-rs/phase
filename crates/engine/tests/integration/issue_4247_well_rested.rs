//! Regression for issue #4247: Well Rested's granted untap trigger is
//! controlled by the *enchanted creature's* controller, not the Aura's
//! controller.
//!
//! Oracle text:
//!   Enchant creature
//!   Enchanted creature has "Whenever this creature becomes untapped, put two
//!   +1/+1 counters on it, then you gain 2 life and draw a card. This ability
//!   triggers only once each turn."
//!
//! CR 109.5 + CR 113.7/113.8 + CR 603.3a: a triggered ability's controller is
//! the player who controlled its source when it triggered. The granted ability
//! is an ability *of the enchanted creature*, so the enchanted creature's
//! controller is the "you" that gains life and draws — even when an opponent
//! controls the Aura. CR 303.4e only carves out *activated* abilities for the
//! Aura's controller; granted triggered abilities follow the host.
//!
//! So with P0's Well Rested attached to P1's creature, P1 (the host's
//! controller) gains life and draws; P0 (the Aura's controller) does not.

use engine::analysis::resource::loop_states_equal_modulo_resources;
use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::types::ability::{
    occurrence_for_grant, TargetRef, TriggerDefinitionRef, TriggerGrantInstanceRef,
    TriggerGrantProducerKey, TriggerOccurrenceState, TriggerProducerOrigin,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

const WELL_RESTED_ORACLE: &str = "Enchant creature\nEnchanted creature has \
\"Whenever this creature becomes untapped, put two +1/+1 counters on it, \
then you gain 2 life and draw a card. This ability triggers only once each turn.\"";

#[test]
fn well_rested_granted_untap_trigger_routes_to_host_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The host's controller (P1) is the "you" that draws — seed P1's library.
    scenario.with_library_top(P1, &["Forest"]);

    let host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let well_rested = {
        let mut builder = scenario.add_creature(P0, "Well Rested", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura"]);
        builder.from_oracle_text(WELL_RESTED_ORACLE);
        builder.id()
    };

    let mut runner = scenario.build();

    // Attach P0's Well Rested to P1's creature.
    {
        let state = runner.state_mut();
        let aura_obj = state.objects.get_mut(&well_rested).unwrap();
        aura_obj.attached_to = Some(AttachTarget::Object(host));
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .attachments
            .push(well_rested);
    }
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), host);

    runner.state_mut().objects.get_mut(&host).unwrap().tapped = true;

    let p0_life_before = runner.life(P0);
    let p1_life_before = runner.life(P1);
    let p0_hand_before = runner.state().players[0].hand.len();
    let p1_hand_before = runner.state().players[1].hand.len();

    let events = vec![GameEvent::PermanentUntapped { object_id: host }];
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before + 2,
        "the enchanted creature's controller (P1) is the granted ability's \"you\" and gains 2 life"
    );
    assert_eq!(
        runner.state().players[1].hand.len(),
        p1_hand_before + 1,
        "the enchanted creature's controller (P1) draws from the granted trigger"
    );
    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "the Aura's controller (P0) must NOT gain life from a granted triggered ability (CR 303.4e)"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        p0_hand_before,
        "the Aura's controller (P0) must NOT draw from a granted triggered ability"
    );
}

const SIXTH_SENSE_ORACLE: &str = "Enchant creature\nEnchanted creature has \
\"Whenever this creature deals combat damage to a player, you may draw a card.\"";
const EMERALD_CHARM_ORACLE: &str = "Choose one —\n• Untap target permanent.\n\
• Destroy target non-Aura enchantment.\n• Target creature loses flying until end of turn.";
const FLICKER_OF_FATE_ORACLE: &str = "Exile target creature or enchantment, then \
return it to the battlefield under its owner's control.";

fn current_aura_grant(
    state: &GameState,
    host: ObjectId,
    aura: ObjectId,
) -> (TriggerGrantProducerKey, TriggerDefinitionRef) {
    let object = &state.objects[&host];
    let grants = object
        .trigger_occurrence_state
        .active_grants()
        .filter(|(producer, _)| {
            matches!(
                producer,
                TriggerGrantProducerKey::Granted {
                    origin: TriggerProducerOrigin::Static { source, .. },
                    ..
                } if source.object_id == aura
                    && source.incarnation == state.objects[&aura].incarnation
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(grants.len(), 1, "one current producer from this live Aura");
    let (producer, generation) = grants[0];
    let occurrence = occurrence_for_grant(producer, generation);
    let entry = object
        .trigger_definitions
        .iter_all()
        .find(|entry| entry.occurrence == occurrence)
        .expect("active allocator metadata must agree with a derived live trigger");
    (producer.clone(), object.trigger_definition_ref(entry))
}

fn assert_well_rested_completion(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must finish, not suspend at an unanswered continuation"
    );
}

fn real_charm_untap(
    runner: &mut GameRunner,
    host: ObjectId,
    charm: ObjectId,
    forest: ObjectId,
    payoffs: u32,
) {
    assert!(runner.state().objects[&host].tapped);
    assert!(!runner.state().objects[&forest].tapped);
    let counters_before = runner.state().objects[&host]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    let hand_before = runner.state().players[1].hand.len();
    runner.activate(forest, 0).resolve();
    assert!(runner.state().objects[&forest].tapped);
    let committed = runner.cast(charm).modes(&[0]).target_object(host).commit();
    let spell = committed
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == charm)
        .expect("the real, paid mode-0 Charm must reach the stack");
    assert_eq!(spell.controller, P1);
    assert_eq!(
        spell.ability().expect("Charm has a resolution").targets,
        vec![TargetRef::Object(host)],
        "validate the selected target even if the casting pipeline auto-selected it"
    );
    let outcome = committed.resolve();
    // CR 603.2g: admission must come from the actual untap, not an injected event.
    assert!(outcome.events().iter().any(
        |event| matches!(event, GameEvent::PermanentUntapped { object_id } if *object_id == host)
    ));
    assert!(!outcome.state().objects[&host].tapped);
    outcome.assert_zone(&[charm], engine::types::zones::Zone::Graveyard);
    outcome.assert_life_delta(P1, 2 * payoffs as i32);
    outcome.assert_hand_drawn(P1, payoffs as i64);
    outcome.assert_life_delta(P0, 0);
    outcome.assert_hand_drawn(P0, 0);
    assert_eq!(
        outcome.state().objects[&host]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        counters_before + 2 * payoffs
    );
    assert_eq!(
        outcome.state().players[1].hand.len(),
        hand_before - 1 + payoffs as usize,
        "one actual Charm spent, independently of cards drawn by Well Rested"
    );
    assert_well_rested_completion(runner);
}

/// Controlled IDENTITY-LAYOUT fixture, NOT independently played allocation
/// history. Producer keys and ordinary game data share one real post-payoff
/// frame; only recipient-local generations and the spent occurrence change.
/// Raw layouts are resumed below to probe the gate; comparands never are.
fn controlled_well_rested_layout(
    common: &GameState,
    host: ObjectId,
    well_rested: ObjectId,
    sixth_sense: ObjectId,
    wr_generation: u64,
    ss_generation: u64,
    spent_generation: u64,
) -> GameState {
    let (wr_producer, wr_ref) = current_aura_grant(common, host, well_rested);
    let (ss_producer, ss_ref) = current_aura_grant(common, host, sixth_sense);
    let object = &common.objects[&host];
    let definition = |reference: &TriggerDefinitionRef| {
        object
            .trigger_definitions
            .iter_all()
            .find(|entry| entry.occurrence == reference.occurrence)
            .expect("captured real definition")
            .definition
            .clone()
    };
    let mut occurrences = TriggerOccurrenceState::default();
    let last_allocated = wr_generation
        .max(ss_generation)
        .max(spent_generation)
        .max(13);
    for generation in 1..=last_allocated {
        let allocated = if generation == ss_generation {
            occurrences.grant_instance_for(ss_producer.clone()).unwrap()
        } else if generation == wr_generation || generation == spent_generation {
            let allocated = occurrences.grant_instance_for(wr_producer.clone()).unwrap();
            if generation != wr_generation {
                occurrences
                    .reconcile_grant_instances(vec![(ss_producer.clone(), ())])
                    .expect("retire the old WR, retaining the same live Sixth Sense");
            }
            allocated
        } else {
            occurrences.allocate_trigger_grant_instance().unwrap()
        };
        assert_eq!(allocated, TriggerGrantInstanceRef(generation));
    }
    // Exercise the real typed reconciler, not serialized private allocator fields.
    let entries = occurrences
        .reconcile_trigger_entries(vec![
            (wr_producer.clone(), definition(&wr_ref)),
            (ss_producer.clone(), definition(&ss_ref)),
        ])
        .unwrap();
    assert_eq!(
        occurrences
            .clone()
            .allocate_trigger_grant_instance()
            .unwrap(),
        TriggerGrantInstanceRef(last_allocated + 1),
        "the next clock is strictly greater than every allocated generation"
    );
    let mut frame = common.clone();
    let object = frame.objects.get_mut(&host).unwrap();
    object.trigger_occurrence_state = occurrences;
    object.trigger_definitions = entries.into();
    assert_eq!(frame.triggers_fired_this_turn.len(), 1);
    frame.triggers_fired_this_turn.clear();
    frame.triggers_fired_this_turn.insert(TriggerDefinitionRef {
        source: wr_ref.source,
        occurrence: occurrence_for_grant(&wr_producer, TriggerGrantInstanceRef(spent_generation)),
    });
    // Refresh both caches and derived entries after the controlled metadata edit.
    evaluate_layers(&mut frame);
    reindex_object_triggers(&mut frame, host);
    let (current_wr_producer, current_wr) = current_aura_grant(&frame, host, well_rested);
    let (current_ss_producer, current_ss) = current_aura_grant(&frame, host, sixth_sense);
    assert_eq!(current_wr_producer, wr_producer);
    assert_eq!(current_ss_producer, ss_producer);
    assert_eq!(
        current_wr.occurrence,
        occurrence_for_grant(&wr_producer, TriggerGrantInstanceRef(wr_generation))
    );
    assert_eq!(
        current_ss.occurrence,
        occurrence_for_grant(&ss_producer, TriggerGrantInstanceRef(ss_generation))
    );
    assert_eq!(current_wr.source, wr_ref.source);
    assert_eq!(current_ss.source, ss_ref.source);
    assert_eq!(
        frame.objects[&host]
            .trigger_occurrence_state
            .active_grants()
            .count(),
        2
    );
    frame
}

#[test]
fn loop_relational_well_rested_real_untaps_distinguish_current_spent_from_retired_grants() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    let host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    // A second legal host makes the returned Aura's choice mandatory/observable.
    let other_host = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let well_rested = scenario
        .add_creature(P1, "Well Rested", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], WELL_RESTED_ORACLE)
        .id();
    let sixth_sense = scenario
        .add_creature(P1, "Sixth Sense", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], SIXTH_SENSE_ORACLE)
        .id();
    let charms = std::array::from_fn::<_, 3, _>(|_| {
        scenario
            .add_spell_to_hand_from_oracle(P1, "Emerald Charm", true, EMERALD_CHARM_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            })
            .id()
    });
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P1, "Flicker of Fate", true, FLICKER_OF_FATE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .id();
    let forests = std::array::from_fn::<_, 3, _>(|_| scenario.add_basic_land(P1, ManaColor::Green));
    let plains = std::array::from_fn::<_, 2, _>(|_| scenario.add_basic_land(P1, ManaColor::White));
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        for aura in [well_rested, sixth_sense] {
            state.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(host));
            state.objects.get_mut(&host).unwrap().attachments.push(aura);
        }
        // Controlled tapping is setup only; the real Charm below produces the untap.
        state.objects.get_mut(&host).unwrap().tapped = true;
        evaluate_layers(state);
        reindex_object_triggers(state, host);
    }
    assert_eq!(
        runner.state().objects[&host]
            .trigger_occurrence_state
            .active_grants()
            .count(),
        2
    );
    let (_, first_wr) = current_aura_grant(runner.state(), host, well_rested);
    let (_, first_ss) = current_aura_grant(runner.state(), host, sixth_sense);
    assert_ne!(
        first_wr, first_ss,
        "independent WR and Sixth Sense instances"
    );
    assert!(runner.state().triggers_fired_this_turn.is_empty());
    real_charm_untap(&mut runner, host, charms[0], forests[0], 1);
    assert_eq!(runner.life(P1), 22);
    assert_eq!(runner.state().players[1].library.len(), 2);
    assert_eq!(runner.state().triggers_fired_this_turn.len(), 1);
    assert!(runner.state().triggers_fired_this_turn.contains(&first_wr));
    assert!(!runner.state().triggers_fired_this_turn.contains(&first_ss));
    let common = runner.state().clone();

    // Controlled re-tapping is not claimed as a producer of an untap event.
    runner.state_mut().objects.get_mut(&host).unwrap().tapped = true;
    real_charm_untap(&mut runner, host, charms[1], forests[1], 0);
    assert_eq!(
        current_aura_grant(runner.state(), host, well_rested).1,
        first_wr
    );
    assert_eq!(runner.state().triggers_fired_this_turn.len(), 1);
    assert!(runner.state().triggers_fired_this_turn.contains(&first_wr));
    assert_eq!(runner.state().players[1].library.len(), 2);

    let aura_epoch = runner.state().objects[&well_rested].incarnation;
    for plains in plains {
        assert!(!runner.state().objects[&plains].tapped);
        runner.activate(plains, 0).resolve();
        assert!(runner.state().objects[&plains].tapped);
    }
    let outcome = runner.cast(flicker).target_object(well_rested).resolve();
    // CR 303.4f: the returning Aura's controller chooses its legal enchant host.
    match outcome.final_waiting_for() {
        WaitingFor::ReturnAsAuraTarget {
            player,
            returned_id,
            legal_targets,
            ..
        } => {
            assert_eq!(*player, P1);
            assert_eq!(*returned_id, well_rested);
            assert!(legal_targets.contains(&TargetRef::Object(host)));
            assert!(legal_targets.contains(&TargetRef::Object(other_host)));
        }
        waiting => panic!("Flicker must reach its mandatory Aura reattachment choice: {waiting:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host)),
        })
        .expect("reattach Well Rested to the same live host");
    runner.advance_until_stack_empty();
    assert_well_rested_completion(&runner);
    assert_eq!(
        runner.state().objects[&flicker].zone,
        engine::types::zones::Zone::Graveyard
    );
    assert_eq!(
        runner.state().objects[&well_rested].attached_to,
        Some(AttachTarget::Object(host))
    );
    assert!(runner.state().objects[&host]
        .attachments
        .contains(&well_rested));
    // CR 400.7: the returned Aura is a new object; CR 113.2c: its grant is independent.
    assert_ne!(runner.state().objects[&well_rested].incarnation, aura_epoch);
    let (_, reminted_wr) = current_aura_grant(runner.state(), host, well_rested);
    assert_eq!(
        runner.state().objects[&host]
            .trigger_occurrence_state
            .active_grants()
            .count(),
        2,
        "blink retires the old WR producer instead of accumulating a third active grant"
    );
    assert_ne!(reminted_wr.occurrence, first_wr.occurrence);
    assert_eq!(
        reminted_wr.source, first_wr.source,
        "the host itself never blinked"
    );
    assert_eq!(
        current_aura_grant(runner.state(), host, sixth_sense).1,
        first_ss
    );
    assert!(runner.state().triggers_fired_this_turn.contains(&first_wr));
    assert!(!runner
        .state()
        .triggers_fired_this_turn
        .contains(&reminted_wr));
    runner.state_mut().objects.get_mut(&host).unwrap().tapped = true;
    real_charm_untap(&mut runner, host, charms[2], forests[2], 1);
    assert_eq!(runner.life(P1), 24);
    assert_eq!(
        runner.state().objects[&host]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&4)
    );
    assert_eq!(runner.state().players[1].library.len(), 1);
    assert_eq!(
        runner.state().players[1].hand.len(),
        2,
        "four spells spent, two cards drawn"
    );
    assert_eq!(runner.state().triggers_fired_this_turn.len(), 2);
    assert!(runner.state().triggers_fired_this_turn.contains(&first_wr));
    assert!(runner
        .state()
        .triggers_fired_this_turn
        .contains(&reminted_wr));
    assert!(!runner.state().triggers_fired_this_turn.contains(&first_ss));
    assert!(forests
        .iter()
        .all(|land| runner.state().objects[land].tapped));

    // The actual spent/reminted runs above prove the future gate before any
    // refusal assertion. Do NOT compare those runs: spell zones, mana, life,
    // counters and cards differ. These twins share one real captured frame.
    // Strict normalization, numeric-rank collisions and strict refusal are
    // covered by the private unit tests; this integration uses only the public
    // resource comparator, including every shifted/unspent true control.
    let layout = |wr, ss, spent| {
        controlled_well_rested_layout(&common, host, well_rested, sixth_sense, wr, ss, spent)
    };
    let live_spent = layout(10, 11, 10);
    let retired_spent = layout(13, 9, 12);
    let same_raw_spent_remint = layout(13, 9, 10);
    for (raw, payoffs) in [
        (&live_spent, 0),
        (&retired_spent, 1),
        (&same_raw_spent_remint, 1),
    ] {
        // Controlled tapping is setup; the paid Charm produces the actual
        // untap. These are RAW layouts, never normalized/projected comparands.
        let mut future = GameRunner::from_state(raw.clone());
        let current_wr = current_aura_grant(future.state(), host, well_rested).1;
        future.state_mut().objects.get_mut(&host).unwrap().tapped = true;
        real_charm_untap(&mut future, host, charms[1], forests[1], payoffs);
        assert_eq!(
            current_aura_grant(future.state(), host, well_rested).1,
            current_wr,
            "untapping does not remint the selected current grant"
        );
        assert!(future
            .state()
            .triggers_fired_this_turn
            .contains(&current_wr));
        assert_eq!(
            future.state().players[1].library.len(),
            2 - payoffs as usize
        );
    }
    for (a, b) in [
        (&live_spent, layout(110, 111, 110)),
        (&retired_spent, layout(113, 109, 112)),
        (&live_spent, live_spent.clone()),
    ] {
        assert!(loop_states_equal_modulo_resources(a, &b));
    }
    let mut unspent = live_spent.clone();
    unspent.triggers_fired_this_turn.clear();
    let mut reversed_unspent = layout(11, 10, 11);
    reversed_unspent.triggers_fired_this_turn.clear();
    assert!(loop_states_equal_modulo_resources(
        &unspent,
        &reversed_unspent
    ));
    for retired in [&retired_spent, &same_raw_spent_remint] {
        assert_ne!(
            live_spent.objects[&host].trigger_occurrence_state,
            retired.objects[&host].trigger_occurrence_state
        );
        let mut retired_unspent = retired.clone();
        retired_unspent.triggers_fired_this_turn.clear();
        assert!(
            loop_states_equal_modulo_resources(&unspent, &retired_unspent),
            "all nonrelational data agree; unspent allocation layout alone is irrelevant"
        );
        assert!(
            !loop_states_equal_modulo_resources(&live_spent, retired),
            "a spent CURRENT WR cannot recur as a reminted WR with only a RETIRED spent key"
        );
    }
}
