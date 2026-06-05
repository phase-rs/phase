//! Regression: GitHub issue #406 — The Wise Mothman ("Whenever one or more
//! nonland cards are milled, …") and the broader milled-trigger class.
//!
//! Bug: the Oracle parser never emitted `TriggerMode::Milled` for any
//! "…cards are milled" / "…mills a card" condition. Every milled-trigger card
//! (The Wise Mothman, Glowing One, Infesting Radroach, Mirelurk Queen,
//! Screeching Scorchbeast, Zellix Sanity Flayer) parsed its mill trigger to
//! `TriggerMode::Unknown`, so the trigger never fired even though the runtime
//! matcher (`game/trigger_matchers.rs::match_milled`) was already correct.
//!
//! Fix: `parser/oracle_trigger.rs::try_parse_event` now recognizes both the
//! passive ("are milled") and active ("mills <object>") mill predicates and
//! emits `TriggerMode::Milled` (CR 701.17a). The "one or more …" batched
//! semantics are stamped by the existing caller plumbing (`def.batched`).
//!
//! These tests drive a *real* mill through `apply` — a Tome Scour spell
//! ("Target player mills five cards") resolves and produces genuine
//! `ZoneChanged { from: Library, to: Graveyard }` events — and assert the
//! milled trigger fires as a consequence. No synthetic `GameEvent` is injected.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{ActionResult, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

use crate::support::shared_card_db as load_db;

/// Give P0 the mana to cast Tome Scour ({U}).
fn add_blue_mana(runner: &mut engine::game::scenario::GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    pool.add(ManaUnit::new(ManaType::Blue, dummy, false, vec![]));
}

/// Cast P0's Tome Scour ("Target player mills five cards") aimed at
/// `mill_target`'s library and return the `ActionResult` after the target is
/// chosen — the spell is on the stack, not yet resolved. The mill events are
/// produced when the caller resolves the stack.
fn cast_tome_scour(
    runner: &mut engine::game::scenario::GameRunner,
    tome_scour: ObjectId,
    mill_target: PlayerId,
) -> ActionResult {
    let card_id = runner.state().objects[&tome_scour].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: tome_scour,
            card_id,
            targets: vec![],
        })
        .expect("Tome Scour cast should be accepted");

    // Tome Scour targets a player — choose `mill_target` explicitly so the
    // mill lands on the intended library.
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        result = runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(mill_target)),
            })
            .expect("Tome Scour should accept the chosen player target");
    }
    result
}

/// Issue #406 — the issue card. The Wise Mothman's passive batched milled
/// trigger ("Whenever one or more nonland cards are milled, put a +1/+1
/// counter on each of up to X target creatures…") must FIRE when nonland cards
/// are milled. Because the trigger has up-to-X creature targets, a fired
/// trigger surfaces an interactive `TriggerTargetSelection`; pre-fix the
/// `Unknown` mode meant the trigger never fired and no prompt ever appeared.
#[test]
fn wise_mothman_passive_milled_trigger_fires() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The Wise Mothman on P0's battlefield — the milled-trigger payoff card.
    scenario.add_real_card(P0, "The Wise Mothman", Zone::Battlefield, db);

    // Tome Scour in P0's hand — the real mill source ({U}: mill five cards).
    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // P0's library top: five nonland cards (Lightning Bolt is an instant), so
    // the Mill 5 mills exactly five nonland cards. Padding keeps the library
    // non-empty so the mill is not truncated.
    for _ in 0..9 {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);

    // Mill P0's own library — the passive "are milled" trigger fires
    // regardless of whose cards are milled.
    let result = cast_tome_scour(&mut runner, tome_scour, P0);

    // Drive the stack until the milled trigger surfaces its target selection.
    // A `TriggerTargetSelection` here is direct proof the milled trigger fired:
    // pre-fix (mode == Unknown) the trigger never entered `process_triggers`.
    let mut result = result;
    let mut guard = 0;
    while !matches!(
        result.waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        guard += 1;
        assert!(
            guard < 64,
            "The Wise Mothman's milled trigger never fired — expected a \
             TriggerTargetSelection prompt after milling five nonland cards; \
             last waiting_for = {:?}",
            result.waiting_for
        );
        result = match runner.act(GameAction::PassPriority) {
            Ok(r) => r,
            Err(_) => panic!(
                "stack stalled before the milled trigger fired; \
                 last waiting_for = {:?}",
                result.waiting_for
            ),
        };
    }

    // The five nonland cards really moved Library -> Graveyard.
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        6, // 5 milled + Tome Scour itself
        "five cards should have been milled into P0's graveyard (plus Tome Scour)"
    );

    // Resolve the milled trigger by choosing zero of the up-to-X targets.
    runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("choosing zero up-to-X targets should be legal");
    runner.advance_until_stack_empty();
}

/// Active-voice milled trigger: Glowing One ("Whenever a player mills a nonland
/// card, you gain 1 life."). This per-card (non-batched) trigger fires once per
/// milled nonland card, with an observable life-gain effect — so we can assert
/// the exact firing count end-to-end.
#[test]
fn glowing_one_active_milled_trigger_gains_life_per_card() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    // Glowing One on P0's battlefield — the active-voice milled-trigger card.
    scenario.add_real_card(P0, "Glowing One", Zone::Battlefield, db);

    // Tome Scour in P0's hand; it mills the *opponent's* library so the
    // active-voice "a player mills" subject is satisfied.
    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // P1's library top: five nonland cards — each milled nonland card fires
    // Glowing One's trigger once (CR 603.2c — per-event, not batched).
    for _ in 0..9 {
        scenario.add_real_card(P1, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);

    let life_before = runner.life(P0);

    // Mill P1's library — the active-voice "a player mills" subject fires for
    // any milling player.
    cast_tome_scour(&mut runner, tome_scour, P1);
    runner.advance_until_stack_empty();

    // Five nonland cards milled => Glowing One's trigger fired five times =>
    // P0 gained five life. Pre-fix (mode == Unknown) the trigger never fired
    // and life would be unchanged.
    assert_eq!(
        runner.life(P0),
        life_before + 5,
        "Glowing One's active-voice milled trigger must fire once per milled \
         nonland card (5 cards => +5 life)"
    );

    // The five cards genuinely left P1's library for their graveyard.
    assert_eq!(
        runner.state().players[1].graveyard.len(),
        5,
        "five cards should have been milled into P1's graveyard"
    );
}

/// Drain priority/state-based passes until either an `OptionalEffectChoice`
/// prompt appears (a fired optional trigger) or the stack settles. Returns
/// `true` if an `OptionalEffectChoice` was surfaced.
fn run_until_optional_choice_or_settled(runner: &mut engine::game::scenario::GameRunner) -> bool {
    for _ in 0..64 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            return true;
        }
        // CR 603.3b (#531): drain the per-controller ordering prompt with identity.
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return false;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            return false;
        }
    }
    false
}

/// Opponent-scoped active-voice milled trigger: Infesting Radroach ("Whenever
/// an opponent mills a nonland card, if this creature is in your graveyard, you
/// may return it to your hand."). Its parsed `valid_card` carries
/// `controller: Opponent`, and the trigger source lives in the *graveyard*.
///
/// This pins the subtlest part of the #406 fix — the `controller: Opponent`
/// match path for a graveyard-resident trigger source. A milled card was never
/// on the battlefield, so its `controller` equals its `owner`; the trigger
/// fires only when the milling player is an opponent of the trigger's
/// controller.
///
/// POSITIVE: P0's Infesting Radroach (in P0's graveyard) sees P1 — an opponent
/// — mill a nonland card, so the trigger fires (surfacing its optional
/// return-to-hand choice).
#[test]
fn infesting_radroach_opponent_milled_trigger_fires_on_opponent_mill() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Infesting Radroach in P0's graveyard — the opponent-scoped milled trigger
    // is active from the graveyard (`trigger_zones: [Graveyard]`).
    scenario.add_real_card(P0, "Infesting Radroach", Zone::Graveyard, db);

    // Tome Scour in P0's hand; aimed at P1's library so the milling player is
    // an opponent of Infesting Radroach's controller (P0).
    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // P1's library top: nonland cards to mill.
    for _ in 0..9 {
        scenario.add_real_card(P1, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);

    cast_tome_scour(&mut runner, tome_scour, P1);

    // The opponent-scoped trigger must fire — surfacing the optional
    // return-to-hand choice. Pre-fix (mode == Unknown) no prompt ever appears.
    assert!(
        run_until_optional_choice_or_settled(&mut runner),
        "Infesting Radroach's opponent-scoped milled trigger must fire when an \
         opponent mills a nonland card; expected an OptionalEffectChoice prompt, \
         got {:?}",
        runner.state().waiting_for
    );

    // Decline the optional return — Infesting Radroach stays in P0's graveyard.
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional return-to-hand should be legal");
    runner.advance_until_stack_empty();
}

/// Opponent-scoped active-voice milled trigger — NEGATIVE case. Infesting
/// Radroach's `controller: Opponent` filter must NOT match when the
/// trigger's controller (P0) mills their OWN library: a card P0 milled has
/// `controller == owner == P0`, which is not an opponent of P0, so the trigger
/// must not fire. This locks the opponent-scoping against false positives.
#[test]
fn infesting_radroach_opponent_milled_trigger_silent_on_own_mill() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "Infesting Radroach", Zone::Graveyard, db);
    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // P0's OWN library — milling it must NOT fire the opponent-scoped trigger.
    for _ in 0..9 {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);

    // Mill P0's own library through a real Tome Scour cast.
    cast_tome_scour(&mut runner, tome_scour, P0);

    // No `OptionalEffectChoice` may appear — the opponent-scoped trigger must
    // stay silent when P0 mills their own cards.
    assert!(
        !run_until_optional_choice_or_settled(&mut runner),
        "Infesting Radroach's opponent-scoped milled trigger must NOT fire when \
         the trigger's controller mills their OWN library; got an unexpected \
         OptionalEffectChoice prompt"
    );

    // The five cards really were milled (so the negative result is not just an
    // empty mill) and Infesting Radroach is untouched in P0's graveyard.
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        7, // Infesting Radroach + 5 milled + Tome Scour
        "five cards should have been milled into P0's own graveyard"
    );
}

// ---------------------------------------------------------------------------
// Trigger-target-selection re-stamp + take-ordering regression (the hang fix)
// ---------------------------------------------------------------------------
//
// These tests drive The Wise Mothman's batched milled trigger ("put a +1/+1
// counter on each of up to X target creatures, where X is the number of nonland
// cards milled this way") all the way through to an *interactive* target
// selection that chooses one or more creatures — the path the earlier
// `wise_mothman_passive_milled_trigger_fires` test never exercised because it
// resolved with zero targets.
//
// Two coupled defects lived in
// `game/engine_stack.rs::handle_trigger_target_selection_{select_targets,
// choose_target}`:
//
//   * BUG 1 (CR 601.2c + CR 603.2c): `multi_target.max = EventContextAmount`
//     reads `state.current_trigger_match_count`, which is stamped only while a
//     trigger surfaces its prompt and is `None` again by the *later* `apply()`
//     that completes the target walk. So the assignment re-resolved the bound to
//     0, consumed 0 of the selected slots, and returned
//     `InvalidAction("Unused selected targets")` (the all-at-once `SelectTargets`
//     path) / `InvalidAction("Unused selected target slots")` (the step-by-step
//     `ChooseTarget` path) even though the player had chosen legal creatures.
//
//   * BUG 2 (CR 603.3d): the handlers took `state.pending_trigger` *before* the
//     fallible assignment. `apply()` does not roll back on `Err`, and
//     `sync_waiting_for` never runs after an `Err`, so the Bug-1 error left
//     `pending_trigger = None` while `waiting_for` was still
//     `TriggerTargetSelection` — bricking every subsequent action.
//
// The fix re-stamps `current_trigger_match_count` (save/restore) around the
// assignment and takes the pending trigger only after the assignment succeeds.

/// Drive the stack until The Wise Mothman's milled trigger surfaces its
/// interactive `TriggerTargetSelection` prompt, passing priority as needed.
fn advance_to_trigger_target_selection(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        guard += 1;
        assert!(
            guard < 64,
            "milled trigger never surfaced a TriggerTargetSelection prompt; \
             last waiting_for = {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should be accepted while reaching the trigger");
    }
}

/// Read the creature `TargetRef`s offered by the *current* slot of a live
/// `TriggerTargetSelection` prompt (the slot the next `ChooseTarget` fills).
fn current_slot_legal_creatures(runner: &engine::game::scenario::GameRunner) -> Vec<TargetRef> {
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            let slot = selection
                .current_slot
                .min(target_slots.len().saturating_sub(1));
            target_slots[slot].legal_targets.clone()
        }
        other => panic!("expected TriggerTargetSelection prompt, got {other:?}"),
    }
}

/// `+1/+1` counters on a battlefield object.
fn p1p1_counters(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
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

/// Build a Wise Mothman scenario: the Mothman + `creature_count` vanilla
/// creatures on P0's battlefield, a Tome Scour in P0's hand, and a library of
/// nonland cards to mill. Returns the runner plus the creature ids so the test
/// can target them.
fn wise_mothman_scenario(
    db: &'static CardDatabase,
    creature_count: usize,
) -> (engine::game::scenario::GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "The Wise Mothman", Zone::Battlefield, db);
    let creatures: Vec<ObjectId> = (0..creature_count)
        .map(|_| scenario.add_vanilla(P0, 2, 2))
        .collect();
    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // Library top: nonland cards so Mill 5 mills five nonland cards.
    for _ in 0..9 {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);
    (runner, tome_scour, creatures)
}

/// BUG 1 — all-at-once `SelectTargets` path. Milling five nonland cards with two
/// legal creatures present must let the controller put a `+1/+1` counter on each
/// of TWO chosen creatures. Pre-fix, the EventContextAmount bound collapsed to 0
/// at assign time and `SelectTargets` returned
/// `InvalidAction("Unused selected targets")`.
#[test]
fn wise_mothman_select_targets_places_counters_on_chosen_creatures() {
    let Some(db) = load_db() else {
        return;
    };

    let (mut runner, tome_scour, creatures) = wise_mothman_scenario(db, 2);
    let result = cast_tome_scour(&mut runner, tome_scour, P0);
    let _ = result;
    advance_to_trigger_target_selection(&mut runner);

    // Five nonland cards genuinely milled into the graveyard.
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        6, // 5 milled + Tome Scour
        "five cards should have been milled into P0's graveyard"
    );

    // Choose both creatures in one shot. Pre-fix this errors with
    // "Unused selected targets"; post-fix it succeeds.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                TargetRef::Object(creatures[0]),
                TargetRef::Object(creatures[1]),
            ],
        })
        .expect("selecting two creature targets must succeed (Bug 1 fix)");

    runner.advance_until_stack_empty();

    // Each chosen creature received exactly one +1/+1 counter. The number of
    // counters placed equals min(milled_count = 5, legal_count = 2) = 2.
    assert_eq!(
        p1p1_counters(&runner, creatures[0]),
        1,
        "first chosen creature must receive exactly one +1/+1 counter"
    );
    assert_eq!(
        p1p1_counters(&runner, creatures[1]),
        1,
        "second chosen creature must receive exactly one +1/+1 counter"
    );
}

/// BUG 1 — step-by-step `ChooseTarget` path. Same trigger, but the controller
/// walks the up-to-X slots one creature at a time. Exercises
/// `handle_trigger_target_selection_choose_target`'s `Complete` arm, which has
/// the identical re-stamp + take-ordering fix.
#[test]
fn wise_mothman_choose_target_walk_places_counters_on_chosen_creatures() {
    let Some(db) = load_db() else {
        return;
    };

    let (mut runner, tome_scour, creatures) = wise_mothman_scenario(db, 2);
    cast_tome_scour(&mut runner, tome_scour, P0);
    advance_to_trigger_target_selection(&mut runner);

    // Walk the slots: pick creatures[0] then creatures[1], one ChooseTarget per
    // step, until the prompt clears (the `Complete` arm fires on the last step).
    let mut to_pick = vec![
        TargetRef::Object(creatures[0]),
        TargetRef::Object(creatures[1]),
    ];
    let mut guard = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        guard += 1;
        assert!(guard < 16, "ChooseTarget walk did not terminate");
        let legal = current_slot_legal_creatures(&runner);
        // Pick the next intended creature if it is legal at this slot; otherwise
        // stop adding targets (an empty ChooseTarget completes the up-to-X walk).
        let next = to_pick.first().filter(|t| legal.contains(t)).cloned();
        match next {
            Some(target) => {
                to_pick.remove(0);
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target),
                    })
                    .expect("choosing a legal creature target must succeed (Bug 1 fix)");
            }
            None => {
                // No further intended target is legal here — complete the walk.
                runner
                    .act(GameAction::ChooseTarget { target: None })
                    .expect("completing the up-to-X walk must succeed (Bug 1 fix)");
            }
        }
    }

    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1_counters(&runner, creatures[0]),
        1,
        "first chosen creature must receive exactly one +1/+1 counter (ChooseTarget path)"
    );
    assert_eq!(
        p1p1_counters(&runner, creatures[1]),
        1,
        "second chosen creature must receive exactly one +1/+1 counter (ChooseTarget path)"
    );
}

/// BUG 2 — recoverability / no-brick invariant. This drives the *same* real
/// trigger and submits a legal one-creature selection. The discriminating
/// assertion is the post-action invariant: the game must NEVER be left in the
/// bricked shape `pending_trigger == None && waiting_for == TriggerTargetSelection`.
///
/// Pre-fix, the Bug-1 EventContextAmount=0 error fired *after* the pending
/// trigger was already taken, stranding exactly that bricked shape — so this
/// invariant failed. Post-fix, the selection resolves cleanly and the invariant
/// holds. (If a future regression reintroduced only Bug 2 without Bug 1, the
/// same invariant would still catch an assign-Err that strands the prompt.)
#[test]
fn wise_mothman_target_selection_never_bricks_pending_trigger() {
    let Some(db) = load_db() else {
        return;
    };

    let (mut runner, tome_scour, creatures) = wise_mothman_scenario(db, 1);
    cast_tome_scour(&mut runner, tome_scour, P0);
    advance_to_trigger_target_selection(&mut runner);

    // Submit one legal creature target. Pre-fix: Err + bricked state. Post-fix:
    // Ok and the prompt is consumed.
    let act_result = runner.act(GameAction::SelectTargets {
        targets: vec![TargetRef::Object(creatures[0])],
    });

    // Whatever the outcome, the game must never be stranded with a
    // TriggerTargetSelection prompt and no pending trigger to satisfy it.
    let bricked = runner.state().pending_trigger.is_none()
        && matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        );
    assert!(
        !bricked,
        "trigger target selection must never strand a TriggerTargetSelection \
         prompt with no pending trigger (CR 603.3d recoverability); \
         act_result = {act_result:?}"
    );

    // Post-fix the selection succeeds and the counter lands.
    act_result.expect("selecting one legal creature target must succeed (Bug 1 + Bug 2 fix)");
    runner.advance_until_stack_empty();
    assert_eq!(
        p1p1_counters(&runner, creatures[0]),
        1,
        "the single chosen creature must receive exactly one +1/+1 counter"
    );
}
