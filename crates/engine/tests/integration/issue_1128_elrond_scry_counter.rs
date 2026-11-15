//! Regression for issue #1128: Elrond, Master of Healing — first ability.
//!
//! "Whenever you scry, put a +1/+1 counter on each of up to X target
//! creatures, where X is the number of cards looked at while scrying this
//! way."
//!
//! CR 701.22a + CR 701.22d: X is not the requested scry amount — it is the
//! *effective* look count after the engine clamps to library size. Root cause
//! of the reported bug: no `QuantityRef` existed to represent "cards looked
//! at while scrying this way" at all, so the "where X is …" binder fell
//! through and the whole ability lowered to `Effect::Unimplemented`. Fixed by
//! adding `QuantityRef::TriggeringScryLookCount`, carried as PER-EVENT
//! provenance: the scry resolution stamps its effective look count onto its
//! own `PlayerPerformedAction::Scry` event, each queued "whenever you scry"
//! trigger preserves that event through target selection and stack resolution
//! (`PendingTriggerContext`), and the quantity resolves from the CURRENT
//! trigger's event — never from a global scalar, which a multi-scry
//! resolution would overwrite before the queued triggers construct their
//! target slots (see the multi-scry test below). Parsed via the composed
//! grammar in `oracle_nom::quantity::parse_scry_look_count_ref`.
//!
//! The first test seeds a library with FEWER cards than the requested scry
//! amount (scry 3 with only 2 cards left) to prove X is bound to the clamped
//! look count (2), not the literal requested amount (3). The second resolves
//! TWO scries with different look counts in ONE resolution and proves the two
//! queued triggers expose DIFFERENT target-slot limits.

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

/// Maintainer review on PR #5872 (multi-scry blocker): one resolution that
/// scries TWICE with different amounts ("Scry 3.\nScry 1." — contiguous
/// resolution lines chain into a single ability) queues TWO independent
/// "whenever you scry" triggers (CR 603.2: once per scry event), and each
/// trigger exposes ITS OWN scry's look count as its target-slot limit — never
/// the value of whichever scry happened LAST.
///
/// Two revert discriminators, matching the two halves of the fix:
///
/// 1. Per-event provenance: with a global last-scry scalar instead of the
///    look count riding each scry's own event, both queued triggers would
///    read the second scry's 1 — the first prompt would offer 1 slot, not 3.
///
/// 2. Pause/resume event collection: the second scry's
///    `PlayerPerformedAction` event is emitted while the action resumes
///    between the two interactive `ScryChoice` pauses of the same
///    resolution. `run_post_action_pipeline` scans an action's events only
///    when it settles to `Priority`, so before the repair in the
///    `ScryChoice` resume seam (`engine_resolution_choices.rs`, which parks
///    the resumed slice via `park_observer_triggers_if_paused`) that event
///    was dropped and only ONE trigger ever fired — the second prompt below
///    never appeared.
#[test]
fn elrond_multi_scry_queues_one_trigger_per_scry_with_its_own_look_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Elrond, Master of Healing", 3, 4, ELROND_ABILITY_1);
    // More creatures than the larger scry's slot count (3), so both triggers'
    // target selections are genuinely ambiguous and pause interactively.
    let c1 = scenario.add_creature(P0, "Ward A", 2, 2).id();
    let c2 = scenario.add_creature(P0, "Ward B", 2, 2).id();
    let c3 = scenario.add_creature(P0, "Ward C", 2, 2).id();
    let c4 = scenario.add_creature(P0, "Ward D", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Double Scrying Rod", false, "Scry 3.\nScry 1.")
        .id();
    // Enough library that neither scry clamps: look counts are exactly 3 and 1.
    scenario.with_library_top(P0, &["Lib 1", "Lib 2", "Lib 3", "Lib 4"]);

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&spell).unwrap().card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the Scry 3 / Scry 1 spell");
    runner.advance_until_stack_empty();

    // Complete both interactive scry choices (keep everything on top). The
    // first prompt must show 3 cards, the second 1 card.
    let mut scry_prompt_sizes = Vec::new();
    for _ in 0..2 {
        let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
            panic!(
                "expected a ScryChoice prompt (saw {:?} so far), got {}",
                scry_prompt_sizes,
                runner.waiting_for_kind()
            );
        };
        scry_prompt_sizes.push(cards.len());
        runner
            .act(GameAction::SelectCards { cards })
            .expect("submit the scry (keep all looked-at cards on top)");
    }
    assert_eq!(
        scry_prompt_sizes,
        vec![3, 1],
        "the resolution must scry 3 first, then 1"
    );

    // CR 603.2 + CR 603.3b: BOTH scry events must have queued a trigger. The
    // two same-controller triggers surface an OrderTriggers prompt, then each
    // trigger's OWN target selection. Stack ordering may surface the later
    // scry's trigger first, so select from each prompt by its own slot limit
    // and assert the two limits as an order-independent pair.
    let mut slot_limits = Vec::new();
    for _ in 0..2 {
        advance_to_trigger_target_selection(&mut runner);
        let WaitingFor::TriggerTargetSelection { target_slots, .. } =
            runner.state().waiting_for.clone()
        else {
            unreachable!("advance_to_trigger_target_selection guarantees this variant");
        };
        slot_limits.push(target_slots.len());
        let targets = match target_slots.len() {
            1 => vec![TargetRef::Object(c3)],
            3 => vec![TargetRef::Object(c1), TargetRef::Object(c2)],
            count => panic!("expected the scry-1 or scry-3 target limit, got {count}"),
        };
        runner
            .act(GameAction::SelectTargets { targets })
            .expect("each completed scry trigger must accept targets within its own limit");
    }
    runner.advance_until_stack_empty();

    slot_limits.sort_unstable();

    assert_eq!(
        slot_limits,
        vec![1, 3],
        "each trigger must expose ITS OWN scry's look count as its slot limit: \
         one offers 3 slots and the other 1 — independently, in the same resolution"
    );

    // Disjoint targets: each targeted creature got exactly one counter from
    // exactly one trigger; the creature targeted by neither got none.
    assert_eq!(
        p1p1_counters(&runner, c1),
        1,
        "scry-3 trigger, first target"
    );
    assert_eq!(
        p1p1_counters(&runner, c2),
        1,
        "scry-3 trigger, second target"
    );
    assert_eq!(p1p1_counters(&runner, c3), 1, "scry-1 trigger's target");
    assert_eq!(
        p1p1_counters(&runner, c4),
        0,
        "creature targeted by neither trigger must receive no counter"
    );
}
