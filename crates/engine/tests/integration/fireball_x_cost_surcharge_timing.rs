//! Fireball — runtime cost must scale with target count on the {X}+distribute
//! casting route.
//!
//! Real Oracle: "This spell costs {1} more to cast for each target beyond the
//! first. / Fireball deals X damage divided evenly, rounded down, among any
//! number of targets."
//!
//! Fireball is the one real card combining an `{X}` cost + a Strive-shaped
//! per-target surcharge (CR 207.2c + CR 601.2f) + a distribute-among-targets
//! effect. Because its distribution defers target selection until AFTER `{X}`
//! is announced (CR 601.2b, gated by
//! `ability_utils::ability_distribution_pool_needs_chosen_x`), it commits its
//! division through `WaitingFor::DistributeAmong`. The pre-fix
//! `GameAction::DistributeAmong` mid-cast handler called
//! `casting_costs::finalize_cast` directly with `pending.cost` — the cost
//! locked in at `ChooseXValue` time (CR 601.2b), BEFORE targets were known — so
//! the per-target surcharge (CR 601.2f, computed by
//! `casting::apply_target_dependent_cost_modifiers`) was never applied: the
//! spell always cost as if it had a single target. The fix routes that handler
//! through `casting_costs::finish_pending_cast_cost_or_pay`, the same
//! post-target-selection cost authority every other casting route uses.
//!
//! FIXTURE NOTE (scope isolation): the surcharge is pinned directly with
//! `with_strive_cost` (CR 207.2c + CR 601.2f) rather than parsed from Oracle
//! text, exactly as this branch's plan directs (the runtime timing fix must not
//! depend on the separate parser fix, PR #5545). The card's effect text is the
//! VERBATIM distribution clause only ("Fireball deals X damage divided evenly,
//! rounded down, among any number of targets"), which is already parsed
//! correctly today and drives the real EvenSplitDamage distribute-among branch.
//! The printed surcharge line ("This spell costs {1} more to cast for each
//! target beyond the first") is deliberately EXCLUDED from the fixture text:
//! today's parser mis-lowers that "for each" clause into a self-spell cost
//! increase that evaluates to +{1} per battlefield permanent (the exact PR
//! #5545 defect), which would swamp this test with an unrelated, out-of-scope
//! cost inflation. Including it verbatim was verified to make the spell cost
//! `{X + battlefield-creature-count}{R}` — a parser bug this branch does not
//! own. Excluding one printed line while pinning its true effect keeps this
//! test focused strictly on the runtime distribute-route timing seam.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones;
use engine::types::ability::{CastingPermission, ManaSpendPermission, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, CastingPermissionIndex, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::CastFrequency;
use engine::types::zones::{EtbTapState, Zone};
use engine::types::{Duration, PlayerId};

const FIREBALL_ORACLE: &str =
    "Fireball deals X damage divided evenly, rounded down, among any number of targets.";

/// {X}{R} — one red plus the variable generic.
fn fireball_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Red],
        generic: 0,
    }
}

/// "{1} more per target beyond the first" — one generic per extra target.
fn strive_one_generic() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 1,
    }
}

fn red_pool(amount: usize) -> Vec<ManaUnit> {
    (0..amount)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

fn blue_pool(amount: usize) -> Vec<ManaUnit> {
    (0..amount)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect()
}

fn pool_total(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0)
}

/// Build a fresh Fireball scenario: three opposing creatures to target, a
/// pinned `{1}`-per-extra-target surcharge, and a red mana pool of `pool` mana.
fn fireball_scenario(pool: usize) -> (GameRunner, ObjectId, CardId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creatures: Vec<ObjectId> = (0..3)
        .map(|i| {
            scenario
                .add_creature(P1, &format!("Grizzly {i}"), 3, 3)
                .id()
        })
        .collect();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Fireball", false, FIREBALL_ORACLE)
        .with_mana_cost(fireball_cost())
        .with_strive_cost(strive_one_generic())
        .id();
    let card_id = CardId(spell.0);

    scenario.with_mana_pool(P0, red_pool(pool));

    (scenario.build(), spell, card_id, creatures)
}

fn cast_fireball(runner: &mut GameRunner, spell: ObjectId, card_id: CardId) {
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Fireball cast announcement should be accepted");
}

/// CR 601.2b: X is announced before targets for a distribute-among spell.
fn drive_choose_x(runner: &mut GameRunner, x: u32) {
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseXValue { .. } => {
            runner
                .act(GameAction::ChooseX { value: x })
                .expect("ChooseX should succeed");
        }
        WaitingFor::TargetSelection { .. } => {
            panic!("X must be announced before targets for Fireball's distribute route");
        }
        other => panic!("expected ChooseXValue immediately after CastSpell, got {other:?}"),
    }
}

/// CR 601.2c: choose the given targets slot-by-slot (mirrors the production
/// client), then finish any remaining optional "any number of targets" slots.
fn drive_targets(runner: &mut GameRunner, targets: &[ObjectId]) {
    for &t in targets {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ),
            "expected TargetSelection while choosing targets, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(t)),
            })
            .expect("ChooseTarget should succeed");
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget { target: None })
            .expect("finishing optional target slots should succeed");
    }
}

/// Reach `WaitingFor::DistributeAmong` for `x` damage over the chosen targets,
/// asserting the buggy seam is actually driven, and return `(total, targets)`.
fn drive_to_distribute(
    runner: &mut GameRunner,
    spell: ObjectId,
    card_id: CardId,
    x: u32,
    targets: &[ObjectId],
) -> (u32, Vec<TargetRef>) {
    cast_fireball(runner, spell, card_id);
    drive_choose_x(runner, x);
    drive_targets(runner, targets);

    match runner.state().waiting_for.clone() {
        WaitingFor::DistributeAmong { total, targets, .. } => (total, targets),
        other => panic!("expected DistributeAmong after target selection, got {other:?}"),
    }
}

/// Split `total` as evenly as the submitted targets allow (each ≥ 1). For the
/// X-divisible-by-count cases used here this is the rounded-even split; the
/// engine's `DistributeAmong` validator only requires sum == total and each ≥ 1.
fn even_distribution(total: u32, targets: &[TargetRef]) -> Vec<(TargetRef, u32)> {
    let n = targets.len() as u32;
    assert!(
        n > 0 && total.is_multiple_of(n),
        "test uses evenly divisible X"
    );
    let share = total / n;
    targets.iter().map(|t| (t.clone(), share)).collect()
}

/// Primary fix: on the {X}+distribute route the final paid cost must include the
/// CR 601.2f per-target surcharge. Same X, same starting pool, different target
/// counts ⇒ the 3-target cast must pay {1}×2 more than the 1-target cast.
#[test]
fn fireball_surcharge_scales_with_target_count_on_distribute_route() {
    const POOL: usize = 20;
    const X: u32 = 3;

    // 1 target: {X}{R} = {3}{R} = 4 mana, no surcharge (0 targets beyond first).
    let (mut runner, spell, card_id, creatures) = fireball_scenario(POOL);
    let (total, targets) = drive_to_distribute(&mut runner, spell, card_id, X, &creatures[..1]);
    assert_eq!(total, X, "damage pool equals announced X");
    assert_eq!(targets.len(), 1, "single-target reach guard");
    runner
        .act(GameAction::DistributeAmong {
            distribution: even_distribution(total, &targets),
        })
        .expect("1-target distribution + payment should succeed");
    let residual_one = pool_total(&runner, P0);
    assert_eq!(
        residual_one,
        POOL - 4,
        "1 target pays only {{X}}{{R}} = {{3}}{{R}} = 4 mana (no surcharge)"
    );

    // 3 targets: {3}{R} + {1}×2 surcharge = 6 mana. Pre-fix this stayed at 4.
    let (mut runner, spell, card_id, creatures) = fireball_scenario(POOL);
    let (total, targets) = drive_to_distribute(&mut runner, spell, card_id, X, &creatures[..3]);
    assert_eq!(total, X, "damage pool equals announced X");
    assert_eq!(
        targets.len(),
        3,
        "three-target reach guard drives the buggy seam"
    );
    runner
        .act(GameAction::DistributeAmong {
            distribution: even_distribution(total, &targets),
        })
        .expect("3-target distribution + payment should succeed");
    let residual_three = pool_total(&runner, P0);
    // Revert-failing assertion: pre-fix (finalize_cast with stale cost) drops
    // the surcharge, leaving residual at POOL - 4 == 16. The fix charges the
    // two extra targets' {1} each, so residual == POOL - 6 == 14.
    assert_eq!(
        residual_three,
        POOL - 6,
        "3 targets must pay {{3}}{{R}} + {{1}}x2 surcharge = 6 mana (CR 601.2f)"
    );
    assert_eq!(
        residual_one - residual_three,
        2,
        "the 2 targets beyond the first each add {{1}} to the paid cost"
    );
}

/// Rollback wrapper: when the recomputed (correctly higher) cost is unpayable,
/// the handler must leave a clean, retryable state — `pending_cast` restored and
/// `waiting_for` unchanged — rather than dropping `pending_cast` while
/// `waiting_for` still reports `DistributeAmong` (CR 601.2h).
#[test]
fn fireball_unpayable_recomputed_cost_rolls_back_cleanly() {
    // Fund EXACTLY the 1-target cost ({3}{R} = 4). Choosing 3 targets raises the
    // real cost to 6 (surcharge), which this pool cannot afford.
    const X: u32 = 3;
    let (mut runner, spell, card_id, creatures) = fireball_scenario(4);
    let (total, targets) = drive_to_distribute(&mut runner, spell, card_id, X, &creatures[..3]);
    assert_eq!(targets.len(), 3, "three-target reach guard");
    assert_eq!(
        pool_total(&runner, P0),
        4,
        "pool funded for the 1-target cost only"
    );

    // Snapshot the exact pre-failure state (reach guard: pending_cast is Some).
    let pre_pending = runner.state().pending_cast.clone();
    let pre_waiting = runner.state().waiting_for.clone();
    assert!(
        pre_pending.is_some(),
        "reach guard: a pending cast must exist before the failing distribution"
    );

    // CR 601.2d: the handler stashes the announced division into
    // `pending.ability.distribution` BEFORE it clones `pending_for_restore`, so a
    // correct clean restore carries exactly that stash (and nothing else changes).
    // Build the expected post-failure pending from the pre-failure snapshot plus
    // that one stash — a full structural check that both (i) proves the restore is
    // not a subtly-wrong `Some(_)`, and (ii) proves the distribution stash is
    // present (a restore that dropped it would fail here just as a bare
    // pre-snapshot compare would).
    let distribution = even_distribution(total, &targets);
    let mut expected_pending = pre_pending.clone();
    expected_pending
        .as_mut()
        .expect("reach guard set above")
        .ability
        .distribution = Some(distribution.clone());

    let result = runner.act(GameAction::DistributeAmong { distribution });

    // (a) the unpayable recomputed cost is rejected.
    assert!(
        result.is_err(),
        "recomputed 6-mana cost must be unpayable from a 4-mana pool (CR 601.2h)"
    );
    // (b) full structural restore of pending_cast — not just is_some(): catches a
    // restore that pushes back a subtly-wrong PendingCast (e.g. a dropped
    // `.ability.distribution` stash, or a cost/target field mutated in place).
    assert_eq!(
        runner.state().pending_cast,
        expected_pending,
        "pending_cast must be restored exactly (pre-failure state + the CR 601.2d distribution stash)"
    );
    // (c) waiting_for is unchanged — a clean, retryable/cancellable DistributeAmong
    // state, not a corrupted stale one.
    assert_eq!(
        runner.state().waiting_for,
        pre_waiting,
        "waiting_for must still be the same DistributeAmong after the failed payment"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DistributeAmong { .. }
        ),
        "state must remain at DistributeAmong for a clean retry/cancel"
    );
}

/// CR 601.2a + CR 609.4b: the exact exile permission elected at announcement
/// survives the ChooseX -> target selection -> distribution pause. The first
/// sibling is deliberately ineligible during announcement and made eligible
/// before resumption; losing the index would make legacy first-match lookup
/// select that sibling and drop the elected permission's AnyColor concession.
#[test]
fn fireball_distribution_resume_preserves_elected_exile_permission() {
    const X: u32 = 2;
    let (mut runner, spell, card_id, creatures) = fireball_scenario(3);
    zones::move_to_zone(runner.state_mut(), spell, Zone::Exile, &mut Vec::new());
    runner.state_mut().players[0].mana_pool.mana = blue_pool(3);
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions = vec![
        CastingPermission::PlayFromExile {
            duration: Duration::Permanent,
            granted_to: P1,
            frequency: CastFrequency::Unlimited,
            source_id: None,
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: None,
            land_enter_tapped: EtbTapState::Unspecified,
        },
        CastingPermission::PlayFromExile {
            duration: Duration::Permanent,
            granted_to: P0,
            frequency: CastFrequency::Unlimited,
            source_id: None,
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: Some(ManaSpendPermission::AnyColor),
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: None,
            land_enter_tapped: EtbTapState::Unspecified,
        },
    ];

    let (total, targets) = drive_to_distribute(&mut runner, spell, card_id, X, &creatures[..1]);
    assert_eq!(total, X);
    assert_eq!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .and_then(|pending| pending.casting_permission_index),
        Some(CastingPermissionIndex(1)),
        "the second permission must remain elected across X and target pauses"
    );

    let CastingPermission::PlayFromExile { granted_to, .. } = &mut runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions[0]
    else {
        panic!("hostile sibling must be a PlayFromExile permission");
    };
    *granted_to = P0;

    runner
        .act(GameAction::DistributeAmong {
            distribution: even_distribution(total, &targets),
        })
        .expect("resume must retain the elected AnyColor permission");
    assert!(runner.state().stack.iter().any(|entry| entry.id == spell));
    assert_eq!(pool_total(&runner, P0), 0, "blue mana must pay {{2}}{{R}}");
}
