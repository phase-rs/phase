//! Controls for the controller-phrase grammar and binder: an anaphoric
//! controller phrase keeps binding to the player its ability introduced.
//!
//! - Balefire Dragon: "that player" is the damaged player (trigger path).
//! - Reservoir Kraken: "they" is the accepting opponent, not the caster's
//!   opponent set evaluated from the accepting player's frame.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const BALEFIRE_DRAGON: &str = "Flying\nWhenever this creature deals combat damage to a player, it deals that much damage to each creature that player controls.";

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
