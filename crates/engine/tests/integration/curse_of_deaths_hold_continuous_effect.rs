//! Curse of Death's Hold — "Creatures enchanted player controls get -1/-1."
//!
//! Runtime regression coverage for the continuous static P/T anthem building
//! block on the **enchanted-player-controlled** filter axis with a NEGATIVE
//! power AND toughness modification. Axes:
//!   - **enchanted player filter** — only creatures the enchanted player controls
//!     are debuffed (CR 303.4b),
//!   - **negative modification** — both power and toughness are reduced
//!     (CR 613.4c),
//!   - **controller's creatures excluded** — the curse controller's own creatures
//!     are untouched,
//!   - **lifetime** — the debuff ends when the source leaves (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test.
use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

/// Oracle text for Curse of Death's Hold (Innistrad).
const CURSE_OF_DEATHS_HOLD: &str = "Creatures enchanted player controls get -1/-1.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

/// CR 303.4b + CR 613.4c: The enchanted player's creatures get -1/-1.
#[test]
fn curse_of_deaths_hold_debuffs_enchanted_players_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Create the curse as an enchantment on the battlefield under P0's control.
    let curse = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Death's Hold",
            0,
            0,
            CURSE_OF_DEATHS_HOLD,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // Enchanted player's creature — gets -1/-1.
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    // Controller's own creature — outside the "enchanted player controls" filter.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Attach the curse to P1 (the enchanted player).
    attach_to_player(runner.state_mut(), curse, P1);

    // CR 613.4c: the enchanted player's creature gets -1/-1 → 1/1.
    assert_eq!(
        effective_pt(&mut runner, foe),
        (1, 1),
        "enchanted player's creature must get -1/-1: 2/2 → 1/1"
    );

    // CR 303.4b: the controller's own creature is excluded.
    assert_eq!(
        effective_pt(&mut runner, ally),
        (2, 2),
        "curse controller's own creature must NOT be debuffed"
    );
}

/// CR 611.3: The continuous effect ends when its source leaves the battlefield.
#[test]
fn curse_of_deaths_hold_debuff_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Death's Hold",
            0,
            0,
            CURSE_OF_DEATHS_HOLD,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P1);

    assert_eq!(
        effective_pt(&mut runner, foe),
        (1, 1),
        "baseline: enchanted player's creature debuffed to 1/1 while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != curse);
        state.objects.remove(&curse);
    }

    assert_eq!(
        effective_pt(&mut runner, foe),
        (2, 2),
        "enchanted player's creature reverts to base 2/2 once the source is gone"
    );
}
