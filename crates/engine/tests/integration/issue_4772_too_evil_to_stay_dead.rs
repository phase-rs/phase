//! Issue #4772: Too Evil to Stay Dead did nothing when cast WITHOUT paying its
//! Teamwork cost — it spent the mana, let the caster choose a target, and then
//! never returned the creature card to the battlefield.
//!
//! Oracle text (verified against Scryfall, MSH):
//!   "Teamwork 4 (As an additional cost to cast this spell, you may tap any
//!   number of creatures you control with total power 4 or more.)
//!   Choose target creature card in your graveyard with mana value 4 or less.
//!   If this spell was cast using teamwork, instead choose target creature
//!   card in your graveyard. Return the chosen card to the battlefield."
//!
//! Root cause: this lowers to a 3-node chain — a base `TargetOnly` clause (the
//! narrow, mana-value-gated "choose target"), whose `sub_ability` is the
//! teamwork-gated override (`AbilityCondition::AdditionalCostPaidInstead`, the
//! broader "choose target" from `strip_additional_cost_conditional`), whose OWN
//! `sub_ability` is the trailing `Effect::ChangeZone` (`SubAbilityLink::
//! SequentialSibling`, no `else_ability`) that actually returns the card to the
//! battlefield. `resolve_ability_chain` in `crates/engine/src/game/effects/mod.rs`
//! correctly declines to swap the base's effect for the override's when Teamwork
//! was NOT paid, but — before this fix — only its `ConditionInstead` arm had a
//! fallback that walks into `sub.sub_ability` when `sub.else_ability` is `None`.
//! The `AdditionalCostPaidInstead` / `CastVariantPaidInstead` /
//! `TargetHasKeywordInstead` arm had no such fallback: it checked `else_ability`
//! and then unconditionally returned, so the not-swapped branch silently dropped
//! the `ChangeZone` reanimation effect. The fix merges the two arms so all four
//! "instead" condition kinds share the same not-swap tail-runner (mirroring the
//! existing `condition_instead_not_swap_tail_runner_honors_gates` coverage in
//! `crates/engine/src/game/effects/mod.rs`).
//!
//! This test pins BOTH branches:
//!   - WITHOUT teamwork: the mana-value-gated target is returned to the
//!     battlefield (was previously a complete no-op — the bug).
//!   - WITH teamwork: the broader (no mana-value restriction) target is
//!     returned to the battlefield (already worked before this fix, kept here
//!     as a differential control so a revert of either branch fails a test).

use engine::game::casting::{
    can_cast_object_now, legal_target_slots_for_castable_spell, spell_has_legal_targets,
};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

const TOO_EVIL_TO_STAY_DEAD: &str = "Teamwork 4 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 4 or more.)\nChoose target creature card in your graveyard with mana value 4 or less. If this spell was cast using teamwork, instead choose target creature card in your graveyard. Return the chosen card to the battlefield.";

/// Build a scenario with Too Evil to Stay Dead in P0's hand (cost {0} so the
/// test doesn't need to model mana payment), a mana-value-3 creature card in
/// P0's graveyard (a legal target under BOTH the narrow and broad filters),
/// and (if `tapper_power` is `Some`) an eligible teamwork tap creature.
fn setup(tapper_power: Option<i32>) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut gy_creature = scenario.add_creature_to_graveyard(P0, "Fallen Champion", 3, 3);
    gy_creature.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 3,
    });
    let gy_creature_id = gy_creature.id();

    let tapper_id =
        tapper_power.map(|power| scenario.add_creature(P0, "Tapper", power, power).id());

    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Too Evil to Stay Dead",
        false,
        TOO_EVIL_TO_STAY_DEAD,
    );
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();

    let runner = scenario.build();
    (runner, spell, gy_creature_id, tapper_id)
}

/// Cast `spell`, decline the optional Teamwork cost at the first opportunity,
/// and choose `target` at the first target-selection window.
fn drive_cast_declining_teamwork(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: false })
                    .expect("declining teamwork must be accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choosing the target must be accepted");
            }
            WaitingFor::Priority { .. } => return,
            _ => return,
        }
    }
}

/// Cast `spell`, ACCEPT the optional Teamwork cost, tap `tapper` to pay the
/// aggregate power requirement, and choose `target` at the first
/// target-selection window.
fn drive_cast_paying_teamwork(runner: &mut GameRunner, tapper: ObjectId, target: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("paying teamwork must be accepted");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![tapper],
                    })
                    .expect("tapping the teamwork creature must be accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choosing the target must be accepted");
            }
            WaitingFor::Priority { .. } => return,
            _ => return,
        }
    }
}

/// Resolve the stack to empty by passing priority.
fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// Regression for issue #4772: WITHOUT paying Teamwork, the targeted graveyard
/// creature must still be returned to the battlefield. Before the fix, the
/// `ChangeZone` step was silently dropped and the creature stayed in the
/// graveyard — the spell "spent the mana with no effect".
#[test]
fn too_evil_to_stay_dead_without_teamwork_still_reanimates() {
    let (mut runner, spell, gy_creature, _tapper) = setup(None);
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Too Evil to Stay Dead must be accepted");

    drive_cast_declining_teamwork(&mut runner, gy_creature);
    resolve_stack(&mut runner);

    assert!(
        runner.state().battlefield.contains(&gy_creature),
        "WITHOUT teamwork, the targeted graveyard creature must be returned to \
         the battlefield — this is the exact issue #4772 symptom (spell resolved, \
         mana was spent, but nothing happened)"
    );
    assert!(
        !runner.state().players[0].graveyard.contains(&gy_creature),
        "the reanimated creature must have left the graveyard"
    );
}

/// Differential control: WITH Teamwork paid, the broader (no mana-value
/// restriction) target is chosen and still returned to the battlefield. This
/// branch already worked before the fix (the Cow-swap correctly adopts the
/// override's own `sub_ability` as its continuation); it is pinned here
/// alongside the without-teamwork case so a revert of either the swap path or
/// the new not-swap tail-runner is caught.
#[test]
fn too_evil_to_stay_dead_with_teamwork_still_reanimates() {
    let (mut runner, spell, gy_creature, tapper) = setup(Some(4));
    let tapper = tapper.expect("tapper requested");
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Too Evil to Stay Dead must be accepted");

    drive_cast_paying_teamwork(&mut runner, tapper, gy_creature);
    resolve_stack(&mut runner);

    assert!(
        runner.state().objects[&tapper].tapped,
        "the teamwork tap creature must be tapped"
    );
    assert!(
        runner.state().battlefield.contains(&gy_creature),
        "WITH teamwork, the targeted graveyard creature must be returned to the battlefield"
    );
}

// ---------------------------------------------------------------------------
// dq-f: the NARROW (no-teamwork) branch's "mana value 4 or less" clause TRAILS
// its zone clause ("target creature card in your graveyard with mana value 4 or
// less"). Before the zone-then-mana-value second pass in
// `parse_type_phrase_with_ctx`, that clause was dropped, so the narrow branch
// behaved identically to the broad (teamwork-paid) branch — any creature card in
// the graveyard was a legal target regardless of mana value. This test pins that
// dq-f now correctly restricts the no-teamwork target slot.
//
// The teamwork-paid BROAD branch (previously a documented stop-and-return: the
// engine did not apply the "...instead choose target creature card in your
// graveyard" conditional target-filter swap at CAST-time target legality) is now
// covered by `too_evil_paying_teamwork_broad_filter_includes_high_mv` below, which
// pins the finding-#1 generalization of `additional_cost_paid` cast-time
// propagation from kicker to every `AdditionalCost`-"instead" with a non-empty
// effective queue (Teamwork here).
// ---------------------------------------------------------------------------

/// Build a scenario with Too Evil to Stay Dead in P0's hand (cost {0}) and two
/// mana-value-3 creature cards plus one mana-value-5 creature card in P0's
/// graveyard. The two MV3 cards keep the narrow-branch target slot interactive (no
/// sole-legal-target auto-pick); the MV5 card is the discriminator. If
/// `tapper_power` is `Some`, also adds an eligible teamwork tap creature of that
/// power so the caller can pay Teamwork and reach the broad branch.
fn setup_mv_discriminating(
    tapper_power: Option<i32>,
) -> (
    GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
    Option<ObjectId>,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut mv3a = scenario.add_creature_to_graveyard(P0, "MV Three A", 3, 3);
    mv3a.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 3,
    });
    let mv3a = mv3a.id();
    let mut mv3b = scenario.add_creature_to_graveyard(P0, "MV Three B", 3, 3);
    mv3b.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 3,
    });
    let mv3b = mv3b.id();
    let mut mv5 = scenario.add_creature_to_graveyard(P0, "MV Five", 5, 5);
    mv5.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 5,
    });
    let mv5 = mv5.id();

    let tapper_id =
        tapper_power.map(|power| scenario.add_creature(P0, "Tapper", power, power).id());

    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Too Evil to Stay Dead",
        false,
        TOO_EVIL_TO_STAY_DEAD,
    );
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();

    let runner = scenario.build();
    (runner, spell, mv3a, mv3b, mv5, tapper_id)
}

/// dq-f (T4-A): WITHOUT teamwork, the narrow "mana value 4 or less" filter must
/// exclude the mana-value-5 card. Reverting the zone-then-mana-value second pass
/// drops the filter, making the narrow branch behave like the broad branch — the
/// mana-value-5 card would then be legal and the discriminator assertion flips
/// (confirmed by revert-probe: this test passes with the fix, fails without it).
#[test]
fn too_evil_no_teamwork_narrow_filter_excludes_high_mv() {
    let (mut runner, spell, mv3a, mv3b, mv5, _tapper) = setup_mv_discriminating(None);
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Too Evil to Stay Dead must be accepted");

    // Decline teamwork, then halt at target selection to inspect legality. The
    // teamwork decision is guaranteed to precede targeting because Too Evil's
    // additional cost has a non-empty effective queue, so `continue_with_prepared`
    // routes through `begin_target_dependent_additional_cost_declaration`
    // (`deferred_target_selection = true`) before target slots are ever built —
    // this is what lets the decision select which branch's filter is used.
    let mut legal = None;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: false })
                    .expect("declining teamwork must be accepted");
            }
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                legal = Some(target_slots[selection.current_slot].legal_targets.clone());
                break;
            }
            _ => break,
        }
    }
    let legal = legal.expect("cast must halt at target selection after declining teamwork");

    // Non-vacuous positive reach-guard: both MV3 cards satisfy the narrow filter.
    assert!(
        legal.contains(&TargetRef::Object(mv3a)),
        "MV3 card A (<= 4) must be legal in the narrow branch, got {legal:?}"
    );
    assert!(
        legal.contains(&TargetRef::Object(mv3b)),
        "MV3 card B (<= 4) must be legal in the narrow branch, got {legal:?}"
    );
    // Discriminator.
    assert!(
        !legal.contains(&TargetRef::Object(mv5)),
        "MV5 card (> 4) must NOT be legal in the narrow (no-teamwork) branch, got {legal:?}"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(mv3a)),
        })
        .expect("choosing the MV3 card must be accepted");
    resolve_stack(&mut runner);

    assert!(
        runner.state().battlefield.contains(&mv3a),
        "the chosen MV3 card must be reanimated to the battlefield"
    );
}

/// Finding #1 (generalized kicker->all AdditionalCost-"instead" cast-time
/// propagation): WITH teamwork paid, the BROAD "instead" filter (no mana-value
/// restriction) must be in effect AT the first target-selection window — the
/// mana-value-5 card must be a legal target, not just reachable after resolving
/// with an MV<=4 pick. Before finding #1's engine generalization, only
/// Kicker-"instead" spells propagated `additional_cost_paid = true` to
/// cast-time target-slot construction, so this cast would incorrectly halt at
/// the NARROW filter (MV5 excluded) even though teamwork was paid.
///
/// REVERT-PROBE (measured): with the Edit-3 else-if commented out, this test's
/// `legal.contains(&TargetRef::Object(mv5))` assertion FAILS — the cast halts
/// at the narrow `{mv3a, mv3b}` set because `begin_target_dependent_
/// additional_cost_declaration` is skipped and Teamwork is offered only via the
/// ordinary post-target-slot `OptionalCostChoice` path, which does not flip
/// `additional_cost_paid` before `build_target_slots` runs.
#[test]
fn too_evil_paying_teamwork_broad_filter_includes_high_mv() {
    let (mut runner, spell, mv3a, _mv3b, mv5, tapper) = setup_mv_discriminating(Some(4));
    let tapper = tapper.expect("tapper requested");
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Too Evil to Stay Dead must be accepted");

    // Pay teamwork (tapping the power-4 creature, total power 4 >= Teamwork 4),
    // then halt at the FIRST target selection to inspect legality.
    let mut legal = None;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("paying teamwork must be accepted");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![tapper],
                    })
                    .expect("tapping the teamwork creature must be accepted");
            }
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                legal = Some(target_slots[selection.current_slot].legal_targets.clone());
                break;
            }
            _ => break,
        }
    }
    let legal = legal.expect("cast must halt at target selection after paying teamwork");

    // Non-vacuous positive reach-guard: the MV3 card stays legal in the broad
    // branch too (the broad filter is a superset of the narrow one).
    assert!(
        legal.contains(&TargetRef::Object(mv3a)),
        "MV3 card must remain legal in the broad (teamwork-paid) branch, got {legal:?}"
    );
    // Discriminator: the broad "instead" branch has no mana-value restriction.
    assert!(
        legal.contains(&TargetRef::Object(mv5)),
        "MV5 card (> 4) must be legal in the broad (teamwork-paid) branch, got {legal:?}"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(mv5)),
        })
        .expect("choosing the MV5 card must be accepted");
    resolve_stack(&mut runner);

    assert!(
        runner.state().battlefield.contains(&mv5),
        "the chosen MV5 card must be reanimated to the battlefield"
    );
}

// ---------------------------------------------------------------------------
// PR #6143 finding #1 review follow-up ([MED]): two changed seams had their
// KICKER path tested elsewhere (issue_3989_bloodchiefs_thirst.rs) but their
// TEAMWORK / queue-cost path unmapped. The two tests below close that gap.
// ---------------------------------------------------------------------------

/// Build a scenario with Too Evil to Stay Dead in P0's hand and a graveyard
/// containing ONLY a mana-value-5 creature card — the narrow "mana value 4
/// or less" branch has NO legal target, so `base_ok` is false and
/// castability depends entirely on the broad teamwork-"instead" branch. No
/// teamwork tap creature is added: `additional_cost_instead_spell_has_legal_
/// targets` reads keyword presence (an available, not-yet-paid cost), not
/// actual tap-eligibility.
fn setup_only_high_mv() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut mv5 = scenario.add_creature_to_graveyard(P0, "MV Five Only", 5, 5);
    mv5.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 5,
    });
    let mv5 = mv5.id();

    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Too Evil to Stay Dead",
        false,
        TOO_EVIL_TO_STAY_DEAD,
    );
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();

    let runner = scenario.build();
    (runner, spell, mv5)
}

/// Seam 1 (Gap A castability precheck): `additional_cost_instead_spell_has_
/// legal_targets` gates on Kicker OR a non-empty effective additional-cost
/// queue. The Kicker arm is covered by
/// `bloodchiefs_thirst_castable_on_opponent_pyrogoyf_when_kicked`
/// (issue_3989_bloodchiefs_thirst.rs); this pins the TEAMWORK / queue-cost
/// arm reached via `spell_has_legal_targets` (used by AI legal-action
/// generation: `ai_support::candidates.rs` + `free_cast_from_zones.rs`).
///
/// With a graveyard containing ONLY a mana-value-5 card, the narrow branch
/// has no legal target (`base_ok == false`), so `spell_has_legal_targets`
/// returning `true` proves the broad teamwork branch was consulted.
///
/// REVERT-PROBE (measured): narrowing the Gap A guard back to kicker-only
/// (`if !has_kicker_cost { return false; }` instead of
/// `if !has_kicker_cost && !has_queue_cost { return false; }`) makes this
/// assertion FAIL — `spell_has_legal_targets` returns `false` (was `true`)
/// because Too Evil to Stay Dead carries no `AdditionalCost::Kicker`.
#[test]
fn too_evil_castability_precheck_admits_teamwork_when_narrow_filter_is_unsatisfiable() {
    let (runner, spell, _mv5) = setup_only_high_mv();
    let obj = &runner.state().objects[&spell];

    assert!(
        spell_has_legal_targets(runner.state(), obj, P0),
        "Too Evil to Stay Dead must be castable via the broad teamwork-'instead' \
         branch when the narrow mana-value<=4 branch has no legal target"
    );
}

/// Seam 2 (preview gate): `legal_target_slots_for_castable_spell` must
/// DEFER (return an empty slot list) for Too Evil to Stay Dead exactly as
/// the live cast path defers (CR 601.2c — the teamwork declaration happens
/// before targets, so a preview computed before that declaration cannot yet
/// know which filter applies). Only the Casualty arm of this deferral was
/// previously covered (`legal_target_slots_for_castable_spell_empty_before_
/// casualty_choice` in `casting_tests.rs`); the TEAMWORK / queue-cost arm
/// (Edit 4's `else if`) was unmapped.
///
/// Reuses the MIXED graveyard from `setup_mv_discriminating` (two
/// mana-value<=4 cards + one mana-value-5 card, no tapper) so the
/// narrow-vs-deferred distinction is observable: see the revert-probe below.
///
/// REVERT-PROBE (measured): removing Edit 4's `else if` arm makes the
/// preview fall through to `build_target_slots` on the base (narrow)
/// ability and return a NON-EMPTY slot containing only the two
/// mana-value<=4 cards, instead of an empty (deferred) list —
/// `slots.is_empty()` FAILS.
#[test]
fn too_evil_preview_defers_target_slots_when_teamwork_available() {
    let (runner, spell, _mv3a, _mv3b, _mv5, tapper) = setup_mv_discriminating(None);
    assert!(
        tapper.is_none(),
        "this preview test never pays teamwork, so no tapper is needed"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, spell),
        "the spell must be castable for the preview to be meaningful"
    );

    let slots = legal_target_slots_for_castable_spell(runner.state(), spell);

    assert!(
        slots.is_empty(),
        "preview must defer until the teamwork additional-cost declaration, got {slots:?}"
    );
}
