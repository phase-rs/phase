//! Regression test for issue #4272: A creature put onto the battlefield by
//! Birthing Ritual's effect must fire its ETB triggered abilities.
//!
//! Previously, a Thraben Inspector put onto the battlefield by Birthing Ritual
//! did not trigger "investigate" and create a Clue token. The root cause was
//! that the old chain order (Dig → sacrifice sub_ability) emitted the ZoneChanged
//! event for the battlefield entry inside the DigChoice action handler, but then
//! the sacrifice continuation set a non-Priority WaitingFor. Since
//! `run_post_action_pipeline` is only invoked when the action settles at Priority,
//! the ZoneChanged event was never scanned for triggers.
//!
//! Fixed in #4289: the sacrifice now precedes the DigChoice in the chain, so
//! after the DigChoice resolves the chain is fully drained and Priority is given,
//! allowing `process_triggers` to see the ZoneChanged event and queue the ETB.
//!
//! https://github.com/phase-rs/phase/issues/4272

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BIRTHING_RITUAL_ORACLE: &str = "At the beginning of your end step, if you control a creature, look at the top seven cards of your library. Then you may sacrifice a creature. If you do, you may put a creature card with mana value X or less from among those cards onto the battlefield, where X is 1 plus the sacrificed creature's mana value. Put the rest on the bottom of your library in a random order.";

const THRABEN_INSPECTOR_ORACLE: &str = "When this creature enters, investigate.";

fn reach_end_step_with_trigger(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("empty attack declaration should succeed");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => return,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            _ if runner.state().phase == Phase::End => return,
            _ => runner.pass_both_players(),
        }
    }
}

/// Drive the game from the end-step priority window (with Birthing Ritual's
/// trigger on the stack) through the full resolution chain until the stack is
/// empty. Returns when the action settles at Priority with an empty stack in
/// Phase::End, or after 50 iterations (test failure path).
fn resolve_birthing_ritual_chain(
    runner: &mut GameRunner,
    thraben_id: engine::types::identifiers::ObjectId,
) {
    for _ in 0..50 {
        match runner.state().waiting_for.clone() {
            // Done: stack empty in end step.
            WaitingFor::Priority { .. }
                if runner.state().stack.is_empty() && runner.state().phase == Phase::End =>
            {
                return
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare empty attackers");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            // "Then you may sacrifice a creature." — accept the optional.
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept sacrifice");
            }
            // "which creature to sacrifice?" — pick the first eligible one.
            // With only one creature on the battlefield this prompt may be
            // skipped (auto-sacrifice), but handle it defensively.
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Sacrifice,
                cards,
                ..
            } => {
                let victim = cards.first().copied().expect("sacrificeable creature");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![victim],
                    })
                    .expect("sacrifice creature");
            }
            // "put a creature from among those onto the battlefield" — pick
            // Thraben Inspector.
            WaitingFor::DigChoice { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![thraben_id],
                    })
                    .expect("select Thraben Inspector");
            }
            _ => {
                // Catch-all: pass priority to drain any unexpected state.
                runner.act(GameAction::PassPriority).ok();
            }
        }
    }
}

#[test]
fn birthing_ritual_etb_triggers_fire_for_creature_put_onto_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Birthing Ritual as a permanent enchantment.
    let _ritual = scenario
        .add_creature(P0, "Birthing Ritual", 0, 0)
        .as_enchantment()
        .from_oracle_text(BIRTHING_RITUAL_ORACLE)
        .id();

    // A creature P0 controls to sacrifice. Mana value 1 → X = 1 + 1 = 2,
    // so any creature with MV ≤ 2 passes the filter.
    let _goblin = scenario
        .add_creature(P0, "Goblin Soldier", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    // Thraben Inspector in the library (position 0 = top after insert).
    // Parsed with its "When this creature enters, investigate." ETB trigger.
    // MV = 1 satisfies the filter (1 ≤ X = 2).
    for i in 0..6 {
        scenario.add_card_to_library_top(P0, &format!("Library Filler {i}"));
    }
    let thraben = scenario
        .add_spell_to_library_top(P0, "Thraben Inspector", false)
        .as_creature()
        .from_oracle_text(THRABEN_INSPECTOR_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();

    // Advance to end step and wait for Birthing Ritual's trigger on the stack.
    reach_end_step_with_trigger(&mut runner);

    assert_eq!(
        runner.state().phase,
        Phase::End,
        "scenario must reach the end step before driving the resolution"
    );

    let trigger_on_stack = runner
        .state()
        .stack
        .iter()
        .any(|entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. }));
    assert!(
        trigger_on_stack,
        "Birthing Ritual's end-step trigger must be on the stack; stack = {:?}",
        runner.state().stack
    );

    // Drive the full Birthing Ritual resolution: sacrifice → DigChoice →
    // Thraben Inspector enters → ETB trigger resolves.
    resolve_birthing_ritual_chain(&mut runner, thraben);

    // CR 701.16a + CR 603.2: Thraben Inspector's ETB trigger ("When this
    // creature enters, investigate.") must have fired and created a Clue token.
    let clue_on_battlefield = runner
        .state()
        .objects
        .values()
        .any(|obj| obj.zone == Zone::Battlefield && obj.is_token && obj.name == "Clue");
    assert!(
        clue_on_battlefield,
        "Thraben Inspector's investigate ETB must have created a Clue token; \
         battlefield = {:?}",
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield)
            .map(|o| &o.name)
            .collect::<Vec<_>>()
    );

    // Thraben Inspector itself must be on the battlefield.
    assert_eq!(
        runner.state().objects.get(&thraben).map(|o| o.zone),
        Some(Zone::Battlefield),
        "Thraben Inspector must be on the battlefield after Birthing Ritual's effect"
    );
}
