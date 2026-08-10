//! Issue #5941: True-Name Nemesis must not be targetable by objects controlled
//! by the player chosen as it entered the battlefield.
//!
//! The regression casts the parsed card through its as-enters replacement,
//! answers that production choice through `ChooseOption`, then checks the
//! production target-legality predicate with sources controlled by both players.

use engine::game::scenario::{GameScenario, P0};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::ChoiceType;
use engine::types::ability::TargetFilter;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

const P1: PlayerId = PlayerId(1);
const TRUE_NAME_ORACLE: &str = "As True-Name Nemesis enters the battlefield, choose a player.\nTrue-Name Nemesis has protection from the chosen player. (This creature can't be blocked, targeted, dealt damage by, or enchanted by anything controlled by that player.)";

fn add_source(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario.add_creature(player, name, 2, 2).id()
}

#[test]
fn true_name_protection_uses_the_protected_objects_chosen_player() {
    let mut scenario = GameScenario::new_n_player(2, 5941);
    let true_name = scenario
        .add_creature_to_hand_from_oracle(P0, "True-Name Nemesis", 3, 1, TRUE_NAME_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let chosen_player_source = add_source(&mut scenario, P1, "Song of the Dryads");
    let other_player_source = add_source(&mut scenario, P0, "Friendly Spell");
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&true_name].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: true_name,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting True-Name Nemesis must succeed");
    runner.advance_until_stack_empty();

    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "True-Name's as-enters replacement must produce a player choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(choice_type, ChoiceType::Player { .. }));
    assert_eq!(options, vec![P0.0.to_string(), P1.0.to_string()]);
    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choosing the player must succeed");

    assert_eq!(runner.state().objects[&true_name].chosen_player(), Some(P1));

    let targets_from_chosen_player =
        find_legal_targets(runner.state(), &TargetFilter::Any, P1, chosen_player_source);
    assert!(
        !targets_from_chosen_player.contains(&engine::types::ability::TargetRef::Object(true_name)),
        "True-Name must not be targetable by the chosen player's source, got {targets_from_chosen_player:?}"
    );

    let targets_from_other_player =
        find_legal_targets(runner.state(), &TargetFilter::Any, P0, other_player_source);
    assert!(
        targets_from_other_player.contains(&engine::types::ability::TargetRef::Object(true_name)),
        "True-Name must remain targetable by another player's source, got {targets_from_other_player:?}"
    );
}
