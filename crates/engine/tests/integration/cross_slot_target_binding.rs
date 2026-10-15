//! Cross-slot object-relative target binding (parser-misparse backlog root
//! cause #27): Puca's Mischief / Spawnbroker / Daring Thief.
//!
//! These tests drive the REAL trigger-through-stack pipeline end to end
//! (upkeep/ETB/untap trigger -> `WaitingFor::TriggerTargetSelection` ->
//! `WaitingFor::OptionalEffectChoice` -> resolution), unlike the inline
//! `#[cfg(test)]` unit tests in `game/ability_utils.rs`, which call the
//! production seam functions directly against hand-built `GameState`s. Both
//! layers are required: this file is the one that fails if the fix is
//! reverted end to end, per `docs/AI-CONTRIBUTOR.md` §5 check (i).
//!
//! Verbatim Oracle text (Scryfall-verified, see the implementation plan's
//! Step 0):
//!   Puca's Mischief: "At the beginning of your upkeep, you may exchange
//!     control of target nonland permanent you control and target nonland
//!     permanent an opponent controls with equal or lesser mana value."
//!   Spawnbroker: "When this creature enters, you may exchange control of
//!     target creature you control and target creature with power less than
//!     or equal to that creature's power an opponent controls."
//!   Daring Thief: "Inspired — Whenever this creature becomes untapped, you
//!     may exchange control of target nonland permanent you control and
//!     target permanent an opponent controls that shares a card type with
//!     it."
//!   Perplexing Chimera (T16, MG-A class 4 -- verbatim, Scryfall-verified):
//!     "Whenever an opponent casts a spell, you may exchange control of this
//!     creature and that spell. If you do, you may choose new targets for
//!     the spell." (target_a = SelfRef, target_b = TriggeringSource.)
//!
//! None of the first three cards is in
//! `crates/engine/tests/fixtures/integration_cards.json.gz`, so every board
//! for T1/T2/T3/T5 is built from verbatim Oracle text via the
//! `*_from_oracle` scenario helpers. T16 is a direct `validate_targets_in_
//! chain` unit test on a hand-built stack-object ability (the plan's
//! documented fallback) rather than a full SpellCast-trigger fixture -- see
//! that test's doc comment for why.
//!
//! Mutiny and Breaking of the Fellowship ("another target creature that player
//! controls") bind their second slot to the first slot's controller, both when
//! the targets are chosen and when the spell resolves.

use engine::game::ability_utils::validate_targets_in_chain;
use engine::game::effects::exchange_control;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::zones::create_object;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    ContinuousModification, Duration, Effect, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, ChainStep, GameState, RetargetSlotAddress, StackEntry,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

use super::controller_anaphor_binds_through_context::{
    cast_free, main_phase_scenario, settle_recording_offers,
};

const PUCA_MISCHIEF_ORACLE: &str = "At the beginning of your upkeep, you may exchange control of \
target nonland permanent you control and target nonland permanent an opponent controls with \
equal or lesser mana value.";

const SPAWNBROKER_ORACLE: &str = "When this creature enters, you may exchange control of target \
creature you control and target creature with power less than or equal to that creature's power \
an opponent controls.";

const DARING_THIEF_ORACLE: &str = "Inspired — Whenever this creature becomes untapped, you may \
exchange control of target nonland permanent you control and target permanent an opponent \
controls that shares a card type with it.";

// ---------------------------------------------------------------------------
// Shared driving helpers
// ---------------------------------------------------------------------------

/// Pass priority (draining any per-controller trigger-ordering prompt along
/// the way) until either a `TriggerTargetSelection` prompt appears or the
/// stack settles empty at `Priority`. Used both to REACH a target-selection
/// prompt and to prove one is NEVER reached (T5).
fn advance_until_trigger_targets_or_settled(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => return,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
}

/// Pass priority (draining trigger-ordering prompts) until the CR 608.2d
/// "you may" resolution-time prompt appears, or the stack settles empty.
fn advance_until_optional_or_settled(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
}

/// The offered set for `target_slots[index]`. CRITICAL: `target_slots[i].
/// legal_targets` is the STATIC pre-selection superset built at announcement
/// (for a prior-target-relative slot, B5's UNION over every candidate of the
/// prior object slot — the same superset regardless of which prior candidate
/// is chosen). The LIVE, per-choice-EXACT set (B4's binding) is published
/// only via `selection.current_legal_targets`, and only for whichever slot
/// is CURRENTLY `selection.current_slot`. Reading the static field for the
/// NEXT slot right after choosing the prior one would NOT discriminate B4's
/// exact narrowing from B5's static union (both contain the same superset) —
/// a real risk of a non-discriminating assertion. This helper reads the live
/// field for the current slot and falls back to the static field only for a
/// NOT-yet-current slot (informational only, never asserted against for the
/// exact-narrowing claims below).
fn slot_legal_targets(runner: &GameRunner, index: usize) -> Vec<TargetRef> {
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            if index == selection.current_slot {
                selection.current_legal_targets.clone()
            } else {
                target_slots[index].legal_targets.clone()
            }
        }
        other => panic!("expected TriggerTargetSelection, got {other:?}"),
    }
}

/// Submit `id` as the object target for the CURRENT slot of an in-progress
/// `TriggerTargetSelection`, asserting it is actually legal first (so a
/// mis-set-up fixture fails loudly here rather than downstream).
fn choose_object_target(runner: &mut GameRunner, id: ObjectId) {
    let current_slot = match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { selection, .. } => selection.current_slot,
        other => panic!("expected TriggerTargetSelection, got {other:?}"),
    };
    let legal = slot_legal_targets(runner, current_slot);
    assert!(
        legal.contains(&TargetRef::Object(id)),
        "object {id:?} must be a legal target for slot {current_slot}, legal={legal:?}"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        })
        .expect("choosing the trigger target must succeed");
}

/// Accept the pending CR 608.2d optional-effect prompt.
fn accept_optional(runner: &mut GameRunner) {
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { .. } => {}
        other => panic!("expected OptionalEffectChoice, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional exchange must succeed");
}

fn controller_of(runner: &GameRunner, id: ObjectId) -> engine::types::player::PlayerId {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object exists")
        .controller
}

// ---------------------------------------------------------------------------
// T2-e2e / T5-e2e — Puca's Mischief (mana-value axis, Units A + B)
// ---------------------------------------------------------------------------

/// Build a board with Puca's Mischief on P0's battlefield plus P0- and
/// P1-controlled nonland artifacts at the given mana values. Starts at
/// `Phase::Untap` (before P0's upkeep) so `advance_to_upkeep` drives the
/// REAL upkeep-trigger pipeline.
fn puca_mischief_board(
    mine_mv: &[u32],
    theirs_mv: &[u32],
) -> (GameRunner, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_enchantment_from_oracle(P0, "Puca's Mischief", PUCA_MISCHIEF_ORACLE);

    let mut mine = Vec::new();
    for (i, mv) in mine_mv.iter().enumerate() {
        let id = scenario
            .add_creature(P0, &format!("My Permanent {i}"), 0, 0)
            .as_artifact()
            .with_mana_cost(ManaCost::generic(*mv))
            .id();
        mine.push(id);
    }
    let mut theirs = Vec::new();
    for (i, mv) in theirs_mv.iter().enumerate() {
        let id = scenario
            .add_creature(P1, &format!("Their Permanent {i}"), 0, 0)
            .as_artifact()
            .with_mana_cost(ManaCost::generic(*mv))
            .id();
        theirs.push(id);
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    (runner, mine, theirs)
}

/// T2-e2e: with MV6 chosen for slot A, the opponent's MV4 permanent is
/// offered for slot B (before the fix, only the MV0 permanent was) and the
/// full exchange swaps control on resolution.
#[test]
fn t2_e2e_puca_mischief_mv6_choice_offers_and_swaps_mv4_permanent() {
    let (mut runner, mine, theirs) = puca_mischief_board(&[6, 1], &[4, 0]);
    let (my_mv6, my_mv1) = (mine[0], mine[1]);
    let (their_mv4, their_mv0) = (theirs[0], theirs[1]);

    // Sanity: mana values landed as intended.
    assert_eq!(runner.state().objects[&my_mv6].effective_mana_value(), 6);
    assert_eq!(runner.state().objects[&their_mv4].effective_mana_value(), 4);
    assert_eq!(runner.state().objects[&their_mv0].effective_mana_value(), 0);

    runner.advance_to_upkeep();
    advance_until_trigger_targets_or_settled(&mut runner);
    assert_eq!(
        runner.waiting_for_kind(),
        "TriggerTargetSelection",
        "Puca's Mischief must reach trigger target selection on a satisfiable board"
    );

    // Positive reach-guard: slot A (mine) offers >= 2 candidates.
    let slot_a = slot_legal_targets(&runner, 0);
    assert!(
        slot_a.len() >= 2,
        "slot A must offer both of my permanents as candidates, got {slot_a:?}"
    );
    assert!(slot_a.contains(&TargetRef::Object(my_mv6)));
    assert!(slot_a.contains(&TargetRef::Object(my_mv1)));

    // Choose the MV6 permanent for slot A.
    choose_object_target(&mut runner, my_mv6);

    // T2's core claim, with the T4 reach-guard folded in: slot B's offered
    // set is a STRICT superset of {the MV0 permanent} — it must now also
    // contain the MV4 permanent, which the pre-fix threshold (reading
    // ObjectManaValue{CostPaidObject} as 0) excluded.
    let slot_b = slot_legal_targets(&runner, 1);
    assert!(
        slot_b.contains(&TargetRef::Object(their_mv0)),
        "slot B must still offer the MV0 permanent (0 <= 6), got {slot_b:?}"
    );
    assert!(
        slot_b.contains(&TargetRef::Object(their_mv4)),
        "slot B must offer the MV4 permanent once MV6 is bound for slot A \
         (4 <= 6) — this is the symptom-1 fix, got {slot_b:?}"
    );
    assert!(
        slot_b.len() > 1,
        "slot B's offered set must be a STRICT superset of {{MV0}}, got {slot_b:?}"
    );

    choose_object_target(&mut runner, their_mv4);

    advance_until_optional_or_settled(&mut runner);
    accept_optional(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        controller_of(&runner, my_mv6),
        P1,
        "my MV6 permanent must now be controlled by the opponent"
    );
    assert_eq!(
        controller_of(&runner, their_mv4),
        P0,
        "the opponent's MV4 permanent must now be controlled by me"
    );
    // Untouched permanents keep their original controllers.
    assert_eq!(controller_of(&runner, my_mv1), P0);
    assert_eq!(controller_of(&runner, their_mv0), P1);
}

/// Multi-authority hostile fixture (plan Verification Matrix, T2's paired
/// hostile-fixture row): on the SAME board, slot B's live offered set must
/// narrow DIFFERENTLY depending on which slot-A candidate is chosen. A
/// latched or last-wins binding (rather than a live per-selection bind)
/// would offer the SAME slot-B set regardless of the slot-A choice; this
/// fails that shape by choosing MV1 (rather than MV6) for slot A and
/// asserting slot B collapses to {MV0} only.
#[test]
fn t2_multi_authority_slot_b_narrows_differently_per_slot_a_choice() {
    let (mut runner, mine, theirs) = puca_mischief_board(&[6, 1], &[4, 0]);
    let my_mv1 = mine[1];
    let (their_mv4, their_mv0) = (theirs[0], theirs[1]);

    runner.advance_to_upkeep();
    advance_until_trigger_targets_or_settled(&mut runner);
    assert_eq!(runner.waiting_for_kind(), "TriggerTargetSelection");

    choose_object_target(&mut runner, my_mv1);

    let slot_b = slot_legal_targets(&runner, 1);
    assert!(
        slot_b.contains(&TargetRef::Object(their_mv0)),
        "slot B must still offer the MV0 permanent (0 <= 1), got {slot_b:?}"
    );
    assert!(
        !slot_b.contains(&TargetRef::Object(their_mv4)),
        "with MV1 bound for slot A, the MV4 permanent must NOT be offered \
         (4 > 1) — a latched/last-wins binding would wrongly still show it \
         (the MV6-choice test shows it DOES appear when MV6 is bound), \
         got {slot_b:?}"
    );
}

/// T5-e2e (CR 603.3d): when every permanent the opponent controls exceeds
/// the mana value of every permanent I control, no assignment satisfies both
/// slots as a set (Puca's Gatherer ruling) and the trigger is removed from
/// the stack entirely — `TriggerTargetSelection` must never appear, and the
/// game must simply return to priority. The positive reach-guard is folded
/// in first: an adjacent (satisfiable) board DOES reach
/// `TriggerTargetSelection`, proving the trigger is detectable at all.
#[test]
fn t5_e2e_puca_mischief_removed_when_no_satisfying_assignment_exists() {
    // Positive reach-guard: an adjacent, satisfiable board reaches
    // TriggerTargetSelection.
    let (mut satisfiable, _, _) = puca_mischief_board(&[6, 1], &[4, 0]);
    satisfiable.advance_to_upkeep();
    advance_until_trigger_targets_or_settled(&mut satisfiable);
    assert_eq!(
        satisfiable.waiting_for_kind(),
        "TriggerTargetSelection",
        "the adjacent satisfiable board must reach trigger target selection"
    );

    // Main claim: every opponent permanent (MV5, MV9) strictly exceeds every
    // permanent I control (MV1, MV2) -- no set-wise legal assignment exists.
    let (mut unsatisfiable, _, _) = puca_mischief_board(&[1, 2], &[5, 9]);
    unsatisfiable.advance_to_upkeep();
    advance_until_trigger_targets_or_settled(&mut unsatisfiable);
    assert_ne!(
        unsatisfiable.waiting_for_kind(),
        "TriggerTargetSelection",
        "Puca's Mischief must be removed as having no legal targets (CR 603.3d) \
         rather than surface a target-selection prompt"
    );
    assert!(
        unsatisfiable.state().stack.is_empty(),
        "the untargetable trigger must not sit on the stack"
    );
    assert_eq!(
        unsatisfiable.waiting_for_kind(),
        "Priority",
        "the game must return to priority with the stack empty, got {:?}",
        unsatisfiable.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// T3-e2e — Spawnbroker (power axis, characteristic-agnostic proof)
// ---------------------------------------------------------------------------

/// T3-e2e: casting Spawnbroker fires its ETB trigger at all (today, before
/// the fix, the trigger is removed as targetless when the opponent's only
/// candidate reads power <= 0 -- that removal IS the reach-guard), offers
/// the 2/2 for slot B, excludes the 7/7, and swaps control on resolution.
#[test]
fn t3_e2e_spawnbroker_etb_offers_and_swaps_power_bound_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spawnbroker = scenario
        .add_creature_to_hand_from_oracle(P0, "Spawnbroker", 2, 3, SPAWNBROKER_ORACLE)
        .id();
    let my_5_5 = scenario.add_creature(P0, "My 5/5", 5, 5).id();
    let their_7_7 = scenario.add_creature(P1, "Their 7/7", 7, 7).id();
    let their_2_2 = scenario.add_creature(P1, "Their 2/2", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;

    let card_id = runner.state().objects[&spawnbroker].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spawnbroker,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Spawnbroker must succeed");

    advance_until_trigger_targets_or_settled(&mut runner);
    assert_eq!(
        runner.waiting_for_kind(),
        "TriggerTargetSelection",
        "Spawnbroker's ETB trigger must reach the stack with a legal assignment \
         (today, before the fix, it is removed as targetless), got {:?}",
        runner.state().waiting_for
    );

    let slot_a = slot_legal_targets(&runner, 0);
    assert!(slot_a.contains(&TargetRef::Object(my_5_5)));
    choose_object_target(&mut runner, my_5_5);

    let slot_b = slot_legal_targets(&runner, 1);
    assert!(
        slot_b.contains(&TargetRef::Object(their_2_2)),
        "the 2/2 (power 2 <= 5) must be offered for slot B, got {slot_b:?}"
    );
    assert!(
        !slot_b.contains(&TargetRef::Object(their_7_7)),
        "the 7/7 (power 7 > 5) must NOT be offered for slot B, got {slot_b:?}"
    );
    choose_object_target(&mut runner, their_2_2);

    advance_until_optional_or_settled(&mut runner);
    accept_optional(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(controller_of(&runner, my_5_5), P1);
    assert_eq!(controller_of(&runner, their_2_2), P0);
    assert_eq!(
        controller_of(&runner, their_7_7),
        P1,
        "the excluded 7/7 must keep its original controller"
    );
}

// ---------------------------------------------------------------------------
// T1-e2e — Daring Thief (shared-card-type axis; Unit B alone, no parser fix)
// ---------------------------------------------------------------------------

/// T1-e2e: Daring Thief's parser output was already correct at BASE_SHA
/// (`SharesQuality { reference: ParentTarget }` needs no rebind); this row
/// discriminates the RUNTIME half (Unit B) alone.
///
/// DRIVING NOTE (traced, not assumed): a real CR 502.3 untap-STEP untap
/// does not detect this trigger through the scenario harness's
/// `advance_to_upkeep()` -- traced to `GameRunner::advance_to_phase`'s FIRST
/// `turns::auto_advance` call running RAW (outside `apply()`'s post-action
/// pipeline that feeds generated events to `triggers::collect_triggers_into_
/// deferred` / `process_triggers` — see `engine_priority.rs`'s
/// `run_post_action_pipeline_from_with_policy`), so the untap step's own
/// `GameEvent::PermanentUntapped` is generated and silently discarded before
/// any trigger-matching ever sees it. This is a scenario-harness gap, not a
/// production one (real `GameAction` dispatch always routes through the
/// pipeline). The established, precedented workaround for this EXACT
/// trigger class already lives in this suite --
/// `issue_4247_well_rested.rs`'s `well_rested_granted_untap_trigger_routes_
/// to_host_controller`: synthesize the real `GameEvent::PermanentUntapped`
/// and feed it through the real `triggers::process_triggers` detection path.
/// This does NOT hand-construct the ability (`process_triggers` runs the
/// real trigger-matching/announcement machinery from a real event) --
/// it substitutes for the untap STEP's event generation only, which the test
/// harness cannot otherwise deliver to trigger detection.
#[test]
fn t1_e2e_daring_thief_untap_trigger_offers_and_swaps_shared_card_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let daring_thief = scenario
        .add_creature_from_oracle(P0, "Daring Thief", 3, 2, DARING_THIEF_ORACLE)
        .id();
    let my_art = scenario
        .add_creature(P0, "My Artifact", 0, 0)
        .as_artifact()
        .id();
    // A second artifact I control, so slot A has TWO candidates that each
    // leave a non-empty slot B (Daring Thief itself is a THIRD raw candidate
    // for slot A, but binding it would leave slot B empty -- no permanent
    // shares "Creature" with Their Artifact/Their Enchantment -- so it is
    // not a valid joint assignment). With only ONE candidate leaving slot B
    // satisfiable, `forced_unique_targeting`-style auto-resolution would
    // silently fill both slots without ever surfacing
    // `TriggerTargetSelection`; a second valid "mine" candidate keeps this a
    // real interactive choice so the offered/excluded assertions below are
    // meaningful.
    let my_art_2 = scenario
        .add_creature(P0, "My Second Artifact", 0, 0)
        .as_artifact()
        .id();
    let their_art = scenario
        .add_creature(P1, "Their Artifact", 0, 0)
        .as_artifact()
        .id();
    let their_ench = scenario
        .add_creature(P1, "Their Enchantment", 0, 0)
        .as_enchantment()
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner
        .state_mut()
        .objects
        .get_mut(&daring_thief)
        .unwrap()
        .tapped = true;

    // DRIVING NOTE (traced, not assumed): a real CR 502.3 untap-STEP untap
    // does not detect this trigger through the scenario harness's
    // `advance_to_upkeep()` -- traced to `GameRunner::advance_to_phase`'s
    // FIRST `turns::auto_advance` call running RAW, outside `apply()`'s
    // post-action pipeline that feeds generated events to trigger detection
    // (`engine_priority.rs`'s `run_post_action_pipeline_from_with_policy`),
    // so the untap step's own `GameEvent::PermanentUntapped` is silently
    // discarded before any trigger-matching ever sees it. This is a
    // scenario-harness gap, not a production one. The established,
    // precedented workaround for exactly this trigger class already lives in
    // this suite -- `issue_4247_well_rested.rs`'s
    // `well_rested_granted_untap_trigger_routes_to_host_controller` and
    // `orzhov_advokist.rs`'s `resolve_advokist_upkeep` (which drives a
    // TARGETED trigger through `WaitingFor::TriggerTargetSelection` from the
    // same `process_triggers` entry point) -- synthesize the real
    // `GameEvent::PermanentUntapped` and feed it through the real
    // `triggers::process_triggers` detection/announcement path. This does
    // NOT hand-construct the ability: `process_triggers` runs the real
    // trigger-matching, target-slot-building and (per orzhov_advokist)
    // interactive-announcement machinery from a real event; it substitutes
    // only for the untap STEP's event generation, which the test harness
    // cannot otherwise deliver to trigger detection.
    let events = vec![GameEvent::PermanentUntapped {
        object_id: daring_thief,
    }];
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    // `process_triggers` parks the trigger (`state.pending_trigger`) rather
    // than installing `WaitingFor::TriggerTargetSelection` synchronously; the
    // prompt materializes on the next properly-wrapped `apply()` action
    // boundary, exactly like any other post-action pipeline effect.
    advance_until_trigger_targets_or_settled(&mut runner);

    assert_eq!(
        runner.waiting_for_kind(),
        "TriggerTargetSelection",
        "Daring Thief's Inspired trigger must reach the stack after a real \
         PermanentUntapped event, got {:?}",
        runner.state().waiting_for
    );

    let slot_a = slot_legal_targets(&runner, 0);
    assert!(slot_a.contains(&TargetRef::Object(my_art)));
    assert!(slot_a.contains(&TargetRef::Object(my_art_2)));
    choose_object_target(&mut runner, my_art);

    let slot_b = slot_legal_targets(&runner, 1);
    assert!(
        slot_b.contains(&TargetRef::Object(their_art)),
        "the opponent's Artifact shares a card type with mine and must be offered, \
         got {slot_b:?}"
    );
    assert!(
        !slot_b.contains(&TargetRef::Object(their_ench)),
        "the opponent's Enchantment shares NO card type with my Artifact and must \
         NOT be offered, got {slot_b:?}"
    );
    choose_object_target(&mut runner, their_art);

    advance_until_optional_or_settled(&mut runner);
    accept_optional(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(controller_of(&runner, my_art), P1);
    assert_eq!(controller_of(&runner, their_art), P0);
    assert_eq!(
        controller_of(&runner, their_ench),
        P1,
        "the excluded Enchantment must keep its original controller"
    );
    assert_eq!(
        controller_of(&runner, my_art_2),
        P0,
        "the un-chosen second artifact must keep its original controller"
    );
}

// ---------------------------------------------------------------------------
// T16 — Perplexing Chimera (Unit C, MG-A class 4: stack-object slot)
// ---------------------------------------------------------------------------

/// T16: Unit C's class-4 loosening (a stack-object slot now routes through
/// `validate_pinned_targets` instead of being dropped by the terminal `None`
/// arm's bare `state.battlefield.contains(object_id)` test) is outcome-
/// neutral. Perplexing Chimera's real Oracle text (Scryfall-verified):
/// "Whenever an opponent casts a spell, you may exchange control of this
/// creature and that spell. If you do, you may choose new targets for the
/// spell." -- `target_a = SelfRef`, `target_b = TriggeringSource`.
///
/// FALLBACK FORM, TRACED (per GAP-2's explicit permission): a faithful
/// SpellCast-trigger-through-stack integration fixture was traced and found
/// impractical for a DIFFERENT reason than the validation seam under test.
/// `TargetFilter::TriggeringSource` has no arm in `filter::filter_inner`'s
/// exhaustive match (`filter.rs:3608-3609`, "Event-context references
/// resolve to players, not objects" -- `matches_target_filter` returns
/// `false` for it unconditionally). `find_legal_targets_for_ability`
/// (`targeting.rs:27`), which BOTH `collect_target_slots`' ExchangeControl
/// announcement-time loop (`ability_utils.rs:2723-2741`) AND Unit C's new
/// resolution-time `validate_pinned_targets` call route through, therefore
/// returns an EMPTY legal set for `TriggeringSource` on every board -- a
/// pre-existing, orthogonal gap (deferral-worthy on its own) that would make
/// `collect_target_slots` drop Perplexing Chimera's trigger as
/// `no_legal_target_slots()` before it ever reaches the stack, independent of
/// Unit C. Driving a real `SpellCast` trigger through announcement would
/// therefore test THAT gap, not Unit C's resolution-time validation seam --
/// exactly the "faithful fixture proves impractical" case the plan names.
///
/// This test instead hand-builds a `ResolvedAbility` with `targets` ALREADY
/// POPULATED (bypassing `collect_target_slots` entirely -- a distinct code
/// path from `validate_targets_in_chain`) and drives ONLY the resolution-time
/// seam Unit C changes, proving the CR 701.12a no-op holds regardless of
/// whether Unit C's `validate_pinned_targets` call keeps or drops the
/// stack-object target: `exchange_control::resolve`'s independent zone check
/// (`obj_a.zone != Battlefield || obj_b.zone != Battlefield`,
/// `exchange_control.rs:99-106`) reaches the same no-op either way (dropped
/// -> `resolve_slot` returns `None` at `:73`; kept -> the zone check fails at
/// `:99`). The reach-guard: an `EffectResolved { kind: ExchangeControl,
/// subject: None, .. }` event fires, proving the fixture reached the
/// resolver rather than fizzling upstream.
///
/// EMPIRICAL FINDING (recorded, not assumed): because `find_legal_targets_
/// for_ability` returns empty for `TriggeringSource` as traced above, Unit
/// C's `validate_pinned_targets` call ALSO finds an empty legal set here and
/// DROPS the stack-object target -- `validated.targets` comes back empty,
/// same as the OLD terminal-`None` arm would have produced (it also drops a
/// non-battlefield object). Both old and new code drop this specific target,
/// for different reasons that happen to agree; the exchange is a no-op under
/// both. This is a NARROWER neutrality reason than the plan's "kept vs
/// dropped, both reach the same no-op" framing assumed for
/// `TriggeringSource` specifically -- the assertion below pins the observed
/// (dropped) shape explicitly rather than asserting a `len() <= 1` shrug, so
/// a future change that makes `TriggeringSource` newly resolvable is caught
/// here rather than silently reclassifying this row.
#[test]
fn t16_perplexing_chimera_stack_object_slot_validation_is_outcome_neutral() {
    let mut state = GameState::new_two_player(42);
    let chimera = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Perplexing Chimera".to_string(),
        Zone::Battlefield,
    );
    let spell_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Their Instant".to_string(),
        Zone::Stack,
    );
    state.stack.push_back(StackEntry {
        id: spell_id,
        source_id: spell_id,
        controller: PlayerId(1),
        kind: StackEntryKind::Spell {
            card_id: CardId(2),
            ability: None,
            casting_variant: CastingVariant::default(),
            actual_mana_spent: 0,
        },
    });

    let ability = ResolvedAbility::new(
        Effect::ExchangeControl {
            target_a: TargetFilter::SelfRef,
            target_b: TargetFilter::TriggeringSource,
        },
        vec![TargetRef::Object(spell_id)],
        chimera,
        PlayerId(0),
    );

    let validated = validate_targets_in_chain(&state, &ability);
    // Empirical finding pinned: `TriggeringSource` has no live enumeration
    // path (see the doc comment above), so Unit C's branch drops it here —
    // same outcome the pre-Unit-C terminal `None` arm would have produced
    // for a non-battlefield object, for a different reason.
    assert!(
        validated.targets.is_empty(),
        "expected the stack-object TriggeringSource target to be dropped by \
         validate_targets_in_chain (empty find_legal_targets_for_ability set), \
         got {:?} -- if this now KEEPS the target, T16's neutrality argument \
         must be re-derived from the kept-target zone-check branch instead",
        validated.targets
    );

    let mut events = Vec::new();
    exchange_control::resolve(&mut state, &validated, &mut events).expect("resolve must not error");

    // Reach-guard: the resolver was actually reached (not fizzled upstream).
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::ExchangeControl,
                subject: None,
                ..
            }
        )),
        "the resolver must emit its bare no-op EffectResolved event, got {events:?}"
    );

    // CR 701.12a: the exchange did nothing.
    assert_eq!(
        state.objects[&chimera].controller,
        PlayerId(0),
        "no-op: Chimera's controller must be unchanged"
    );
    assert_eq!(
        state.objects[&spell_id].controller,
        PlayerId(1),
        "no-op: the spell's controller must be unchanged"
    );
    assert_eq!(
        state.objects[&spell_id].zone,
        Zone::Stack,
        "no-op: the spell must remain on the stack — it never became a \
         battlefield permanent under Chimera's control"
    );
}

// ---------------------------------------------------------------------------
// Mutiny / Breaking of the Fellowship — a prior-object controller slot
// ---------------------------------------------------------------------------

const P2: PlayerId = PlayerId(2);

const MUTINY: &str = "Target creature an opponent controls deals damage equal to its power to another target creature that player controls.";
const BREAKING_OF_THE_FELLOWSHIP: &str = "Target creature an opponent controls deals damage equal to its power to another target creature that player controls. The Ring tempts you.";
const PRIOR_OBJECT_CONTROLLER_CARDS: [(&str, &str); 2] = [
    ("Mutiny", MUTINY),
    ("Breaking of the Fellowship", BREAKING_OF_THE_FELLOWSHIP),
];

fn add_free_sorcery(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, false, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// The object ids an offer contains, sorted.
fn offered_objects(offer: &[TargetRef]) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = offer
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    ids.sort();
    ids
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

/// Cast `name` at `first` then `second` (each preference used once, in the
/// order the prompts take them) and assert the two slot offers.
fn assert_prior_object_slot_offers(
    runner: &mut GameRunner,
    name: &str,
    spell: ObjectId,
    prefs: &[ObjectId],
    slot_one: Vec<ObjectId>,
    slot_two: Vec<ObjectId>,
) {
    cast_free(runner, spell);
    let prefs: Vec<TargetRef> = prefs.iter().copied().map(TargetRef::Object).collect();
    let offers = settle_recording_offers(runner, &prefs, &[]);
    let offers: Vec<Vec<ObjectId>> = offers.iter().map(|offer| offered_objects(offer)).collect();
    assert_eq!(
        offers,
        vec![sorted(slot_one), sorted(slot_two)],
        "{name}: slot 1, then slot 2"
    );
}

/// CR 115.1 + CR 601.2c + CR 109.4: "another target creature that player
/// controls" is a creature controlled by the controller of the first target,
/// so once the first target is chosen the second slot offers only that
/// controller's other creatures. A first-target candidate whose controller has
/// no other creature is not offered, and with no legal pair the spell cannot be
/// cast.
#[test]
fn prior_object_controller_slot_offers_the_first_targets_controllers_creatures() {
    for (name, text) in PRIOR_OBJECT_CONTROLLER_CARDS {
        // Two players: P1 controls A and B; P0 controls D.
        let mut scenario = main_phase_scenario(2);
        let a = scenario.add_creature(P1, "P1 A", 3, 3).id();
        let b = scenario.add_creature(P1, "P1 B", 2, 2).id();
        let d = scenario.add_creature(P0, "P0 D", 2, 2).id();
        let spell = add_free_sorcery(&mut scenario, name, text);
        let mut runner = scenario.build();
        assert_prior_object_slot_offers(&mut runner, name, spell, &[a, b, d], vec![a, b], vec![b]);
        let state = runner.state();
        assert_eq!(
            state.objects[&b].zone,
            Zone::Graveyard,
            "{name}: A deals 3 to B"
        );
        assert_eq!(state.objects[&a].damage_marked, 0, "{name}: A is undamaged");
        assert_eq!(state.objects[&d].damage_marked, 0, "{name}: D is undamaged");

        // Three players: as above, plus P2 controls only E.
        let mut scenario = main_phase_scenario(3);
        let a = scenario.add_creature(P1, "P1 A", 3, 3).id();
        let b = scenario.add_creature(P1, "P1 B", 2, 2).id();
        let e = scenario.add_creature(P2, "P2 E", 2, 2).id();
        let d = scenario.add_creature(P0, "P0 D", 2, 2).id();
        let spell = add_free_sorcery(&mut scenario, name, text);
        let mut runner = scenario.build();
        assert_prior_object_slot_offers(
            &mut runner,
            name,
            spell,
            &[a, b, e, d],
            vec![a, b],
            vec![b],
        );
        assert_eq!(runner.state().objects[&b].zone, Zone::Graveyard, "{name}");
        assert_eq!(
            runner.state().objects[&e].damage_marked,
            0,
            "{name}: P2's E"
        );

        // Three players, the first target is P2's: P2 controls E1 and E2.
        let mut scenario = main_phase_scenario(3);
        let a = scenario.add_creature(P1, "P1 A", 3, 3).id();
        let e1 = scenario.add_creature(P2, "P2 E1", 3, 3).id();
        let e2 = scenario.add_creature(P2, "P2 E2", 2, 2).id();
        let spell = add_free_sorcery(&mut scenario, name, text);
        let mut runner = scenario.build();
        // P1's lone A has no other P1 creature, so only E1 and E2 are offered.
        assert_prior_object_slot_offers(
            &mut runner,
            name,
            spell,
            &[e1, a, e2],
            vec![e1, e2],
            vec![e2],
        );
        assert_eq!(runner.state().objects[&e2].zone, Zone::Graveyard, "{name}");

        // Owner differs from controller: A is owned by P2 and controlled by
        // P1, so the second slot offers P1's B, not P2's E.
        let mut scenario = main_phase_scenario(3);
        let a = scenario
            .add_creature(P2, "P2-owned P1 A", 3, 3)
            .controlled_by(P1)
            .id();
        let b = scenario.add_creature(P1, "P1 B", 2, 2).id();
        let e = scenario.add_creature(P2, "P2 E", 2, 2).id();
        let spell = add_free_sorcery(&mut scenario, name, text);
        let mut runner = scenario.build();
        assert_prior_object_slot_offers(&mut runner, name, spell, &[a, e, b], vec![a, b], vec![b]);
        assert_eq!(runner.state().objects[&b].zone, Zone::Graveyard, "{name}");
        assert_eq!(
            runner.state().objects[&e].damage_marked,
            0,
            "{name}: P2's E"
        );

        // P1 controls only A: no legal pair exists, so the cast is refused.
        let mut scenario = main_phase_scenario(2);
        scenario.add_creature(P1, "P1 A", 3, 3);
        scenario.add_creature(P0, "P0 D", 2, 2);
        let spell = add_free_sorcery(&mut scenario, name, text);
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&spell].card_id;
        assert!(
            runner
                .act(GameAction::CastSpell {
                    object_id: spell,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .is_err(),
            "{name}: no legal pair of targets"
        );
    }
}

/// Cards whose `ParentTargetController` reference is a resolution-time choice
/// ("that player may put …", "that player exiles a card from their hand") keep
/// their single declared object target.
#[test]
fn resolution_time_parent_target_controller_choices_keep_their_target() {
    const DIVINE_GAMBIT: &str = "Exile target artifact, creature, or enchantment an opponent controls. That player may put a permanent card from their hand onto the battlefield.";
    const METAMORPHOSE: &str = "Put target permanent an opponent controls on top of its owner's library. That opponent may put an artifact, creature, enchantment, or land card from their hand onto the battlefield.";
    const ASHIOK: &str = "[+1]: Create a 2/3 blue and black Nightmare creature token with \"Whenever this token attacks or blocks, each opponent exiles the top two cards of their library.\"\n[−3]: Return target nonland permanent to its owner's hand, then that player exiles a card from their hand.\n[−7]: You may cast up to three spells from among face-up cards your opponents own from exile without paying their mana costs.";

    for (name, text, instant, zone) in [
        ("Divine Gambit", DIVINE_GAMBIT, false, Zone::Exile),
        ("Metamorphose", METAMORPHOSE, true, Zone::Library),
    ] {
        let mut scenario = main_phase_scenario(2);
        let x = scenario.add_creature(P1, "P1 X", 2, 2).id();
        let z = scenario.add_creature(P0, "P0 Z", 2, 2).id();
        scenario.add_creature_to_hand(P1, "P1 Hand Bear", 2, 2);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, name, instant, text)
            .id();
        let mut runner = scenario.build();
        let outcome = runner
            .cast(spell)
            .target_object(x)
            .decline_optional()
            .resolve();
        assert_eq!(outcome.zone_of(x), zone, "{name}");
        assert_eq!(outcome.zone_of(z), Zone::Battlefield, "{name}");
    }

    let mut scenario = main_phase_scenario(2);
    let x = scenario.add_creature(P1, "P1 X", 2, 2).id();
    scenario.add_creature_to_hand(P1, "P1 Hand Bear", 2, 2);
    let ashiok = scenario
        .add_planeswalker_from_oracle(P0, "Ashiok, Nightmare Muse", "Ashiok", 5, ASHIOK)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.activate(ashiok, 1).target_object(x).resolve();
    assert_eq!(outcome.zone_of(x), Zone::Hand, "Ashiok: X returns to hand");
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::EffectZoneChoice { player, .. } if *player == P1
        ),
        "Ashiok: X's controller chooses a card to exile; got {:?}",
        outcome.final_waiting_for()
    );
}

/// The board mutation made while the spell waits on the stack.
#[derive(Clone, Copy, Debug)]
enum Mutation {
    None,
    StealB,
    BounceA,
    StealA,
}

/// P0 gains control of `id` until end of turn.
fn gain_control(state: &mut GameState, source: ObjectId, id: ObjectId) {
    state.add_transient_continuous_effect(
        source,
        P0,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id },
        vec![ContinuousModification::ChangeController],
        None,
    );
    flush_layers(state);
}

/// CR 608.2b: as the spell resolves, the second target is rechecked against
/// the first target's controller. If B is no longer controlled by A's
/// controller, B is illegal and is not dealt damage. If A is illegal, it
/// identifies no player, so B is illegal too; with every target illegal the
/// spell does not resolve, and Breaking of the Fellowship's Ring does not
/// tempt (Mutiny ruling 2018-01-19: "If either or both targets are illegal
/// when Mutiny resolves, no creature will deal or be dealt damage.").
#[test]
fn prior_object_controller_slot_is_rechecked_on_resolution() {
    for (name, text) in PRIOR_OBJECT_CONTROLLER_CARDS {
        let breaking = name == "Breaking of the Fellowship";
        for mutation in [
            Mutation::None,
            Mutation::StealB,
            Mutation::BounceA,
            Mutation::StealA,
        ] {
            let label = format!("{name} / {mutation:?}");
            // P0 controls no creature, so the Ring needs no Ring-bearer prompt.
            let mut scenario = main_phase_scenario(2);
            let a = scenario.add_creature(P1, "P1 A", 3, 3).id();
            let b = scenario.add_creature(P1, "P1 B", 2, 2).id();
            let spell = add_free_sorcery(&mut scenario, name, text);
            let mut runner = scenario.build();

            let mut commit = runner.cast(spell).target_objects(&[a, b]).commit();
            let state = commit.state_mut();
            match mutation {
                Mutation::None => {}
                Mutation::StealB => gain_control(state, spell, b),
                Mutation::StealA => gain_control(state, spell, a),
                Mutation::BounceA => move_to_zone(state, a, Zone::Hand, &mut Vec::new()),
            }
            let outcome = commit.resolve();
            let state = outcome.state();
            let ring = state.ring_level.get(&P0).copied().unwrap_or(0);

            assert_eq!(state.objects[&spell].zone, Zone::Graveyard, "{label}: cast");
            assert_eq!(state.objects[&a].damage_marked, 0, "{label}: A");
            match mutation {
                Mutation::None => {
                    assert_eq!(state.objects[&b].zone, Zone::Graveyard, "{label}: B dies");
                    if breaking {
                        assert_eq!(ring, 1, "{label}: the spell resolved");
                    }
                }
                Mutation::StealB => {
                    assert_eq!(state.objects[&b].zone, Zone::Battlefield, "{label}");
                    assert_eq!(state.objects[&b].damage_marked, 0, "{label}: B");
                    if breaking {
                        assert_eq!(ring, 1, "{label}: A is legal, so the spell resolved");
                    }
                }
                Mutation::BounceA | Mutation::StealA => {
                    assert_eq!(state.objects[&b].zone, Zone::Battlefield, "{label}");
                    assert_eq!(state.objects[&b].damage_marked, 0, "{label}: B");
                    if breaking {
                        assert_eq!(ring, 0, "{label}: every target illegal");
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mutiny / Breaking of the Fellowship — "choose new targets" (CR 115.7d)
// ---------------------------------------------------------------------------

const REDIRECT: &str = "You may choose new targets for target spell.";

/// Cast `spell` at `targets`, apply `before_response` to the board, then cast
/// Redirect at the spell and accept its "you may" up to the CR 115.7d prompt.
/// Returns that prompt's per-position pools, after asserting the positions are
/// the first target (the root) and the second (its `DealDamage` sub).
fn redirect_prompt_pools(
    runner: &mut GameRunner,
    spell: ObjectId,
    redirect: ObjectId,
    targets: &[ObjectId],
    before_response: impl FnOnce(&mut GameState),
) -> Vec<Vec<TargetRef>> {
    runner.cast(spell).target_objects(targets).commit();
    before_response(runner.state_mut());
    runner.cast(redirect).target_objects(&[spell]).commit();
    for _ in 0..12 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::RetargetChoice {
                ref slots,
                ref slot_pools,
                ..
            } => {
                assert_eq!(
                    *slots,
                    vec![
                        RetargetSlotAddress {
                            path: vec![],
                            slot: 0
                        },
                        RetargetSlotAddress {
                            path: vec![ChainStep::SubAbility],
                            slot: 0
                        },
                    ],
                    "the first target, then the second"
                );
                return slot_pools.clone();
            }
            ref other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("no retarget prompt opened");
}

fn retarget(runner: &mut GameRunner, picks: [Option<ObjectId>; 2]) -> bool {
    runner
        .act(GameAction::RetargetSpell {
            new_targets: picks.map(|pick| pick.map(TargetRef::Object)).to_vec(),
        })
        .is_ok()
}

/// CR 115.7d + CR 601.2c: "choose new targets" may move "another target
/// creature that player controls" only onto a creature the first target's
/// controller controls. With the second target still legal, a creature of
/// another player is neither offered nor accepted, and another creature of the
/// first target's controller is. Once P0 has gained control of the second
/// target, the second target is illegal, and the offer still follows the
/// first target's controller rather than the second target's new controller.
#[test]
fn choose_new_targets_keeps_the_second_target_with_the_first_targets_controller() {
    for (name, text) in PRIOR_OBJECT_CONTROLLER_CARDS {
        for steal_b in [false, true] {
            let label = format!("{name} / P0 steals B: {steal_b}");
            // P1 controls A, B and B2; P0 controls D. B and B2 survive 3
            // damage, so the damage is read from `damage_marked`.
            let mut scenario = main_phase_scenario(2);
            let a = scenario.add_creature(P1, "P1 A", 3, 3).id();
            let b = scenario.add_creature(P1, "P1 B", 2, 5).id();
            let b2 = scenario.add_creature(P1, "P1 B2", 2, 5).id();
            let d = scenario.add_creature(P0, "P0 D", 2, 5).id();
            let spell = add_free_sorcery(&mut scenario, name, text);
            let redirect = scenario
                .add_spell_to_hand_from_oracle(P0, "Redirect", true, REDIRECT)
                .with_mana_cost(ManaCost::zero())
                .id();
            let mut runner = scenario.build();

            let pools = redirect_prompt_pools(&mut runner, spell, redirect, &[a, b], |state| {
                if steal_b {
                    gain_control(state, spell, b);
                }
            });
            let second = offered_objects(&pools[1]);
            assert!(second.contains(&b2), "{label}: B2 is offered, {second:?}");
            assert!(
                !second.contains(&d),
                "{label}: D is not offered, {second:?}"
            );

            assert!(
                !retarget(&mut runner, [None, Some(d)]),
                "{label}: P0's D is not controlled by A's controller"
            );
            assert!(
                retarget(&mut runner, [None, Some(b2)]),
                "{label}: B2 is controlled by A's controller"
            );
            runner.advance_until_stack_empty();
            let state = runner.state();
            assert_eq!(
                state.objects[&b2].damage_marked, 3,
                "{label}: A deals 3 to B2"
            );
            assert_eq!(state.objects[&b].damage_marked, 0, "{label}: B");
            assert_eq!(state.objects[&d].damage_marked, 0, "{label}: D");
        }
    }
}

/// CR 115.7d + CR 115.7e: only the final set of targets is evaluated. Moving
/// the first target to P2's creature while the second stays on P1's makes the
/// unchanged second target illegal, and moving only the second target to P2's
/// creature is not legal for the unchanged first target; both are refused.
/// Moving both to P2's creatures is accepted.
#[test]
fn choose_new_targets_judges_the_second_target_against_the_final_first_target() {
    for (name, text) in PRIOR_OBJECT_CONTROLLER_CARDS {
        // P1 controls A and B; P2 controls E1 and E2; P0 controls no creature,
        // so the Ring needs no Ring-bearer prompt.
        let mut scenario = main_phase_scenario(3);
        let a = scenario.add_creature(P1, "P1 A", 3, 3).id();
        let b = scenario.add_creature(P1, "P1 B", 2, 5).id();
        let e1 = scenario.add_creature(P2, "P2 E1", 3, 3).id();
        let e2 = scenario.add_creature(P2, "P2 E2", 2, 5).id();
        let spell = add_free_sorcery(&mut scenario, name, text);
        let redirect = scenario
            .add_spell_to_hand_from_oracle(P0, "Redirect", true, REDIRECT)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();

        let pools = redirect_prompt_pools(&mut runner, spell, redirect, &[a, b], |_| {});
        let second = offered_objects(&pools[1]);
        assert!(
            second.contains(&e2),
            "{name}: E2 is offered, since the same choice may move A to P2's E1; {second:?}"
        );

        assert!(
            !retarget(&mut runner, [Some(e1), None]),
            "{name}: B would no longer be controlled by the first target's controller"
        );
        assert!(
            !retarget(&mut runner, [None, Some(e2)]),
            "{name}: E2 is not controlled by A's controller"
        );
        assert!(
            retarget(&mut runner, [Some(e1), Some(e2)]),
            "{name}: E2 is controlled by E1's controller"
        );
        runner.advance_until_stack_empty();
        let state = runner.state();
        assert_eq!(
            state.objects[&e2].damage_marked, 3,
            "{name}: E1 deals 3 to E2"
        );
        assert_eq!(state.objects[&a].damage_marked, 0, "{name}: A");
        assert_eq!(state.objects[&b].damage_marked, 0, "{name}: B");
        assert_eq!(state.objects[&e1].damage_marked, 0, "{name}: E1");
    }
}
