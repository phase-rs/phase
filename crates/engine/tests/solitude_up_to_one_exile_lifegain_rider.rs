//! Regression test for the Solitude ETB freeze: "When Solitude enters the
//! battlefield, exile up to one other target creature. Its controller gains
//! life equal to its power."
//!
//! Bug (CLASS defect): the trigger chain is a targeted `ChangeZone` parent
//! ("exile **up to one** target creature" — carrying a `multi_target` spec) with
//! a `GainLife` rider whose magnitude is `Power{Target}` and recipient is
//! `ParentTargetController`. Issue #3864 makes that rider INHERIT the parent's
//! chosen creature instead of surfacing its own target slot, so exactly one slot
//! is produced. But the `multi_target` consumption block in
//! `assign_selected_slots_in_chain` / `assign_targets_in_chain` computed its
//! `remaining_minimum` from `minimum_targets_in_chain(rider)` WITHOUT the
//! inheritance filter. That counted a phantom `Power{Target}` companion minimum
//! (1) for the rider, which cancelled the parent's own slot:
//! `current_slots = remaining(1) - phantom(1) = 0`. The parent consumed zero
//! slots, the chosen target was left unassigned, and the assigner hard-errored
//! with "Unused selected target slots", stranding `waiting_for =
//! TriggerTargetSelection` and soft-locking the UI.
//!
//! Plain "exile target creature" riders (Swords to Plowshares, Condemn) take the
//! mandatory single-target `else` branch that never computes `remaining_minimum`,
//! which is why #3864 looked fixed — only the "up to one" variant routes through
//! the buggy `multi_target` path.
//!
//! This test drives the full pipeline (cast Solitude → ETB trigger → choose the
//! exile target via the real `ChooseTarget` dispatch → resolve) and asserts the
//! target is exiled and its controller gains life equal to its power. It FAILS
//! before the `remaining_minimum` inheritance filter (panics on the rejected
//! `ChooseTarget`) and PASSES after.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SOLITUDE: &str = "When Solitude enters the battlefield, exile up to one other target creature. Its controller gains life equal to its power.";

fn white_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn solitude_etb_exiles_target_and_controller_gains_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, white_mana(5));

    // The opponent's 4/4 is the exile target. "Its controller gains life equal
    // to its power" → P1 (the bear's controller) gains 4 — a discriminating
    // assertion that the rider resolved against the parent's chosen creature.
    let bear = scenario.add_creature(P1, "Grizzly Bears", 4, 4).id();
    let solitude = scenario
        .add_creature_to_hand_from_oracle(P0, "Solitude", 3, 2, SOLITUDE)
        .id();

    let mut runner = scenario.build();
    let p1_life_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;

    let card_id = runner.state().objects[&solitude].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: solitude,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Solitude");
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Solitude ETB must pause on trigger target selection, got {:?}",
        runner.state().waiting_for
    );

    // The single "up to one" slot — choosing the bear must be accepted. Pre-fix
    // this `ChooseTarget` returned `Err("Unused selected target slots")`.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("choose the exile target");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "targeting must complete, not soft-lock in TriggerTargetSelection"
    );

    runner.advance_until_stack_empty();

    // The chosen creature was exiled (CR 603.2 trigger resolved cleanly)...
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Exile,
        "the targeted creature should have been exiled by Solitude's ETB"
    );

    // ...and its controller gained life equal to its power (CR 608.2i rider
    // resolved against the parent's chosen target).
    let p1_life_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;
    assert_eq!(
        p1_life_after - p1_life_before,
        4,
        "the exiled creature's controller (P1) should gain life equal to its power (4)"
    );
}
