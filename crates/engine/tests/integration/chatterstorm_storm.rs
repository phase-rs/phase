//! Runtime regression for issue #1679: Chatterstorm's Storm trigger must still
//! resolve after CR 603.4 condition rechecks.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const CHATTERSTORM_ORACLE: &str = "Convoke\n\
Create a 1/1 green Squirrel creature token.\n\
Storm (When you cast this spell, copy it for each spell cast before it this turn. You may choose new targets for the copies.)";

const GRIZZLY_BEARS_ORACLE: &str = "";

fn cast_spell(runner: &mut GameRunner, object_id: ObjectId, targets: Vec<ObjectId>) {
    let card_id = runner.state().objects[&object_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
        })
        .expect("cast spell");

    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: targets.into_iter().map(TargetRef::Object).collect(),
            })
            .expect("select targets");
    }
    runner.advance_until_stack_empty();
}

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

fn spells_cast_by(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&player)
        .map(|v| v.len())
        .unwrap_or(0)
}

fn squirrel_token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("Squirrel"))
            })
        })
        .count()
}

/// Issue #1679: Cast 2 spells (instant + creature), then Chatterstorm.
/// Storm should copy Chatterstorm twice, producing 3 Squirrel tokens total.
#[test]
fn chatterstorm_storm_copies_for_each_prior_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.with_mana_pool(
        P0,
        vec![
            mana(ManaType::Red),
            mana(ManaType::Red),
            mana(ManaType::Green),
            mana(ManaType::Green),
            mana(ManaType::Green),
        ],
    );

    let lightning_bolt = scenario.add_bolt_to_hand(P0);
    let grizzly_bears = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, GRIZZLY_BEARS_ORACLE)
        .id();
    let chatterstorm = scenario
        .add_spell_to_hand_from_oracle(P0, "Chatterstorm", false, CHATTERSTORM_ORACLE)
        .id();

    let dummy_creature = scenario.add_creature(P0, "Memnite", 1, 1).id();

    let mut runner = scenario.build();

    runner.state_mut().turn_number = 1;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    cast_spell(&mut runner, lightning_bolt, vec![dummy_creature]);
    assert_eq!(
        spells_cast_by(&runner, P0),
        1,
        "after Lightning Bolt: should have 1 spell cast this turn"
    );

    cast_spell(&mut runner, grizzly_bears, vec![]);
    assert_eq!(
        spells_cast_by(&runner, P0),
        2,
        "after Grizzly Bears: should have 2 spells cast this turn"
    );
    assert_eq!(
        squirrel_token_count(&runner),
        0,
        "precondition: no Squirrel tokens before Chatterstorm"
    );

    cast_spell(&mut runner, chatterstorm, vec![]);
    assert_eq!(
        spells_cast_by(&runner, P0),
        3,
        "after Chatterstorm: should have 3 spells cast this turn"
    );

    assert_eq!(
        squirrel_token_count(&runner),
        3,
        "Chatterstorm must create 3 Squirrel tokens: original plus 2 Storm copies"
    );
}
