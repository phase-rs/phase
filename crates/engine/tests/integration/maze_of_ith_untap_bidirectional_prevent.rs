//! Runtime regression for issue #1094 (Maze of Ith): the bidirectional
//! combat-damage prevention with anaphoric target binding.
//!
//! Oracle text (verified against Scryfall):
//! > {T}: Untap target attacking creature. Prevent all combat damage that would
//! > be dealt to and dealt by that creature this turn.
//!
//! This drives the real activation + combat pipeline end to end:
//! `add_land_from_oracle` → the parser lowers the Prevent sentence into TWO
//! `PreventDamage` nodes (a recipient-scoped "to" node bound to the chosen
//! creature via `ParentTarget`, and a source-only "by" `SequentialSibling`
//! whose `damage_source_filter` binds the same creature) → the ability is
//! activated on an attacking creature → combat damage is dealt → both the "to"
//! and "by" shields fire.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 615 / 615.1a: prevention-effect shields.
//!   - CR 608.2c: an anaphor ("that creature") binds to a target chosen earlier
//!     in the same effect.
//!   - CR 701.26a/b: Untap (the pre-existing `SetTapState` lowering — untapping
//!     the attacker; unmodified by this fix, exercised here for completeness).
//!
//! https://github.com/phase-rs/phase/issues/1094

use super::rules::{GameScenario, Phase, P0, P1};
use engine::game::combat::AttackTarget;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;

const MAZE_OF_ITH: &str = "{T}: Untap target attacking creature. Prevent all combat damage that would be dealt to and dealt by that creature this turn.";

fn damage_marked(runner: &engine::game::scenario::GameRunner, obj: ObjectId) -> u32 {
    runner.state().objects[&obj].damage_marked
}

/// CR 615 + CR 608.2c + CR 701.26a/b (issue #1094): activating Maze of Ith on a
/// blocked attacker untaps it AND prevents combat damage in BOTH directions —
/// the attacker deals none to its blocker ("by") and takes none from it ("to").
/// A second, un-Mazed attacker/blocker pair in the same combat takes normal
/// damage on both sides — the hostile fixture proving the shields are scoped to
/// the Mazed creature's identity, not applied globally.
#[test]
fn maze_of_ith_untaps_and_prevents_both_directions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();

    // The Mazed pair: P0's attacker vs P1's blocker (2/3 each so both survive
    // and `damage_marked` is observable without lethal SBAs firing).
    let mazed_attacker = scenario.add_creature(P0, "Mazed Attacker", 2, 3).id();
    let mazed_blocker = scenario.add_creature(P1, "Mazed Blocker", 2, 3).id();

    // The hostile pair (un-Mazed): normal combat damage on both sides.
    let free_attacker = scenario.add_creature(P0, "Free Attacker", 2, 3).id();
    let free_blocker = scenario.add_creature(P1, "Free Blocker", 2, 3).id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (mazed_attacker, AttackTarget::Player(P1)),
            (free_attacker, AttackTarget::Player(P1)),
        ])
        .expect("declaring both attackers must be accepted");

    // Reach-guard: an attacker without vigilance taps when it attacks
    // (CR 508.1f). This proves the Untap that follows is observable.
    assert!(
        runner.state().objects[&mazed_attacker].tapped,
        "attacker must be tapped after declaring the attack (reach-guard for Untap)"
    );

    // Activate Maze of Ith (its only ability, index 0) targeting the attacker.
    runner
        .activate(maze, 0)
        .target_object(mazed_attacker)
        .resolve();

    // (a) CR 701.26b: the attacker is untapped after Maze resolves.
    assert!(
        !runner.state().objects[&mazed_attacker].tapped,
        "Maze of Ith must untap the target attacking creature"
    );

    // Advance to the declare-blockers step and declare both blocks.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner
        .declare_blockers(&[
            (mazed_blocker, mazed_attacker),
            (free_blocker, free_attacker),
        ])
        .expect("declaring both blocks must be accepted");

    runner.combat_damage();

    // (b) "to" direction: the blocker's 2 damage to the Mazed attacker is
    // prevented — attacker takes 0.
    assert_eq!(
        damage_marked(&runner, mazed_attacker),
        0,
        "'to' shield: the Mazed attacker must take no combat damage"
    );
    // (c) "by" direction: the Mazed attacker's 2 damage to the blocker is
    // prevented — blocker takes 0.
    assert_eq!(
        damage_marked(&runner, mazed_blocker),
        0,
        "'by' shield: the Mazed attacker must deal no combat damage"
    );

    // Hostile fixture: the un-Mazed pair takes normal damage on both sides,
    // proving the shields are scoped to the Mazed creature (SpecificObject),
    // not global.
    assert_eq!(
        damage_marked(&runner, free_attacker),
        2,
        "un-Mazed attacker must take its blocker's 2 damage"
    );
    assert_eq!(
        damage_marked(&runner, free_blocker),
        2,
        "un-Mazed blocker must take its attacker's 2 damage"
    );
}

/// CR 615 + CR 608.2c (issue #1094, Gap B): the defending-player-damage case.
/// Two UNBLOCKED attackers are declared; only `attacker_a` (power 2) is Mazed.
/// The defending player must lose EXACTLY `attacker_b.power` (3), never
/// 2 + 3 = 5 — the "by" shield must prevent the Mazed creature's combat damage
/// to the PLAYER, not merely damage dealt TO the creature. This is the fixture
/// that fails without the `source_scoped_prevent` gate widening (Fix 2): without
/// it, the "by" shield is mis-hosted onto `attacker_a` with `valid_card`
/// auto-filled to `SelfRef`, so it only guards damage TO `attacker_a` and the
/// player still takes damage from BOTH attackers (-5).
#[test]
fn maze_of_ith_prevents_mazed_attacker_damage_to_defending_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let attacker_a = scenario.add_creature(P0, "Mazed Attacker", 2, 2).id();
    let attacker_b = scenario.add_creature(P0, "Free Attacker", 3, 3).id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (attacker_a, AttackTarget::Player(P1)),
            (attacker_b, AttackTarget::Player(P1)),
        ])
        .expect("declaring both attackers must be accepted");

    runner.activate(maze, 0).target_object(attacker_a).resolve();
    assert!(
        !runner.state().objects[&attacker_a].tapped,
        "Maze of Ith must untap the Mazed attacker"
    );

    let outcome = runner.combat_damage();

    // Only the un-Mazed attacker_b (power 3) lands damage on P1. attacker_a's
    // combat damage to the player is prevented by the "by" shield.
    outcome.assert_life_delta(P1, -3);
}
