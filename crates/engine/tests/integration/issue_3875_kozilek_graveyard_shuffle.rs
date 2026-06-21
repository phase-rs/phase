//! Regression for issue #3875: Kozilek, Butcher of Truth must shuffle its
//! owner's graveyard into their library when it dies.
//!
//! https://github.com/phase-rs/phase/issues/3875

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KOZILEK_ORACLE: &str = "When you cast this spell, draw four cards.\n\
    Annihilator 4 (Whenever this creature attacks, defending player sacrifices four permanents of their choice.)\n\
    When Kozilek is put into a graveyard from anywhere, its owner shuffles their graveyard into their library.";

const MURDER_ORACLE: &str = "Destroy target creature.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn kozilek_graveyard_trigger_shuffles_owners_graveyard_into_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let filler_a = scenario
        .add_creature_to_graveyard(P0, "Filler A", 1, 1)
        .id();
    let filler_b = scenario
        .add_creature_to_graveyard(P0, "Filler B", 1, 1)
        .id();
    let kozilek = scenario
        .add_creature_from_oracle(P0, "Kozilek, Butcher of Truth", 12, 12, KOZILEK_ORACLE)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Black));

    let mut runner = scenario.build();
    runner.cast(murder).target_objects(&[kozilek]).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&kozilek].zone,
        Zone::Library,
        "Kozilek itself should be shuffled into its owner's library"
    );
    assert_eq!(
        runner.state().objects[&filler_a].zone,
        Zone::Library,
        "other cards in the graveyard must shuffle into the library"
    );
    assert_eq!(
        runner.state().objects[&filler_b].zone,
        Zone::Library,
        "other cards in the graveyard must shuffle into the library"
    );
    assert!(
        runner.state().players[0].graveyard.is_empty(),
        "graveyard must be empty after Kozilek's trigger"
    );
    assert!(
        runner.state().players[0].library.contains(&filler_a)
            && runner.state().players[0].library.contains(&filler_b)
            && runner.state().players[0].library.contains(&kozilek),
        "shuffled cards must end up in the library"
    );
    assert!(
        runner.state().players[1].graveyard.is_empty(),
        "opponent graveyard must be untouched"
    );
}
