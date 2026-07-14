use engine::ai_support::legal_actions_for_viewer;
use engine::game::preview::preview_auto_payment_sources;
use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const BROODLORD_ORACLE: &str = "Ravenous (This creature enters with X +1/+1 counters on it. If X is 5 or more, draw a card when it enters.)\nBrood Telepathy — When this creature enters, distribute X +1/+1 counters among any number of other target creatures you control.";

fn cast_action_for(state: &engine::types::game_state::GameState, object_id: u64) -> GameAction {
    legal_actions_for_viewer(state, P0)
        .0
        .into_iter()
        .find(|action| {
            matches!(action, GameAction::CastSpell { object_id: candidate, .. } if candidate.0 == object_id)
        })
        .expect("test setup must offer the spell as an exact legal CastSpell action")
}

#[test]
fn auto_payment_preview_uses_the_real_cast_pipeline_without_mutating_state() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dark Ritual", true, "Add {B}{B}{B}.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let runner = scenario.build();
    let action = cast_action_for(runner.state(), spell.0);

    assert_eq!(
        preview_auto_payment_sources(runner.state(), P0, &action).unwrap(),
        vec![swamp],
        "the preview must report the source emitted by the automatic payment path",
    );
    assert!(
        !runner.state().objects[&swamp].tapped,
        "previewing must not tap the live mana source",
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "previewing must not move the live spell onto the stack",
    );
}

#[test]
fn auto_payment_preview_assumes_zero_for_an_unannounced_x_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forests = (0..4)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect::<Vec<_>>();
    let broodlord = scenario
        .add_spell_to_hand_from_oracle(P0, "Broodlord", false, BROODLORD_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 3,
        })
        .id();
    let runner = scenario.build();
    let action = cast_action_for(runner.state(), broodlord.0);

    // CR 107.3g: X in a card's mana cost is 0 outside the stack.
    assert_eq!(
        preview_auto_payment_sources(runner.state(), P0, &action).unwrap(),
        forests,
        "drag preview must use the automatic {{3}}{{G}} payment for Broodlord at X=0",
    );
    assert!(forests.iter().all(|id| !runner.state().objects[id].tapped));
    assert_eq!(runner.state().objects[&broodlord].zone, Zone::Hand);
}

#[test]
fn manual_or_uncommitted_casts_have_no_automatic_payment_preview() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let runner = scenario.build();
    let action = cast_action_for(runner.state(), spell.0);

    assert!(
        preview_auto_payment_sources(runner.state(), P0, &action)
            .unwrap()
            .is_empty(),
        "a cast that pauses for target selection has no final payment yet",
    );

    let GameAction::CastSpell {
        object_id,
        card_id,
        targets,
        ..
    } = action
    else {
        unreachable!("cast_action_for returns CastSpell");
    };
    let manual = GameAction::CastSpell {
        object_id,
        card_id,
        targets,
        payment_mode: CastPaymentMode::Manual,
    };
    assert!(
        preview_auto_payment_sources(runner.state(), P0, &manual)
            .unwrap()
            .is_empty(),
        "manual payment must never produce an automatic-payment source preview",
    );
}
