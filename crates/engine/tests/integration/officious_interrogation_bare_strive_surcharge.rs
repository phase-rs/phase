//! Officious Interrogation (Murders at Karlov Manor, 2024) carries a BARE,
//! unlabeled per-target cost-increase clause — "This spell costs {W}{U} more
//! to cast for each target beyond the first." — printed nine years after the
//! "Strive" ability word existed, but WotC chose not to apply the label. The
//! parser's `parse_strive_cost_line` previously required an em-dash ability-
//! word label and silently dropped this clause (the CR 601.2f cost increase),
//! so the per-target surcharge was never applied at cast time.
//!
//! This test drives the REAL cast pipeline. Officious targets "any number of
//! target players" (no {X} in its mana cost, no damage-distribution step), so
//! its cast route is: cast → slot-by-slot `ChooseTarget` → target-dependent
//! cost determination (CR 601.2f, `apply_target_dependent_cost_modifiers`) →
//! auto-payment. Each target beyond the first adds a {W}{U} surcharge, so
//! casting at two targets must consume exactly two more mana (one white, one
//! blue) than casting at one target.
//!
//! Reverting the parser fix makes `strive_cost` `None`, the surcharge vanishes,
//! the residual pool stops depending on the target count, and the
//! discriminating assertions below fail.
//!
//! NOTE (see implementation report): Fireball — the OTHER bare-clause card —
//! cannot be used for this runtime proof. Its "divided evenly … among any
//! number of targets" + {X} cost routes through a DistributeAmong pause that
//! occurs BEFORE the CR 601.2f surcharge seam, so its surcharge is dropped at
//! runtime by an INDEPENDENT engine gap unrelated to this parser fix. Officious
//! exercises the same parser fix on a route that actually reaches the surcharge.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const OFFICIOUS_ORACLE: &str = "This spell costs {W}{U} more to cast for each target beyond the first.\nChoose any number of target players. Investigate X times, where X is the total number of creatures those players control.";

/// White + blue units seeded to each side of the pool. Must comfortably cover
/// the base {1} plus, for the two-target case, an extra {W}{U} surcharge.
const COLOR_UNITS: usize = 5;

fn p0_pool_total(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 must exist")
        .mana_pool
        .total()
}

/// Cast Officious Interrogation at the given player targets, drive the full
/// announce→pay pipeline, and return P0's residual mana pool count.
fn residual_pool_after_cast(player_targets: &[engine::types::PlayerId]) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Base cost {1} generic — the surcharge, not the base, is under test.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Officious Interrogation", true, OFFICIOUS_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: Vec::new(),
            generic: 1,
        })
        .id();

    // Abundant white + blue so auto-payment can always satisfy the colored
    // {W}{U} surcharge without starving the {1} generic base.
    let mut pool: Vec<ManaUnit> = Vec::new();
    for _ in 0..COLOR_UNITS {
        pool.push(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
        pool.push(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    }
    let seeded = pool.len();
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Officious Interrogation should be accepted");

    // Reach-guard: target selection is the announced-cast follow-up (no {X} to
    // announce first — Officious's "X" is a resolution-time dynamic count).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "expected TargetSelection after cast announcement, got {:?}",
        runner.state().waiting_for
    );

    for &target in player_targets {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(target)),
            })
            .expect("choosing a player target should succeed");
    }
    // Terminate optional target selection if we didn't exhaust the legal set.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget { target: None })
            .expect("terminating optional target selection should succeed");
    }

    // Reach-guard: payment (CR 601.2f) must have consumed mana — a full pool
    // here would mean we never reached the paid path.
    let residual = p0_pool_total(&runner);
    assert!(
        residual < seeded,
        "payment must have consumed mana; residual={residual} seeded={seeded}, waiting_for={:?}",
        runner.state().waiting_for
    );
    residual
}

/// CR 601.2c + CR 601.2f: Officious Interrogation's bare per-target surcharge
/// must apply at cast time. Casting at two players must consume exactly two
/// more mana (one white + one blue = {W}{U}) than casting at one player.
#[test]
fn officious_bare_strive_surcharge_adds_wu_per_extra_target() {
    let residual_one_target = residual_pool_after_cast(&[P1]);
    let residual_two_targets = residual_pool_after_cast(&[P0, P1]);

    let seeded = COLOR_UNITS * 2;
    // One target: pays only the {1} base.
    assert_eq!(
        residual_one_target,
        seeded - 1,
        "single-target Officious pays only its {{1}} base"
    );
    // Two targets: pays {1} base + one {W}{U} surcharge = 3 mana.
    assert_eq!(
        residual_two_targets,
        seeded - 3,
        "two-target Officious pays {{1}} base plus a {{W}}{{U}} surcharge = 3 mana"
    );
    assert_eq!(
        residual_one_target - residual_two_targets,
        2,
        "the second target must add a {{W}}{{U}} surcharge (CR 601.2f): \
         residual@1={residual_one_target}, residual@2={residual_two_targets}"
    );
}
