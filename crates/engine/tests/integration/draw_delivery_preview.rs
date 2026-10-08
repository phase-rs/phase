//! Clone-isolated exact delivery coverage for `preview_draw_delivery`.
//!
//! Each row first observes a declared live-pipeline result on an independent
//! runner, then proves the preview reports the same completed instruction fact
//! without mutating its caller's state. Choice rows additionally pin the live
//! prompt, so `Unknown` cannot be satisfied by silently selecting a branch.

use engine::game::effects::draw::{preview_draw_delivery, DrawDeliveryPreview};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CardSelectionMode, DrawReplacementScope, Effect, QuantityExpr,
    QuantityModification, ReplacementDefinition, ReplacementMode, ResolvedAbility,
    SearchSelectionConstraint, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{
    CastingVariant, GameState, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::Zone;

fn runner(library: usize, replacements: Vec<ReplacementDefinition>) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..library {
        scenario.add_card_to_library_top(P0, &format!("P0 library card {index}"));
    }
    for index in 0..5 {
        scenario.add_card_to_library_top(P1, &format!("P1 library card {index}"));
    }
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    for (index, replacement) in replacements.into_iter().enumerate() {
        let source = create_object(
            runner.state_mut(),
            CardId(90_000 + index as u64),
            P0,
            format!("Draw replacement {index}"),
            Zone::Battlefield,
        );
        let object = runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("replacement source must exist");
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(1);
        object.toughness = Some(1);
        object.replacement_definitions.push(replacement);
    }

    runner
}

fn preview_is_read_only(runner: &GameRunner, requested: u32, expected: DrawDeliveryPreview) {
    let before = runner.state().clone();
    assert_eq!(
        preview_draw_delivery(runner.state(), P0, requested),
        expected
    );
    assert_eq!(
        runner.state(),
        &before,
        "the clone-backed preview must not mutate its caller's game state"
    );
}

fn issue_draw(runner: &mut GameRunner, requested: u32) {
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: requested,
        }))
        .expect("debug draw must be accepted");
}

fn resolve_stacked_draw(runner: &mut GameRunner, requested: i32) {
    runner.state_mut().debug_mode = false;
    let card_id = CardId(97_100);
    let source_id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Draw spell".to_string(),
        Zone::Stack,
    );
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: requested },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        source_id,
        P0,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: source_id,
        source_id,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id,
            ability: Some(Box::new(ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    runner
        .act(GameAction::PassPriority)
        .expect("active player passes priority");
    runner
        .act(GameAction::PassPriority)
        .expect("second player passes and resolves the draw spell");
}

fn completed_delivery(mut runner: GameRunner, requested: u32) -> (usize, GameState) {
    issue_draw(&mut runner, requested);
    runner.advance_until_stack_empty();
    // CR 121.4 can make the player lose after a partial draw from a short library.
    // Elimination may clear that player's hand, so the completed draw frame's
    // delivery count is the live authority rather than the post-game zone size.
    let delivered = runner
        .state()
        .last_effect_count
        .expect("completed draw records its live delivery count")
        .try_into()
        .expect("draw delivery count is nonnegative");
    (delivered, runner.state().clone())
}

fn individual_draw_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Draw)
        .draw_scope(DrawReplacementScope::IndividualDraw)
}

fn mandatory_prevent() -> ReplacementDefinition {
    individual_draw_replacement().quantity_modification(QuantityModification::Prevent)
}

fn optional_prevent() -> ReplacementDefinition {
    individual_draw_replacement()
        .quantity_modification(QuantityModification::Prevent)
        .mode(ReplacementMode::Optional { decline: None })
}

fn gain_life_substitute() -> ReplacementDefinition {
    individual_draw_replacement().execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        },
    ))
}

fn draw_two_instead() -> ReplacementDefinition {
    individual_draw_replacement().execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
    ))
}

fn draw_two_then_discard() -> ReplacementDefinition {
    let mut draw = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
    );
    draw.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
            selection: CardSelectionMode::Chosen,
            unless_filter: None,
            filter: None,
        },
    )));
    individual_draw_replacement().execute(draw)
}

fn search_library_substitute() -> ReplacementDefinition {
    individual_draw_replacement().execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SearchLibrary {
            source_zones: vec![Zone::Library],
            filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
        },
    ))
}

fn add_library_land(state: &mut GameState) {
    let card = create_object(
        state,
        CardId(95_000),
        P0,
        "Preview search land".to_string(),
        Zone::Library,
    );
    state
        .objects
        .get_mut(&card)
        .expect("search candidate must exist")
        .card_types
        .core_types
        .push(CoreType::Land);
}

fn add_draw_limit(state: &mut GameState) {
    let source = create_object(
        state,
        CardId(96_000),
        P1,
        "Preview draw limit".to_string(),
        Zone::Battlefield,
    );
    let object = state
        .objects
        .get_mut(&source)
        .expect("draw-limit source must exist");
    object.card_types.core_types.push(CoreType::Creature);
    object.power = Some(1);
    object.toughness = Some(1);
    object
        .static_definitions
        .push(engine::types::ability::StaticDefinition::new(
            StaticMode::PerTurnDrawLimit {
                who: ProhibitionScope::AllPlayers,
                max: 1,
            },
        ));
}

/// E1: the completed frame's total equals normal live delivery, not merely the
/// request amount by convention.
#[test]
fn preview_reports_normal_instruction_delivery() {
    preview_is_read_only(
        &runner(4, Vec::new()),
        3,
        DrawDeliveryPreview::Exact { delivered: 3 },
    );
    assert_eq!(completed_delivery(runner(4, Vec::new()), 3).0, 3);
}

/// E2: CR 121.2b permits only the one individual draw left under the limit.
#[test]
fn preview_reports_partial_delivery_from_draw_limit() {
    let mut preview = runner(4, Vec::new());
    add_draw_limit(preview.state_mut());
    preview_is_read_only(&preview, 3, DrawDeliveryPreview::Exact { delivered: 1 });

    let mut live = runner(4, Vec::new());
    add_draw_limit(live.state_mut());
    assert_eq!(completed_delivery(live, 3).0, 1);
}

/// E3: CR 121.4's empty-library attempt follows the one card that was actually
/// delivered, so a short library is also an exact partial result.
#[test]
fn preview_reports_partial_delivery_from_short_library() {
    preview_is_read_only(
        &runner(1, Vec::new()),
        3,
        DrawDeliveryPreview::Exact { delivered: 1 },
    );
    assert_eq!(completed_delivery(runner(1, Vec::new()), 3).0, 1);
}

/// E4: CR 614.6 prevention is a fully settled zero, not an unknown branch.
#[test]
fn preview_reports_prevented_draw_as_exact_zero() {
    preview_is_read_only(
        &runner(1, vec![mandatory_prevent()]),
        1,
        DrawDeliveryPreview::Exact { delivered: 0 },
    );
    assert_eq!(
        completed_delivery(runner(1, vec![mandatory_prevent()]), 1).0,
        0
    );
    assert_eq!(completed_delivery(runner(1, Vec::new()), 1).0, 1);
}

/// E5: a mandatory non-draw continuation that settles without a choice is also
/// exact zero, while its substitute effect still happens live.
#[test]
fn preview_reports_settled_non_draw_substitute_as_exact_zero() {
    preview_is_read_only(
        &runner(1, vec![gain_life_substitute()]),
        1,
        DrawDeliveryPreview::Exact { delivered: 0 },
    );

    let live = runner(1, vec![gain_life_substitute()]);
    let life_before = live.state().players[P0.0 as usize].life;
    let (delivered, state) = completed_delivery(live, 1);
    assert_eq!(delivered, 0);
    assert_eq!(state.players[P0.0 as usize].life, life_before + 5);
}

/// E6: count-modifying replacements execute through the real pipeline instead
/// of being recognized only as a prevention special case.
#[test]
fn preview_reports_count_modified_draw_delivery() {
    preview_is_read_only(
        &runner(5, vec![draw_two_instead()]),
        2,
        DrawDeliveryPreview::Exact { delivered: 4 },
    );
    assert_eq!(
        completed_delivery(runner(5, vec![draw_two_instead()]), 2).0,
        4
    );
}

/// A count-changing Draw replacement must finish every child delivery before
/// its non-LoseLife follow-up starts, including when one delivery pauses.
#[test]
fn count_modified_draw_follow_up_waits_for_paused_child_delivery() {
    let mut runner = runner(4, vec![draw_two_then_discard()]);
    let first_library_card = runner.state().players[P0.0 as usize].library[0];
    let existing_hand_cards: Vec<_> = (0..2)
        .map(|index| {
            create_object(
                runner.state_mut(),
                CardId(97_000 + index),
                P0,
                format!("Existing hand card {index}"),
                Zone::Hand,
            )
        })
        .collect();
    let pause_source = create_object(
        runner.state_mut(),
        CardId(97_002),
        P0,
        "Paused draw delivery replacement".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&pause_source)
        .expect("pause replacement source exists")
        .replacement_definitions
        .push(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .valid_card(TargetFilter::SpecificObject {
                    id: first_library_card,
                })
                .mode(ReplacementMode::Optional { decline: None }),
        );

    resolve_stacked_draw(&mut runner, 1);

    let WaitingFor::ReplacementChoice {
        player: P0,
        kind: engine::types::game_state::ReplacementChoiceKind::OptionalBranch,
        candidates,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the child delivery must remain paused before its Discard follow-up; got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == pause_source));
    let (has_pending_delivery, child_id, owner) = runner
        .state()
        .active_draw_sequence()
        .map(|child| {
            (
                child.pending_delivery.is_some(),
                child.frame_id,
                child.delivery_owner,
            )
        })
        .expect("the paused child draw remains the active sequence");
    assert!(has_pending_delivery);
    let owner = owner.expect("the child delivery is owned by the interrupted parent draw");
    let parent = runner
        .state_mut()
        .draw_sequence_frame_mut(owner)
        .expect("the paused child delivery owner remains live");
    assert_eq!(parent.player, P0);
    assert_ne!(parent.frame_id, child_id);
    assert!(runner
        .state()
        .active_multi_draw_frame()
        .is_some_and(|frame| frame.draw_sequences.validate().is_ok()));

    let pause_index = candidates
        .iter()
        .position(|candidate| candidate.source_id == pause_source)
        .expect("the delivery pause source is offered");
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: pause_index })
        .expect("accept the movement replacement and resume the child delivery");
    assert_eq!(
        resumed
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
            .count(),
        2,
        "both replacement-created child draws finish before Discard is offered"
    );
    runner.advance_until_stack_empty();
    let WaitingFor::DiscardChoice {
        player: P0,
        count: 1,
        cards,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the follow-up starts after both child draws complete; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        cards.len(),
        4,
        "the Discard sees both existing cards and both draws"
    );

    let discarded = runner
        .act(GameAction::SelectCards {
            cards: vec![existing_hand_cards[0]],
        })
        .expect("resolve the single follow-up Discard");
    runner.advance_until_stack_empty();
    assert_eq!(
        discarded
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::Discarded { player_id: P0, .. }))
            .count(),
        1,
        "the replacement follow-up executes exactly once"
    );
    assert_eq!(runner.state().last_effect_count, Some(1));
    assert_eq!(runner.state().players[P0.0 as usize].hand.len(), 3);
    assert!(runner.state().active_draw_sequence().is_none());
    assert!(runner.state().resolution_stack.is_empty());
}

/// E7: an optional replacement is owned by the affected player, so no preview
/// branch is selected on their behalf.
#[test]
fn preview_returns_unknown_for_direct_replacement_choice() {
    preview_is_read_only(
        &runner(1, vec![optional_prevent()]),
        1,
        DrawDeliveryPreview::Unknown,
    );

    let mut live = runner(1, vec![optional_prevent()]);
    issue_draw(&mut live, 1);
    assert!(matches!(
        live.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
}

/// E8: materially different mandatory replacements still require the affected
/// player's CR 616.1 ordering choice.
#[test]
fn preview_returns_unknown_for_material_replacement_ordering() {
    preview_is_read_only(
        &runner(1, vec![mandatory_prevent(), gain_life_substitute()]),
        1,
        DrawDeliveryPreview::Unknown,
    );

    let mut live = runner(1, vec![mandatory_prevent(), gain_life_substitute()]);
    issue_draw(&mut live, 1);
    assert!(matches!(
        live.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
}

/// E9: CR 614.11a requires a mandatory replacement's search continuation to
/// finish before resuming the sequence. Its `SearchChoice` is not a replacement
/// order prompt, but it still makes the instruction unpriceable.
#[test]
fn preview_returns_unknown_for_mandatory_continuation_choice() {
    let mut preview = runner(0, vec![search_library_substitute()]);
    add_library_land(preview.state_mut());
    preview_is_read_only(&preview, 1, DrawDeliveryPreview::Unknown);

    let mut live = runner(0, vec![search_library_substitute()]);
    add_library_land(live.state_mut());
    issue_draw(&mut live, 1);
    assert!(matches!(
        live.state().waiting_for,
        WaitingFor::SearchChoice { .. }
    ));
    assert!(
        !matches!(
            live.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the mandatory continuation must park at SearchChoice, not ReplacementChoice"
    );
}

/// E10: CR 614.11 applies an optional draw replacement before the empty-library
/// delivery attempt. The no-replacement sibling remains an exact zero.
#[test]
fn preview_checks_empty_library_replacements_before_exact_zero() {
    preview_is_read_only(
        &runner(0, Vec::new()),
        1,
        DrawDeliveryPreview::Exact { delivered: 0 },
    );
    preview_is_read_only(
        &runner(0, vec![optional_prevent()]),
        1,
        DrawDeliveryPreview::Unknown,
    );

    let mut live = runner(0, vec![optional_prevent()]);
    issue_draw(&mut live, 1);
    assert!(matches!(
        live.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
}

#[test]
fn scrivener_child_draw_delivery_matches_live_events_and_preview() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("P0 library card {index}"));
    }
    scenario.add_creature_from_oracle(
        P0,
        "Blood Scrivener",
        2,
        1,
        "If you would draw a card while you have no cards in hand, instead you draw two cards and you lose 1 life.",
    );
    let mut live = scenario.build();
    live.state_mut().debug_mode = true;

    let preview = preview_draw_delivery(live.state(), P0, 1);
    let action = live
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("the draw action must succeed");
    live.advance_until_stack_empty();
    let card_drawn_events = action
        .events
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
        .count();
    let draw_action_events = action
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P0,
                    action: PlayerActionKind::Draw,
                    ..
                }
            )
        })
        .count();
    let draw_action_ledger_entries = live
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|entry| **entry == (P0, PlayerActionKind::Draw))
        .count();
    println!(
        "task30 F3: CardDrawn={card_drawn_events}, PlayerPerformedAction::Draw={draw_action_events}, player_actions_this_turn={draw_action_ledger_entries}"
    );
    assert_eq!(
        draw_action_events, 2,
        "child and parent instructions each complete once"
    );
    assert_eq!(
        draw_action_ledger_entries, 2,
        "completed draw instructions are recorded once"
    );
    let observed = (
        preview,
        card_drawn_events,
        live.state().players[P0.0 as usize].hand.len(),
        live.state().players[P0.0 as usize].life,
        live.state().last_effect_count,
    );

    assert_eq!(
        observed,
        (
            DrawDeliveryPreview::Exact { delivered: 2 },
            2,
            2,
            19,
            Some(2),
        ),
        "the child draw must contribute its actual delivery to the owning draw result"
    );
}
