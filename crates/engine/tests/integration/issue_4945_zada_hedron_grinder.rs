//! Issue #4945: Zada, Hedron Grinder — when you cast an instant or sorcery that
//! targets only Zada, copy that spell for each other creature you control that
//! the spell could target, with each copy targeting a different creature.

use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const ZADA: &str = "Whenever you cast an instant or sorcery spell that targets only Zada, copy that spell for each other creature you control that the spell could target. Each copy targets a different one of those creatures.";

const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(
        color,
        engine::types::identifiers::ObjectId(0),
        false,
        vec![],
    )
}

#[test]
fn zada_copies_giant_growth_onto_each_other_legal_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            mana(ManaType::Green),
            mana(ManaType::Green),
            mana(ManaType::Green),
        ],
    );

    let zada = scenario
        .add_creature_from_oracle(P0, "Zada, Hedron Grinder", 3, 3, ZADA)
        .id();
    let elf_a = scenario.add_creature(P0, "Elf A", 1, 1).id();
    let elf_b = scenario.add_creature(P0, "Elf B", 1, 1).id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 1;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;

    let outcome = runner.cast(growth).target_object(zada).resolve();
    let state = outcome.state();

    // Original targets Zada (+3/+3); copies target Elf A and Elf B.
    let zada_pt = state.objects.get(&zada).unwrap();
    let elf_a_pt = state.objects.get(&elf_a).unwrap();
    let elf_b_pt = state.objects.get(&elf_b).unwrap();

    assert_eq!(
        zada_pt.power,
        Some(6),
        "Zada should be pumped by original spell"
    );
    assert_eq!(
        elf_a_pt.power,
        Some(4),
        "Elf A should be pumped by a Zada copy"
    );
    assert_eq!(
        elf_b_pt.power,
        Some(4),
        "Elf B should be pumped by a Zada copy"
    );
    assert!(
        state.stack.is_empty(),
        "stack should be empty after full resolution; waiting_for={:?}",
        outcome.final_waiting_for()
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "game should return to priority after resolution"
    );
}
