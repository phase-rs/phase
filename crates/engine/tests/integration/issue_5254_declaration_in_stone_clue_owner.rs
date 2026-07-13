//! Issue #5254 — Declaration in Stone: "That player investigates for each
//! nontoken creature exiled this way" handed the Clue to the CASTER instead of
//! to "that player" (the controller of the exiled creature).
//!
//! The second sentence's subject ("That player" → the exiled creature's
//! controller, CR 109.4 / CR 608.2h) lowered to a bare `Effect::Investigate`,
//! whose fieldless unit shape gave `inject_subject_target` nowhere to stamp the
//! subject — so it was dropped and the resolver defaulted the Clue to
//! `ability.controller` (the spell's caster). Lifting the dropped subject to the
//! Investigate sub-ability's `player_scope` fans the effect out to the anchored
//! player, so the OPPONENT (whose creature was exiled) gets the Clue.
//!
//! https://github.com/phase-rs/phase/issues/5254

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const DECLARATION_IN_STONE: &str = "Exile target creature and all other creatures its \
controller controls with the same name as that creature. That player investigates for each \
nontoken creature exiled this way.";

/// Battlefield Clue tokens (CR 111.10f) controlled by `player`.
fn clues_controlled(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.controller == player && o.card_types.subtypes.iter().any(|s| s == "Clue"))
        .count()
}

#[test]
fn declaration_in_stone_gives_clue_to_exiled_creatures_controller_not_caster() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The OPPONENT (P1) controls the creature that gets exiled. Its controller —
    // the opponent — is "that player" who investigates.
    let victim = scenario.add_creature(P1, "Grizzly Bear", 2, 2).id();

    let dis = scenario
        .add_spell_to_hand(P0, "Declaration in Stone", false)
        .from_oracle_text(DECLARATION_IN_STONE)
        // Free cast — the scoring/routing under test is orthogonal to the mana cost.
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![],
        })
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(dis).target_objects(&[victim]).resolve();

    // The exile → investigate chain resolves cleanly back to priority.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Declaration in Stone must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );

    // The creature was exiled (it left the battlefield).
    assert!(
        !runner.state().battlefield.contains(&victim),
        "the targeted creature must be exiled"
    );

    // THE FIX: the Clue goes to P1 (the exiled creature's controller — "that
    // player"), NOT P0 (the caster). On `main` this is inverted: P0 gets the Clue.
    assert_eq!(
        clues_controlled(runner.state(), P1),
        1,
        "the exiled creature's controller (the opponent) must receive the Clue"
    );
    assert_eq!(
        clues_controlled(runner.state(), P0),
        0,
        "the caster must NOT receive the Clue"
    );
}

#[test]
fn declaration_in_stone_investigates_once_per_nontoken_creature_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The opponent (P1) controls TWO same-name nontoken creatures. Declaration in
    // Stone exiles the target AND all other same-name creatures its controller
    // controls — so BOTH are exiled — and "investigates for each nontoken creature
    // exiled this way" must create TWO Clues, both to P1. On `main` (and on the
    // recipient-only fix) the count is dropped and only ONE Clue is made.
    let victim = scenario.add_creature(P1, "Grizzly Bear", 2, 2).id();
    let twin = scenario.add_creature(P1, "Grizzly Bear", 2, 2).id();

    let dis = scenario
        .add_spell_to_hand(P0, "Declaration in Stone", false)
        .from_oracle_text(DECLARATION_IN_STONE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![],
        })
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(dis).target_objects(&[victim]).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Declaration in Stone must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );

    // Both same-name creatures were exiled.
    assert!(
        !runner.state().battlefield.contains(&victim)
            && !runner.state().battlefield.contains(&twin),
        "both same-name creatures must be exiled"
    );

    // THE COUNT FIX: two nontoken creatures exiled this way → TWO Clues, both to
    // the opponent (the exiled creatures' controller); zero to the caster.
    assert_eq!(
        clues_controlled(runner.state(), P1),
        2,
        "one Clue per nontoken creature exiled this way (two exiled → two Clues), all to the opponent"
    );
    assert_eq!(
        clues_controlled(runner.state(), P0),
        0,
        "the caster must NOT receive any Clue"
    );
}
