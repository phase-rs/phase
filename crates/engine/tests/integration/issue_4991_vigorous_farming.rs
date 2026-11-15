//! Vigorous Farming's long-form entry replacement must reach the real zone
//! pipeline, not merely parse to a replacement definition.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectScope, ReplacementDefinition, TapStateChange,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const VIGOROUS_FARMING: &str = "Lands you control enter the battlefield untapped.";

/// CR 614.1c: a self replacement that makes the entering land tapped. Combined
/// with Vigorous Farming's parsed untap replacement, this creates a material
/// ordering choice whose selected order leaves the Farming effect last.
fn enters_tapped_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .description("This land enters tapped.".to_string())
}

#[test]
fn vigorous_farming_long_form_untaps_a_land_through_the_entry_pipeline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Vigorous Farming", VIGOROUS_FARMING);
    let land = scenario
        .add_land_to_hand(P0, "Tangled Field")
        .with_replacement_definition(enters_tapped_replacement())
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("play land through the real entry pipeline");

    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "the parsed Vigorous Farming replacement and the self-tap replacement \
             must produce an ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    let self_tap_index = candidates
        .iter()
        .position(|candidate| candidate.source_id == land)
        .expect("the self-tap replacement must be offered alongside Vigorous Farming");

    runner
        .act(GameAction::ChooseReplacement {
            index: self_tap_index,
        })
        .expect("choose the self-tap replacement first");

    let entered = &runner.state().objects[&land];
    assert_eq!(entered.zone, Zone::Battlefield, "land must finish entering");
    assert!(
        !entered.tapped,
        "CR 614.1c + CR 616.1: Vigorous Farming's parsed untap replacement must apply last"
    );
}
