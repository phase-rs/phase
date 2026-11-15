//! Issue #6769 — Serra's Emissary's as-enters card-type choice must be offered
//! when it returns from a graveyard.
//!
//! The report described a missing ETB trigger, but Serra's Emissary actually
//! has an as-enters replacement effect (CR 614.1c), so this regression drives
//! the replacement-choice path rather than an ETB trigger.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SERRA_ORACLE: &str = "Flying\nAs this creature enters, choose a card type.\nYou and creatures you control have protection from the chosen card type.";
const REANIMATE_ORACLE: &str =
    "Return target creature card from your graveyard to the battlefield.";

#[test]
fn issue_6769_reanimated_serras_emissary_prompts_for_card_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emissary = scenario
        .add_creature_to_graveyard(P0, "Serra's Emissary", 7, 7)
        .from_oracle_text(SERRA_ORACLE)
        .id();
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(reanimate).target_object(emissary).resolve();

    let WaitingFor::NamedChoice {
        choice_type,
        source: Some(source),
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "reanimated Serra's Emissary must pause for its card-type choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(
        matches!(choice_type, ChoiceType::CardType { .. }),
        "Serra's Emissary must offer a card-type choice, got {choice_type:?}"
    );
    assert_eq!(
        source.prompt.identity.reference.object_id, emissary,
        "the prompt must be bound to the entering Serra's Emissary"
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "Artifact".to_string(),
        })
        .expect("choose Artifact for Serra's Emissary");
    runner.advance_until_stack_empty();

    let emissary = &runner.state().objects[&emissary];
    assert_eq!(emissary.zone, Zone::Battlefield);
    assert_eq!(emissary.chosen_card_type(), Some(CoreType::Artifact));
}
