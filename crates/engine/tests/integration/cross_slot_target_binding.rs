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

use engine::game::ability_utils::validate_targets_in_chain;
use engine::game::effects::exchange_control;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, GameState, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

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
