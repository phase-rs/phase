//! Controls for the controller-phrase grammar and binder: an anaphoric
//! controller phrase keeps binding to the player its ability introduced.
//!
//! - Balefire Dragon: "that player" is the damaged player (trigger path).
//! - Reservoir Kraken: "they" is the accepting opponent, not the caster's
//!   opponent set evaluated from the accepting player's frame.
//! - Temporal Distortion: "that player" is the upkeep player, bound through
//!   the counter-target fallback that reads the caller's player antecedents.
//! - Counter-placement watchers: "they" in "another creature they control" /
//!   "one or more creatures they control" is the opponent who placed the
//!   counters, bound through every subject arm that reads the actor scope.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const BALEFIRE_DRAGON: &str = "Flying\nWhenever this creature deals combat damage to a player, it deals that much damage to each creature that player controls.";

const TEMPORAL_DISTORTION: &str = "Whenever a creature or land becomes tapped, put an hourglass counter on it.\nEach permanent with an hourglass counter on it doesn't untap during its controller's untap step.\nAt the beginning of each player's upkeep, remove all hourglass counters from permanents that player controls.";

const RESERVOIR_KRAKEN: &str = "Trample, ward {2}\nAt the beginning of each combat, if this creature is untapped, any opponent may tap an untapped creature they control. If they do, tap this creature and create a 1/1 blue Fish creature token with \"This token can't be blocked.\"";

/// Pass the declare-attackers priority window and declare no blockers, leaving
/// the runner ready for `combat_damage`.
fn declare_no_blockers(runner: &mut GameRunner) {
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
                return;
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::DeclareAttackers => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
}

/// CR 510.2 + CR 603.2 + CR 608.2c: Balefire Dragon's trigger names the damaged
/// player, so "each creature that player controls" is that player's creatures
/// and never the attacker's controller's.
#[test]
fn balefire_dragon_damages_only_the_damaged_players_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature_from_oracle(P0, "Balefire Dragon", 6, 6, BALEFIRE_DRAGON)
        .id();
    let own_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let their_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let their_giant = scenario.add_creature(P1, "P1 Giant", 5, 5).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(dragon, AttackTarget::Player(P1))])
        .expect("declare Balefire Dragon");
    declare_no_blockers(&mut runner);
    let outcome = runner.combat_damage();

    // Reach guard: the dragon dealt its 6 combat damage and its trigger resolved.
    assert_eq!(outcome.life_delta(P1), -6, "P1 takes 6 combat damage");
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { source_id, .. } if *source_id == dragon
        )),
        "Balefire Dragon's trigger must resolve; events={:?}",
        outcome.events()
    );

    assert_eq!(outcome.zone_of(their_bear), Zone::Graveyard);
    assert_eq!(outcome.zone_of(their_giant), Zone::Graveyard);
    assert_eq!(
        outcome.zone_of(own_bear),
        Zone::Battlefield,
        "the attacker's controller's creature is not the damaged player's"
    );
}

/// CR 608.2c: in "any opponent may tap an untapped creature they control",
/// "they" is the opponent who accepts, so the choice is among that opponent's
/// creatures.
#[test]
fn reservoir_kraken_accepting_opponent_taps_their_own_creature() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let kraken = scenario
        .add_creature_from_oracle(P0, "Reservoir Kraken", 6, 6, RESERVOIR_KRAKEN)
        .id();
    let own_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let their_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);

    let mut prompted = None;
    let mut legal_for_acceptor = None;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                prompted = Some(player);
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::MultiTargetSelection {
                player,
                legal_targets,
                ..
            } => {
                legal_for_acceptor = Some((player, legal_targets.clone()));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![their_bear],
                    })
                    .expect("pick P1's creature");
            }
            // Pre-existing spurious target prompt for the trigger's controller;
            // answered so the trigger proceeds, not asserted.
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(own_bear)],
                    })
                    .expect("answer trigger target prompt");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::BeginCombat => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => break,
        }
    }

    // Reach guard: the accepting opponent was asked.
    assert_eq!(prompted, Some(P1), "P1 must be offered the choice");
    assert_eq!(
        legal_for_acceptor,
        Some((P1, vec![their_bear])),
        "the accepting opponent chooses among their own creatures"
    );
    let state = runner.state();
    assert!(state.objects[&their_bear].tapped, "P1's creature is tapped");
    assert!(state.objects[&kraken].tapped, "Reservoir Kraken is tapped");
    assert!(
        !state.objects[&own_bear].tapped,
        "P0's creature is not the accepting opponent's"
    );
}

/// CR 608.2c: in Temporal Distortion's "at the beginning of each player's
/// upkeep, remove all hourglass counters from permanents that player
/// controls", "that player" is the upkeep player, so the source controller's
/// permanents keep their counters.
///
/// The filter currently becomes a stack-time target slot (a pre-existing
/// defect, #9899), so the drive answers that prompt when it appears and checks
/// only what the binding decides: whose permanents are offered and reached.
/// "Every P1 permanent loses its counters" (CR 115.10a) is not asserted.
#[test]
fn temporal_distortion_removes_counters_from_the_upkeep_players_permanents() {
    let hourglass = || CounterType::Generic("hourglass".to_string());
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.with_library_top(P0, &["P0 Top A", "P0 Top B", "P0 Top C"]);
    scenario.with_library_top(P1, &["P1 Top A", "P1 Top B", "P1 Top C"]);
    scenario.at_phase(Phase::End);
    let td = scenario
        .add_enchantment_from_oracle(P0, "Temporal Distortion", TEMPORAL_DISTORTION)
        .id();
    let c0 = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let c1a = scenario.add_creature(P1, "P1 Bear A", 2, 2).id();
    let c1b = scenario.add_creature(P1, "P1 Bear B", 2, 2).id();
    for creature in [c0, c1a, c1b] {
        scenario.with_counter(creature, hourglass(), 1);
    }
    let mut runner = scenario.build();

    // Advance once: re-calling `advance_to_phase` during P1's upkeep restarts
    // the step and fires the upkeep trigger again. Settle with `act` only.
    runner.advance_to_phase(Phase::Draw);
    let mut offered: Vec<TargetRef> = Vec::new();
    for _ in 0..40 {
        let state = runner.state();
        if state.phase == Phase::Draw && state.active_player == P1 {
            break;
        }
        match state.waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                source_id: Some(source),
                target_slots,
                ..
            } if source == td => {
                let slot = target_slots
                    .iter()
                    .find(|slot| slot.effect_kind == EffectKind::RemoveCounter)
                    .expect("Temporal Distortion's prompt has a RemoveCounter slot");
                offered.extend(slot.legal_targets.iter().cloned());
                let first = slot.legal_targets[0].clone();
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(first),
                    })
                    .expect("answer Temporal Distortion's target prompt");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for during P1's upkeep: {other:?}"),
        }
    }

    let state = runner.state();
    // Reach guard: the drive got past P1's upkeep.
    assert!(
        state.phase == Phase::Draw && state.active_player == P1,
        "the drive must reach P1's draw step; phase={:?} active={:?} waiting_for={:?}",
        state.phase,
        state.active_player,
        state.waiting_for
    );
    let counters = |id| {
        state.objects[&id]
            .counters
            .get(&hourglass())
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(
        counters(c0),
        1,
        "the source controller's creature is not the upkeep player's"
    );
    assert!(
        counters(c1a) == 0 || counters(c1b) == 0,
        "an upkeep player's creature loses its hourglass counter; (c1a, c1b) = ({}, {})",
        counters(c1a),
        counters(c1b)
    );
    assert!(
        offered
            .iter()
            .all(|target| *target == TargetRef::Object(c1a) || *target == TargetRef::Object(c1b)),
        "every offered target is the upkeep player's; offered={offered:?}"
    );
}

/// An opponent (P1) casts "Put a +1/+1 counter on target creature" at a
/// creature controlled by `recipient_controller` while P0's watcher, built from
/// `watcher_oracle`, looks on. Returns how many of the watcher's effects
/// resolved and P0's life change.
fn watcher_response_to_opponent_counter(
    watcher_oracle: &str,
    recipient_controller: PlayerId,
) -> (usize, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let watcher = scenario
        .add_creature_from_oracle(P0, "Counter Watcher", 2, 2, watcher_oracle)
        .id();
    let recipient = scenario
        .add_creature(recipient_controller, "Recipient Bear", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Opp Bolster",
            true,
            "Put a +1/+1 counter on target creature.",
        )
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner.cast(spell).target_objects(&[recipient]).resolve();

    // Reach guard: the opponent's counter landed.
    outcome.assert_counters(recipient, CounterType::Plus1Plus1, 1);
    let firings = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved { source_id, .. } if *source_id == watcher
            )
        })
        .count();
    (firings, outcome.life_delta(P0))
}

/// CR 608.2c + CR 603.2: in "whenever an opponent puts one or more counters
/// on another creature they control" and "… on one or more creatures they
/// control", "they" is the opponent who placed the counters (the trigger's
/// actor), not the watcher's controller, so only a placement on that
/// opponent's creature matches the trigger event. The two forms reach the
/// "another " and "one or more " subject arms, which must read the actor scope
/// like the "a "/"an " arm (Bold Plagiarist).
///
/// Per form: a counter on the placing opponent's creature fires once (CR
/// 603.2c: 1 firing, +1 life); a counter on the watcher controller's creature
/// does not (the actor gate still passes, so only the "they control" recipient
/// gate suppresses it).
#[test]
fn counter_placement_subject_binds_they_control_to_the_placing_opponent() {
    let forms = [
        "Whenever an opponent puts one or more counters on another creature they control, you gain 1 life.",
        "Whenever an opponent puts one or more counters on one or more creatures they control, you gain 1 life.",
    ];
    let observed: Vec<_> = forms
        .iter()
        .map(|form| {
            (
                *form,
                watcher_response_to_opponent_counter(form, P1),
                watcher_response_to_opponent_counter(form, P0),
            )
        })
        .collect();
    let expected: Vec<_> = forms.iter().map(|form| (*form, (1, 1), (0, 0))).collect();
    assert_eq!(
        observed, expected,
        "(form, on the opponent's creature, on your creature) as (firings, P0 life change)"
    );
}
