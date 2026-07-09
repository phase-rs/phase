//! Wire-payload bounds for in-game `GameAction` bodies (see
//! `server_core::game_action_payload_guard`).

use engine::types::actions::{DebugAction, DebugTokenRequest};
use engine::types::counter::CounterType;
use engine::types::game_state::{ManaChoice, ShardChoice};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaType;
use engine::types::match_config::DeckCardCount;
use engine::types::player::PlayerId;
use engine::types::proposed_event::TokenCharacteristics;
use engine::types::{GameAction, ObjectId};
use server_core::game_action_payload_guard::{
    guard_game_action_payload, MAX_ACTION_LIST_LEN, MAX_CHOICE_LEN, MAX_DEBUG_AST_JSON_LEN,
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

#[test]
fn rejects_oversized_nested_sideboard_card_name() {
    let action = GameAction::SubmitSideboard {
        main: vec![DeckCardCount {
            name: "x".repeat(MAX_CHOICE_LEN + 1),
            count: 1,
        }],
        sideboard: Vec::new(),
    };

    let err = guard_game_action_payload(&action).unwrap_err();
    assert!(err.contains("SubmitSideboard.main[0].name"));
}

#[test]
fn rejects_oversized_debug_counter_name() {
    let action = GameAction::Debug(DebugAction::ModifyCounters {
        object_id: ObjectId(1),
        counter_type: CounterType::Generic("x".repeat(MAX_CHOICE_LEN + 1)),
        delta: 1,
    });

    let err = guard_game_action_payload(&action).unwrap_err();
    assert!(err.contains("Debug.ModifyCounters.counter_type.Generic"));
}

#[test]
fn rejects_oversized_debug_keyword_ast_payload() {
    let action = GameAction::Debug(DebugAction::GrantKeyword {
        object_id: ObjectId(1),
        keyword: Keyword::Unknown("x".repeat(MAX_DEBUG_AST_JSON_LEN + 1)),
    });

    let err = guard_game_action_payload(&action).unwrap_err();
    assert!(err.contains("Debug.GrantKeyword.keyword"));
}

#[test]
fn rejects_oversized_debug_token_counter_name() {
    let action = GameAction::Debug(DebugAction::CreateToken {
        request: DebugTokenRequest::Preset {
            preset_id: "soldier".to_string(),
            owner: PlayerId(0),
            power_override: None,
            toughness_override: None,
            enter_with_counters: vec![(CounterType::Generic("x".repeat(MAX_CHOICE_LEN + 1)), 1)],
        },
        run_etb: true,
    });

    let err = guard_game_action_payload(&action).unwrap_err();
    assert!(err.contains("Debug.CreateToken.request.enter_with_counters[0].counter_type.Generic"));
}

#[test]
fn accepts_debug_token_preset_pt_override_fields() {
    let action = GameAction::Debug(DebugAction::CreateToken {
        request: DebugTokenRequest::Preset {
            preset_id: "source-defined-ooze".to_string(),
            owner: PlayerId(0),
            power_override: Some(4),
            toughness_override: Some(5),
            enter_with_counters: Vec::new(),
        },
        run_etb: true,
    });

    guard_game_action_payload(&action).expect("numeric P/T overrides are semantic engine input");
}

#[test]
fn rejects_oversized_debug_token_keyword_ast_payload() {
    let action = GameAction::Debug(DebugAction::CreateToken {
        request: DebugTokenRequest::Custom {
            owner: PlayerId(0),
            characteristics: TokenCharacteristics {
                display_name: "Test Token".to_string(),
                power: Some(1),
                toughness: Some(1),
                core_types: Vec::new(),
                subtypes: Vec::new(),
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: vec![Keyword::Unknown("x".repeat(MAX_DEBUG_AST_JSON_LEN + 1))],
            },
            enter_with_counters: Vec::new(),
        },
        run_etb: true,
    });

    let err = guard_game_action_payload(&action).unwrap_err();
    assert!(err.contains("Debug.CreateToken.request.characteristics.keywords[0]"));
}
