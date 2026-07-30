//! Regression tests for Leeching Sliver's `defending player` resolution.
//!
//! Oracle text (verbatim):
//! > Whenever a Sliver you control attacks, defending player loses 1 life.
//!
//! Leeching Sliver's trigger is a `valid_card` (any Sliver you control) attack
//! trigger whose effect is `LoseLife { target: DefendingPlayer }`. The trigger
//! source (Leeching Sliver) is frequently NOT the attacking Sliver — a *different*
//! Sliver you control attacks and Leeching Sliver's ability triggers.
//!
//! Before the fix, the `TargetFilter::DefendingPlayer` resolution arm in
//! `targeting.rs` looked up the defender using only the ability *source* id
//! (Leeching Sliver), ignoring the triggering event. When the source was not the
//! attacker the source lookup returned `None`, and `LoseLife` fell through to the
//! ability's controller — so the *attacking* player lost life (2-player) or the
//! wrong defender was hit (multiplayer). The fix delegates to the event-aware
//! `combat::resolve_defending_player`, which tries the source as the attacker
//! first (byte-identical to the old lookup) and falls back to the attacker
//! carried by the current triggering event (CR 508.5 + CR 508.5a).
//!
//! These tests drive the real combat → Attacks trigger → `LoseLife` pipeline via
//! `GameRunner`, asserting life totals after the trigger resolves.
//!
//! CR references:
//!   - CR 508.5: an ability of / referring to an attacking creature that refers
//!     to "defending player" means the player *that* creature is attacking.
//!   - CR 508.5a: in multiplayer, the defending player is determined
//!     individually per attacking creature.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const LEECHING_SLIVER: &str =
    "Whenever a Sliver you control attacks, defending player loses 1 life.";

/// Add a Sliver-subtyped creature controlled by `player`, parsing `oracle`
/// (empty string → vanilla). Returns its object id.
fn add_sliver(scenario: &mut GameScenario, player: PlayerId, name: &str, oracle: &str) -> ObjectId {
    let mut builder = scenario.add_creature_from_oracle(player, name, 2, 2, oracle);
    builder.with_subtypes(vec!["Sliver"]);
    builder.id()
}

/// KEY revert-failing test: 2-player, trigger source is NOT the attacker.
///
/// P0 controls Leeching Sliver (stays back) and a plain Sliver. Only the plain
/// Sliver attacks P1. Leeching Sliver's trigger fires watching that attack; its
/// `defending player` must resolve to P1 (the player the *attacking* Sliver is
/// attacking), not to the trigger source's controller.
///
/// On the buggy code the source-only lookup returned `None` and `LoseLife` fell
/// through to the ability controller — P0 lost the life instead of P1. So BOTH
/// assertions below flip when the fix is reverted (P0==19, P1==20), making this a
/// non-vacuous paired guard.
#[test]
fn leeching_sliver_hits_defender_when_a_different_sliver_attacks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _leeching = add_sliver(&mut scenario, P0, "Leeching Sliver", LEECHING_SLIVER);
    let attacker = add_sliver(&mut scenario, P0, "Muscle Sliver", "");

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declaring the plain Sliver as attacker should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        19,
        "defending player P1 must lose 1 life (CR 508.5), not the attacking player"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "the attacking player P0 must NOT lose life — this flips on the buggy source-only lookup"
    );
}

/// Multiplayer per-attacker defender (CR 508.5a): 3-player. P0 controls Leeching
/// Sliver plus Sliver A and Sliver B; A attacks P1, B attacks P2 (Leeching stays
/// back). Each attack fires its own Leeching trigger, and each must resolve the
/// defending player individually from its own triggering event — P1 and P2 each
/// lose exactly 1 life, and the controller P0 loses none.
#[test]
fn leeching_sliver_resolves_defender_per_attacker_in_multiplayer() {
    let p2 = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let _leeching = add_sliver(&mut scenario, P0, "Leeching Sliver", LEECHING_SLIVER);
    let sliver_a = add_sliver(&mut scenario, P0, "Sliver A", "");
    let sliver_b = add_sliver(&mut scenario, P0, "Sliver B", "");

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (sliver_a, AttackTarget::Player(P1)),
            (sliver_b, AttackTarget::Player(p2)),
        ])
        .expect("declaring both Slivers at distinct defenders should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        19,
        "P1 (defender of Sliver A) must lose 1 life"
    );
    assert_eq!(
        runner.life(p2),
        19,
        "P2 (defender of Sliver B) must lose 1 life — individual per attacker (CR 508.5a)"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "the attacking player P0 must lose no life"
    );
}

/// Regression guard for the source-IS-attacker path (byte-identical to the prior
/// source-only lookup). P0's Leeching Sliver attacks P1 itself; the defending
/// player resolves via `defending_player_for_attacker(source)` on the first try,
/// so P1 loses 1 life on both the old and new code.
#[test]
fn leeching_sliver_hits_defender_when_it_attacks_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let leeching = add_sliver(&mut scenario, P0, "Leeching Sliver", LEECHING_SLIVER);

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(leeching, AttackTarget::Player(P1))])
        .expect("declaring Leeching Sliver as attacker should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        19,
        "defending player P1 must lose 1 life when Leeching Sliver itself attacks"
    );
    assert_eq!(runner.life(P0), 20, "the attacking player P0 loses no life");
}
