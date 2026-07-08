//! Curse of the Nightly Hunt — "Creatures enchanted player controls attack each
//! combat if able."
//!
//! Runtime regression coverage for the cross-permanent `MustAttack` static on
//! the **enchanted-player-controlled** filter axis. Axes:
//!   - **enchanted player filter** — only creatures the enchanted player controls
//!     are forced to attack (CR 303.4b + CR 508.1d),
//!   - **controller's creatures excluded** — the curse controller's own creatures
//!     are NOT forced to attack,
//!   - **lifetime** — the requirement ends when the source leaves (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer → combat-enforcement pipeline and
//! queries `creature_must_attack` — a runtime test, not an AST-shape test.
//!
//! NOTE: `creature_must_attack` returns `false` for creatures whose controller
//! is not the active player (CR 508.1d only applies during your own combat).
//! The tests set P1 as the active player (the enchanted player) so the positive
//! assertion exercises the enchanted-player filter, and the negative assertion
//! verifies P0's creature is excluded (P0 is not the active player in that
//! scenario, so the guard fires first — which is the correct game behavior).
use engine::game::combat::creature_must_attack;
use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

/// Oracle text for Curse of the Nightly Hunt (Innistrad).
const CURSE_OF_THE_NIGHTLY_HUNT: &str =
    "Creatures enchanted player controls attack each combat if able.";

/// Evaluate layers and check whether a creature must attack.
fn must_attack(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    creature_must_attack(runner.state(), id)
}

/// CR 508.1d + CR 303.4b: The enchanted player's creatures must attack during
/// that player's combat.
#[test]
fn curse_of_the_nightly_hunt_forces_enchanted_players_creatures_to_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    // Create the curse as an enchantment on the battlefield under P0's control.
    let curse = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of the Nightly Hunt",
            0,
            0,
            CURSE_OF_THE_NIGHTLY_HUNT,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // Enchanted player's creature — must attack (P1 is active).
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    // Controller's own creature — outside the "enchanted player controls" filter
    // AND not the active player, so creature_must_attack returns false.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Set P1 as the active player (it's their combat step).
    runner.state_mut().active_player = P1;

    // Attach the curse to P1 (the enchanted player).
    attach_to_player(runner.state_mut(), curse, P1);

    // CR 508.1d: the enchanted player's creature must attack.
    assert!(
        must_attack(&mut runner, foe),
        "enchanted player's creature must be forced to attack"
    );

    // CR 303.4b: the controller's own creature is NOT forced.
    assert!(
        !must_attack(&mut runner, ally),
        "curse controller's own creature must NOT be forced to attack"
    );
}

/// CR 611.3: The continuous effect ends when its source leaves the battlefield.
#[test]
fn curse_of_the_nightly_hunt_requirement_ends_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    let curse = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of the Nightly Hunt",
            0,
            0,
            CURSE_OF_THE_NIGHTLY_HUNT,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    // Set P1 as the active player (it's their combat step).
    runner.state_mut().active_player = P1;

    attach_to_player(runner.state_mut(), curse, P1);

    // Baseline: creature must attack while curse is present.
    assert!(
        must_attack(&mut runner, foe),
        "baseline: enchanted player's creature forced to attack while source is present"
    );

    // CR 611.3: remove the source — requirement ends.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != curse);
        state.objects.remove(&curse);
    }

    assert!(
        !must_attack(&mut runner, foe),
        "enchanted player's creature no longer forced once the source is gone"
    );
}

/// CR 303.4b: During the curse controller's own combat (P0 active), the
/// controller's creature is NOT forced to attack — the enchanted-player filter
/// itself rejects it (not just the active-player guard).
#[test]
fn curse_of_the_nightly_hunt_controller_not_forced_during_own_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    // Create the curse under P0's control, attached to P1.
    let curse = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of the Nightly Hunt",
            0,
            0,
            CURSE_OF_THE_NIGHTLY_HUNT,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // P0's creature — the curse controller's own creature.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // P0 is the active player (it's P0's combat step).
    runner.state_mut().active_player = P0;

    // Attach the curse to P1 (the enchanted player).
    attach_to_player(runner.state_mut(), curse, P1);

    // The filter scopes to "enchanted player controls" (P1), so P0's creature
    // passes the active-player guard but is rejected by the enchanted-player
    // filter — it must NOT be forced to attack.
    assert!(
        !must_attack(&mut runner, ally),
        "curse controller's creature must NOT be forced even during controller's own combat"
    );
}
