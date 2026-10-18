//! CR 613.1f + CR 602.2: an Elf card in your graveyard grants Thranduil its
//! activated ability. The legal action, cost, and effect all use Thranduil as
//! the activation source.

use engine::ai_support::legal_actions;
use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, EffectKind, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Complete Oracle text from `data/mtgjson/AtomicCards.json` (2026-09-22).
const THRANDUIL: &str =
    "Thranduil has all activated abilities of all Elf cards in your graveyard.\n\
Whenever another legendary Elf you control enters, draw two cards, then discard a card.";

#[test]
fn thranduil_activates_graveyard_elf_ability_as_its_own() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let thranduil = scenario
        .add_creature(P0, "Thranduil, the Elvenking", 5, 6)
        .with_subtypes(vec!["Elf", "Noble"])
        .from_oracle_text(THRANDUIL)
        .id();

    // Synthetic Elf donor isolates the grant: its typed fixture ability is
    // {T}: You gain 2 life. The donor remains in the graveyard throughout.
    let donor_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Tap);
    let donor = scenario
        .add_creature_to_graveyard(P0, "Test Elf Donor", 1, 1)
        .with_subtypes(vec!["Elf"])
        .with_ability_definition(donor_ability)
        .id();
    let mut runner = scenario.build();

    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    let offered = legal_actions(runner.state());
    let ability_index = offered
        .iter()
        .find_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == thranduil => Some(*ability_index),
            _ => None,
        })
        .expect("Thranduil's granted ability must be offered as its own legal action");

    let outcome = runner.activate(thranduil, ability_index).resolve();
    assert!(outcome.state().objects[&thranduil].tapped);
    assert!(!outcome.state().objects[&donor].tapped);
    assert_eq!(outcome.state().objects[&donor].zone, Zone::Graveyard);
    assert_eq!(outcome.life_delta(P0), 2);
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::GainLife,
            source_id,
            ..
        } if *source_id == thranduil
    )));
}
