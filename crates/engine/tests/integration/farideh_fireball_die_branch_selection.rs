//! Row 1.B — a die-results-table card must select EXACTLY ONE branch by its
//! rolled value.
//!
//! Farideh's Fireball is the live rules bug this phase fixes. At base its
//! printed table was lost entirely (`RollDie.results` was empty) and both
//! branches survived as UNCONDITIONAL chain siblings, so the spell dealt
//! 2 damage to each player AND 2 damage to each opponent on every resolution,
//! regardless of the roll.
//!
//! The two fixtures below fail at base for DIFFERENT reasons, which is what
//! makes the pair discriminate branch SELECTION rather than merely table
//! PRESENCE:
//!   * low band  — base gives P1 −4 (both branches hit P1); the row requires −2.
//!   * high band — base gives P0 −2 (the "each player" row hit P0); requires 0.
//!
//! A fix that restored the table but not branch selection still fails both.
//!
//! The card is built from VERBATIM Oracle text through the live parser rather
//! than loaded from the committed integration fixture: that fixture is a
//! snapshot of the BASE parse (and does not contain this card at all), so a
//! fixture-backed test could not observe this change.

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

/// Verbatim Oracle text, including the printed em-dash result rows.
const FARIDEHS_FIREBALL: &str = "Farideh's Fireball deals 5 damage to target creature or planeswalker. Roll a d20.\n1\u{2014}9 | Farideh's Fireball deals 2 damage to each player.\n10\u{2014}20 | Farideh's Fireball deals 2 damage to each opponent.";

/// Cast the real spell at a fat creature P1 controls, forcing the die result.
///
/// The RNG is reset AFTER `commit()` and immediately before resolution, so the
/// seed -> face mapping is a property of the documented seed rather than of
/// setup activity that may evolve independently of this card.
fn cast_fireball(seed: u64) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Toughness 9 so the 5 damage is survivable and readable as marked damage;
    // a creature that died would make "took 5" indistinguishable from "took 7".
    let victim = scenario.add_creature(P1, "Test Victim", 9, 9).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Farideh's Fireball", false, FARIDEHS_FIREBALL)
        .id();

    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).target_object(victim).commit();
    let state = committed.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = ChaCha20Rng::seed_from_u64(seed);
    (committed.resolve(), victim)
}

/// Read the emitted d20 face. This is a POSITIVE REACH-GUARD, not a
/// convenience: a fixture that never rolled cannot satisfy any assertion below.
fn rolled_face(outcome: &CastOutcome) -> Option<u32> {
    outcome.events().iter().find_map(|event| match event {
        GameEvent::DieRolled {
            sides: 20,
            result: Some(result),
            ..
        } => Some(*result as u32),
        _ => None,
    })
}

fn marked_damage(outcome: &CastOutcome, object: ObjectId) -> u32 {
    outcome
        .state()
        .objects
        .get(&object)
        .expect("the targeted creature must still exist")
        .damage_marked
}

#[test]
fn farideh_low_band_damages_every_player_including_its_controller() {
    let (outcome, victim) = cast_fireball(6);
    let face = rolled_face(&outcome);
    // Pinned empirically, so a drift in RNG word consumption fails loudly here
    // rather than silently flipping the fixture into the other band.
    assert_eq!(face, Some(1), "seed 6 must reach its pinned d20 face");
    let face = face.expect("reach-guard: the roll must have happened");
    assert!(
        (1..=9).contains(&face),
        "this fixture must land in the printed 1-9 band, got {face}"
    );

    // SECOND REACH-GUARD: the chain resolved past its first clause.
    assert_eq!(
        marked_damage(&outcome, victim),
        5,
        "the targeted creature must have taken the spell's 5 damage"
    );

    // THE DISCRIMINATOR. `1-9 | ... deals 2 damage to each player.`
    // At base P1 took -4, because the `10-20` opponent row also fired.
    outcome.assert_life_delta(P0, -2);
    outcome.assert_life_delta(P1, -2);
}

#[test]
fn farideh_high_band_spares_its_controller() {
    let (outcome, victim) = cast_fireball(15);
    let face = rolled_face(&outcome);
    assert_eq!(face, Some(16), "seed 15 must reach its pinned d20 face");
    let face = face.expect("reach-guard: the roll must have happened");
    assert!(
        (10..=20).contains(&face),
        "this fixture must land in the printed 10-20 band, got {face}"
    );

    assert_eq!(
        marked_damage(&outcome, victim),
        5,
        "the targeted creature must have taken the spell's 5 damage"
    );

    // THE DISCRIMINATOR. `10-20 | ... deals 2 damage to each OPPONENT.`
    // At base P0 took -2, because the `1-9` each-player row also fired.
    outcome.assert_life_delta(P0, 0);
    outcome.assert_life_delta(P1, -2);
}
