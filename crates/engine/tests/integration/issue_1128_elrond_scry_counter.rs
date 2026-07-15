//! Regression for issue #1128: Elrond, Master of Healing — first ability.
//!
//! "Whenever you scry, put a +1/+1 counter on each of up to X target
//! creatures, where X is the number of cards looked at while scrying this
//! way."
//!
//! CR 701.22a: X is not the requested scry amount — it is the *effective*
//! look count after clamping to library size. Root cause of the reported bug:
//! no `QuantityRef` existed to represent "cards looked at while scrying this
//! way" at all, so the "where X is …" binder fell through and the whole
//! ability lowered to `Effect::Unimplemented`. Fixed by adding
//! `QuantityRef::TriggeringScryLookCount`, backed by
//! `GameState::last_scry_look_count` (set in
//! `game::effects::scry::apply_scry_after_replacement_without_draw`, mirroring
//! the existing `last_discover_value` / `TriggeringDiscoverValue` pattern for
//! Curator of Sun's Creation's "discover again for the same value"), plus a
//! parser branch recognizing the "where X is the number of cards looked at
//! while scrying this way" phrase.
//!
//! This test seeds a library with FEWER cards than the requested scry amount
//! (scry 3 with only 2 cards left) to prove X is bound to the clamped look
//! count (2), not the literal requested amount (3).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const ELROND_ABILITY_1: &str = "Whenever you scry, put a +1/+1 counter on each of up to X target creatures, where X is the number of cards looked at while scrying this way.";

fn p1p1_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object still present")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Drive the stack until Elrond's scry trigger surfaces its interactive
/// `TriggerTargetSelection` prompt, passing priority / draining trigger
/// ordering as needed (mirrors `wise_mothman_milled_trigger.rs`'s
/// `advance_to_trigger_target_selection`).
fn advance_to_trigger_target_selection(runner: &mut GameRunner) {
    let mut guard = 0;
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        guard += 1;
        assert!(
            guard < 16,
            "scry trigger never surfaced a TriggerTargetSelection prompt; \
             last waiting_for = {}",
            runner.waiting_for_kind()
        );
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should be accepted while reaching the trigger");
    }
}

#[test]
fn elrond_scry_counters_are_capped_by_clamped_look_count_not_requested_amount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Elrond, Master of Healing", 3, 4, ELROND_ABILITY_1);
    let c1 = scenario.add_creature(P0, "Ward A", 2, 2).id();
    let c2 = scenario.add_creature(P0, "Ward B", 2, 2).id();
    let c3 = scenario.add_creature(P0, "Ward C", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scrying Rod", false, "Scry 3.")
        .id();
    // Only 2 cards left in the library: requested N=3 must clamp to X=2.
    scenario.with_library_top(P0, &["Lib 1", "Lib 2"]);

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&spell).unwrap().card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the Scry 3 spell");
    runner.advance_until_stack_empty();

    let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected ScryChoice after the Scry 3 spell resolves, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        cards.len(),
        2,
        "library only has 2 cards; scry 3 must clamp its look-count to 2"
    );
    runner
        .act(GameAction::SelectCards { cards })
        .expect("submit the scry (keep both on top)");

    advance_to_trigger_target_selection(&mut runner);

    let WaitingFor::TriggerTargetSelection { target_slots, .. } =
        runner.state().waiting_for.clone()
    else {
        unreachable!("advance_to_trigger_target_selection guarantees this variant");
    };
    assert_eq!(
        target_slots.len(),
        2,
        "X must resolve to the clamped scry look-count (2), not the requested scry amount (3)"
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(c1), TargetRef::Object(c2)],
        })
        .expect("selecting two creature targets must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1_counters(&runner, c1),
        1,
        "first chosen creature must receive exactly one +1/+1 counter"
    );
    assert_eq!(
        p1p1_counters(&runner, c2),
        1,
        "second chosen creature must receive exactly one +1/+1 counter"
    );
    assert_eq!(
        p1p1_counters(&runner, c3),
        0,
        "unselected creature must receive no counter"
    );
}
