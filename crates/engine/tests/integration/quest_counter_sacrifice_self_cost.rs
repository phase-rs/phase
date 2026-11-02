//! Quest-cycle costs: "Remove three quest counters from this enchantment and
//! sacrifice it" pays with the ability's own source.
//!
//! Grimoire of the Dead, Hellion Crucible, Goblin Bomb and the ten Enchantment
//! Quest cards (Ior Ruin Expedition, Khalni Heart Expedition, Quest for Ancient
//! Secrets / Pure Flame / the Gemblades / the Gravelord / the Holy Relic, Soul
//! Stair Expedition, Sunspring Expedition, Zektar Shrine Expedition) all print
//! a counter-removal-and-sacrifice cost whose second leg is the pronoun "it".

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SUNSPRING_ORACLE: &str = "Landfall — Whenever a land you control enters, you may put a quest counter on this enchantment.\nRemove three quest counters from this enchantment and sacrifice it: You gain 8 life.";

/// CR 608.2c + CR 701.21a: "it" in a cost names the source permanent, so the
/// only payable cost is three quest counters from Sunspring Expedition plus
/// Sunspring Expedition itself. A bystander permanent must never be a legal
/// payment, even when the driver offers it.
#[test]
fn quest_counter_cost_sacrifices_only_its_own_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let quest = scenario
        .add_enchantment_from_oracle(P0, "Sunspring Expedition", SUNSPRING_ORACLE)
        .id();
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&quest)
        .expect("quest permanent exists")
        .counters
        .insert(CounterType::Generic("quest".to_string()), 3);
    let life_before = runner.state().players[P0.0 as usize].life;

    // The driver declares the bystander as an intended sacrifice. The cost must
    // ignore it and use the source instead.
    let outcome = runner.activate(quest, 0).pay_with(&[bystander]).resolve();

    assert_eq!(
        outcome.zone_of(quest),
        Zone::Graveyard,
        "the cost's \"sacrifice it\" puts the source into its owner's graveyard"
    );
    assert_eq!(
        outcome.zone_of(bystander),
        Zone::Battlefield,
        "no other permanent may pay a self-sacrifice cost"
    );
    assert_eq!(
        outcome.state().players[P0.0 as usize].life,
        life_before + 8,
        "the ability resolves after its cost is paid"
    );
}
