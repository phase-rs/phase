//! Production parser → phase-trigger → damage-resolution coverage for Power Surge.

use std::collections::HashMap;

use engine::game::game_object::PhaseOutCause;
use engine::game::phasing::phase_out_object;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::game::{layers, turns};
use engine::types::ability::{
    ChoiceType, ContinuousModification, Effect, PhaseTriggerFanout, PlayerChoicePopulation,
    PlayerScope, QuantityExpr, QuantityRef, StaticDefinition, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{BeginningOfTurnSnapshot, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::FormatConfig;

use crate::support::shared_card_db;

const POWER_SURGE: &str = "Power Surge";
const ASYLUM_VISITOR: &str = "Asylum Visitor";
const P2: PlayerId = PlayerId(2);

fn fixture_db() -> &'static engine::database::card_db::CardDatabase {
    shared_card_db().expect("committed integration card fixture must load")
}

fn mana_pool_total(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .expect("scenario player")
        .mana_pool
        .total()
}

fn assert_power_surge_runtime_tree_supported(db: &engine::database::card_db::CardDatabase) {
    let face = db
        .get_face_by_name(POWER_SURGE)
        .expect("committed integration card fixture must contain Power Surge");
    let details = engine::game::coverage::build_parse_details_for_face(face);
    assert!(
        details.iter().all(|item| item.is_fully_supported()),
        "Power Surge's deployed runtime tree must contain no unsupported node: {details:#?}"
    );
}

fn resolve_power_surge_trigger(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passing must resolve Power Surge's trigger");
    }
    assert!(
        runner.state().stack.is_empty(),
        "Power Surge's trigger must leave the stack"
    );
}

/// CR 603.2b + CR 608.2i: the upkeep trigger uses the committed historical
/// count even when the same lands are tapped before the trigger resolves.
#[test]
fn power_surge_damages_the_upkeep_player_from_the_turn_start_snapshot() {
    let db = fixture_db();
    assert_power_surge_runtime_tree_supported(db);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let power_surge = scenario.add_real_card(P0, POWER_SURGE, Zone::Battlefield, db);
    let first = scenario.add_land_from_oracle(P1, "Land A", "").id();
    let second = scenario.add_land_from_oracle(P1, "Land B", "").id();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), power_surge);
    assert_eq!(
        runner.state().objects[&power_surge].trigger_definitions[0]
            .definition()
            .phase_fanout,
        PhaseTriggerFanout::EachPlayer,
        "the production export must preserve the CR 805.4d participant scope"
    );
    assert_eq!(
        runner.state().objects[&power_surge]
            .trigger_definitions
            .len(),
        1,
        "the production Oracle parser must install Power Surge's upkeep trigger"
    );
    let turn_number = runner.state().turn_number;
    runner.state_mut().beginning_of_turn_snapshot = Some(BeginningOfTurnSnapshot {
        turn_number,
        untapped_lands_controlled: HashMap::from([(P0, 0), (P1, 2)]),
    });
    runner.state_mut().objects.get_mut(&first).unwrap().tapped = true;
    runner.state_mut().objects.get_mut(&second).unwrap().tapped = true;
    let life_before = runner.life(P1);

    runner.advance_to_upkeep();
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == power_surge),
        "Power Surge's trigger must enter the production stack at P1's upkeep"
    );
    let stack_ability = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == power_surge)
        .and_then(|entry| entry.ability())
        .expect("Power Surge stack entry must carry its resolved ability");
    assert_eq!(stack_ability.scoped_player, Some(P1));
    let engine::types::ability::Effect::DealDamage { amount, .. } = &stack_ability.effect else {
        panic!("Power Surge stack entry must deal damage");
    };
    assert_eq!(
        engine::game::quantity::resolve_quantity_with_targets(
            runner.state(),
            amount,
            stack_ability,
        ),
        2,
        "the production stack ability must resolve P1's current-stamp historical row"
    );
    resolve_power_surge_trigger(&mut runner);

    assert_eq!(
        runner.life(P1),
        life_before - 2,
        "Power Surge must read P1's historical row, not recount the now-tapped lands"
    );
    assert_eq!(runner.life(P0), 20, "the scoped trigger damages only P1");
}

/// CR 500.1 + CR 502.1 + CR 603.2b + CR 702.26b-c: the global turn-start
/// history is captured while a phased-out source is treated as absent, then the
/// source phases in before untap and can trigger using the already-committed row.
#[test]
fn phased_out_power_surge_uses_history_captured_before_it_phases_in() {
    let db = fixture_db();
    assert_power_surge_runtime_tree_supported(db);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let power_surge = scenario.add_real_card(P1, POWER_SURGE, Zone::Battlefield, db);
    let first = scenario.add_land_from_oracle(P1, "Land A", "").id();
    let second = scenario.add_land_from_oracle(P1, "Land B", "").id();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let mut phase_events = Vec::new();
    assert_eq!(
        phase_out_object(
            runner.state_mut(),
            power_surge,
            PhaseOutCause::Directly,
            &mut phase_events,
        ),
        vec![power_surge],
        "the hostile source fixture must actually phase Power Surge out"
    );
    assert!(runner.state().objects[&power_surge].is_phased_out());

    turns::start_next_turn(runner.state_mut(), &mut phase_events);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(
        runner
            .state()
            .beginning_of_turn_snapshot
            .as_ref()
            .unwrap()
            .untapped_lands_controlled[&P1],
        2,
        "turn history must be captured globally while Power Surge is absent"
    );
    let life_before = runner.life(P1);
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    runner.advance_to_upkeep();
    assert!(
        !runner.state().objects[&power_surge].is_phased_out(),
        "Power Surge must phase in before the upkeep trigger check"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == power_surge),
        "the phased-in Power Surge must trigger at upkeep"
    );
    runner.state_mut().objects.get_mut(&first).unwrap().tapped = true;
    runner.state_mut().objects.get_mut(&second).unwrap().tapped = true;
    assert_eq!(
        runner
            .state()
            .beginning_of_turn_snapshot
            .as_ref()
            .unwrap()
            .untapped_lands_controlled[&P1],
        2,
        "advancing through untap must preserve the committed history row"
    );
    resolve_power_surge_trigger(&mut runner);

    assert_eq!(
        runner.life(P1),
        life_before - 2,
        "the phased-in source must use history committed while it was absent"
    );
    assert_eq!(runner.life(P0), 20, "the non-upkeep player is not damaged");
}

/// CR 500.1 + CR 608.2i + CR 613.4a: replacing the historical snapshot at the
/// production turn boundary invalidates a previously clean dynamic CDA cache.
#[test]
fn turn_start_snapshot_replacement_invalidates_dynamic_cda_cache() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let creature = scenario.add_creature(P1, "Historical CDA", 0, 1).id();
    scenario.add_land_from_oracle(P1, "Land A", "");
    scenario.add_land_from_oracle(P1, "Land B", "");
    let mut runner = scenario.build();
    let starting_turn = runner.state().turn_number;

    runner.state_mut().beginning_of_turn_snapshot = Some(BeginningOfTurnSnapshot {
        turn_number: starting_turn,
        untapped_lands_controlled: HashMap::from([(P0, 0), (P1, 0)]),
    });
    {
        let object = runner.state_mut().objects.get_mut(&creature).unwrap();
        let definition = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .cda()
            .modifications(vec![ContinuousModification::SetDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::UntappedLandsAtTurnStart {
                        player: PlayerScope::Controller,
                    },
                },
            }]);
        object.static_definitions = vec![definition.clone()].into();
        object.base_static_definitions = std::sync::Arc::new(vec![definition]);
    }

    runner.state_mut().layers_dirty.mark_full();
    layers::flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&creature].power, Some(0));

    runner.advance_to_upkeep();

    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(
        runner
            .state()
            .beginning_of_turn_snapshot
            .as_ref()
            .unwrap()
            .turn_number,
        starting_turn + 1
    );
    assert_eq!(
        runner.state().objects[&creature].power,
        Some(2),
        "normal turn advancement must expose the fresh snapshot to a previously clean CDA cache"
    );
}

/// CR 805.4d: In a shared team turn, an "each player's upkeep" ability whose
/// effect refers to that player triggers once for each active-team player. Each
/// firing keeps its own player binding, so the two Power Surge instances read
/// distinct beginning-of-turn snapshot rows.
#[test]
fn two_headed_giant_power_surge_fans_out_per_active_team_player() {
    let db = fixture_db();
    assert_power_surge_runtime_tree_supported(db);

    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::Untap);
    let power_surge = scenario.add_real_card(P2, POWER_SURGE, Zone::Battlefield, db);
    scenario.add_land_from_oracle(P0, "P0 Land", "");
    scenario.add_land_from_oracle(P1, "P1 Land A", "");
    scenario.add_land_from_oracle(P1, "P1 Land B", "");
    scenario.add_land_from_oracle(P1, "P1 Land C", "");
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), power_surge);
    assert_eq!(
        runner.state().objects[&power_surge].trigger_definitions[0]
            .definition()
            .phase_fanout,
        PhaseTriggerFanout::EachPlayer,
        "the production export must preserve the CR 805.4d participant scope"
    );
    let turn_number = runner.state().turn_number;
    runner.state_mut().beginning_of_turn_snapshot = Some(BeginningOfTurnSnapshot {
        turn_number,
        untapped_lands_controlled: HashMap::from([(P0, 1), (P1, 3), (P2, 0)]),
    });
    let life_before = runner.life(P0);
    let team_life_before = engine::game::players::team_life_total(runner.state(), P0);

    runner.advance_to_upkeep();
    let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone() else {
        panic!(
            "the two participant-bound firings must reach CR 603.3b ordering, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P2, "Power Surge's controller orders its triggers");
    assert_eq!(
        triggers.len(),
        2,
        "one shared upkeep must collect one firing per active-team player"
    );
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("the two Power Surge firings must settle onto the stack");

    let scoped_amounts: HashMap<_, _> = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == power_surge)
        .map(|entry| {
            let ability = entry
                .ability()
                .expect("Power Surge stack entry must carry its resolved ability");
            let player = ability
                .scoped_player
                .expect("each firing must own one active-team participant");
            let Effect::DealDamage { amount, .. } = &ability.effect else {
                panic!("Power Surge stack entry must deal damage");
            };
            (
                player,
                engine::game::quantity::resolve_quantity_with_targets(
                    runner.state(),
                    amount,
                    ability,
                ),
            )
        })
        .collect();
    assert_eq!(
        scoped_amounts,
        HashMap::from([(P0, 1), (P1, 3)]),
        "one shared upkeep must produce distinct P0/P1 snapshot-bound firings"
    );

    let mut damage_events = Vec::new();
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("priority passing must resolve both Power Surge firings");
        damage_events.extend(result.events.into_iter().filter_map(|event| match event {
            GameEvent::DamageDealt {
                target: TargetRef::Player(player),
                amount,
                ..
            } => Some((player, amount)),
            _ => None,
        }));
    }
    assert!(runner.state().stack.is_empty());
    damage_events.sort_by_key(|(player, _)| player.0);
    assert_eq!(damage_events, vec![(P0, 1), (P1, 3)]);
    assert_eq!(runner.life(P0), life_before - 1);
    assert_eq!(
        runner.life(P1),
        life_before - 3,
        "each Power Surge firing deals damage to its own active-team player"
    );
    assert_eq!(
        engine::game::players::team_life_total(runner.state(), P0),
        team_life_before - 4,
        "the two individual damage events reduce the shared 2HG life total by four"
    );
}

/// CR 603.4 + CR 805.4d: An intervening-if on an each-player phase trigger is
/// checked once for each active-team participant, and the same participant is
/// checked again at resolution. P0 is empty while P1 is not, so only P0's
/// firing exists; giving P0 a card before resolution then makes that firing do
/// nothing.
#[test]
fn two_headed_giant_phase_intervening_if_tracks_each_bound_participant() {
    let db = fixture_db();
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::Untap);
    let asylum_visitor = scenario.add_real_card(P2, ASYLUM_VISITOR, Zone::Battlefield, db);
    scenario.add_card_to_hand(P1, "P1 held card");
    let p0_late_card = scenario.add_card_to_library_top(P0, "P0 late card");
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), asylum_visitor);
    assert_eq!(
        runner.state().objects[&asylum_visitor].trigger_definitions[0]
            .definition()
            .phase_fanout,
        PhaseTriggerFanout::EachPlayer,
        "Asylum Visitor's condition names the individual upkeep player"
    );

    runner.advance_to_upkeep();
    let asylum_entries: Vec<_> = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == asylum_visitor)
        .collect();
    assert_eq!(
        asylum_entries.len(),
        1,
        "only empty-handed P0 may create an Asylum Visitor firing"
    );
    assert_eq!(
        asylum_entries[0]
            .ability()
            .expect("triggered ability")
            .scoped_player,
        Some(P0),
        "the pending firing must retain the participant whose condition passed"
    );

    let mut zone_events = Vec::new();
    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::draw(p0_late_card, Default::default()),
            &mut zone_events,
        ),
        "the late hand move must not pause for a replacement choice"
    );
    assert_eq!(
        runner.state().objects[&p0_late_card].zone,
        Zone::Hand,
        "the replacement-aware move must put P0's late card into hand"
    );
    let p0_hand_before = runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("P0")
        .hand
        .len();
    let p0_life_before = engine::game::players::team_life_total(runner.state(), P0);

    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passing must settle Asylum Visitor's trigger");
    }
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|player| player.id == P0)
            .expect("P0")
            .hand
            .len(),
        p0_hand_before,
        "the resolution-time CR 603.4 recheck must prevent bound participant P0 from drawing"
    );
    assert_eq!(
        engine::game::players::team_life_total(runner.state(), P0),
        p0_life_before,
        "the failed recheck must also prevent P0's team life loss"
    );
}

fn assert_shared_turn_active_player_mana_recipient(recipient: PlayerId) {
    const ORACLE: &str = "At the beginning of each player's upkeep, the active player adds {C}.";

    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(P2, "Active Player Mana Test", ORACLE)
        .id();
    let mut runner = scenario.build();

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), source);

    runner.advance_to_upkeep();
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == source)
            .count(),
        1,
        "CR 805.9 creates one firing, not one firing per active teammate"
    );

    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::NamedChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passing must reach the active-player choice");
            }
            other => panic!("unexpected window before active-player choice: {other:?}"),
        }
    }

    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("active-player mana trigger must pause for its controller's choice");
    };
    assert_eq!(
        player, P2,
        "the ability's controller makes the CR 805.9 choice"
    );
    assert!(matches!(
        choice_type,
        ChoiceType::Player {
            population: PlayerChoicePopulation::ActivePlayers,
            ..
        }
    ));
    assert_eq!(
        options,
        vec![P0.0.to_string(), P1.0.to_string()],
        "only the two active teammates may be chosen"
    );

    runner
        .act(GameAction::ChooseOption {
            choice: recipient.0.to_string(),
        })
        .expect("either active teammate must be a legal recipient");

    let other = if recipient == P0 { P1 } else { P0 };
    assert_eq!(mana_pool_total(&runner, recipient), 1);
    assert_eq!(mana_pool_total(&runner, other), 0);
}

/// CR 805.9: on a shared team turn, the ability controller chooses one active
/// player as the effect is applied. The trigger still occurs only once, and
/// either active teammate can receive the mana.
#[test]
fn two_headed_giant_active_player_reference_is_one_controller_made_choice() {
    assert_shared_turn_active_player_mana_recipient(P0);
    assert_shared_turn_active_player_mana_recipient(P1);
}

fn assert_shared_turn_active_player_population(recipient: PlayerId) {
    const ORACLE: &str = "At the beginning of each player's upkeep, destroy all creatures the active player controls.";

    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 43);
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(P2, "Active Player Population Test", ORACLE)
        .id();
    let p0_creature = scenario.add_creature(P0, "P0 Creature", 2, 2).id();
    let p1_creature = scenario.add_creature(P1, "P1 Creature", 2, 2).id();
    let mut runner = scenario.build();

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), source);

    runner.advance_to_upkeep();
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::NamedChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passing must reach the active-player choice");
            }
            other => panic!("unexpected window before active-player choice: {other:?}"),
        }
    }

    runner
        .act(GameAction::ChooseOption {
            choice: recipient.0.to_string(),
        })
        .expect("either active teammate must be a legal population binding");

    let (destroyed, spared) = if recipient == P0 {
        (p0_creature, p1_creature)
    } else {
        (p1_creature, p0_creature)
    };
    assert_eq!(runner.state().objects[&destroyed].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&spared].zone, Zone::Battlefield);
}

/// CR 805.9: The chosen active teammate also binds controller filters used to
/// enumerate an object population; the nominal active-player representative is
/// not read live by the resolver.
#[test]
fn two_headed_giant_active_player_choice_binds_object_population() {
    assert_shared_turn_active_player_population(P0);
    assert_shared_turn_active_player_population(P1);
}

/// CR 603.4 + CR 805.4d: A failing condition for the shared turn's team
/// representative must not suppress a teammate's independently valid firing.
#[test]
fn two_headed_giant_phase_intervening_if_does_not_pre_gate_on_representative() {
    let db = fixture_db();
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::Untap);
    let asylum_visitor = scenario.add_real_card(P2, ASYLUM_VISITOR, Zone::Battlefield, db);
    scenario.add_card_to_hand(P0, "P0 held card");
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), asylum_visitor);

    runner.advance_to_upkeep();
    let asylum_entries: Vec<_> = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == asylum_visitor)
        .collect();
    assert_eq!(
        asylum_entries.len(),
        1,
        "P0's failed condition must not suppress empty-handed P1's firing"
    );
    assert_eq!(
        asylum_entries[0]
            .ability()
            .expect("triggered ability")
            .scoped_player,
        Some(P1),
        "the surviving firing must retain P1 as its bound participant"
    );
}
