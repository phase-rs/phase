//! CR 608.2c + CR 707.10: reflexive "if you don't copy a spell this way, draw a
//! card" gate (Shiko and Narset, Unified — issue #1370).
//!
//! Flurry copies the controller's second spell each turn and, *only when no copy
//! is made*, draws a card. The draw is gated on
//! `Not(EffectOutcome(OptionalEffectPerformed))` — "the preceding copy did NOT
//! occur this resolution". Before the fix, `mandatory_parent_effect_performed`
//! lacked a `CopySpell` arm and fell into its `_ => true` default, so the engine
//! always claimed a copy had happened and suppressed the draw even when no copy
//! was actually made. These tests discriminate the two branches:
//!   - a copyable second spell => a copy IS made => NO draw;
//!   - an uncopyable second spell => NO copy => the draw fires.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::zones::Zone;
use engine::types::Phase;

const SHIKO: &str = "Flying, vigilance\nFlurry — Whenever you cast your second spell each turn, copy that spell if it targets a permanent or player, and you may choose new targets for the copy. If you don't copy a spell this way, draw a card.";

fn red_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit {
            color: ManaType::Red,
            source_id: ObjectId(0),
            pip_id: engine::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        })
        .collect()
}

fn card_id_of(runner: &GameRunner, id: ObjectId) -> CardId {
    runner.state().objects.get(&id).unwrap().card_id
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = card_id_of(runner, spell);
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast spell");
    drive(runner);
}

/// Drive the pipeline to stack-empty, answering each prompt with a fixed policy:
/// player targets choose P1, trigger/copy targets keep their default, priority
/// passes. The Flurry copy's `MayChooseNewTargets` retarget keeps targets.
fn drive(runner: &mut GameRunner) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let t = slot
                    .legal_targets
                    .iter()
                    .find(|t| matches!(t, TargetRef::Player(P1)))
                    .or_else(|| slot.legal_targets.first())
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: t })
                    .expect("choose cast target");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let t = target_slots[selection.current_slot]
                    .legal_targets
                    .first()
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: t })
                    .expect("choose trigger target");
            }
            WaitingFor::CopyRetarget {
                target_slots,
                current_slot,
                ..
            } => {
                let keep = target_slots[current_slot].current.clone();
                runner
                    .act(GameAction::ChooseTarget { target: keep })
                    .expect("keep copy target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// "Opt" sits on top of P0's library; it reaches P0's hand iff Flurry draws.
fn opt_drawn(runner: &GameRunner) -> bool {
    runner
        .state()
        .objects
        .values()
        .any(|o| o.name == "Opt" && o.zone == Zone::Hand && o.owner == P0)
}

/// CR 608.2c + CR 707.10: a COPYABLE second spell is copied this way, so the
/// "if you don't copy a spell this way" rider must NOT draw.
#[test]
fn copyable_second_spell_does_not_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Shiko and Narset, Unified", 3, 3, SHIKO);
    let bolt1 = scenario.add_bolt_to_hand(P0);
    let bolt2 = scenario.add_bolt_to_hand(P0);
    scenario.add_spell_to_library_top(P0, "Opt", true);
    scenario.with_mana_pool(P0, red_mana(6));
    let mut runner = scenario.build();

    cast(&mut runner, bolt1);
    cast(&mut runner, bolt2);

    assert!(
        !opt_drawn(&runner),
        "CR 608.2c: Flurry copied the second spell, so it must NOT also draw."
    );
}

/// CR 608.2c + CR 707.10: an UNCOPYABLE second spell isn't put onto the stack as
/// a copy, so the rider's negated gate holds and Flurry MUST draw a card.
#[test]
fn uncopyable_second_spell_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Shiko and Narset, Unified", 3, 3, SHIKO);
    let bolt = scenario.add_bolt_to_hand(P0);
    let uncopyable = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Uncopyable Bolt",
            true,
            "Uncopyable Bolt deals 3 damage to any target.\nThis spell can't be copied.",
        )
        .id();
    scenario.add_spell_to_library_top(P0, "Opt", true);
    scenario.with_mana_pool(P0, red_mana(6));
    let mut runner = scenario.build();

    cast(&mut runner, bolt);
    cast(&mut runner, uncopyable);

    assert!(
        opt_drawn(&runner),
        "CR 608.2c: Flurry made no copy (the spell can't be copied), so it must draw."
    );
}
