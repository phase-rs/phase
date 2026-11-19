//! Kalitas, Traitor of Ghet:
//!
//! "Lifelink
//!  If a nontoken creature an opponent controls would die, instead exile that
//!  card and create a 2/2 black Zombie creature token.
//!  {2}{B}, Sacrifice another Vampire or Zombie: Put two +1/+1 counters on
//!  Kalitas."
//!
//! CR 614.1a + CR 614.6 + CR 701.8a: Wrath of God destroys Kalitas together
//! with the opposing creatures, so Kalitas is still on the battlefield when
//! their deaths are replaced. Every replaced death carries out its own
//! replacing instructions, so each one exiles its card and makes a Zombie.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const KALITAS: &str = "Lifelink\nIf a nontoken creature an opponent controls would die, instead exile that card and create a 2/2 black Zombie creature token.\n{2}{B}, Sacrifice another Vampire or Zombie: Put two +1/+1 counters on Kalitas.";
const WRATH_OF_GOD: &str = "Destroy all creatures. They can't be regenerated.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

fn zombies(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::TokenCreated { name, .. } if name.contains("Zombie")))
        .count()
}

fn zone_changes_of(events: &[GameEvent], id: ObjectId) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::ZoneChanged { object_id, .. } if *object_id == id))
        .count()
}

#[test]
fn wrath_of_god_exiles_each_opposing_creature_and_makes_a_zombie_for_each() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kalitas = scenario
        .add_creature_from_oracle(P0, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
        .id();
    let bear_a = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let bear_b = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .id();
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&kalitas]
            .replacement_definitions
            .iter_all()
            .any(|r| r.execute.is_some()),
        "reach guard: Kalitas must parse its would-die replacement"
    );

    let outcome = runner.cast(wrath).resolve();
    assert_eq!(
        zombies(outcome.events()),
        2,
        "one Zombie per replaced death"
    );
    runner.advance_until_stack_empty();

    let state = runner.state();
    for bear in [bear_a, bear_b] {
        assert_eq!(state.objects[&bear].zone, Zone::Exile, "{bear:?}: exiled");
    }
    assert_eq!(
        state.objects[&kalitas].zone,
        Zone::Graveyard,
        "Kalitas's own death is not replaced"
    );
    let zombie_tokens: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name.contains("Zombie"))
        .collect();
    assert_eq!(
        zombie_tokens.len(),
        2,
        "both Zombies are on the battlefield"
    );
    assert!(
        zombie_tokens
            .iter()
            .all(|id| state.objects[id].controller == P0),
        "CR 109.5: Kalitas's controller creates the Zombies"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// Answer the open CR 616.1 replacement choice with `source`'s replacement.
fn choose_replacement_from(runner: &mut GameRunner, source: ObjectId) -> Vec<GameEvent> {
    let index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.source_id == source)
            .expect("the replacement is a candidate"),
        other => panic!("expected a replacement choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choose the replacement")
        .events
}

fn zombies_on_battlefield(state: &GameState) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].name.contains("Zombie"))
        .collect()
}

/// A remembered Murder replacement initially leaves only P1's simultaneous
/// choices visible. Clearing P0's record restores P0's first decision before P1 acts.
#[test]
fn clearing_a_remembered_replacement_reschedules_the_parked_wrath() {
    use engine::game::engine::apply;
    use engine::types::actions::ReplacementAutoChoice;
    use engine::types::player::PlayerId;

    for clear_all in [false, true] {
        let p2 = PlayerId(2);
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let later = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
        let active_bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let murder_target = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
        let kalitas = scenario
            .add_creature_from_oracle(p2, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
            .id();
        let rip = scenario
            .add_enchantment_from_oracle(p2, "Rest in Peace", REST_IN_PEACE)
            .id();
        let murder = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
            .id();
        let wrath = scenario
            .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
            .id();
        let mut runner = scenario.build();

        runner.cast(murder).target_object(murder_target).resolve();
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!("Murder must ask which replacement applies");
        };
        assert_eq!(*player, P0);
        assert_eq!(candidates.len(), 2);
        let rip_index = candidates
            .iter()
            .position(|candidate| candidate.source_id == rip)
            .unwrap();
        runner
            .act(GameAction::ChooseReplacementAndRemember {
                choice: ReplacementAutoChoice::Order {
                    order: vec![rip_index, 1 - rip_index],
                },
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&murder_target].zone, Zone::Exile);
        let stored = &runner.state().replacement_auto_choices;
        assert_eq!(stored.len(), 1);
        let id = stored[0].id.clone();

        let mut events = runner.cast(wrath).resolve().events().to_vec();
        let posted = runner.state().waiting_for.clone();
        engine::game::interaction::bind_interaction_authority(
            runner.state_mut(),
            engine::types::interaction::InteractionSessionId("wrath-member-order".to_owned()),
        )
        .unwrap();
        let original_slots = runner.state().active_interaction_slots.clone();
        assert_eq!(original_slots.len(), 1);
        assert_eq!(original_slots[0].semantic_owner, P1.0);
        assert!(
            matches!(&posted, WaitingFor::MemberOrderChoice { player, choices, .. }
            if *player == P1 && choices.as_slice() == [first, later]),
            "Wrath's first manual choices are P1's: {posted:?}"
        );

        // P1 may change their own preferences but cannot remove P0's record.
        let unchanged = apply(
            runner.state_mut(),
            P1,
            GameAction::SetReplacementAutoChoice {
                selector: Some(id.clone()),
            },
        )
        .unwrap();
        assert_eq!(unchanged.waiting_for, posted);
        assert_eq!(runner.state().active_interaction_slots, original_slots);
        assert!(unchanged.events.is_empty());
        assert_eq!(runner.state().replacement_auto_choices.len(), 1);

        let refreshed = apply(
            runner.state_mut(),
            P0,
            GameAction::SetReplacementAutoChoice {
                selector: (!clear_all).then_some(id),
            },
        )
        .unwrap();
        assert!(runner.state().replacement_auto_choices.is_empty());
        assert!(
            matches!(refreshed.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P0),
            "P0 now owes the earliest manual replacement decision: {:?}",
            refreshed.waiting_for
        );
        events.extend(refreshed.events);
        let replacement_slots = &runner.state().active_interaction_slots;
        assert_eq!(replacement_slots.len(), 1);
        assert_eq!(replacement_slots[0].semantic_owner, P0.0);
        assert_ne!(
            replacement_slots[0].interaction_id,
            original_slots[0].interaction_id
        );
        events.extend(choose_replacement_from(&mut runner, rip));
        assert_eq!(runner.state().waiting_for, posted);
        events.extend(
            runner
                .act(GameAction::SelectCards { cards: vec![later] })
                .unwrap()
                .events,
        );
        events.extend(choose_replacement_from(&mut runner, kalitas));
        events.extend(choose_replacement_from(&mut runner, rip));
        runner.advance_until_stack_empty();

        for id in [first, later, active_bear, kalitas] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
            assert_eq!(zone_changes_of(&events, id), 1);
        }
        let tokens = zombies_on_battlefield(runner.state());
        assert_eq!(tokens.len(), 1);
        assert_eq!(runner.state().objects[&tokens[0]].controller, p2);
    }
}

/// CR 616.1 + CR 101.4: both Kalitas controllers choose between the opposing
/// Kalitas and Rest in Peace. The active player decides first in either board order.
#[test]
fn both_kalitas_choose_replacements_in_apnap_order_before_wrath_delivers() {
    for first in [P1, P0] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let second = if first == P0 { P1 } else { P0 };
        let first_kalitas = scenario
            .add_creature_from_oracle(first, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
            .id();
        let second_kalitas = scenario
            .add_creature_from_oracle(second, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
            .id();
        let rest = scenario
            .add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE)
            .id();
        let wrath = scenario
            .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
            .id();
        let mut runner = scenario.build();
        let mut events = runner.cast(wrath).commit().resolve().events().to_vec();
        let (active, nonactive) = if first == P0 {
            (first_kalitas, second_kalitas)
        } else {
            (second_kalitas, first_kalitas)
        };
        for (chooser, opponent_kalitas, answer) in [(P0, nonactive, nonactive), (P1, active, rest)]
        {
            let WaitingFor::ReplacementChoice {
                player, candidates, ..
            } = &runner.state().waiting_for
            else {
                panic!("expected a replacement choice for {chooser:?}");
            };
            assert_eq!(*player, chooser, "first permanent belonged to {first:?}");
            assert!(candidates.iter().any(|c| c.source_id == opponent_kalitas));
            assert!(candidates.iter().any(|c| c.source_id == rest));
            for id in [active, nonactive] {
                assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
            }
            events.extend(choose_replacement_from(&mut runner, answer));
        }
        runner.advance_until_stack_empty();
        for id in [active, nonactive] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
            assert_eq!(zone_changes_of(&events, id), 1);
        }
        let departures: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged { object_id, .. }
                    if [active, nonactive].contains(object_id) =>
                {
                    Some(*object_id)
                }
                _ => None,
            })
            .collect();
        assert_eq!(departures, [first_kalitas, second_kalitas]);
        let tokens = zombies_on_battlefield(runner.state());
        assert_eq!(tokens.len(), 1);
        assert_eq!(runner.state().objects[&tokens[0]].controller, P1);
    }
}

/// CR 101.4c + CR 616.1: one player orders their simultaneous choices. Select
/// the later battlefield Bear first, then give the two deaths different replacements.
#[test]
fn wrath_lets_the_affected_player_choose_which_bear_to_consult_first() {
    use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization};

    for restore in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let later = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
        let kalitas = scenario
            .add_creature_from_oracle(P1, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
            .id();
        let rip = scenario
            .add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE)
            .id();
        let wrath = scenario
            .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
            .id();
        let mut runner = scenario.build();
        let mut events = runner.cast(wrath).commit().resolve().events().to_vec();
        let WaitingFor::MemberOrderChoice {
            player, choices, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "P0 must first choose which Bear to consult, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(choices.len(), 2);
        assert!(choices.contains(&first) && choices.contains(&later));
        if restore {
            let saved =
                serde_json::to_vec(&PersistedGameState::capture(runner.state().clone())).unwrap();
            let state = serde_json::from_slice::<PersistedGameState>(&saved)
                .unwrap()
                .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
                .unwrap()
                .finalize_after_rehydration(|_| Ok(()))
                .unwrap();
            runner = GameRunner::from_state(state);
        }
        assert!(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![kalitas]
                })
                .is_err(),
            "P0 cannot select the opponent's Kalitas"
        );
        events.extend(
            runner
                .act(GameAction::SelectCards { cards: vec![later] })
                .unwrap()
                .events,
        );
        for (bear, answer) in [(later, kalitas), (first, rip)] {
            let WaitingFor::ReplacementChoice {
                player, candidates, ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "expected the replacement decision for {bear:?}, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(*player, P0);
            assert!(candidates
                .iter()
                .any(|candidate| candidate.source_id == kalitas));
            assert!(candidates
                .iter()
                .any(|candidate| candidate.source_id == rip));
            assert_eq!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .and_then(|choice| choice.proposed.affected_object_id()),
                Some(bear)
            );
            assert!([first, later, kalitas]
                .iter()
                .all(|id| runner.state().objects[id].zone == Zone::Battlefield));
            events.extend(choose_replacement_from(&mut runner, answer));
        }
        runner.advance_until_stack_empty();
        for id in [first, later, kalitas] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
            assert_eq!(zone_changes_of(&events, id), 1);
        }
        assert_eq!(runner.state().objects[&wrath].zone, Zone::Exile);
        assert_eq!(zombies(&events), 1);
        let tokens = zombies_on_battlefield(runner.state());
        assert_eq!(tokens.len(), 1);
        assert_eq!(runner.state().objects[&tokens[0]].controller, P1);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 800.4a + CR 616.1: leaving after selecting a permanent does not
/// answer its replacement choice. Reconsult the same proposed move using
/// the surviving controller and replacement effects before allowing priority.
#[test]
fn conceding_during_wraths_selected_bear_reconsults_that_member() {
    use engine::types::player::PlayerId;
    use BearAtDeparture::{Selected, Unconsulted};

    // Losing the chooser reassigns the choice. Losing its replacement sources
    // instead removes the choice from a surviving controller.
    run_borrowed_bear_departures(
        false,
        &[
            (P1, Selected, false),
            (P1, Unconsulted, false),
            (PlayerId(2), Selected, false),
            (P1, Selected, true),
        ],
    );
}

/// CR 800.4a + CR 702.12b: when Control Magic's controller leaves, Avacyn
/// protects the very same Bear again. Its destruction cannot cause a graveyard
/// move, whether its own replacement choice or another Bear's was pending.
#[test]
fn avacyn_protects_the_returning_bear_before_wrath_finishes() {
    use BearAtDeparture::{Selected, Unconsulted};
    run_borrowed_bear_departures(
        true,
        &[
            (P1, Selected, false),
            (P1, Unconsulted, false),
            (P1, Selected, true),
        ],
    );
}

#[test]
fn avacyn_protects_the_returning_bear_and_cancels_its_queued_zombie() {
    run_borrowed_bear_departures(true, &[(P1, BearAtDeparture::Queued, false)]);
}

#[derive(Clone, Copy)]
enum BearAtDeparture {
    Unconsulted,
    Selected,
    Queued,
}

fn add_avacyn(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(
            P0,
            "Avacyn, Angel of Hope",
            8,
            8,
            "Flying, vigilance, indestructible\nOther permanents you control have indestructible.",
        )
        .as_legendary()
        .with_subtypes(vec!["Angel"])
        .with_mana_cost(engine::types::mana::parse_mtgjson_mana_cost("{5}{W}{W}{W}"))
        .id()
}

fn add_control_magic(scenario: &mut GameScenario) -> ObjectId {
    use engine::types::ability::TypedFilter;
    use engine::types::keywords::Keyword;

    scenario
        .add_enchantment_from_oracle(
            P1,
            "Control Magic",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TypedFilter::creature().into()))
        .id()
}

/// Control Magic changes Avacyn's controller, not the Bear's. When the Aura's
/// owner leaves, the affected player and replacement sources stay the same,
/// but the Bear newly gains indestructible (CR 800.4a + CR 702.12b).
#[test]
fn avacyn_returning_to_the_same_bear_controller_invalidates_wraths_replacement_choice() {
    use engine::game::effects::attach::attach_to;
    use engine::game::layers::evaluate_layers;
    use engine::types::keywords::Keyword;
    use engine::types::player::PlayerId;

    let db = shared_card_db().expect("integration card fixture must load");
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let avacyn = add_avacyn(&mut scenario);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let aura = add_control_magic(&mut scenario);
    let kalitas = scenario.add_real_card(p2, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let rip = scenario.add_real_card(p2, "Rest in Peace", Zone::Battlefield, db);
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, avacyn);
    evaluate_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&avacyn].controller, P1);
    assert_eq!(runner.state().objects[&bear].controller, P0);
    assert!(!runner.state().objects[&bear].has_keyword(&Keyword::Indestructible));
    let incarnation = runner.state().objects[&bear].incarnation;

    let mut events = runner.cast(wrath).commit().resolve().events().to_vec();
    assert!(matches!(&runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, candidates, .. }
        if candidates.iter().any(|c| c.source_id == kalitas)
        && candidates.iter().any(|c| c.source_id == rip)));
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|p| p.proposed.affected_object_id()),
        Some(bear)
    );
    events.extend(
        runner
            .act(GameAction::Concede { player_id: P1 })
            .unwrap()
            .events,
    );
    let state = runner.state();
    assert_eq!(state.objects[&avacyn].controller, P0);
    assert_eq!(state.objects[&bear].controller, P0);
    assert_eq!(state.objects[&bear].incarnation, incarnation);
    assert!(state.objects[&bear].has_keyword(&Keyword::Indestructible));
    assert_eq!(state.objects[&bear].zone, Zone::Battlefield);
    assert_eq!(zone_changes_of(&events, bear), 0);
    assert_eq!(zombies(&events), 0);
    assert_eq!(state.objects[&kalitas].zone, Zone::Exile);
    assert_eq!(zone_changes_of(&events, kalitas), 1);
    assert_eq!(state.objects[&wrath].zone, Zone::Exile);
    assert!(state.pending_replacement.is_none());
    assert!(state.active_simultaneous_departure().is_none());
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

fn run_borrowed_bear_departures(
    protected_by_avacyn: bool,
    cases: &[(engine::types::player::PlayerId, BearAtDeparture, bool)],
) {
    use engine::game::effects::attach::attach_to;
    use engine::game::layers::evaluate_layers;
    use engine::types::keywords::Keyword;
    use engine::types::player::PlayerId;

    let p2 = PlayerId(2);
    for &(leaving, stage, loss_sba) in cases {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let avacyn = protected_by_avacyn.then(|| add_avacyn(&mut scenario));
        let borrowed = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let owned = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
        let aura = add_control_magic(&mut scenario);
        let kalitas = scenario
            .add_creature_from_oracle(p2, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
            .id();
        let rest = scenario
            .add_enchantment_from_oracle(p2, "Rest in Peace", REST_IN_PEACE)
            .id();
        let wrath = scenario
            .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
            .id();
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), aura, borrowed);
        evaluate_layers(runner.state_mut());
        assert_eq!(runner.state().objects[&borrowed].controller, P1);
        let incarnation = runner.state().objects[&borrowed].incarnation;
        assert!(!runner.state().objects[&borrowed].has_keyword(&Keyword::Indestructible));
        runner.cast(wrath).commit().resolve();
        assert!(matches!(&runner.state().waiting_for,
            WaitingFor::MemberOrderChoice { player: P1, choices, .. }
                if choices.contains(&borrowed) && choices.contains(&owned)));
        let selected = match stage {
            BearAtDeparture::Unconsulted => owned,
            BearAtDeparture::Selected | BearAtDeparture::Queued => borrowed,
        };
        if loss_sba {
            // Arrange a pending player-loss SBA. The public selection opens
            // the replacement prompt and then reconciles the terminal result.
            runner.state_mut().players[leaving.0 as usize].life = 0;
        }
        let mut events = runner
            .act(GameAction::SelectCards {
                cards: vec![selected],
            })
            .unwrap()
            .events;
        if !loss_sba {
            assert!(matches!(&runner.state().waiting_for,
                WaitingFor::ReplacementChoice {player: P1, candidates, ..}
                    if candidates.iter().any(|candidate| candidate.source_id == kalitas)
                    && candidates.iter().any(|candidate| candidate.source_id == rest)));
            assert_eq!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .proposed
                    .affected_object_id(),
                Some(selected)
            );
            if matches!(stage, BearAtDeparture::Queued) {
                // P1 has already chosen Kalitas's replacing instructions for
                // the borrowed Bear. The other Bear's ordering choice holds
                // delivery, so the first Bear and its rider are still pending.
                events.extend(choose_replacement_from(&mut runner, kalitas));
                assert_eq!(runner.state().objects[&borrowed].zone, Zone::Battlefield);
                assert_eq!(zombies(&events), 0);
                assert!(matches!(
                    &runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { player: P1, .. }
                ));
                assert_eq!(
                    runner
                        .state()
                        .pending_replacement
                        .as_ref()
                        .and_then(|pending| pending.proposed.affected_object_id()),
                    Some(owned)
                );
            }
            events.extend(
                runner
                    .act(GameAction::Concede { player_id: leaving })
                    .unwrap()
                    .events,
            );
        }
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::PlayerLost {player_id, ..} if *player_id == leaving)).count(), 1);
        if let Some(avacyn) = avacyn {
            let bear = &runner.state().objects[&borrowed];
            assert_eq!(bear.controller, P0);
            assert_eq!(bear.incarnation, incarnation);
            assert!(bear.has_keyword(&Keyword::Indestructible));
            assert_eq!(bear.zone, Zone::Battlefield);
            assert!(
                runner.state().pending_replacement.is_none(),
                "Wrath cannot offer replacements for the protected Bear's destruction: {:?}",
                runner.state().waiting_for
            );
            assert_eq!(zone_changes_of(&events, borrowed), 0);
            assert_eq!(zombies(&events), 0);
            assert!(zombies_on_battlefield(runner.state()).is_empty());
            assert_eq!(runner.state().objects[&avacyn].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&kalitas].zone, Zone::Exile);
            assert_eq!(
                zone_changes_of(&events, kalitas),
                1,
                "Wrath finished Kalitas's departure"
            );
            assert_eq!(runner.state().objects[&wrath].zone, Zone::Exile);
        } else if leaving == P1 {
            assert_eq!(runner.state().objects[&borrowed].controller, P0);
            assert!(matches!(&runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P0, candidates, .. }
                    if candidates.iter().any(|candidate| candidate.source_id == kalitas)
                    && candidates.iter().any(|candidate| candidate.source_id == rest)));
            assert_eq!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .proposed
                    .affected_object_id(),
                Some(borrowed)
            );
            events.extend(choose_replacement_from(&mut runner, kalitas));
            assert_eq!(runner.state().objects[&borrowed].zone, Zone::Exile);
            assert_eq!(zone_changes_of(&events, borrowed), 1);
            assert_eq!(zombies(&events), 1);
        } else {
            for bear in [borrowed, owned] {
                assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
            }
            assert_eq!(zone_changes_of(&events, owned), 1);
            assert_eq!(zone_changes_of(&events, borrowed), 1);
        }
        assert!(runner.state().active_simultaneous_departure().is_none());
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 800.4a + CR 616.1: a concession can remove either the player ordering
/// the deaths or the competing replacements that made the ordering necessary.
#[test]
fn conceding_during_wraths_member_order_reconsults_the_surviving_board() {
    use engine::types::player::PlayerId;

    let p2 = PlayerId(2);
    for conceder in [P1, p2] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let second = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
        let kalitas = scenario
            .add_creature_from_oracle(p2, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
            .id();
        scenario.add_enchantment_from_oracle(p2, "Rest in Peace", REST_IN_PEACE);
        let wrath = scenario
            .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
            .id();
        let mut runner = scenario.build();
        runner.cast(wrath).commit().resolve();
        assert!(matches!(&runner.state().waiting_for,
            WaitingFor::MemberOrderChoice { player: P1, choices, .. }
                if choices.len() == 2 && choices.contains(&first) && choices.contains(&second)));

        let result = runner
            .act(GameAction::Concede {
                player_id: conceder,
            })
            .unwrap();
        let losses = result.events.iter().filter(
            |event| matches!(event, GameEvent::PlayerLost { player_id, .. } if *player_id == conceder),
        );
        assert_eq!(losses.count(), 1);
        let state = runner.state();
        assert!(state.active_simultaneous_departure().is_none());
        assert!(state.stack.is_empty());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        if conceder == P1 {
            assert_eq!(state.objects[&kalitas].zone, Zone::Exile);
            assert_eq!(zone_changes_of(&result.events, kalitas), 1);
            assert_eq!(state.objects[&wrath].zone, Zone::Exile);
        } else {
            for bear in [first, second] {
                assert_eq!(state.objects[&bear].zone, Zone::Graveyard);
                assert_eq!(zone_changes_of(&result.events, bear), 1);
            }
            assert_eq!(state.objects[&wrath].zone, Zone::Graveyard);
        }
    }
}

/// CR 616.1 + CR 701.8a: Rest in Peace and Kalitas both apply to the opposing
/// creature's death, so its controller chooses one. Answering that choice does
/// not abandon the rest of Wrath of God: the batch resumes, so Kalitas is
/// destroyed too (and exiled by Rest in Peace, CR 614.6), each creature leaves
/// the battlefield exactly once, and exactly one Zombie is made.
#[test]
fn rest_in_peace_ordering_choice_resumes_the_whole_wrath() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let kalitas = scenario
        .add_creature_from_oracle(P0, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
        .id();
    let rest_in_peace = scenario
        .add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE)
        .id();
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(wrath).commit().resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { player, .. } if *player == P1
        ),
        "the Bear's controller is asked to order the replacements, got {:?}",
        outcome.final_waiting_for()
    );
    let mut events = outcome.events().to_vec();
    assert_eq!(
        zone_changes_of(&events, bear),
        0,
        "nothing moved before the choice"
    );
    assert_eq!(
        zone_changes_of(&events, kalitas),
        0,
        "Kalitas is still on the battlefield while the batch waits"
    );

    events.extend(choose_replacement_from(&mut runner, kalitas));
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(
        state.objects[&bear].zone,
        Zone::Exile,
        "Kalitas exiled the Bear"
    );
    assert_eq!(
        state.objects[&kalitas].zone,
        Zone::Exile,
        "CR 701.8a + CR 614.6: Wrath destroys Kalitas and Rest in Peace exiles him"
    );
    assert_eq!(
        state.objects[&wrath].zone,
        Zone::Exile,
        "Rest in Peace exiles the resolved Wrath"
    );
    assert_eq!(state.objects[&rest_in_peace].zone, Zone::Battlefield);
    assert_eq!(zone_changes_of(&events, bear), 1, "the Bear moved once");
    assert_eq!(zone_changes_of(&events, kalitas), 1, "Kalitas moved once");
    assert_eq!(zombies(&events), 1, "exactly one Zombie");
    let zombie_tokens = zombies_on_battlefield(state);
    assert_eq!(zombie_tokens.len(), 1);
    assert_eq!(state.objects[&zombie_tokens[0]].controller, P0);
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 614.1a + CR 616.1: the same choice reached after Kalitas's own death has
/// already been consulted. Kalitas must still be on the battlefield — and so
/// still a candidate — when the Bear's death is consulted, because Wrath
/// destroys them simultaneously; the consulted Kalitas is delivered only after
/// every member has been consulted.
#[test]
fn ordering_choice_after_kalitas_is_consulted_keeps_kalitas_a_candidate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kalitas = scenario
        .add_creature_from_oracle(P0, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .id();
    let mut runner = scenario.build();
    assert!(
        runner
            .state()
            .battlefield
            .iter()
            .position(|id| *id == kalitas)
            < runner.state().battlefield.iter().position(|id| *id == bear),
        "reach guard: Kalitas is consulted before the Bear"
    );

    let outcome = runner.cast(wrath).commit().resolve();
    let mut events = outcome.events().to_vec();
    match outcome.final_waiting_for() {
        WaitingFor::ReplacementChoice { candidates, .. } => assert!(
            candidates.iter().any(|c| c.source_id == kalitas),
            "Kalitas is still a candidate for the Bear's death"
        ),
        other => panic!("expected the Bear's ordering choice, got {other:?}"),
    }
    assert_eq!(
        zone_changes_of(&events, kalitas),
        0,
        "Kalitas has not moved"
    );

    events.extend(choose_replacement_from(&mut runner, kalitas));
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&bear].zone, Zone::Exile);
    assert_eq!(state.objects[&kalitas].zone, Zone::Exile);
    assert_eq!(zombies(&events), 1, "Kalitas's replacement made the Zombie");
    assert_eq!(zombies_on_battlefield(state).len(), 1);
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 614.6 + CR 608.2c + CR 701.8a: Kalitas's replacing instructions run with
/// each opposing creature's delivery, and Jinnie Fay, Jetmir's Second ("If you
/// would create one or more tokens, you may instead create ...") offers its
/// optional replacement for every Zombie. Jinnie survives Wrath of God with a
/// shield counter (Boon of Safety: "Put a shield counter on target creature",
/// CR 122.1c). The first offer pauses the delivery of the batch; answering it
/// must resume the remaining deliveries, so every opposing creature is still
/// exiled with its own Zombie — none is stranded on the battlefield or
/// re-derived after Kalitas has gone.
#[test]
fn jinnie_token_choice_mid_delivery_resumes_the_remaining_deliveries() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let kalitas = scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let jinnie = scenario.add_real_card(P0, "Jinnie Fay, Jetmir's Second", Zone::Battlefield, db);
    scenario.with_counter(jinnie, CounterType::Shield, 1);
    let bears: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db))
        .collect();
    let wrath = scenario.add_real_card(P0, "Wrath of God", Zone::Hand, db);
    let mut runner = scenario.build();

    let outcome = runner.cast(wrath).commit().resolve();
    let mut events = outcome.events().to_vec();
    let mut prompts = 0;
    loop {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                assert_eq!(*player, P0, "Jinnie's controller decides");
                assert!(
                    candidates.iter().any(|c| c.source_id == jinnie),
                    "the prompt is Jinnie's optional token replacement"
                );
                prompts += 1;
                let decline = candidates
                    .iter()
                    .position(|c| c.description == "Decline")
                    .expect("an optional replacement offers Decline");
                events.extend(
                    runner
                        .act(GameAction::ChooseReplacement { index: decline })
                        .expect("decline Jinnie's replacement")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt during the Wrath: {other:?}"),
        }
        assert!(prompts <= bears.len(), "one offer per Zombie at most");
    }
    assert_eq!(
        prompts,
        bears.len(),
        "Jinnie is offered for every opposing creature's Zombie"
    );
    runner.advance_until_stack_empty();

    let state = runner.state();
    for bear in &bears {
        assert_eq!(state.objects[bear].zone, Zone::Exile, "{bear:?}: exiled");
        assert_eq!(zone_changes_of(&events, *bear), 1, "{bear:?}: moved once");
    }
    assert_eq!(
        state.objects[&kalitas].zone,
        Zone::Graveyard,
        "Kalitas's own death is not replaced"
    );
    assert_eq!(
        state.objects[&jinnie].zone,
        Zone::Battlefield,
        "the shield counter kept Jinnie"
    );
    assert_eq!(
        state.objects[&jinnie]
            .counters
            .get(&CounterType::Shield)
            .copied()
            .unwrap_or(0),
        0,
        "CR 122.1c: the shield counter was removed instead"
    );
    assert_eq!(state.objects[&wrath].zone, Zone::Graveyard);
    assert_eq!(
        zombies(&events),
        bears.len(),
        "one Zombie per replaced death"
    );
    let zombie_tokens = zombies_on_battlefield(state);
    assert_eq!(zombie_tokens.len(), bears.len());
    assert!(zombie_tokens
        .iter()
        .all(|id| state.objects[id].controller == P0));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::CreatureDestroyed { object_id, .. } if bears.contains(object_id)))
            .count(),
        bears.len(),
        "every opposing creature was destroyed"
    );
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// Answer every Jinnie offer, accepting the first with its first substitute and
/// declining the rest, and return how many offers were made.
fn answer_jinnie_offers(
    runner: &mut GameRunner,
    jinnie: ObjectId,
    events: &mut Vec<GameEvent>,
) -> usize {
    let mut offers = 0;
    loop {
        let index = match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                assert_eq!(*player, P0, "Jinnie's controller decides");
                assert!(candidates.iter().any(|c| c.source_id == jinnie));
                offers += 1;
                if offers == 1 {
                    0
                } else {
                    candidates
                        .iter()
                        .position(|c| c.description == "Decline")
                        .expect("an optional replacement offers Decline")
                }
            }
            WaitingFor::ChooseOneOfBranch { .. } => {
                events.extend(
                    runner
                        .act(GameAction::ChooseBranch { index: 0 })
                        .expect("choose the first substitute")
                        .events,
                );
                continue;
            }
            WaitingFor::Priority { .. } => return offers,
            other => panic!("unexpected prompt during the departure: {other:?}"),
        };
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index })
                .expect("answer Jinnie's offer")
                .events,
        );
        assert!(offers <= 8, "runaway offers");
    }
}

fn has_subtype(state: &GameState, id: ObjectId, subtype: &str) -> bool {
    state.objects[&id]
        .card_types
        .subtypes
        .iter()
        .any(|s| s == subtype)
}

/// The accept polarity of the mid-delivery pause: accepting Jinnie's offer for
/// the first Zombie opens a further substitute choice inside that delivery's
/// replacing instructions. The batch stays parked beneath that whole chain and
/// still delivers the other opposing creature once it settles.
#[test]
fn accepted_jinnie_substitute_mid_delivery_still_resumes_the_batch() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let kalitas = scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let jinnie = scenario.add_real_card(P0, "Jinnie Fay, Jetmir's Second", Zone::Battlefield, db);
    scenario.with_counter(jinnie, CounterType::Shield, 1);
    let bears: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db))
        .collect();
    let wrath = scenario.add_real_card(P0, "Wrath of God", Zone::Hand, db);
    let mut runner = scenario.build();

    let outcome = runner.cast(wrath).commit().resolve();
    let mut events = outcome.events().to_vec();
    assert_eq!(
        answer_jinnie_offers(&mut runner, jinnie, &mut events),
        bears.len()
    );
    runner.advance_until_stack_empty();

    let state = runner.state();
    for bear in &bears {
        assert_eq!(state.objects[bear].zone, Zone::Exile, "{bear:?}: exiled");
    }
    assert_eq!(state.objects[&kalitas].zone, Zone::Graveyard);
    assert_eq!(state.objects[&jinnie].zone, Zone::Battlefield);
    assert_eq!(state.objects[&wrath].zone, Zone::Graveyard);
    let p0_tokens: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token && state.objects[id].controller == P0)
        .collect();
    // The substitute's "that many" count is resolved by the branch
    // continuation, not by the batch seam this row pins (see
    // `optional_replacement_decline_unreplaced_event`), so only the replaced
    // Zombie is asserted: the accepted offer created none.
    assert_eq!(
        p0_tokens
            .iter()
            .filter(|id| has_subtype(state, **id, "Zombie"))
            .count(),
        1,
        "only the declined offer created its Zombie"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::CreatureDestroyed { object_id, .. } if bears.contains(object_id)))
            .count(),
        bears.len(),
        "every opposing creature was destroyed"
    );
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 704.3 + CR 704.5g + CR 614.6: the same pause in a state-based-action
/// death batch. Pyroclasm ("Pyroclasm deals 2 damage to each creature.")
/// leaves Kalitas and the opposing creatures with lethal damage, so they die
/// together; Kalitas exiles each opposing creature and makes a Zombie, and
/// Jinnie's offer pauses the first delivery. Answering it resumes the batch
/// with the deaths it already consulted, rather than re-deriving the rest
/// after Kalitas has left.
#[test]
fn jinnie_token_choice_mid_state_based_death_batch_resumes_the_batch() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let kalitas = scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let jinnie = scenario.add_real_card(P0, "Jinnie Fay, Jetmir's Second", Zone::Battlefield, db);
    let bears: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db))
        .collect();
    let pyroclasm = scenario.add_real_card(P0, "Pyroclasm", Zone::Hand, db);
    let mut runner = scenario.build();
    // Two damage already marked: Pyroclasm's two more is lethal to Kalitas.
    runner
        .state_mut()
        .objects
        .get_mut(&kalitas)
        .unwrap()
        .damage_marked = 2;

    let outcome = runner.cast(pyroclasm).commit().resolve();
    let mut events = outcome.events().to_vec();
    let mut offers = 0;
    loop {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                assert!(candidates.iter().any(|c| c.source_id == jinnie));
                offers += 1;
                let decline = candidates
                    .iter()
                    .position(|c| c.description == "Decline")
                    .expect("an optional replacement offers Decline");
                events.extend(
                    runner
                        .act(GameAction::ChooseReplacement { index: decline })
                        .expect("decline Jinnie's replacement")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt during the deaths: {other:?}"),
        }
        assert!(offers <= bears.len(), "one offer per Zombie at most");
    }
    assert_eq!(offers, bears.len(), "Jinnie is offered for every Zombie");
    runner.advance_until_stack_empty();

    let state = runner.state();
    for bear in &bears {
        assert_eq!(state.objects[bear].zone, Zone::Exile, "{bear:?}: exiled");
        assert_eq!(zone_changes_of(&events, *bear), 1, "{bear:?}: moved once");
    }
    assert_eq!(state.objects[&kalitas].zone, Zone::Graveyard);
    assert_eq!(state.objects[&jinnie].zone, Zone::Battlefield);
    assert_eq!(state.objects[&pyroclasm].zone, Zone::Graveyard);
    assert_eq!(zombies(&events), bears.len());
    assert_eq!(zombies_on_battlefield(state).len(), bears.len());
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

fn select_first_bear(runner: &mut GameRunner, bears: &[ObjectId]) -> Vec<GameEvent> {
    let WaitingFor::MemberOrderChoice {
        player, choices, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a choice between the two Bears, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert_eq!(choices.len(), bears.len());
    assert!(bears.iter().all(|bear| choices.contains(bear)));
    runner
        .act(GameAction::SelectCards {
            cards: vec![bears[0]],
        })
        .unwrap()
        .events
}

/// Answer every CR 616.1 ordering prompt with Kalitas's replacement, collecting
/// the events, until the game is back at priority.
fn answer_orderings_with(
    runner: &mut GameRunner,
    source: ObjectId,
    events: &mut Vec<GameEvent>,
) -> usize {
    let mut prompts = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        prompts += 1;
        assert!(prompts <= 8, "runaway ordering prompts");
        events.extend(choose_replacement_from(runner, source));
    }
    prompts
}

/// CR 608.2c + CR 616.1 + CR 701.8a: Fumigate ("Destroy all creatures. You
/// gain 1 life for each creature destroyed this way.") with Kalitas and an
/// opponent's Rest in Peace. Each opposing creature's death needs an ordering
/// choice. The life gain is Fumigate's later instruction: it waits for the
/// whole destruction, so it happens after every creature is destroyed and
/// counts every creature destroyed this way, including those delivered after
/// the prompts.
#[test]
fn fumigate_gains_life_for_every_creature_after_the_whole_paused_destruction() {
    const FUMIGATE: &str =
        "Destroy all creatures. You gain 1 life for each creature destroyed this way.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kalitas = scenario
        .add_creature_from_oracle(P0, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
        .id();
    let bears: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id())
        .collect();
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let fumigate = scenario
        .add_spell_to_hand_from_oracle(P0, "Fumigate", false, FUMIGATE)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    let outcome = runner.cast(fumigate).commit().resolve();
    let mut events = outcome.events().to_vec();
    events.extend(select_first_bear(&mut runner, &bears));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the first opposing death asks for an ordering, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "no life is gained while the destruction is paused"
    );
    let prompts = answer_orderings_with(&mut runner, kalitas, &mut events);
    assert_eq!(prompts, bears.len(), "one ordering per opposing creature");
    runner.advance_until_stack_empty();

    let state = runner.state();
    for bear in &bears {
        assert_eq!(state.objects[bear].zone, Zone::Exile, "{bear:?}: exiled");
        assert_eq!(zone_changes_of(&events, *bear), 1, "{bear:?}: moved once");
    }
    assert_eq!(
        state.objects[&kalitas].zone,
        Zone::Exile,
        "Kalitas is destroyed too, and Rest in Peace exiles him"
    );
    assert_eq!(state.objects[&fumigate].zone, Zone::Exile);
    assert_eq!(zombies(&events), bears.len());
    let destroyed: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, GameEvent::CreatureDestroyed { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(destroyed.len(), bears.len() + 1, "Kalitas and both Bears");
    assert_eq!(
        state.players[0].life,
        life_before + destroyed.len() as i32,
        "1 life for each creature destroyed this way"
    );
    let gain = events
        .iter()
        .position(|e| matches!(e, GameEvent::LifeChanged { player_id, amount, .. } if *player_id == P0 && *amount > 0))
        .expect("the life gain happened");
    assert!(
        destroyed.iter().all(|&i| i < gain),
        "CR 608.2c: the gain follows every destruction"
    );
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 704.3 + CR 616.1: the consult-time ordering choice in a state-based
/// death batch. Pyroclasm leaves Kalitas and the opposing creatures with lethal
/// damage; Rest in Peace and Kalitas both apply to each opposing death. The
/// answers resume the same batch, so Kalitas is still a candidate for every
/// opposing death even though he dies with them.
#[test]
fn state_based_death_batch_resumes_after_consult_time_ordering_choices() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let kalitas = scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let bears: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db))
        .collect();
    scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
    let pyroclasm = scenario.add_real_card(P0, "Pyroclasm", Zone::Hand, db);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&kalitas)
        .unwrap()
        .damage_marked = 2;

    let outcome = runner.cast(pyroclasm).commit().resolve();
    let mut events = outcome.events().to_vec();
    events.extend(select_first_bear(&mut runner, &bears));
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => assert!(
            candidates.iter().any(|c| c.source_id == kalitas),
            "Kalitas is a candidate while the batch is consulted"
        ),
        other => panic!("expected an ordering choice, got {other:?}"),
    }
    let prompts = answer_orderings_with(&mut runner, kalitas, &mut events);
    assert_eq!(prompts, bears.len());
    runner.advance_until_stack_empty();

    let state = runner.state();
    for bear in &bears {
        assert_eq!(state.objects[bear].zone, Zone::Exile, "{bear:?}: exiled");
        assert_eq!(zone_changes_of(&events, *bear), 1, "{bear:?}: moved once");
    }
    assert_eq!(state.objects[&kalitas].zone, Zone::Exile);
    assert_eq!(state.objects[&pyroclasm].zone, Zone::Exile);
    assert_eq!(
        zombies(&events),
        bears.len(),
        "Kalitas's replacement each time"
    );
    assert_eq!(zombies_on_battlefield(state).len(), bears.len());
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 701.8a + CR 616.1: Decimate ("Destroy target artifact, target creature,
/// target enchantment, and target land.") destroys its targets at once. The
/// opposing artifact creature's death needs an ordering choice between Kalitas
/// (also a target) and Rest in Peace; answering it must not abandon the other
/// three targets.
#[test]
fn decimate_destroys_every_target_after_an_ordering_choice() {
    const DECIMATE: &str =
        "Destroy target artifact, target creature, target enchantment, and target land.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let golem = scenario
        .add_creature(P0, "Test Golem", 2, 2)
        .as_artifact_creature()
        .id();
    let land = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    let kalitas = scenario
        .add_creature_from_oracle(P1, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
        .id();
    let enchantment = scenario
        .add_enchantment_from_oracle(P1, "Test Enchantment", "")
        .id();
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let decimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Decimate", false, DECIMATE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(decimate)
        .target_objects(&[golem, kalitas, enchantment, land])
        .commit()
        .resolve();
    let mut events = outcome.events().to_vec();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { player, .. } if *player == P0
        ),
        "the artifact creature's controller orders the replacements, got {:?}",
        outcome.final_waiting_for()
    );
    events.extend(choose_replacement_from(&mut runner, kalitas));
    runner.advance_until_stack_empty();

    let state = runner.state();
    for target in [golem, kalitas, enchantment, land] {
        assert_eq!(
            state.objects[&target].zone,
            Zone::Exile,
            "{target:?}: destroyed and exiled by Rest in Peace"
        );
        assert_eq!(
            zone_changes_of(&events, target),
            1,
            "{target:?}: moved once"
        );
    }
    assert_eq!(zombies(&events), 1, "Kalitas replaced the Golem's death");
    let zombie_tokens = zombies_on_battlefield(state);
    assert_eq!(zombie_tokens.len(), 1);
    assert_eq!(state.objects[&zombie_tokens[0]].controller, P1);
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

#[test]
fn decimate_consults_every_slot_before_the_coated_kalitas_leaves() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let coating = scenario.add_real_card(P0, "Liquimetal Coating", Zone::Battlefield, db);
    let kalitas = scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let rip = scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
    let forest = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Green);
    let decimate = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Decimate",
            false,
            "Destroy target artifact, target creature, target enchantment, and target land.",
        )
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: coating,
            ability_index: 0,
            payment_mode: Default::default(),
        })
        .expect("activate Liquimetal Coating");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(kalitas)),
        })
        .expect("choose Kalitas as the Coating target");
    runner.advance_until_stack_empty();

    let outcome = runner
        .cast(decimate)
        .target_objects(&[kalitas, bears, rip, forest])
        .commit()
        .resolve();
    let mut events = outcome.events().to_vec();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "the Bear's controller must choose between Kalitas and Rest in Peace: {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*player, P1);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == kalitas));
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == rip));
    assert_eq!(runner.state().objects[&kalitas].zone, Zone::Battlefield);
    events.extend(choose_replacement_from(&mut runner, kalitas));
    runner.advance_until_stack_empty();
    for id in [kalitas, bears, rip, forest] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
        assert_eq!(zone_changes_of(&events, id), 1);
    }
    assert_eq!(zombies(&events), 1);
    let zombies = zombies_on_battlefield(runner.state());
    assert_eq!(zombies.len(), 1);
    assert_eq!(runner.state().objects[&zombies[0]].controller, P0);
}

/// Synthetic Decimate siblings. A second printed verb starts another
/// instruction; a further slot of the first verb shares its replacement
/// consultation. The life-gain sentence follows both targets in either case.
#[test]
fn destroy_list_ends_at_the_next_instruction_and_resumes_its_tail_once() {
    for (oracle, joined) in [
        (
            "Destroy target artifact and target creature. You gain 1 life.",
            true,
        ),
        (
            "Destroy target artifact. Destroy target creature. You gain 1 life.",
            false,
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let kalitas = scenario
            .add_creature_from_oracle(P0, "Artifact Kalitas", 3, 4, KALITAS)
            .as_artifact_creature()
            .id();
        let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic destruction", false, oracle)
            .id();
        let mut runner = scenario.build();
        let life = runner.life(P0);
        let outcome = runner
            .cast(spell)
            .target_objects(&[kalitas, bear])
            .commit()
            .resolve();
        let mut events = outcome.events().to_vec();
        if joined {
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::ReplacementChoice { .. }
            ));
            assert_eq!(
                runner.life(P0),
                life,
                "the common tail waits behind both targets"
            );
            events.extend(choose_replacement_from(&mut runner, kalitas));
        } else {
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
        }
        runner.advance_until_stack_empty();
        assert_eq!(runner.life(P0), life + 1, "resolve the written tail once");
        assert_eq!(runner.state().objects[&bear].zone, Zone::Exile);
        assert_eq!(zombies(&events), usize::from(joined));
    }
}

/// CR 608.2c + CR 614.6: Fumigate's trailing "You gain 1 life for each creature
/// destroyed this way" with a DELIVERY-time pause. Kalitas's Zombie for each
/// opposing creature meets Jinnie Fay's optional token replacement (Jinnie kept
/// alive by a shield counter, as Boon of Safety gives). The life gain waits for
/// the whole departure: it happens only after the last token choice, and counts
/// every creature destroyed this way.
#[test]
fn fumigate_life_gain_waits_for_the_last_delivery_time_token_choice() {
    const FUMIGATE: &str =
        "Destroy all creatures. You gain 1 life for each creature destroyed this way.";
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kalitas = scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let jinnie = scenario.add_real_card(P0, "Jinnie Fay, Jetmir's Second", Zone::Battlefield, db);
    scenario.with_counter(jinnie, CounterType::Shield, 1);
    let bears: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db))
        .collect();
    let fumigate = scenario
        .add_spell_to_hand_from_oracle(P0, "Fumigate", false, FUMIGATE)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    let outcome = runner.cast(fumigate).commit().resolve();
    let mut events = outcome.events().to_vec();
    let mut offers = 0;
    while let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = runner.state().waiting_for.clone()
    {
        assert_eq!(player, P0, "Jinnie's controller decides");
        assert!(candidates.iter().any(|c| c.source_id == jinnie));
        assert_eq!(
            runner.state().players[0].life,
            life_before,
            "no life is gained while a token choice is open"
        );
        offers += 1;
        assert!(offers <= bears.len(), "one offer per Zombie at most");
        let decline = candidates
            .iter()
            .position(|c| c.description == "Decline")
            .expect("an optional replacement offers Decline");
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: decline })
                .expect("decline Jinnie's replacement")
                .events,
        );
    }
    assert_eq!(offers, bears.len(), "Jinnie is offered for every Zombie");
    runner.advance_until_stack_empty();

    let state = runner.state();
    for bear in &bears {
        assert_eq!(state.objects[bear].zone, Zone::Exile, "{bear:?}: exiled");
    }
    assert_eq!(state.objects[&kalitas].zone, Zone::Graveyard);
    assert_eq!(state.objects[&jinnie].zone, Zone::Battlefield);
    assert_eq!(state.objects[&fumigate].zone, Zone::Graveyard);
    let destroyed: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, GameEvent::CreatureDestroyed { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(destroyed.len(), bears.len() + 1, "Kalitas and both Bears");
    assert_eq!(
        state.players[0].life,
        life_before + destroyed.len() as i32,
        "1 life for each creature destroyed this way"
    );
    let gain = events
        .iter()
        .position(|e| matches!(e, GameEvent::LifeChanged { player_id, amount, .. } if *player_id == P0 && *amount > 0))
        .expect("the life gain happened");
    let last_zombie = events
        .iter()
        .rposition(|e| matches!(e, GameEvent::TokenCreated { name, .. } if name.contains("Zombie")))
        .expect("the Zombies were created");
    assert!(last_zombie < gain, "the gain follows the last token choice");
    assert!(destroyed.iter().all(|&i| i < gain), "and every destruction");
    assert_eq!(zombies(&events), bears.len());
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 805.2 + CR 805.6: the team orders its simultaneous decisions, then each
/// affected player answers their own replacement choice (CR 616.1).
#[test]
fn shared_team_chooses_the_later_bear_to_consult_without_answering_for_its_controller() {
    use engine::game::engine::apply;
    use engine::types::format::FormatConfig;
    use engine::types::player::PlayerId;

    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let early = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let late = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let kalitas = scenario
        .add_creature_from_oracle(p2, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
        .id();
    let rip = scenario
        .add_enchantment_from_oracle(p2, "Rest in Peace", REST_IN_PEACE)
        .id();
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .id();
    let mut runner = scenario.build();
    let mut events = runner.cast(wrath).commit().resolve().events().to_vec();
    let WaitingFor::MemberOrderChoice {
        player, choices, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the active team must order its two Bears' decisions, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        *player, P0,
        "the shared-turn representative submits the order"
    );
    assert_eq!(choices.len(), 2);
    assert!(choices.contains(&early) && choices.contains(&late));
    let choose_late = GameAction::SelectCards { cards: vec![late] };
    assert!(
        apply(runner.state_mut(), p2, choose_late.clone()).is_err(),
        "the opposing team cannot order these decisions"
    );
    events.extend(apply(runner.state_mut(), P0, choose_late).unwrap().events);
    for (bear, chooser, teammate, source) in [(late, P1, P0, kalitas), (early, P0, P1, rip)] {
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected the individual replacement choice for {bear:?}, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, chooser);
        assert_eq!(
            runner
                .state()
                .pending_replacement
                .as_ref()
                .and_then(|pending| pending.proposed.affected_object_id()),
            Some(bear)
        );
        let index = candidates
            .iter()
            .position(|candidate| candidate.source_id == source)
            .unwrap();
        let action = GameAction::ChooseReplacement { index };
        assert!(
            apply(runner.state_mut(), teammate, action.clone()).is_err(),
            "one teammate cannot answer for the other"
        );
        events.extend(apply(runner.state_mut(), chooser, action).unwrap().events);
    }
    runner.advance_until_stack_empty();
    for id in [early, late, kalitas] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
        assert_eq!(zone_changes_of(&events, id), 1);
    }
    assert_eq!(zombies(&events), 1);
    let tokens = zombies_on_battlefield(runner.state());
    assert_eq!(tokens.len(), 1);
    assert_eq!(runner.state().objects[&tokens[0]].controller, p2);
    assert!(runner.state().stack.is_empty());
}
