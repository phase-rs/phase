use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::Duration;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const CODE_OF_CONSTRAINT: &str = "Target creature gets -4/-0 until end of turn.\n\
Draw a card.\n\
Addendum — If you cast this spell during your main phase, tap that creature and it doesn't untap during its controller's next untap step.";

#[test]
fn code_of_constraint_cast_during_main_phase_taps_and_locks_untap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target_creature = scenario.add_creature(P1, "Storm Crow", 1, 2).id();
    scenario.with_library_top(P0, &["Island", "Island"]);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Code of Constraint", true, CODE_OF_CONSTRAINT)
        .id();

    let mut runner = scenario.build();
    let initial_hand = runner.state().players[P0.0 as usize].hand.len();
    assert!(!runner.state().objects[&target_creature].tapped);
    assert_eq!(runner.state().objects[&target_creature].power, Some(1));

    // Cast during P0's own main phase (Addendum condition met)
    runner.cast(spell).target_object(target_creature).resolve();

    // -4/-0 resolves
    assert_eq!(
        runner.state().objects[&target_creature].power,
        Some(-3),
        "target creature gets -4/-0 until end of turn"
    );

    // Draw a card resolves: cast consumed 1 spell, draw added 1 card
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        initial_hand,
        "draw a card must replace the cast spell in hand"
    );

    // The creature should be tapped immediately by the Addendum effect
    assert!(
        runner.state().objects[&target_creature].tapped,
        "creature must be tapped upon spell resolution when cast during own main phase"
    );

    // Next-untap restriction must be installed in transient continuous effects
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| matches!(
                tce.duration,
                Duration::UntilNextStepOf {
                    step: Phase::Untap,
                    ..
                }
            )),
        "next-untap restriction must be installed in transient continuous effects"
    );

    // Advance past opponent's untap step into their upkeep
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().active_player, P1);

    // The creature must NOT untap during its controller's next untap step
    assert!(
        runner.state().objects[&target_creature].tapped,
        "creature must remain tapped during its controller's next untap step"
    );
}

#[test]
fn code_of_constraint_cast_outside_main_phase_does_not_tap_or_lock() {
    let mut scenario = GameScenario::new();
    // BeginCombat is not a main phase
    scenario.at_phase(Phase::BeginCombat);

    let target_creature = scenario.add_creature(P1, "Storm Crow", 1, 2).id();
    scenario.with_library_top(P0, &["Island", "Island"]);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Code of Constraint", true, CODE_OF_CONSTRAINT)
        .id();

    let mut runner = scenario.build();
    let initial_hand = runner.state().players[P0.0 as usize].hand.len();
    assert!(!runner.state().objects[&target_creature].tapped);
    assert_eq!(runner.state().objects[&target_creature].power, Some(1));

    // Cast outside main phase (Addendum condition NOT met)
    runner.cast(spell).target_object(target_creature).resolve();

    // -4/-0 and draw resolve
    assert_eq!(
        runner.state().objects[&target_creature].power,
        Some(-3),
        "target creature gets -4/-0 even when cast outside main phase"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        initial_hand,
        "draw a card must resolve even when cast outside main phase"
    );

    // Creature should NOT be tapped
    assert!(
        !runner.state().objects[&target_creature].tapped,
        "creature should not be tapped when cast outside main phase"
    );

    // Next-untap restriction must NOT be installed
    assert!(
        !runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| matches!(
                tce.duration,
                Duration::UntilNextStepOf {
                    step: Phase::Untap,
                    ..
                }
            )),
        "next-untap restriction must NOT be installed when cast outside main phase"
    );
}

#[test]
fn code_of_constraint_cast_during_opponent_main_phase_does_not_tap_or_lock() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target_creature = scenario.add_creature(P1, "Storm Crow", 1, 2).id();
    scenario.with_library_top(P0, &["Island", "Island"]);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Code of Constraint", true, CODE_OF_CONSTRAINT)
        .id();

    let mut runner = scenario.build();
    // CR 109.5 + CR 102.1 + CR 117.1a: It is P1's turn (P1 is the active player), but P0 has priority to cast instant.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let initial_hand = runner.state().players[P0.0 as usize].hand.len();
    assert!(!runner.state().objects[&target_creature].tapped);
    assert_eq!(runner.state().objects[&target_creature].power, Some(1));

    // P0 casts during P1's main phase (NOT "your main phase", so Addendum condition NOT met)
    runner.cast(spell).target_object(target_creature).resolve();

    // -4/-0 and draw resolve
    assert_eq!(
        runner.state().objects[&target_creature].power,
        Some(-3),
        "target creature gets -4/-0 when cast during opponent main phase"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        initial_hand,
        "draw a card must resolve when cast during opponent main phase"
    );

    // Creature should NOT be tapped by Addendum
    assert!(
        !runner.state().objects[&target_creature].tapped,
        "creature should not be tapped when cast during opponent's main phase"
    );

    // Next-untap restriction must NOT be installed
    assert!(
        !runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| matches!(
                tce.duration,
                Duration::UntilNextStepOf {
                    step: Phase::Untap,
                    ..
                }
            )),
        "next-untap restriction must NOT be installed when cast during opponent's main phase"
    );
}
