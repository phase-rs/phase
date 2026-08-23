//! CR733 P2 coverage for bounded, journaled resolution-frame transitions.

use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PostReplacementContinuation, QuantityExpr,
    ResolvedAbility, TargetFilter,
};
use engine::types::game_state::{
    DrawSequenceStack, GameState, PendingContinuation, PostReplacementDrain,
    PostReplacementDrainStack, ResidentDrainPolicy, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::resolution::{
    CipherEncodeStage, FrameKind, MultiDrawFrame, OptionalEffectFrame, PendingCipherEncode,
    PendingCoinFlip, PendingCoinFlipKind, ResolutionFrame, ResolutionStackError,
    ResolutionStateWire,
};
use engine::types::resolved_commands::{
    ResolvedCommandOrdinal, ResolvedFrameTransition, ResolvedFrameTransitionCommand,
    ResolvedFrameTransitionReplayInvariantError, ResolvedRulesCommand, ResolvedRulesJournal,
    RulesExecutionNodeRef,
};

fn cause() -> RulesExecutionNodeRef {
    RulesExecutionNodeRef::Proposal(ResolvedCommandOrdinal(0))
}

fn command(transition: ResolvedFrameTransition) -> ResolvedFrameTransitionCommand {
    ResolvedFrameTransitionCommand {
        transition,
        cause: cause(),
    }
}

fn multi_draw_frame() -> ResolutionFrame {
    ResolutionFrame::MultiDraw(MultiDrawFrame {
        draw_sequences: DrawSequenceStack::default(),
        connive_reentry: None,
    })
}

fn coin_flip_frame() -> ResolutionFrame {
    ResolutionFrame::CoinFlip(PendingCoinFlip {
        source_id: ObjectId(91),
        controller: PlayerId(0),
        flipper: PlayerId(0),
        targets: Vec::new(),
        win_effect: None,
        lose_effect: None,
        kind: PendingCoinFlipKind::Single,
    })
}

fn optional_effect_frame(state: &GameState) -> ResolutionFrame {
    ResolutionFrame::OptionalEffect(OptionalEffectFrame {
        ability: pending_continuation(state).chain,
        trigger_event: None,
        trigger_events: Vec::new(),
        trigger_match_count: None,
    })
}

fn frames(state: &GameState) -> Vec<ResolutionFrame> {
    state.resolution_stack.iter().cloned().collect()
}

fn assert_command_round_trip(command: &ResolvedFrameTransitionCommand) {
    let value = serde_json::to_value(command).expect("frame transition command serializes");
    assert_eq!(
        serde_json::from_value::<ResolvedFrameTransitionCommand>(value)
            .expect("frame transition command deserializes"),
        *command
    );
}

/// Every supported command round-trips independently and applies the native
/// structural primitive to exactly the expected stack prefix.
#[test]
fn frame_transition_commands_round_trip_and_apply_exact_stack_results() {
    let push_frame = multi_draw_frame();
    let push = command(ResolvedFrameTransition::Push {
        frame: push_frame.clone(),
    });
    assert_command_round_trip(&push);
    let mut push_state = GameState::new_two_player(91);
    push_state
        .apply_resolved_frame_transition(&push)
        .expect("push command applies");
    assert_eq!(frames(&push_state), vec![push_frame]);

    let inserted_frame = ResolutionFrame::PostReplacement(PostReplacementDrainStack::default());
    let insert = command(ResolvedFrameTransition::InsertParentOfActive {
        frame: inserted_frame.clone(),
    });
    assert_command_round_trip(&insert);
    let mut insert_state = GameState::new_two_player(92);
    insert_state.resolution_stack.push_inner(multi_draw_frame());
    insert_state
        .apply_resolved_frame_transition(&insert)
        .expect("parent insertion command applies");
    assert_eq!(
        frames(&insert_state),
        vec![inserted_frame, multi_draw_frame()]
    );

    let pop = command(ResolvedFrameTransition::PopExpected {
        kind: FrameKind::MultiDraw,
    });
    assert_command_round_trip(&pop);
    let mut pop_state = GameState::new_two_player(93);
    pop_state.resolution_stack.push_inner(multi_draw_frame());
    pop_state
        .apply_resolved_frame_transition(&pop)
        .expect("expected frame pop applies");
    assert!(frames(&pop_state).is_empty());

    let replacement_frame = ResolutionFrame::PostReplacement(PostReplacementDrainStack::default());
    let replace = command(ResolvedFrameTransition::ReplaceActive {
        frame: replacement_frame.clone(),
    });
    assert_command_round_trip(&replace);
    let mut replace_state = GameState::new_two_player(94);
    replace_state
        .resolution_stack
        .push_inner(multi_draw_frame());
    replace_state
        .apply_resolved_frame_transition(&replace)
        .expect("active frame replacement applies");
    assert_eq!(frames(&replace_state), vec![replacement_frame]);
}

/// Structural validation runs against a cloned stack, so a prompt mismatch
/// cannot leave a partially applied direct-choice frame behind.
#[test]
fn malformed_prompt_coherence_errors_atomically() {
    let mut state = GameState::new_two_player(95);
    let before = state.resolution_stack.clone();

    assert_eq!(
        state.apply_resolved_frame_transition(&command(ResolvedFrameTransition::Push {
            frame: coin_flip_frame(),
        })),
        Err(ResolvedFrameTransitionReplayInvariantError::Stack(
            ResolutionStackError::PromptMismatch {
                frame: FrameKind::CoinFlip,
                waiting_for: "Priority",
            }
        ))
    );
    assert_eq!(state.resolution_stack, before);
}

/// Regression for #6805: an optional-effect prompt owner must be removed before
/// an accepted optional search installs `SearchChoice`. Parent insertion is the
/// first boundary that validates this stale-owner state, and must reject it
/// atomically rather than burying the direct-choice frame.
#[test]
fn optional_effect_frame_cannot_survive_into_search_choice_parent_insertion() {
    let mut state = GameState::new_two_player(97);
    let optional_ability = pending_continuation(&state).chain;
    state
        .resolution_stack
        .push_inner(ResolutionFrame::OptionalEffect(OptionalEffectFrame {
            ability: optional_ability,
            trigger_event: None,
            trigger_events: Vec::new(),
            trigger_match_count: None,
        }));
    state.waiting_for = WaitingFor::SearchChoice {
        player: PlayerId(0),
        library_owner: Some(PlayerId(0)),
        cards: Vec::new(),
        count: 0,
        reveal: false,
        up_to: false,
        allows_partial_find: false,
        constraint: Default::default(),
        ordering_hint: Default::default(),
        split: None,
    };
    let before = state.resolution_stack.clone();

    assert_eq!(
        state.insert_ability_continuation_parent_of_active(pending_continuation(&state)),
        Err(ResolutionStackError::PromptMismatch {
            frame: FrameKind::OptionalEffect,
            waiting_for: "SearchChoice",
        })
    );
    assert_eq!(state.resolution_stack, before);
}

/// Regression for #6867: direct-choice ownership and its live prompt are one
/// atomic state transition. A second optional owner is rejected at installation
/// and leaves the first player-facing prompt untouched.
#[test]
fn direct_choice_install_rejects_a_second_optional_owner_atomically() {
    let mut state = GameState::new_two_player(98);
    let first_prompt = WaitingFor::OptionalEffectChoice {
        player: PlayerId(0),
        source_id: ObjectId(100),
        description: None,
        may_trigger_key: None,
    };
    let first_frame = optional_effect_frame(&state);
    state
        .install_direct_choice_frame(first_frame, first_prompt.clone())
        .expect("first optional prompt installs");
    let before_stack = state.resolution_stack.clone();
    let before_waiting_for = state.waiting_for.clone();

    let second_frame = optional_effect_frame(&state);
    assert_eq!(
        state.install_direct_choice_frame(
            second_frame,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(1),
                source_id: ObjectId(101),
                description: None,
                may_trigger_key: None,
            },
        ),
        Err(ResolutionStackError::MultipleDirectChoiceOwners)
    );
    assert_eq!(state.resolution_stack, before_stack);
    assert_eq!(state.waiting_for, before_waiting_for);
}

/// Missing parents and wrong active kinds are typed failures; neither failure
/// changes the existing stack.
#[test]
fn parentless_and_wrong_kind_frame_transitions_error_atomically() {
    let mut parentless = GameState::new_two_player(96);
    let parentless_before = parentless.resolution_stack.clone();
    assert_eq!(
        parentless.apply_resolved_frame_transition(&command(
            ResolvedFrameTransition::InsertParentOfActive {
                frame: ResolutionFrame::PostReplacement(PostReplacementDrainStack::default()),
            },
        )),
        Err(ResolvedFrameTransitionReplayInvariantError::Stack(
            ResolutionStackError::NoActiveChild
        ))
    );
    assert_eq!(parentless.resolution_stack, parentless_before);

    let mut empty = GameState::new_two_player(97);
    let empty_before = empty.resolution_stack.clone();
    assert_eq!(
        empty.apply_resolved_frame_transition(&command(ResolvedFrameTransition::ReplaceActive {
            frame: multi_draw_frame(),
        })),
        Err(ResolvedFrameTransitionReplayInvariantError::Stack(
            ResolutionStackError::Empty
        ))
    );
    assert_eq!(empty.resolution_stack, empty_before);

    let mut wrong_kind = GameState::new_two_player(97);
    wrong_kind.resolution_stack.push_inner(multi_draw_frame());
    let wrong_kind_before = wrong_kind.resolution_stack.clone();
    assert_eq!(
        wrong_kind.apply_resolved_frame_transition(&command(
            ResolvedFrameTransition::PopExpected {
                kind: FrameKind::CoinFlip,
            },
        )),
        Err(ResolvedFrameTransitionReplayInvariantError::Stack(
            ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CoinFlip,
                actual: FrameKind::MultiDraw,
            }
        ))
    );
    assert_eq!(wrong_kind.resolution_stack, wrong_kind_before);
}

/// The command's causal node is journal authority: a tampered serialized
/// cause is rejected before it can be restored as replay input.
#[test]
fn serialized_frame_transition_with_tampered_cause_is_rejected() {
    let mut state = GameState::new_two_player(98);
    state
        .resolve_and_apply_frame_transition(ResolvedFrameTransition::Push {
            frame: multi_draw_frame(),
        })
        .expect("resolved transition journals under a proposal node");
    let second_proposal = state
        .resolved_rules_journal
        .begin_proposal()
        .expect("second valid proposal node opens");

    let mut journal =
        serde_json::to_value(&state.resolved_rules_journal).expect("journal serializes");
    let entry = journal["entries"]
        .as_array_mut()
        .expect("journal has entries")
        .iter_mut()
        .find(|entry| entry["command"].get("FrameTransition").is_some())
        .expect("transition command is present");
    entry["command"]["FrameTransition"]["cause"] =
        serde_json::to_value(second_proposal).expect("second proposal serializes");

    let error = serde_json::from_value::<ResolvedRulesJournal>(journal)
        .expect_err("a valid but unrelated cause is rejected");
    assert!(error.to_string().contains("unrelated cause"));
}

/// Resolving a malformed transition can allocate the proposal provenance slot,
/// but failure occurs before a semantic command is appended.
#[test]
fn failed_resolve_keeps_only_the_proposal_slot_without_a_semantic_entry() {
    let mut state = GameState::new_two_player(99);

    assert!(matches!(
        state.resolve_and_apply_frame_transition(ResolvedFrameTransition::InsertParentOfActive {
            frame: ResolutionFrame::PostReplacement(PostReplacementDrainStack::default()),
        }),
        Err(ResolvedFrameTransitionReplayInvariantError::Stack(
            ResolutionStackError::NoActiveChild
        ))
    ));
    assert_eq!(state.resolved_rules_journal.entries().len(), 1);
    assert_eq!(state.resolved_rules_journal.nodes().len(), 1);
    assert!(state.resolved_rules_journal.entries()[0].command.is_none());
}

fn pending_continuation(state: &GameState) -> PendingContinuation {
    PendingContinuation::new(
        Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        )),
        state,
    )
}

fn post_replacement_drain() -> PostReplacementDrain {
    PostReplacementDrain::ready(PostReplacementContinuation::Template(Box::new(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("CR733 frame transition fixture", "test-only drain"),
        ),
    )))
}

/// The only two production parent-insertion callers now use the resolved
/// command boundary, retain their exact adjacency, and journal that insertion.
#[test]
fn production_parent_insertions_preserve_adjacency_and_journal_the_transition() {
    let mut continuation_state = GameState::new_two_player(100);
    continuation_state
        .resolution_stack
        .push_inner(multi_draw_frame());
    let pending = pending_continuation(&continuation_state);
    continuation_state
        .insert_ability_continuation_parent_of_active(pending)
        .expect("active child accepts its continuation parent");
    assert!(matches!(
        frames(&continuation_state).as_slice(),
        [
            ResolutionFrame::AbilityContinuation(_),
            ResolutionFrame::MultiDraw(_)
        ]
    ));
    assert!(continuation_state
        .resolved_rules_journal
        .entries()
        .iter()
        .any(|entry| matches!(
            &entry.command,
            Some(ResolvedRulesCommand::FrameTransition(command))
                if matches!(
                    &command.transition,
                    ResolvedFrameTransition::InsertParentOfActive {
                        frame: ResolutionFrame::AbilityContinuation(_),
                    }
                )
        )));

    let mut replacement_state = GameState::new_two_player(101);
    replacement_state
        .resolution_stack
        .push_inner(multi_draw_frame());
    assert!(replacement_state
        .install_post_replacement_drain(post_replacement_drain(), ResidentDrainPolicy::Replace,));
    assert!(matches!(
        frames(&replacement_state).as_slice(),
        [
            ResolutionFrame::PostReplacement(_),
            ResolutionFrame::MultiDraw(_)
        ]
    ));
    assert!(replacement_state
        .resolved_rules_journal
        .entries()
        .iter()
        .any(|entry| matches!(
            &entry.command,
            Some(ResolvedRulesCommand::FrameTransition(command))
                if matches!(
                    &command.transition,
                    ResolvedFrameTransition::InsertParentOfActive {
                        frame: ResolutionFrame::PostReplacement(_),
                    }
                )
        )));
}

/// Frame-transition commands use the existing v2 resolution-state wire and
/// preserve both their journal evidence and their structural stack exactly.
#[test]
fn resolution_state_wire_v2_round_trip_preserves_frame_transition_journal_and_stack() {
    let mut state = GameState::new_two_player(102);
    state
        .resolve_and_apply_frame_transition(ResolvedFrameTransition::Push {
            frame: multi_draw_frame(),
        })
        .expect("resolved transition applies");
    let expected_journal = state.resolved_rules_journal.clone();
    let expected_stack = frames(&state);

    let wire = serde_json::to_value(ResolutionStateWire::from_game_state(state))
        .expect("v2 resolution state serializes");
    assert_eq!(wire["resolution_state_version"], 2);
    let restored = serde_json::from_value::<ResolutionStateWire>(wire)
        .expect("v2 resolution state restores")
        .into_game_state();

    assert_eq!(restored.resolved_rules_journal, expected_journal);
    assert_eq!(frames(&restored), expected_stack);
}

/// The parking transition records an operand and no position: replay reaches
/// the same stack by asking the stack again, not by reading back a slot.
///
/// The fixture is the shape that makes the distinction observable — a paused
/// post-replacement/draw pair, where "below the top" and "outside the pair" are
/// different positions (CR 614.11a + CR 121.6b keep the pair adjacent).
#[test]
fn parking_beneath_a_live_prompt_journals_its_operand_and_replays_to_the_same_stack() {
    let mut state = GameState::new_two_player(103);
    state
        .resolution_stack
        .push_inner(paused_post_replacement_frame());
    state.resolution_stack.push_inner(active_multi_draw_frame());
    let prompt_owner = optional_effect_frame(&state);
    state.resolution_stack.push_inner(prompt_owner.clone());
    state.waiting_for = WaitingFor::OpponentMayChoice {
        player: PlayerId(1),
        source_id: ObjectId(7),
        description: None,
        remaining: Vec::new(),
    };

    // The parked frame must own no prompt — parking a direct-choice owner
    // beneath another frame buries it, which `validate` rejects on its own
    // terms. A `Parked` Cipher offer is exactly such a frame.
    let parked = ResolutionFrame::CipherEncode(PendingCipherEncode {
        stage: CipherEncodeStage::Parked,
        card_id: ObjectId(41),
        controller: PlayerId(0),
        creatures: vec![ObjectId(42)],
    });
    state
        .resolve_and_apply_frame_transition(ResolvedFrameTransition::ParkBeneathLivePrompt {
            frame: parked.clone(),
        })
        .expect("a prompt-less frame always has a place beneath the live prompt");

    assert!(
        matches!(
            frames(&state).as_slice(),
            [
                ResolutionFrame::CipherEncode(_),
                ResolutionFrame::PostReplacement(_),
                ResolutionFrame::MultiDraw(_),
                ResolutionFrame::OptionalEffect(_)
            ]
        ),
        "the parked frame sits outside the pair, not between its halves: {:?}",
        frames(&state)
            .iter()
            .map(ResolutionFrame::kind)
            .collect::<Vec<_>>()
    );

    let recorded = state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| match &entry.command {
            Some(ResolvedRulesCommand::FrameTransition(command)) => Some(command),
            _ => None,
        })
        .find(|command| {
            matches!(
                &command.transition,
                ResolvedFrameTransition::ParkBeneathLivePrompt { .. }
            )
        })
        .expect("the parking transition is journaled")
        .clone();
    assert_command_round_trip(&recorded);

    // Replay: the same operand against the same prior stack reaches the same
    // placement, which is what makes a position-free record sufficient.
    let mut replayed = GameState::new_two_player(103);
    replayed
        .resolution_stack
        .push_inner(paused_post_replacement_frame());
    replayed
        .resolution_stack
        .push_inner(active_multi_draw_frame());
    replayed.resolution_stack.push_inner(prompt_owner);
    replayed.waiting_for = WaitingFor::OpponentMayChoice {
        player: PlayerId(1),
        source_id: ObjectId(7),
        description: None,
        remaining: Vec::new(),
    };
    replayed
        .apply_resolved_frame_transition(&command(ResolvedFrameTransition::ParkBeneathLivePrompt {
            frame: parked,
        }))
        .expect("replaying the recorded transition applies");
    assert_eq!(frames(&replayed), frames(&state));
}

/// A post-replacement frame whose resident drain is PAUSED — the only status
/// under which `validate` protects the pair's adjacency (CR 614.11a).
fn paused_post_replacement_frame() -> ResolutionFrame {
    let mut drains = PostReplacementDrainStack::default();
    assert!(drains.install(post_replacement_drain(), ResidentDrainPolicy::KeepResident));
    let (_, dispatch) = drains
        .begin_dispatch()
        .expect("a ready drain begins dispatching");
    assert!(drains.pause_dispatch(dispatch));
    ResolutionFrame::PostReplacement(drains)
}

/// A multi-draw child with an ACTIVE draw sequence, which the paired-adjacency
/// check requires (CR 121.2: a multi-card draw is that many individual draws).
fn active_multi_draw_frame() -> ResolutionFrame {
    let mut draw_sequences = DrawSequenceStack::default();
    draw_sequences.push(PlayerId(0), 1);
    ResolutionFrame::MultiDraw(MultiDrawFrame {
        draw_sequences,
        connive_reentry: None,
    })
}
