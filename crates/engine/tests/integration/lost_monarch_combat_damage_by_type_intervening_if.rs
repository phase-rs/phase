//! Runtime regression for issue #1347 (Lost Monarch of Ifnir).
//!
//! Lost Monarch of Ifnir: "At the beginning of your second main phase, if a
//! player was dealt combat damage by a Zombie this turn, mill three cards,
//! then you may return a creature card from your graveyard to your hand."
//!
//! Bug (pre-fix): the intervening-if condition "a player was dealt combat
//! damage by a [creature type] this turn" (CR 603.4) did not parse in
//! `parse_inner_condition`, so the trigger carried NO condition and the mill
//! happened UNCONDITIONALLY at every second main phase — even when no Zombie
//! dealt combat damage.
//!
//! Fix: parse the condition into
//! `QuantityRef::DamageDealtThisTurn { source: <Zombie creature filter>,
//! target: Player, damage_kind: CombatOnly } >= 1`, which the existing
//! intervening-if bridge composes onto the trigger and the existing
//! `resolve_damage_dealt_this_turn` resolver evaluates against the per-turn
//! `state.damage_dealt_this_turn` ledger (matching the CR 608.2i look-back
//! source-type snapshot recorded at damage time).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.4: an intervening "if" is checked when the ability would trigger
//!     and again when it resolves; if the condition is false at either point
//!     the ability does nothing.
//!   - CR 510.1c / CR 120.1: combat damage is dealt and recorded this turn.
//!   - CR 608.2i: damage-source look-back uses the source's characteristics as
//!     they were at the time it dealt damage.
//!
//! These tests drive the real pipeline (build → `from_oracle_text` parses the
//! trigger + intervening-if → combat damage records the source-type snapshot →
//! second main-phase trigger evaluates the condition → `Effect::Mill`). The
//! discriminator is *whether any mill happens at all*: the trigger must mill
//! when a Zombie dealt combat damage and must NOT mill otherwise. The exact
//! milled count is deliberately not asserted (it is orthogonal to the
//! intervening-if gate). Reverting the parser fix makes the condition parse to
//! `None`, so the negative case (no Zombie damage) WOULD mill — failing
//! `no_mill_when_no_zombie_combat_damage`.

use super::rules::{run_combat, GameRunner, GameScenario, ObjectId, Phase, PlayerId, P0};

const LOST_MONARCH_LIKE: &str =
    "At the beginning of your second main phase, if a player was dealt combat \
     damage by a Zombie this turn, mill three cards.";

/// Count of cards in `player`'s library — the mill discriminator.
fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .library
        .len()
}

/// Build a Lost-Monarch-like Zombie on the battlefield plus a single attacker
/// of the given subtype, seed P0's library, and return the assembled runner.
fn build_scenario(attacker_subtype: &str) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's trigger source carries the Lost-Monarch-like second-main mill.
    scenario
        .add_creature(P0, "Lost Monarch", 4, 4)
        .as_creature()
        .with_subtypes(vec!["Zombie", "Noble"])
        .from_oracle_text(LOST_MONARCH_LIKE);

    let attacker = scenario
        .add_creature(P0, "Attacker", 2, 2)
        .with_subtypes(vec![attacker_subtype])
        .id();

    // A library deep enough that an unconditional mill would visibly shrink it.
    scenario.with_library_top(
        P0,
        &["L0", "L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9"],
    );

    (scenario.build(), attacker)
}

/// CR 603.4: When a Zombie dealt combat damage this turn, the intervening-if is
/// satisfied and the trigger mills (library shrinks).
#[test]
fn mills_when_zombie_dealt_combat_damage() {
    let (mut runner, zombie_attacker) = build_scenario("Zombie");
    let lib_before = library_len(&runner, P0);

    // Zombie deals combat damage to P1 during the combat phase.
    run_combat(&mut runner, vec![zombie_attacker], vec![]);

    // Advance to the second main phase, where the trigger fires + resolves.
    runner.advance_to_phase(Phase::PostCombatMain);
    runner.advance_until_stack_empty();

    assert!(
        library_len(&runner, P0) < lib_before,
        "CR 603.4: a Zombie dealt combat damage this turn, so the intervening-if \
         is satisfied and P0 mills (library must shrink): before={lib_before}, \
         after={}",
        library_len(&runner, P0)
    );
}

/// CR 603.4: With no Zombie combat damage this turn, the intervening-if is
/// FALSE and the trigger does nothing — no mill. Pre-fix (condition not
/// parsed) this case milled unconditionally and the assert fails.
#[test]
fn no_mill_when_no_zombie_combat_damage() {
    // The attacker is a Human, NOT a Zombie — its combat damage does NOT
    // satisfy "dealt combat damage by a Zombie".
    let (mut runner, human_attacker) = build_scenario("Human");
    let lib_before = library_len(&runner, P0);

    run_combat(&mut runner, vec![human_attacker], vec![]);

    runner.advance_to_phase(Phase::PostCombatMain);
    runner.advance_until_stack_empty();

    assert_eq!(
        library_len(&runner, P0),
        lib_before,
        "CR 603.4: no Zombie dealt combat damage this turn, so the intervening-if \
         is FALSE and P0 must NOT mill (pre-fix this milled unconditionally)"
    );
}
