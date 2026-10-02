//! Surveillance Phantasm (FRA): "As long as you've scried or surveilled this
//! turn, this creature can attack as though it didn't have defender."
//! Exercises the `StaticCondition::Or` verb-list parse end to end.

use engine::game::combat::{get_valid_attacker_ids, AttackTarget};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;

const PHANTASM_ORACLE: &str = "Defender, flying, vigilance\nAs long as you've scried or surveilled this turn, this creature can attack as though it didn't have defender.\n{3}{U}: Surveil 1. (Look at the top card of your library. You may put it into your graveyard.)";

/// No action taken this turn — Phantasm cannot attack, but a vanilla
/// non-Defender creature in the same state can (reach-guard).
#[test]
fn phantasm_cannot_attack_with_no_scry_or_surveil() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let phantasm = scenario
        .add_creature(P0, "Surveillance Phantasm", 2, 3)
        .with_subtypes(vec!["Illusion"])
        .from_oracle_text_with_keywords(&["Defender", "Flying", "Vigilance"], PHANTASM_ORACLE)
        .id();
    let vanilla = scenario.add_vanilla(P0, 1, 1);
    let mut runner = scenario.build();
    runner.advance_to_combat();

    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        !valid.contains(&phantasm),
        "Phantasm must not be a valid attacker with no scry/surveil this turn"
    );
    assert!(
        valid.contains(&vanilla),
        "reach-guard: a vanilla non-Defender creature must be a valid attacker"
    );
    assert!(
        runner
            .declare_attackers(&[(phantasm, AttackTarget::Player(P1))])
            .is_err(),
        "declaring Phantasm as an attacker must be rejected"
    );
}

/// After `Surveil 1.` resolves this turn, Phantasm becomes a valid
/// attacker.
#[test]
fn phantasm_can_attack_after_surveil() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    let phantasm = scenario
        .add_creature(P0, "Surveillance Phantasm", 2, 3)
        .with_subtypes(vec!["Illusion"])
        .from_oracle_text_with_keywords(&["Defender", "Flying", "Vigilance"], PHANTASM_ORACLE)
        .id();
    let surveil_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Surveil Spell", true, "Surveil 1.")
        .id();
    let mut runner = scenario.build();
    runner.cast(surveil_spell).resolve();

    runner.advance_to_combat();
    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        valid.contains(&phantasm),
        "Phantasm must be a valid attacker after surveilling this turn"
    );
    assert!(runner
        .declare_attackers(&[(phantasm, AttackTarget::Player(P1))])
        .is_ok());
}

/// After `Scry 1.` resolves this turn, Phantasm becomes a valid attacker.
#[test]
fn phantasm_can_attack_after_scry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    let phantasm = scenario
        .add_creature(P0, "Surveillance Phantasm", 2, 3)
        .with_subtypes(vec!["Illusion"])
        .from_oracle_text_with_keywords(&["Defender", "Flying", "Vigilance"], PHANTASM_ORACLE)
        .id();
    let scry_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Scry Spell", true, "Scry 1.")
        .id();
    let mut runner = scenario.build();
    runner.cast(scry_spell).resolve();

    runner.advance_to_combat();
    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        valid.contains(&phantasm),
        "Phantasm must be a valid attacker after scrying this turn"
    );
    assert!(runner
        .declare_attackers(&[(phantasm, AttackTarget::Player(P1))])
        .is_ok());
}

/// (Multi-authority): P1 resolving a scry does not satisfy P0's Phantasm —
/// "you've" binds to the controller of the static ability, not to whichever
/// player last scried. Reach-guard: P1's `(P1, Scry)` is actually recorded,
/// so the negative isn't explained by P1's scry silently not happening.
#[test]
fn phantasm_unaffected_by_an_opponents_scry() {
    use engine::types::events::PlayerActionKind;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P1, "P1 Library Card");
    let phantasm = scenario
        .add_creature(P0, "Surveillance Phantasm", 2, 3)
        .with_subtypes(vec!["Illusion"])
        .from_oracle_text_with_keywords(&["Defender", "Flying", "Vigilance"], PHANTASM_ORACLE)
        .id();
    let scry_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Scry Spell", true, "Scry 1.")
        .id();
    let mut runner = scenario.build();
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };
    runner.cast(scry_spell).resolve();

    assert_eq!(
        runner
            .state()
            .player_actions_this_turn
            .iter()
            .filter(|(player, action)| *player == P1 && *action == PlayerActionKind::Scry)
            .count(),
        1,
        "reach-guard: P1's scry must actually be recorded"
    );

    runner.advance_to_combat();
    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        !valid.contains(&phantasm),
        "an opponent's scry must not satisfy Phantasm's controller-scoped condition"
    );
}
