//! Runtime regression for issue #1679 — Chatterstorm's Storm copies 0 times
//! (only one Squirrel token created despite 2 prior spells this turn).
//!
//! Chatterstorm: "{1}{G}, Sorcery, Convoke. Create a 1/1 green Squirrel creature token.
//! Storm (When you cast this spell, copy it for each spell cast before it this turn.
//! You may choose new targets for the copies.)"
//!
//! Root cause hypothesis: The storm copy count computed by
//! `storm_copy_count_before_cast` resolves to 0 when it should be 2 (for 2 prior spells
//! cast this turn), suggesting `spells_cast_this_turn_by_player` is not correctly
//! tracking prior spells at the moment the storm trigger fires.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const CHATTERSTORM_ORACLE: &str = "Convoke\n\
Create a 1/1 green Squirrel creature token.\n\
Storm (When you cast this spell, copy it for each spell cast before it this turn. You may choose new targets for the copies.)";

const GRIZZLY_BEARS_ORACLE: &str = "";

fn cast_spell(
    runner: &mut engine::game::scenario::GameRunner,
    object_id: engine::types::identifiers::ObjectId,
    targets: Vec<engine::types::identifiers::ObjectId>,
) {
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

/// Issue #1679: Cast 2 spells (instant + creature), then Chatterstorm.
/// Storm should copy Chatterstorm twice, producing 3 Squirrel tokens total.
#[test]
fn chatterstorm_storm_copies_for_each_prior_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Give P0 mana to cast spells
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Red,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ),
            ManaUnit::new(
                ManaType::Red,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ),
            ManaUnit::new(
                ManaType::Green,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ),
            ManaUnit::new(
                ManaType::Green,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ),
            ManaUnit::new(
                ManaType::Green,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ),
        ],
    );

    // Add test spells to hand using scenario's built-in methods
    let lightning_bolt = scenario.add_bolt_to_hand(P0);
    let grizzly_bears = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, GRIZZLY_BEARS_ORACLE)
        .id();
    let chatterstorm = scenario
        .add_spell_to_hand_from_oracle(P0, "Chatterstorm", false, CHATTERSTORM_ORACLE)
        .id();

    // Add a target for Lightning Bolt
    let dummy_creature = scenario.add_creature(P0, "Memnite", 1, 1).id();

    let mut runner = scenario.build();

    runner.state_mut().turn_number = 1;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Cast Lightning Bolt (first spell this turn)
    cast_spell(&mut runner, lightning_bolt, vec![dummy_creature]);

    // Verify 1 spell cast this turn
    assert_eq!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P0)
            .map(|v| v.len())
            .unwrap_or(0),
        1,
        "after Lightning Bolt: should have 1 spell cast this turn"
    );

    // Cast Grizzly Bears (second spell this turn)
    cast_spell(&mut runner, grizzly_bears, vec![]);

    // Verify 2 spells cast this turn
    assert_eq!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P0)
            .map(|v| v.len())
            .unwrap_or(0),
        2,
        "after Grizzly Bears: should have 2 spells cast this turn"
    );

    // Count Squirrel tokens before Chatterstorm
    let token_count_before = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .map(|obj| {
                    obj.is_token
                        && obj
                            .card_types
                            .subtypes
                            .iter()
                            .any(|s| s.eq_ignore_ascii_case("Squirrel"))
                })
                .unwrap_or(false)
        })
        .count();

    assert_eq!(
        token_count_before, 0,
        "precondition: no Squirrel tokens before Chatterstorm"
    );

    // Cast Chatterstorm (third spell this turn, should trigger Storm)
    cast_spell(&mut runner, chatterstorm, vec![]);

    // Verify 3 spells cast this turn total
    assert_eq!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P0)
            .map(|v| v.len())
            .unwrap_or(0),
        3,
        "after Chatterstorm: should have 3 spells cast this turn"
    );

    // Count Squirrel tokens after Chatterstorm resolves
    let squirrel_tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .map(|obj| {
                    obj.is_token
                        && obj
                            .card_types
                            .subtypes
                            .iter()
                            .any(|s| s.eq_ignore_ascii_case("Squirrel"))
                })
                .unwrap_or(false)
        })
        .copied()
        .collect();

    // Expected: 3 Squirrel tokens (1 from original + 2 from Storm copies)
    assert_eq!(
        squirrel_tokens.len(),
        3,
        "Chatterstorm must create 3 Squirrel tokens (1 original + 2 Storm copies). \
         Tokens found: {:?}",
        squirrel_tokens
            .iter()
            .map(|id| {
                let obj = runner.state().objects.get(id).unwrap();
                (obj.name.clone(), obj.power, obj.toughness)
            })
            .collect::<Vec<_>>()
    );
}
