//! Slice 1/5 acceptance: `ResolutionFrame::Iterate` faithfully reproduces the
//! per-player iteration the legacy `pending_vote_ballot_iteration` and
//! `pending_choose_one_of` fields used to hand-stash, and survives a serde
//! round-trip (the frame is on the save/load + multiplayer wire).
//!
//! The end-to-end vote-ballot iteration (3 voters each parking an interactive
//! per-ballot body, resumed in APNAP order) is exercised through the production
//! parser in the in-crate test
//! `effects::vote::tests::expropriate_money_votes_suspend_and_resume_per_ballot`
//! (that path needs the `pub(crate)` vote-block parser). Here we cover the
//! choose-one-of per-player iteration end-to-end through the public `apply`
//! pipeline, plus a serde round-trip of a live Vote `Iterate` frame.
//!
//! CR 701.55d (per-player choose-one-of), CR 701.38d (voting), CR 608.2c
//! (resolution continuation).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PlayerFilter, QuantityExpr, ResolvedAbility,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    GameState, IterCtx, IterItem, IterKind, ResolutionFrame, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use std::collections::VecDeque;

/// A two-branch `ChooseOneOf` whose chooser is every opponent of the controller:
/// branch 0 draws a card, branch 1 gains 1 life. Each opponent is prompted in
/// turn (CR 701.55d) — the per-player iteration the `Iterate` frame drives.
fn each_opponent_choose_one_of(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let draw = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let gain = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    );
    let def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Opponent,
            branches: vec![draw, gain],
        },
    );
    build_resolved_from_def(&def, source_id, controller)
}

/// CR 701.55d + CR 608.2c: In a 3-player game, both opponents must be prompted
/// for their branch choice in APNAP order, one after the other. The `Iterate`
/// frame carries the not-yet-prompted opponents; each `ChooseBranch` resolves
/// one branch and the driver re-prompts the next opponent.
#[test]
fn choose_one_of_prompts_each_opponent_in_turn_via_iterate_frame() {
    let mut state = GameState::new(FormatConfig::commander(), 3, 42);
    let controller = state.players[0].id;
    let opp1 = state.players[1].id;
    let opp2 = state.players[2].id;
    let ability = each_opponent_choose_one_of(controller, ObjectId(9100));

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // First opponent (APNAP after controller) is prompted; the second is queued
    // on the Iterate frame.
    let first_prompted = match &state.waiting_for {
        WaitingFor::ChooseOneOfBranch { player, .. } => *player,
        other => panic!("expected ChooseOneOfBranch, got {other:?}"),
    };
    assert!(first_prompted == opp1 || first_prompted == opp2);
    let queued = choose_one_of_iterate_remaining(&state)
        .expect("a ChooseOneOf Iterate frame must carry the second opponent");
    assert_eq!(queued.len(), 1, "one opponent remains queued on the frame");
    let second_expected = queued[0];
    assert_ne!(first_prompted, second_expected);

    // First opponent chooses branch 0 (draw a card).
    apply(
        &mut state,
        first_prompted,
        GameAction::ChooseBranch { index: 0 },
    )
    .expect("first opponent chooses a branch");

    // The driver re-prompted the second opponent; the frame is now consumed.
    match &state.waiting_for {
        WaitingFor::ChooseOneOfBranch { player, .. } => {
            assert_eq!(*player, second_expected, "second opponent prompted next");
        }
        other => panic!("expected ChooseOneOfBranch for second opponent, got {other:?}"),
    }
    assert!(
        choose_one_of_iterate_remaining(&state).is_none(),
        "the Iterate frame must be consumed once the last opponent is prompted"
    );

    // Second opponent chooses branch 1 (gain life); the effect fully resolves.
    apply(
        &mut state,
        second_expected,
        GameAction::ChooseBranch { index: 1 },
    )
    .expect("second opponent chooses a branch");

    assert!(
        state.resolution_stack.is_empty(),
        "resolution stack must be empty once both opponents have chosen"
    );
    assert!(!matches!(
        state.waiting_for,
        WaitingFor::ChooseOneOfBranch { .. }
    ));
}

/// CR 608.2: A live Vote `Iterate` frame must survive a serde round-trip
/// (save/load + multiplayer state are serialized). The frame carries a boxed
/// `AbilityDefinition` template and a `VecDeque<IterItem>` of remaining voters;
/// both must reconstruct byte-identically.
#[test]
fn iterate_frame_serde_round_trip() {
    let template = Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::ScopedPlayer,
        },
    ));
    let frame = ResolutionFrame::Iterate {
        remaining: VecDeque::from(vec![
            IterItem::Player(PlayerId(1)),
            IterItem::Player(PlayerId(2)),
        ]),
        ctx: IterCtx {
            source_id: ObjectId(42),
            controller: PlayerId(0),
            kind: IterKind::Vote { template },
        },
    };

    let json = serde_json::to_string(&frame).expect("frame serializes");
    let restored: ResolutionFrame = serde_json::from_str(&json).expect("frame deserializes");
    assert_eq!(frame, restored, "Iterate frame must round-trip via serde");
}

/// A whole `GameState` carrying an `Iterate` frame on its `resolution_stack`
/// must serialize and restore with the stack intact (back-compat: old saves
/// with no `resolution_stack` key default to empty, covered by the field's
/// `#[serde(default)]`).
#[test]
fn game_state_with_iterate_frame_round_trips() {
    let mut state = GameState::new(FormatConfig::commander(), 3, 42);
    state.resolution_stack.push(ResolutionFrame::Iterate {
        remaining: VecDeque::from(vec![IterItem::Player(PlayerId(2))]),
        ctx: IterCtx {
            source_id: ObjectId(7),
            controller: PlayerId(0),
            kind: IterKind::ChooseOneOf {
                branches: vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )],
                parent_targets: Vec::new(),
                context: Default::default(),
            },
        },
    });

    let json = serde_json::to_string(&state).expect("state serializes");
    let restored: GameState = serde_json::from_str(&json).expect("state deserializes");
    assert_eq!(
        restored.resolution_stack, state.resolution_stack,
        "resolution_stack (with an Iterate frame) must round-trip"
    );
}

/// Extract the remaining players carried on a top ChooseOneOf `Iterate` frame.
fn choose_one_of_iterate_remaining(state: &GameState) -> Option<Vec<PlayerId>> {
    match state.resolution_stack.last() {
        Some(ResolutionFrame::Iterate {
            remaining,
            ctx:
                IterCtx {
                    kind: IterKind::ChooseOneOf { .. },
                    ..
                },
            ..
        }) => Some(
            remaining
                .iter()
                .map(|item| match item {
                    IterItem::Player(p) => *p,
                })
                .collect(),
        ),
        _ => None,
    }
}
