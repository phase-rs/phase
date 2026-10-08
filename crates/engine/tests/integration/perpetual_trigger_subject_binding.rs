//! PR #8494 regression: a bare `it` in a perpetual-grant body must keep the
//! trigger condition's object referent (CR 608.2k).
//!
//! Effluence Devourer's trigger is grammatically headed by `you`, so the parser
//! context's subject is a player filter even though the trigger's valid card is
//! the sacrificed creature. `resolve_it_pronoun` binds that antecedent to
//! `TriggeringSource`, whose runtime resolver understands
//! `GameEvent::PermanentSacrificed`; `ParentTarget` does not.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::PerpetualModification;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const EFFLUENCE_DEVOURER: &str = "Whenever you sacrifice Effluence Devourer or another creature, it perpetually gains \"{2}, Exile this card from your graveyard: Create an X/X green Ooze creature token, where X is this card's power. Activate only as a sorcery.\"";

fn has_granted_ability(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .perpetual_mods
        .iter()
        .any(|modification| matches!(modification, PerpetualModification::GrantAbility { .. }))
}

#[test]
fn effluence_devourer_grants_the_sacrificed_creature_not_the_trigger_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let devourer = scenario
        .add_creature_from_oracle(P0, "Effluence Devourer", 3, 4, EFFLUENCE_DEVOURER)
        .id();
    let victim = scenario.add_creature(P0, "Sacrifice Witness", 2, 2).id();
    let sacrifice = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Sacrifice", true, "Sacrifice a creature.")
        .id();

    let mut runner = scenario.build();
    runner.cast(sacrifice).effect_zone(&[victim]).resolve();

    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert!(
        has_granted_ability(&runner, victim),
        "the sacrificed creature is the trigger antecedent and receives the perpetual grant"
    );
    assert!(
        !has_granted_ability(&runner, devourer),
        "the trigger source must not receive a grant meant for the sacrificed creature"
    );
}
