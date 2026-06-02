//! Wire-payload bounds for in-game `GameAction` bodies (see
//! `server_core::game_action_payload_guard`).

use engine::types::actions::DebugAction;
use engine::types::game_state::{ManaChoice, ShardChoice};
use engine::types::mana::ManaType;
use engine::types::{GameAction, ObjectId};
use server_core::game_action_payload_guard::{
    guard_game_action_payload, MAX_ACTION_LIST_LEN, MAX_CHOICE_LEN,
};

#[test]
fn rejects_oversized_action_list() {
    let action = GameAction::ReorderHand {
        order: vec![ObjectId(1); MAX_ACTION_LIST_LEN + 1],
    };
    assert!(
        guard_game_action_payload(&action).is_err(),
        "a list exceeding MAX_ACTION_LIST_LEN must be rejected"
    );
}

#[test]
fn accepts_reasonably_sized_action_list() {
    let action = GameAction::ReorderHand {
        order: vec![ObjectId(1); 20],
    };
    assert!(
        guard_game_action_payload(&action).is_ok(),
        "a realistic action list must be accepted"
    );
}

#[test]
fn passes_scalar_only_action() {
    // Variants with no client-supplied list/string fall through unguarded.
    assert!(guard_game_action_payload(&GameAction::PassPriority).is_ok());
}

#[test]
fn rejects_oversized_category_choice_payload() {
    let action = GameAction::SelectCategoryPermanents {
        choices: vec![None; MAX_ACTION_LIST_LEN + 1],
    };
    assert!(guard_game_action_payload(&action).is_err());
}

#[test]
fn rejects_oversized_phyrexian_choice_payload() {
    let action = GameAction::SubmitPhyrexianChoices {
        choices: vec![ShardChoice::PayLife; MAX_ACTION_LIST_LEN + 1],
    };
    assert!(guard_game_action_payload(&action).is_err());
}

#[test]
fn rejects_oversized_mana_choice_payloads() {
    let combination = GameAction::ChooseManaColor {
        choice: ManaChoice::Combination(vec![ManaType::Red; MAX_ACTION_LIST_LEN + 1]),
        count: 1,
    };
    assert!(guard_game_action_payload(&combination).is_err());

    let batch_count = GameAction::ChooseManaColor {
        choice: ManaChoice::SingleColor(ManaType::Green),
        count: (MAX_ACTION_LIST_LEN + 1) as u32,
    };
    assert!(guard_game_action_payload(&batch_count).is_err());

    let hybrid_payment = GameAction::PayManaAbilityMana {
        payment: vec![ManaType::White; MAX_ACTION_LIST_LEN + 1],
    };
    assert!(guard_game_action_payload(&hybrid_payment).is_err());
}

#[test]
fn rejects_oversized_choice_string() {
    let action = GameAction::ChooseOption {
        choice: "x".repeat(MAX_CHOICE_LEN + 1),
    };
    assert!(guard_game_action_payload(&action).is_err());
}

#[test]
fn rejects_oversized_debug_payload() {
    let action = GameAction::Debug(DebugAction::AddMana {
        player_id: engine::types::player::PlayerId(0),
        mana: vec![ManaType::Blue; MAX_ACTION_LIST_LEN + 1],
    });
    assert!(guard_game_action_payload(&action).is_err());
}
