//! Wire-payload bounds for in-game `GameAction` bodies (see
//! `server_core::game_action_payload_guard`).

use engine::analysis::decision_template::{
    DecisionGroupKey, DecisionKind, DecisionSlot, DecisionTemplate, IterationCount,
    MayChoiceOption, PinnedDecision, ReplayMode, TargetPin, TargetSchedule,
};
use engine::types::actions::{DebugAction, DebugTokenRequest};
use engine::types::counter::CounterType;
use engine::types::game_state::{ManaChoice, ShardChoice, YieldTarget};
use engine::types::identifiers::CardId;
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

// PR-7 Phase 3: CR 732.2a loop-shortcut declaration payload bounds.

fn shortcut_template(decision_count: usize) -> DecisionTemplate {
    let slot = DecisionSlot {
        source: YieldTarget::AllCopies {
            card_id: CardId(1),
            trigger_description: None,
        },
        index: 0,
    };
    DecisionTemplate {
        owner: PlayerId(0),
        decisions: vec![
            PinnedDecision::MayChoice {
                slot,
                take: MayChoiceOption::Take
            };
            decision_count
        ],
        replay: ReplayMode::Static,
        key: DecisionGroupKey {
            sources: vec![],
            kind: DecisionKind::LoopChoice,
        },
    }
}

#[test]
fn rejects_oversized_declare_shortcut_template() {
    let action = GameAction::DeclareShortcut {
        count: IterationCount::UntilLethal,
        template: Some(shortcut_template(MAX_ACTION_LIST_LEN + 1)),
    };
    assert!(
        guard_game_action_payload(&action).is_err(),
        "a DeclareShortcut template pin list exceeding MAX_ACTION_LIST_LEN must be rejected"
    );
}

#[test]
fn accepts_within_bound_declare_shortcut_template() {
    let action = GameAction::DeclareShortcut {
        count: IterationCount::UntilLethal,
        template: Some(shortcut_template(4)),
    };
    assert!(
        guard_game_action_payload(&action).is_ok(),
        "a realistically sized DeclareShortcut template must be accepted — proves the bound is real, not vacuous"
    );
    // The Phase-3 default (no pinned template) is trivially accepted.
    assert!(guard_game_action_payload(&GameAction::DeclareShortcut {
        count: IterationCount::UntilLethal,
        template: None,
    })
    .is_ok());
}

#[test]
fn rejects_over_cap_fixed_shortcut_count() {
    // T3a (CR 732.2a): `Fixed(u32::MAX)` is the real Vector-1 count; the WS belt must reject
    // it. Revert-probe: restore the `..` that discarded `count` ⇒ guard returns Ok ⇒ FAIL.
    let action = GameAction::DeclareShortcut {
        count: IterationCount::Fixed(u32::MAX),
        template: None,
    };
    assert!(
        guard_game_action_payload(&action).is_err(),
        "an over-cap Fixed shortcut count must be rejected at the wire belt"
    );
}

#[test]
fn accepts_realistic_fixed_shortcut_count() {
    // T3b: a plausible honest count must pass — proves the bound is real, not vacuous /
    // wrong-direction. Revert-probe: tighten the threshold to 0 ⇒ Fixed(50) rejects ⇒ FAIL.
    let action = GameAction::DeclareShortcut {
        count: IterationCount::Fixed(50),
        template: None,
    };
    assert!(
        guard_game_action_payload(&action).is_ok(),
        "a realistically sized Fixed shortcut count must be accepted"
    );
}

#[test]
fn rejects_over_cap_shortcut_schedule() {
    // T3c (REV-1 nested memory bound): the decision list and the targets list are both under
    // cap, so ONLY the oversized RoundRobin schedule vec can reject — a discriminating check
    // of the nested `Scheduled` bound (defense-in-depth for in-process callers past the WS
    // frame cap). Revert-probe: drop the schedule `bound_list` ⇒ guard returns Ok ⇒ FAIL.
    let src = YieldTarget::AllCopies {
        card_id: CardId(1),
        trigger_description: None,
    };
    let slot = DecisionSlot {
        source: src.clone(),
        index: 0,
    };
    let action = GameAction::DeclareShortcut {
        count: IterationCount::UntilLethal,
        template: Some(DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Targets {
                slot,
                targets: vec![TargetPin::Scheduled(TargetSchedule::RoundRobin(vec![
                    src;
                    MAX_ACTION_LIST_LEN + 1
                ]))],
            }],
            replay: ReplayMode::Static,
            key: DecisionGroupKey {
                sources: vec![],
                kind: DecisionKind::LoopChoice,
            },
        }),
    };
    assert!(
        guard_game_action_payload(&action).is_err(),
        "an over-cap loop-shortcut schedule vec must be rejected (nested memory bound)"
    );
}
