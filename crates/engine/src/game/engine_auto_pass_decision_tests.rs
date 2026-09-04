use super::*;
use std::sync::Arc;

use crate::ai_support::AiDecisionContract;
use crate::game::combat::AttackTarget;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, CopyRetargetPermission, Effect, PtValue, QuantityExpr,
    ResolvedAbility, StaticDefinition, TargetFilter,
};
use crate::types::actions::{GameAction, ResolveAllScope};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, StackResolutionBudget, StackResolutionPolicy, TurnBoundary,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::ManaColor;
use crate::types::phase::{PhaseStop, PhaseStopScope};
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

fn stack_entry(controller: PlayerId) -> StackEntry {
    StackEntry {
        id: ObjectId(0),
        source_id: ObjectId(0),
        controller,
        kind: StackEntryKind::KeywordAction {
            action: KeywordAction::Equip {
                equipment_id: ObjectId(0),
                target_creature_id: ObjectId(0),
            },
        },
    }
}

fn stop(phase: Phase, scope: PhaseStopScope) -> PhaseStop {
    PhaseStop { phase, scope }
}

fn is_pass(d: &AutoPassDecision) -> bool {
    matches!(d, AutoPassDecision::Pass)
}

fn is_finish(d: &AutoPassDecision) -> bool {
    matches!(d, AutoPassDecision::Finish)
}

fn is_break(d: &AutoPassDecision) -> bool {
    matches!(d, AutoPassDecision::Break)
}

fn priority_state() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 1;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    state
}

fn add_untapped_creature(state: &mut GameState, controller: PlayerId, card_id: u64) -> ObjectId {
    let object_id = create_object(
        state,
        CardId(card_id),
        controller,
        "Combat creature".to_string(),
        Zone::Battlefield,
    );
    let object = state.objects.get_mut(&object_id).unwrap();
    object.card_types.core_types.push(CoreType::Creature);
    object.summoning_sick = false;
    object_id
}

#[test]
fn apply_reconciles_eliminated_two_player_game_to_game_over() {
    let mut state = priority_state();
    state.players[1].is_eliminated = true;
    state.eliminated_players.push(PlayerId(1));

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::GameOver {
            winner: Some(PlayerId(0))
        }
    ));
    assert!(matches!(
        state.waiting_for,
        WaitingFor::GameOver {
            winner: Some(PlayerId(0))
        }
    ));
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::GameOver {
            winner: Some(PlayerId(0))
        }
    )));
}

/// V7: the requested boundary is carried through the production
/// `SetAutoPass` dispatch into the stored `AutoPassMode` — not hardcoded to
/// `EndOfCurrentTurn`. Driven through `apply(GameAction::SetAutoPass)`, the real
/// request→mode conversion seam. The negative sibling proves the conversion is
/// not stuck on a single boundary.
#[test]
fn set_auto_pass_carries_requested_boundary_via_dispatch() {
    for until in [
        TurnBoundary::MyNextTurnStart,
        TurnBoundary::EndOfCurrentTurn,
    ] {
        let mut state = priority_state();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SetAutoPass {
                mode: AutoPassRequest::UntilTurnBoundary { until },
            },
        )
        .unwrap();
        assert_eq!(
            state.auto_pass.get(&PlayerId(0)),
            Some(&AutoPassMode::UntilTurnBoundary { until }),
            "SetAutoPass must store the requested boundary {until:?}"
        );
    }
}

#[test]
fn declare_attackers_accepts_turn_boundary_auto_pass_but_rejects_stack_empty() {
    let waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: Vec::new(),
        valid_attack_targets: Vec::new(),
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    let mut state = priority_state();
    state.phase = Phase::DeclareAttackers;
    state.waiting_for = waiting_for;
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::DeclareAttackers, PhaseStopScope::AllTurns)],
    );

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .expect("turn-boundary auto-pass is valid at Declare Attackers");
    assert_eq!(
        state.auto_pass.get(&PlayerId(0)),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        })
    );
    assert!(matches!(
        result.waiting_for,
        WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            ..
        }
    ));
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::AttackersDeclared { .. })),
        "the phase stop must leave the attacker prompt unsubmitted"
    );

    let error = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect_err("UntilStackEmpty must not bypass attacker declaration");
    assert!(matches!(error, EngineError::ActionNotAllowed(_)));
}

#[test]
fn declare_blockers_accepts_turn_boundary_auto_pass_but_rejects_stack_empty() {
    let waiting_for = WaitingFor::DeclareBlockers {
        player: PlayerId(0),
        valid_blocker_ids: Vec::new(),
        valid_block_targets: Default::default(),
        block_requirements: Default::default(),
        blocker_constraints: Default::default(),
    };
    let mut state = priority_state();
    state.phase = Phase::DeclareBlockers;
    state.active_player = PlayerId(1);
    state.waiting_for = waiting_for;
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::DeclareBlockers, PhaseStopScope::AllTurns)],
    );

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .expect("turn-boundary auto-pass is valid at Declare Blockers");
    assert_eq!(
        state.auto_pass.get(&PlayerId(0)),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        })
    );
    assert!(matches!(
        result.waiting_for,
        WaitingFor::DeclareBlockers {
            player: PlayerId(0),
            ..
        }
    ));
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::BlockersDeclared { .. })),
        "the phase stop must leave the blocker prompt unsubmitted"
    );

    let error = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect_err("UntilStackEmpty must not bypass blocker declaration");
    assert!(matches!(error, EngineError::ActionNotAllowed(_)));
}

#[test]
fn turn_boundary_auto_pass_submits_a_legal_empty_attacker_declaration() {
    let mut state = priority_state();
    let attacker = add_untapped_creature(&mut state, PlayerId(0), 910);
    state.phase = Phase::DeclareAttackers;
    state.combat = Some(crate::game::combat::CombatState::default());
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![attacker],
        valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .expect("a legal empty attack declaration may be auto-submitted");

    assert!(!matches!(
        result.waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
}

#[test]
fn turn_boundary_auto_pass_does_not_bypass_must_attack() {
    let mut state = priority_state();
    let attacker = add_untapped_creature(&mut state, PlayerId(0), 911);
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(StaticMode::MustAttack).affected(TargetFilter::SelfRef));
    state.phase = Phase::DeclareAttackers;
    state.combat = Some(crate::game::combat::CombatState::default());
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![attacker],
        valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .expect("the preference itself is valid at Declare Attackers");

    assert!(matches!(
        result.waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
    assert_eq!(
        state.auto_pass.get(&PlayerId(0)),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        }),
        "an unsatisfied must-attack requirement leaves the requested session armed"
    );
}

fn blockers_declaration_state(must_block: bool) -> GameState {
    let mut state = priority_state();
    let attacker = add_untapped_creature(&mut state, PlayerId(1), 912);
    let blocker = add_untapped_creature(&mut state, PlayerId(0), 913);
    if must_block {
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustBlock).affected(TargetFilter::SelfRef));
    }
    state.phase = Phase::DeclareBlockers;
    state.active_player = PlayerId(1);
    state.combat = Some(crate::game::combat::CombatState {
        attackers: vec![crate::game::combat::AttackerInfo::attacking_player(
            attacker,
            PlayerId(0),
        )],
        ..Default::default()
    });
    state.waiting_for = WaitingFor::DeclareBlockers {
        player: PlayerId(0),
        valid_blocker_ids: vec![blocker],
        valid_block_targets: [(blocker, vec![attacker])].into_iter().collect(),
        block_requirements: Default::default(),
        blocker_constraints: Default::default(),
    };
    state
}

/// CR 509.1a: A defender may choose which creatures, if any, will block.
/// A turn-boundary shortcut cannot make that optional choice on the defender's
/// behalf, regardless of which boundary was requested.
#[test]
fn turn_boundary_auto_pass_retains_optional_blocker_declaration_for_both_boundaries() {
    for until in [
        TurnBoundary::EndOfCurrentTurn,
        TurnBoundary::MyNextTurnStart,
    ] {
        let mut state = blockers_declaration_state(false);
        let (blocker, attacker) = match &state.waiting_for {
            WaitingFor::DeclareBlockers {
                valid_blocker_ids,
                valid_block_targets,
                ..
            } => {
                let blocker = *valid_blocker_ids
                    .first()
                    .expect("fixture supplies one legal blocker");
                let attacker = *valid_block_targets
                    .get(&blocker)
                    .and_then(|targets| targets.first())
                    .expect("fixture supplies one legal block target");
                (blocker, attacker)
            }
            waiting_for => panic!("expected DeclareBlockers fixture, got {waiting_for:?}"),
        };

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SetAutoPass {
                mode: AutoPassRequest::UntilTurnBoundary { until },
            },
        )
        .expect("turn-boundary auto-pass is a valid standing preference");

        assert_eq!(
            state.auto_pass.get(&PlayerId(0)),
            Some(&AutoPassMode::UntilTurnBoundary { until }),
            "the selected boundary remains stored while the defender decides"
        );
        assert!(matches!(
            result.waiting_for,
            WaitingFor::DeclareBlockers {
                player: PlayerId(0),
                ..
            }
        ));
        assert!(
            !result
                .events
                .iter()
                .any(|event| matches!(event, GameEvent::BlockersDeclared { .. })),
            "the preference must not submit the optional declaration"
        );

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::DeclareBlockers {
                assignments: vec![(blocker, attacker)],
            },
        )
        .expect("the defender can still submit an actual legal block");

        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::BlockersDeclared { assignments }
                if assignments == &vec![(blocker, attacker)]
        )));
    }
}

#[test]
fn no_legal_blockers_auto_submit_without_a_turn_boundary_preference() {
    let mut state = priority_state();
    let attacker = add_untapped_creature(&mut state, PlayerId(1), 914);
    state.phase = Phase::DeclareBlockers;
    state.active_player = PlayerId(1);
    state.combat = Some(crate::game::combat::CombatState {
        attackers: vec![crate::game::combat::AttackerInfo::attacking_player(
            attacker,
            PlayerId(0),
        )],
        ..Default::default()
    });
    state.waiting_for = WaitingFor::DeclareBlockers {
        player: PlayerId(0),
        valid_blocker_ids: Vec::new(),
        valid_block_targets: Default::default(),
        block_requirements: Default::default(),
        blocker_constraints: Default::default(),
    };
    let waiting_for = state.waiting_for.clone();
    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for,
        log_entries: Vec::new(),
    };

    assert!(run_auto_pass_loop(&mut state, &mut result));
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::BlockersDeclared { assignments } if assignments.is_empty()
    )));
    assert!(!matches!(
        result.waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
}

#[test]
fn blockers_auto_submit_when_all_attackers_left_play() {
    let mut state = blockers_declaration_state(false);
    let attacker = match &state.waiting_for {
        WaitingFor::DeclareBlockers {
            valid_blocker_ids,
            valid_block_targets,
            ..
        } => {
            let blocker = *valid_blocker_ids
                .first()
                .expect("fixture supplies one legal blocker");
            *valid_block_targets
                .get(&blocker)
                .and_then(|targets| targets.first())
                .expect("fixture supplies one legal block target")
        }
        waiting_for => panic!("expected DeclareBlockers fixture, got {waiting_for:?}"),
    };
    state.objects.get_mut(&attacker).unwrap().zone = Zone::Graveyard;
    let waiting_for = state.waiting_for.clone();
    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for,
        log_entries: Vec::new(),
    };

    assert!(run_auto_pass_loop(&mut state, &mut result));
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::BlockersDeclared { assignments } if assignments.is_empty()
    )));
    assert!(!matches!(
        result.waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
}

#[test]
fn turn_boundary_auto_pass_does_not_bypass_must_block() {
    let mut state = blockers_declaration_state(true);

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .expect("the preference itself is valid at Declare Blockers");

    assert!(matches!(
        result.waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
    assert_eq!(
        state.auto_pass.get(&PlayerId(0)),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        }),
        "an unsatisfied must-block requirement leaves the requested session armed"
    );
}

fn push_simple_stack_entry(state: &mut GameState, id: u64, controller: PlayerId) {
    state.stack.push_back(StackEntry {
        id: ObjectId(id),
        source_id: ObjectId(id),
        controller,
        kind: StackEntryKind::KeywordAction {
            action: KeywordAction::Crew {
                vehicle_id: ObjectId(id),
                paid_creature_ids: Vec::new(),
            },
        },
    });
}

fn draw_ability(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        source_id,
        controller,
    )
}

fn add_non_mana_activated_artifact(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let object_id = create_object(
        state,
        CardId(900),
        controller,
        "Priority Action".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&object_id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ));
    object_id
}

fn add_basic_mana_land(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let object_id = create_object(
        state,
        CardId(901),
        controller,
        "Forest".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&object_id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.subtypes.push("Forest".to_string());
    object_id
}

fn push_spell(state: &mut GameState, id: ObjectId, controller: PlayerId, ability: ResolvedAbility) {
    state.stack.push_back(StackEntry {
        id,
        source_id: id,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(id.0),
            ability: Some(Box::new(ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
}

fn insect_token_effect() -> Effect {
    Effect::Token {
        name: "Insect".to_string(),
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        types: vec!["Creature".to_string()],
        colors: vec![ManaColor::Green],
        keywords: vec![],
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

fn push_natural_token_batch(
    state: &mut GameState,
    source_id: ObjectId,
    first_entry_id: u64,
    count: u64,
) {
    for entry_id in first_entry_id..first_entry_id + count {
        state.stack.push_back(StackEntry {
            id: ObjectId(entry_id),
            source_id,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id,
                ability: Box::new(ResolvedAbility::new(
                    insect_token_effect(),
                    Vec::new(),
                    source_id,
                    PlayerId(0),
                )),
                condition: None,
                trigger_event: None,
                description: Some("Landfall".to_string()),
                source_name: "Batch source".to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });
    }
}

#[test]
fn exit_when_no_auto_pass_set() {
    let state = GameState::default();
    assert!(matches!(
        priority_auto_pass_decision(&state, PlayerId(0)),
        AutoPassDecision::Exit
    ));
}

#[test]
fn until_end_of_turn_passes_through_empty_stack_without_phase_stop() {
    let mut state = GameState {
        phase: Phase::PostCombatMain,
        ..GameState::default()
    };
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    assert!(is_pass(&priority_auto_pass_decision(&state, PlayerId(0))));
}

#[test]
fn until_end_of_turn_breaks_on_unyielded_opponent_stack_activity() {
    // Opponent spell/trigger on top must interrupt auto-pass so the player
    // always gets a chance to respond.
    let mut state = GameState::default();
    state.stack.push_back(stack_entry(PlayerId(1)));
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    assert!(is_break(&priority_auto_pass_decision(&state, PlayerId(0))));
    assert!(
        state.auto_pass.contains_key(&PlayerId(0)),
        "the decision itself must leave the turn-boundary session armed"
    );
}

#[test]
fn turn_boundary_session_resumes_after_opponent_stack_entry_resolves() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 7_000, PlayerId(1));
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );

    let mut paused = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    let advanced_before_response = run_auto_pass_loop(&mut state, &mut paused);

    assert!(
        !advanced_before_response,
        "reach guard: the unyielded opponent entry stops this run before priority passes"
    );
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert_eq!(state.stack.len(), 1);
    assert!(
        state.auto_pass.contains_key(&PlayerId(0)),
        "the interrupted turn-boundary session remains armed while the player responds"
    );

    let after_local_pass = apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    assert!(matches!(
        after_local_pass.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));

    let after_resolution = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    assert!(
        after_resolution
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::StackResolved { .. })),
        "reach guard: the opponent entry resolved through the production priority pipeline"
    );
    assert!(state.stack.is_empty());
    assert!(matches!(
        after_resolution.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
    assert!(
        state.auto_pass.contains_key(&PlayerId(0)),
        "the ordinary post-resolution action boundary re-enters auto-pass without clearing the session"
    );
}

#[test]
fn until_end_of_turn_passes_through_own_stack_activity() {
    // MTGA-style: resolve your own spells without pausing.
    let mut state = GameState::default();
    state.stack.push_back(stack_entry(PlayerId(0)));
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    assert!(is_pass(&priority_auto_pass_decision(&state, PlayerId(0))));
}

#[test]
fn until_end_of_turn_finishes_at_configured_phase_stop() {
    // User-flagged phase stop halts auto-pass even when the stack is empty
    // and no opponent action has interrupted.
    let mut state = GameState {
        phase: Phase::DeclareBlockers,
        ..GameState::default()
    };
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::DeclareBlockers, PhaseStopScope::AllTurns)],
    );
    assert!(is_finish(&priority_auto_pass_decision(&state, PlayerId(0))));
}

/// CR 507.2 + CR 117.3c: A beginning-of-combat phase stop interrupts an
/// `UntilTurnBoundary` shortcut at a usable priority window. The non-mana
/// activation proves this is a real priority window, not merely a rendered
/// phase marker.
#[test]
fn begin_combat_phase_stop_interrupts_auto_pass_with_usable_priority() {
    let mut state = priority_state();
    let artifact = add_non_mana_activated_artifact(&mut state, PlayerId(0));
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::BeginCombat, PhaseStopScope::OwnTurn)],
    );

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let at_begin_combat = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert_eq!(state.phase, Phase::BeginCombat);
    assert!(matches!(
        at_begin_combat.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert!(
        !state.auto_pass.contains_key(&PlayerId(0)),
        "the explicit stop must interrupt the standing auto-pass session"
    );

    let activated = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: artifact,
            ability_index: 0,
        },
    )
    .expect("a non-mana activated ability is legal in the stopped BeginCombat window");
    assert_eq!(state.stack.len(), 1);
    assert!(matches!(
        activated.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

/// V8: the per-window interrupt logic is boundary-agnostic. A
/// `MyNextTurnStart` session must Pass/Break/Finish in exactly the same windows as
/// the `EndOfCurrentTurn` sessions above (empty stack → Pass, opponent stack →
/// Break, phase stop → Finish). This composes with CR 117.3d yield handling
/// (unchanged) and guards against the decision arm ever branching on `until`.
#[test]
fn my_next_turn_start_window_behavior_matches_end_of_current_turn() {
    let mode = AutoPassMode::UntilTurnBoundary {
        until: TurnBoundary::MyNextTurnStart,
    };

    // Empty stack, no phase stop → Pass.
    let mut empty = GameState {
        phase: Phase::PostCombatMain,
        ..GameState::default()
    };
    empty.auto_pass.insert(PlayerId(0), mode);
    assert!(is_pass(&priority_auto_pass_decision(&empty, PlayerId(0))));

    // Opponent-controlled top-of-stack → Break.
    let mut opp = GameState::default();
    opp.stack.push_back(stack_entry(PlayerId(1)));
    opp.auto_pass.insert(PlayerId(0), mode);
    assert!(is_break(&priority_auto_pass_decision(&opp, PlayerId(0))));

    // User-flagged phase stop → Finish.
    let mut stopped = GameState {
        phase: Phase::DeclareBlockers,
        ..GameState::default()
    };
    stopped.auto_pass.insert(PlayerId(0), mode);
    stopped.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::DeclareBlockers, PhaseStopScope::AllTurns)],
    );
    assert!(is_finish(&priority_auto_pass_decision(
        &stopped,
        PlayerId(0)
    )));
}

#[test]
fn until_end_of_turn_scope_gates_session_owner_auto_pass() {
    // The session owner's own-turn stop fires only when they are the active
    // player; an opponents'-turns stop fires only when they are NOT. This
    // proves scope gates the engine.rs:580 session-owner auto-pass site
    // against the live active_player (CR 102.1).
    let base = |active: PlayerId, scope: PhaseStopScope| {
        let mut state = GameState {
            phase: Phase::DeclareBlockers,
            active_player: active,
            ..GameState::default()
        };
        state.auto_pass.insert(
            PlayerId(0),
            AutoPassMode::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        );
        state
            .phase_stops
            .insert(PlayerId(0), vec![stop(Phase::DeclareBlockers, scope)]);
        state
    };

    // OpponentsTurns stop, active player is the opponent → finishes.
    let opp_turn = base(PlayerId(1), PhaseStopScope::OpponentsTurns);
    assert!(is_finish(&priority_auto_pass_decision(
        &opp_turn,
        PlayerId(0)
    )));

    // OwnTurn stop, but active player is the opponent → does NOT finish (passes).
    let own_on_opp_turn = base(PlayerId(1), PhaseStopScope::OwnTurn);
    assert!(is_pass(&priority_auto_pass_decision(
        &own_on_opp_turn,
        PlayerId(0)
    )));
}

#[test]
fn phase_stop_hit_reads_per_player_preferences() {
    // active_player defaults to PlayerId(0) → PlayerId(0)'s own turn.
    let mut state = GameState {
        phase: Phase::DeclareBlockers,
        ..GameState::default()
    };
    // No entry for the player → no stop.
    assert!(!state.phase_stop_hit(PlayerId(0)));

    // Unrelated phase in the list → no stop.
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::Upkeep, PhaseStopScope::AllTurns)],
    );
    assert!(!state.phase_stop_hit(PlayerId(0)));

    // Current phase in the list → stop.
    state.phase_stops.insert(
        PlayerId(0),
        vec![
            stop(Phase::Upkeep, PhaseStopScope::AllTurns),
            stop(Phase::DeclareBlockers, PhaseStopScope::AllTurns),
        ],
    );
    assert!(state.phase_stop_hit(PlayerId(0)));

    // Per-player: player 1's stops don't bleed into player 0.
    state.phase_stops.remove(&PlayerId(0));
    state.phase_stops.insert(
        PlayerId(1),
        vec![stop(Phase::DeclareBlockers, PhaseStopScope::AllTurns)],
    );
    assert!(!state.phase_stop_hit(PlayerId(0)));
    assert!(state.phase_stop_hit(PlayerId(1)));
}

#[test]
fn phase_stop_hit_scope_resolves_against_active_player() {
    // 3 scopes × 2 turn-directions, resolved against the live active_player
    // (CR 102.1). Owner is PlayerId(0).
    let build = |active: PlayerId, scope: PhaseStopScope| {
        let mut state = GameState {
            phase: Phase::DeclareBlockers,
            active_player: active,
            ..GameState::default()
        };
        state
            .phase_stops
            .insert(PlayerId(0), vec![stop(Phase::DeclareBlockers, scope)]);
        state
    };

    // AllTurns: fires regardless of whose turn it is.
    assert!(build(PlayerId(0), PhaseStopScope::AllTurns).phase_stop_hit(PlayerId(0)));
    assert!(build(PlayerId(1), PhaseStopScope::AllTurns).phase_stop_hit(PlayerId(0)));

    // OwnTurn: fires iff active_player == owner.
    assert!(build(PlayerId(0), PhaseStopScope::OwnTurn).phase_stop_hit(PlayerId(0)));
    assert!(!build(PlayerId(1), PhaseStopScope::OwnTurn).phase_stop_hit(PlayerId(0)));

    // OpponentsTurns: fires iff active_player != owner.
    assert!(!build(PlayerId(0), PhaseStopScope::OpponentsTurns).phase_stop_hit(PlayerId(0)));
    assert!(build(PlayerId(1), PhaseStopScope::OpponentsTurns).phase_stop_hit(PlayerId(0)));
}

#[test]
fn phase_stop_hit_is_independent_of_auto_pass_mode() {
    // Phase stops apply even without an active auto-pass session —
    // this is what closes the "no legal blockers auto-submitted
    // regardless of preference" gap.
    let mut state = GameState {
        phase: Phase::DeclareBlockers,
        ..GameState::default()
    };
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::DeclareBlockers, PhaseStopScope::AllTurns)],
    );
    assert!(state.phase_stop_hit(PlayerId(0)));
    assert!(!end_of_turn_active(&state, PlayerId(0)));
}

#[test]
fn declare_blockers_opponents_turns_stop_pauses_empty_blocker_submit() {
    // Matrix row 6: owner = defender P0; the attacker P1 is the active player.
    // An OpponentsTurns stop on Declare Blockers fires (owner != active_player),
    // so the engine must NOT auto-submit the empty blocker declaration — the
    // defender keeps the instant / Ninjutsu window (CR 102.1 live compare).
    let waiting_for = WaitingFor::DeclareBlockers {
        player: PlayerId(0),
        valid_blocker_ids: vec![],
        valid_block_targets: Default::default(),
        block_requirements: Default::default(),
        blocker_constraints: Default::default(),
    };
    let mut state = GameState {
        phase: Phase::DeclareBlockers,
        active_player: PlayerId(1),
        waiting_for: waiting_for.clone(),
        ..GameState::default()
    };
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::DeclareBlockers, PhaseStopScope::OpponentsTurns)],
    );
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for,
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::DeclareBlockers {
                player: PlayerId(0),
                ..
            }
        ),
        "OpponentsTurns stop fires on the attacker's turn → the empty-blocker \
         auto-submit is paused"
    );
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::BlockersDeclared { .. })),
        "the phase stop must not submit an empty blocker declaration"
    );
}

#[test]
fn declare_blockers_own_turn_stop_does_not_pause_on_opponents_turn() {
    // Matrix row 6 revert-discriminator: an OwnTurn stop does NOT fire on the
    // opponent's turn (owner P0 defender, active_player P1 attacker), so the
    // empty blocker declaration auto-submits and combat advances past the step.
    // Pre-scope code (`stops.contains(&phase)`) would have paused here — this
    // assertion flips if the scope fix is reverted.
    let mut state = GameState {
        phase: Phase::DeclareBlockers,
        active_player: PlayerId(1),
        ..GameState::default()
    };
    // Minimal combat: P1's creature attacks P0, no blockers declared yet.
    let attacker = create_object(
        &mut state,
        CardId(950),
        PlayerId(1),
        "Attacker".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    state.combat = Some(crate::game::combat::CombatState {
        attackers: vec![crate::game::combat::AttackerInfo::attacking_player(
            attacker,
            PlayerId(0),
        )],
        ..Default::default()
    });

    let waiting_for = WaitingFor::DeclareBlockers {
        player: PlayerId(0),
        valid_blocker_ids: vec![],
        valid_block_targets: Default::default(),
        block_requirements: Default::default(),
        blocker_constraints: Default::default(),
    };
    state.waiting_for = waiting_for.clone();
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::DeclareBlockers, PhaseStopScope::OwnTurn)],
    );

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for,
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(
        !matches!(result.waiting_for, WaitingFor::DeclareBlockers { .. }),
        "OwnTurn stop must not fire on the opponent's turn; empty blockers \
         auto-submit and combat advances past Declare Blockers"
    );
}

#[test]
fn declare_attackers_own_turn_stop_pauses_empty_attacker_submit() {
    // Matrix row 7: owner = active player P0 declaring attackers on their own
    // turn (CR 508.1). An OwnTurn stop on Declare Attackers fires (owner ==
    // active_player), so the engine must NOT auto-submit the empty attacker
    // declaration even with an armed UntilTurnBoundary session — the player keeps
    // the step to attack (CR 102.1 live compare).
    let waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![],
        valid_attack_targets: vec![],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    let mut state = GameState {
        phase: Phase::DeclareAttackers,
        active_player: PlayerId(0),
        waiting_for: waiting_for.clone(),
        ..GameState::default()
    };
    // Reach-guard: with the session armed, the empty-attacker arm would fire
    // (`end_of_turn_active` is true) absent the stop, so the pause is
    // attributable to the phase stop rather than a missing auto-pass session.
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(Phase::DeclareAttackers, PhaseStopScope::OwnTurn)],
    );

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for,
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::DeclareAttackers {
                player: PlayerId(0),
                ..
            }
        ),
        "OwnTurn stop fires on the owner's own turn → the empty-attacker \
         auto-submit is paused"
    );
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::AttackersDeclared { .. })),
        "the phase stop must not submit an empty attacker declaration"
    );
}

#[test]
fn declare_attackers_opponents_turns_stop_does_not_pause_on_own_turn() {
    // Matrix row 7 revert-discriminator: an OpponentsTurns stop does NOT fire on
    // the owner's own turn (owner == active_player P0), so the armed session
    // auto-submits the empty attacker declaration and combat advances past
    // Declare Attackers. Pre-scope code (`stops.contains(&phase)`) would have
    // paused here — this assertion flips if the scope fix is reverted.
    let waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![],
        valid_attack_targets: vec![],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    let mut state = GameState {
        phase: Phase::DeclareAttackers,
        active_player: PlayerId(0),
        waiting_for: waiting_for.clone(),
        ..GameState::default()
    };
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    state.phase_stops.insert(
        PlayerId(0),
        vec![stop(
            Phase::DeclareAttackers,
            PhaseStopScope::OpponentsTurns,
        )],
    );

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for,
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(
        !matches!(result.waiting_for, WaitingFor::DeclareAttackers { .. }),
        "OpponentsTurns stop must not fire on the owner's own turn; empty \
         attackers auto-submit and combat advances past Declare Attackers"
    );
}

#[test]
fn until_stack_empty_resolves_large_stack_in_one_apply() {
    let mut state = priority_state();
    for idx in 0..264 {
        push_simple_stack_entry(&mut state, 10_000 + idx, PlayerId(0));
    }

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();

    assert!(state.stack.is_empty());
    assert!(!state.auto_pass.contains_key(&PlayerId(0)));
    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
            .count(),
        264
    );
}

#[test]
fn direct_until_stack_empty_installs_a_fenced_session_before_auto_resolving() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 19_900, PlayerId(0));
    add_non_mana_activated_artifact(&mut state, PlayerId(1));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();

    let session = state
        .stack_resolution_session
        .as_ref()
        .expect("a nonempty direct UntilStackEmpty request installs a session");
    assert_eq!(session.cursor, 0);
    assert_eq!(session.entries.len(), 1);
    assert_eq!(
        session.representatives,
        [PlayerId(0)].into_iter().collect(),
        "the direct request stores the semantic priority representative"
    );
    assert_eq!(session.policy, StackResolutionPolicy::Committed);
    assert_eq!(
        session.budget,
        crate::types::game_state::StackResolutionBudget::Unlimited
    );
    assert!(matches!(
        state.auto_pass.get(&PlayerId(0)),
        Some(AutoPassMode::UntilStackEmpty {
            policy: StackResolutionPolicy::Committed,
            ..
        })
    ));
}

#[test]
fn fenced_session_stops_before_passing_when_the_top_entry_changes() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 19_901, PlayerId(0));
    let priority_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));
    state.auto_pass.insert(
        PlayerId(1),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    state.objects.remove(&priority_action);
    state.stack.back_mut().unwrap().source_id = ObjectId(19_902);

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(
        state.stack_resolution_session.is_none(),
        "a changed top fence tears down the authorization"
    );
    assert!(
        result.events.is_empty(),
        "the changed entry was never passed or resolved"
    );
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
    assert_eq!(
        state.auto_pass.get(&PlayerId(1)),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        }),
        "teardown restores the complete pre-overlay preference map"
    );
    assert!(
        !state.auto_pass.contains_key(&PlayerId(0)),
        "the temporary representative overlay is removed with the session"
    );
}

#[test]
fn fenced_session_with_an_empty_stack_preserves_the_current_priority_window() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 19_902, PlayerId(0));
    let priority_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));
    let phase = state.phase;

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    state.objects.remove(&priority_action);
    state.stack.clear();

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(state.stack_resolution_session.is_none());
    assert!(result.events.is_empty());
    assert_eq!(
        state.phase, phase,
        "empty-session teardown must not advance a phase"
    );
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
}

#[test]
fn fenced_session_with_a_new_top_entry_preserves_the_current_priority_window() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 19_903, PlayerId(0));
    let priority_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));
    let phase = state.phase;

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    state.objects.remove(&priority_action);
    push_simple_stack_entry(&mut state, 19_904, PlayerId(1));

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(state.stack_resolution_session.is_none());
    assert!(
        result.events.is_empty(),
        "the new entry is not auto-passed or resolved"
    );
    assert_eq!(state.stack.back().unwrap().id, ObjectId(19_904));
    assert_eq!(state.phase, phase);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
}

#[test]
fn fenced_session_budget_caps_the_resolver_before_a_natural_batch_can_escape() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 19_903, PlayerId(0));
    push_simple_stack_entry(&mut state, 19_904, PlayerId(0));
    let priority_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    state.stack_resolution_session.as_mut().unwrap().budget =
        crate::types::game_state::StackResolutionBudget::from_legacy_max_resolutions(1);
    state.objects.remove(&priority_action);

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert_eq!(state.stack.len(), 1, "the one-entry budget is exact");
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
            .count(),
        1
    );
    assert!(state.stack_resolution_session.is_none());
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

#[test]
fn fenced_session_caps_a_natural_token_batch_at_its_matching_prefix() {
    let mut state = priority_state();
    let source_id = create_object(
        &mut state,
        CardId(19_911),
        PlayerId(0),
        "Batch source".to_string(),
        Zone::Battlefield,
    );
    push_natural_token_batch(&mut state, source_id, 19_912, 3);
    let priority_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    state.objects.remove(&priority_action);
    if let StackEntryKind::TriggeredAbility { source_name, .. } =
        &mut state.stack.get_mut(0).unwrap().kind
    {
        *source_name = "Changed captured provenance".to_string();
    }

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert_eq!(
        state.stack.len(),
        1,
        "the third natural batch member remains frozen out"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
            .count(),
        2,
        "the resolver receives the matching two-entry fence prefix, not its natural three-entry batch"
    );
    assert_eq!(
        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token)
            .count(),
        2,
        "the true token batch ran, but its execution was capped at the authorized prefix"
    );
    assert!(state.stack_resolution_session.is_none());
}

#[test]
fn fenced_session_stops_after_the_matching_top_when_a_lower_entry_changes() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 19_905, PlayerId(0));
    push_simple_stack_entry(&mut state, 19_906, PlayerId(0));
    push_simple_stack_entry(&mut state, 19_907, PlayerId(0));
    let priority_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    state.objects.remove(&priority_action);
    state.stack.get_mut(1).unwrap().source_id = ObjectId(29_906);

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
            .count(),
        1,
        "the still-matching top reaches the ordinary resolver"
    );
    assert_eq!(state.stack.len(), 2);
    assert_eq!(state.stack.back().unwrap().id, ObjectId(19_906));
    assert!(state.stack_resolution_session.is_none());
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

#[test]
fn nonrepresentative_set_auto_pass_survives_later_session_teardown() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 19_908, PlayerId(0));
    add_non_mana_activated_artifact(&mut state, PlayerId(1));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    assert!(
        state.stack_resolution_session.is_some(),
        "P1's action keeps the session paused"
    );

    apply(
        &mut state,
        PlayerId(1),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::MyNextTurnStart,
            },
        },
    )
    .unwrap();

    assert!(
        state.stack_resolution_session.is_none(),
        "the exhausted cohort tears down"
    );
    assert_eq!(
        state.auto_pass.get(&PlayerId(1)),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::MyNextTurnStart,
        }),
        "the nonrepresentative's accepted standing preference merged into the restore baseline"
    );
}

#[test]
fn nonrepresentative_cancel_does_not_resurrect_at_session_teardown() {
    let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 42);
    state.turn_number = 1;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    push_simple_stack_entry(&mut state, 19_908, PlayerId(0));
    add_non_mana_activated_artifact(&mut state, PlayerId(1));
    add_non_mana_activated_artifact(&mut state, PlayerId(2));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    apply(
        &mut state,
        PlayerId(1),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::MyNextTurnStart,
            },
        },
    )
    .unwrap();
    assert!(state.stack_resolution_session.is_some());
    assert!(state
        .stack_resolution_session
        .as_ref()
        .unwrap()
        .auto_pass_overlay
        .baseline
        .contains_key(&PlayerId(1)));

    apply(&mut state, PlayerId(1), GameAction::CancelAutoPass).unwrap();
    assert!(!state.auto_pass.contains_key(&PlayerId(1)));
    assert!(!state
        .stack_resolution_session
        .as_ref()
        .unwrap()
        .auto_pass_overlay
        .baseline
        .contains_key(&PlayerId(1)));

    // A fresh top invalidates P0's frozen cohort and exercises the ordinary
    // session teardown path after P1's out-of-turn preference cancellation.
    push_simple_stack_entry(&mut state, 19_909, PlayerId(2));
    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(state.stack_resolution_session.is_none());
    assert!(
        !state.auto_pass.contains_key(&PlayerId(1)),
        "teardown must not restore P1's cancelled preference"
    );
}

#[test]
fn nonrepresentative_deliberate_action_does_not_resurrect_at_session_teardown() {
    let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 42);
    state.turn_number = 1;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    push_simple_stack_entry(&mut state, 19_910, PlayerId(0));
    let p1_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));
    add_non_mana_activated_artifact(&mut state, PlayerId(2));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    apply(
        &mut state,
        PlayerId(1),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::MyNextTurnStart,
            },
        },
    )
    .unwrap();
    assert!(state.stack_resolution_session.is_some());

    // P2's meaningful action paused the session. Give P1 an ordinary priority
    // window, then activate its artifact through the production action route.
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };
    apply(
        &mut state,
        PlayerId(1),
        GameAction::ActivateAbility {
            source_id: p1_action,
            ability_index: 0,
        },
    )
    .unwrap();

    assert!(state.stack_resolution_session.is_none());
    assert!(
        !state.auto_pass.contains_key(&PlayerId(1)),
        "a deliberate action must not be undone by the session's baseline restore"
    );
}

/// A mana ability is an off-stack, deliberate action. The frozen session must
/// end before its reducer boundary re-enters the authorization runner:
/// resolving its captured top entry after this tap would turn the
/// representative's decision to act into an unwanted pass.
#[test]
fn representative_mana_action_ends_session_without_resolving_frozen_entry() {
    let mut state = priority_state();
    let frozen_entry_id = ObjectId(19_911);
    push_simple_stack_entry(&mut state, frozen_entry_id.0, PlayerId(0));
    let p1_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));
    let forest = add_basic_mana_land(&mut state, PlayerId(0));
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::MyNextTurnStart,
        },
    );

    // P1's actionable artifact pauses the newly installed P0 cohort before
    // any captured entry resolves.
    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    assert!(state.stack_resolution_session.is_some());
    assert_eq!(state.stack.back().unwrap().id, frozen_entry_id);

    // Give P0 a normal priority window with an otherwise inert opponent. The
    // mana action below is driven through the production reducer, not by
    // mutating the session or its cursor directly.
    state.objects.remove(&p1_action);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    let selection =
        crate::game::mana_sources::activatable_mana_source_selections(&state, PlayerId(0))
            .into_iter()
            .find(|selection| selection.source.object_id == forest)
            .expect("the basic land exposes its production mana action");

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::TapLandForMana { selection },
    )
    .unwrap();

    assert!(state.stack_resolution_session.is_none());
    assert_eq!(state.stack.back().unwrap().id, frozen_entry_id);
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::StackResolved { .. })),
        "the former cohort must not resolve after the representative acts"
    );
    assert!(
        !state.auto_pass.contains_key(&PlayerId(0)),
        "teardown must not resurrect P0's pre-overlay auto-pass preference"
    );
}

/// A turn controller's deliberate Priority action belongs to the controlled
/// semantic seat. The frozen session and its restored preference must therefore
/// be keyed by P0, rather than the authenticated controller P2.
#[test]
fn turn_controller_action_ends_controlled_representative_session_without_resolving() {
    let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 42);
    state.turn_number = 1;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.turn_decision_controller = Some(PlayerId(2));
    state.priority_player = PlayerId(2);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    let frozen_entry_id = ObjectId(19_912);
    push_simple_stack_entry(&mut state, frozen_entry_id.0, PlayerId(0));
    let p1_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));
    let forest = add_basic_mana_land(&mut state, PlayerId(0));
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::MyNextTurnStart,
        },
    );

    // P2 is the authenticated controller, but this installs P0's semantic
    // Priority preference and pauses at P1's meaningful response.
    apply(
        &mut state,
        PlayerId(2),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    assert!(state.stack_resolution_session.is_some());
    assert_eq!(state.stack.back().unwrap().id, frozen_entry_id);

    // Give the controlled P0 seat another normal Priority window with no
    // opponent response. Activating a mana ability is a deliberate, off-stack
    // action that remains legal while the frozen entry is on the stack.
    state.objects.remove(&p1_action);
    state.priority_player = PlayerId(2);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    let selection =
        crate::game::mana_sources::activatable_mana_source_selections(&state, PlayerId(0))
            .into_iter()
            .find(|selection| selection.source.object_id == forest)
            .expect("the basic land exposes its production mana action");
    let result = apply(
        &mut state,
        PlayerId(2),
        GameAction::TapLandForMana { selection },
    )
    .unwrap();

    assert!(state.stack_resolution_session.is_none());
    assert_eq!(state.stack.back().unwrap().id, frozen_entry_id);
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::StackResolved { .. })),
        "the former cohort must not resolve after P2 acts for P0"
    );
    assert!(
        !state.auto_pass.contains_key(&PlayerId(0)),
        "teardown must not resurrect P0's pre-overlay preference"
    );
}

#[test]
fn fenced_session_uses_the_captured_entry_when_its_source_id_is_reused() {
    let mut state = priority_state();
    let source_id = create_object(
        &mut state,
        CardId(19_909),
        PlayerId(0),
        "Original source".to_string(),
        Zone::Battlefield,
    );
    state.stack.push_back(StackEntry {
        id: ObjectId(19_910),
        source_id,
        controller: PlayerId(0),
        kind: StackEntryKind::ActivatedAbility {
            source_id,
            ability: Box::new(ResolvedAbility::new(
                Effect::NoOp,
                Vec::new(),
                source_id,
                PlayerId(0),
            )),
        },
    });
    let priority_action = add_non_mana_activated_artifact(&mut state, PlayerId(1));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    state.objects.remove(&priority_action);
    let mut reused_source = state
        .objects
        .remove(&source_id)
        .expect("the original source exists before reuse");
    reused_source.name = "Reused source id".to_string();
    state.objects.insert(source_id, reused_source);

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(state.stack.is_empty());
    assert!(result
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::StackResolved { .. })));
    assert!(state.stack_resolution_session.is_none());
}

#[test]
fn fenced_session_tears_down_when_resolution_ends_the_game() {
    let mut state = priority_state();
    state.players[1].life = 1;
    let ability = ResolvedAbility::new(
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: None,
        },
        vec![crate::types::ability::TargetRef::Player(PlayerId(1))],
        ObjectId(19_916),
        PlayerId(0),
    );
    state.stack.push_back(StackEntry {
        id: ObjectId(19_916),
        source_id: ObjectId(19_916),
        controller: PlayerId(0),
        kind: StackEntryKind::TriggeredAbility {
            source_id: ObjectId(19_916),
            ability: Box::new(ability),
            condition: None,
            trigger_event: None,
            description: Some("Lose the last life".to_string()),
            source_name: "Terminal trigger".to_string(),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        },
    });

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::GameOver {
            winner: Some(PlayerId(0))
        }
    ));
    assert!(state.stack_resolution_session.is_none());
    assert!(!state.auto_pass.contains_key(&PlayerId(0)));
}

#[test]
fn two_hg_session_uses_team_representatives_in_the_live_runner() {
    let mut state = GameState::new(
        crate::types::format::FormatConfig::two_headed_giant(),
        4,
        42,
    );
    state.turn_number = 1;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    push_simple_stack_entry(&mut state, 19_917, PlayerId(0));
    let priority_action = add_non_mana_activated_artifact(&mut state, PlayerId(2));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();
    assert_eq!(
        state
            .stack_resolution_session
            .as_ref()
            .unwrap()
            .representatives,
        [PlayerId(0)].into_iter().collect()
    );
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(2)
        }
    ));

    state.objects.remove(&priority_action);
    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert!(state.stack.is_empty());
    assert!(state.stack_resolution_session.is_none());
    assert!(result
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::StackResolved { .. })));
}

#[test]
fn until_stack_empty_stops_on_non_requester_meaningful_action() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 20_000, PlayerId(1));
    add_non_mana_activated_artifact(&mut state, PlayerId(1));

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();

    assert_eq!(state.stack.len(), 1);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
    assert!(
        state.auto_pass.contains_key(&PlayerId(0)),
        "requester's session stays active while waiting on opponent action"
    );
}

/// Item A (revert-failing perf): the auto-pass meaningful-action probe takes
/// the flat priority-action path, which skips the `legal_actions_full`
/// spell-cost object-walk entirely. Pre-fix the probe called
/// `legal_actions` → `legal_actions_full`, bumping the spell-cost sweep
/// counter once per probe; post-fix it does zero sweeps. The probe still
/// detects the meaningful activated ability (byte-identical verdict).
#[test]
fn priority_probe_skips_spell_cost_sweep() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 30_000, PlayerId(1));
    add_non_mana_activated_artifact(&mut state, PlayerId(0));

    crate::game::perf_counters::reset();
    let meaningful = priority_player_has_meaningful_action(&state);
    let snap = crate::game::perf_counters::snapshot();

    assert!(
        meaningful,
        "probe detects the castable Draw activation (verdict preserved)"
    );
    assert_eq!(
        snap.legal_actions_spell_cost_sweeps, 0,
        "flat probe path takes no spell-cost sweep (revert-failing: pre-fix = 1)"
    );
}

/// Item A behavior parity: with only `PassPriority` available the probe
/// reports no meaningful action, identical to pre-change.
#[test]
fn priority_probe_false_when_only_pass_available() {
    let state = priority_state();
    assert!(
        !priority_player_has_meaningful_action(&state),
        "an empty board with only PassPriority has no meaningful action"
    );
}

#[test]
fn verified_ai_pass_installs_rechecking_session_and_pauses_for_unverified_priority() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 30_101, PlayerId(1));
    let contract = AiDecisionContract::issue(&state, PlayerId(0));

    let result = apply_verified_ai_priority_pass(
        &mut state,
        PlayerId(0),
        &contract,
        GameAction::PassPriority,
    )
    .expect("the issued AI pass starts its fenced recheck session");

    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
    let session = state
        .stack_resolution_session
        .as_ref()
        .expect("an unverified follow-up priority window retains the AI session");
    assert_eq!(
        session.policy,
        StackResolutionPolicy::RecheckNoMeaningfulPriorityAction
    );
    assert_eq!(session.cursor, 0);
    assert!(session.representatives.contains(&PlayerId(0)));
}

#[test]
fn resolve_all_supersedes_a_rechecking_ai_session_and_retains_auto_pass_baseline() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 30_109, PlayerId(1));
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };
    state.priority_player = PlayerId(1);
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );

    let contract = AiDecisionContract::issue(&state, PlayerId(1));
    apply_verified_ai_priority_pass(&mut state, PlayerId(1), &contract, GameAction::PassPriority)
        .expect("the AI pass installs its provisional recheck session");
    assert!(matches!(
        state
            .stack_resolution_session
            .as_ref()
            .map(|session| session.policy),
        Some(StackResolutionPolicy::RecheckNoMeaningfulPriorityAction)
    ));

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::BeginResolveAll {
            max_resolutions: 0,
            scope: ResolveAllScope::Shared,
        },
    )
    .expect("the priority holder may replace an AI recheck with Resolve All");

    assert!(matches!(
        result.waiting_for,
        WaitingFor::ResolveAllConsent {
            representative: PlayerId(1),
            ..
        }
    ));
    assert!(state.stack_resolution_session.is_none());
    assert!(matches!(
        state
            .resolve_all_consent_run
            .as_ref()
            .and_then(|run| run.auto_pass_baseline.as_ref())
            .and_then(|baseline| baseline.get(&PlayerId(0))),
        Some(AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn
        })
    ));
}

#[test]
fn verified_ai_pass_cache_never_passes_an_unverified_representative() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 30_110, PlayerId(1));
    push_simple_stack_entry(&mut state, 30_111, PlayerId(1));
    let contract = AiDecisionContract::issue(&state, PlayerId(0));

    crate::game::perf_counters::reset();
    apply_verified_ai_priority_pass(&mut state, PlayerId(0), &contract, GameAction::PassPriority)
        .expect("the issued pass starts the retained session");
    let counters = crate::game::perf_counters::snapshot();

    assert_eq!(
        counters.priority_cast_probe_builds, 0,
        "the cache must avoid a synthetic probe before the next verified pass"
    );
    assert_eq!(
        counters.priority_cast_probe_state_clones, 0,
        "the cache must not clone state for a synthetic priority probe"
    );
    assert!(
        state.stack_resolution_session.is_some(),
        "the fenced cohort remains available for an unverified representative"
    );
    assert_eq!(state.stack.len(), 2, "no entry resolves without a new pass");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
    assert!(
        apply_verified_ai_priority_pass(
            &mut state,
            PlayerId(0),
            &contract,
            GameAction::PassPriority,
        )
        .is_err(),
        "the prior contract cannot authorize P1's distinct priority window"
    );
}

#[test]
fn stale_verified_ai_pass_does_not_install_a_session() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 30_102, PlayerId(1));
    let contract = AiDecisionContract::issue(&state, PlayerId(0));
    state.state_revision = state.state_revision.saturating_add(1);

    assert!(apply_verified_ai_priority_pass(
        &mut state,
        PlayerId(0),
        &contract,
        GameAction::PassPriority,
    )
    .is_err());
    assert!(
        state.stack_resolution_session.is_none(),
        "a stale AI contract must not promote an ordinary priority window"
    );
}

#[test]
fn verified_ai_pass_rejects_foreign_and_nonpass_submissions_without_mutation() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 30_105, PlayerId(1));
    let contract = AiDecisionContract::issue(&state, PlayerId(0));
    let before = state.clone();

    assert!(apply_verified_ai_priority_pass(
        &mut state,
        PlayerId(1),
        &contract,
        GameAction::PassPriority,
    )
    .is_err());
    assert_eq!(state, before, "a foreign actor must not install a session");

    assert!(apply_verified_ai_priority_pass(
        &mut state,
        PlayerId(0),
        &contract,
        GameAction::Concede {
            player_id: PlayerId(0)
        },
    )
    .is_err());
    assert_eq!(state, before, "a non-pass must not install a session");
}

#[test]
fn another_canonical_representative_can_continue_a_rechecking_session() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 30_106, PlayerId(1));
    push_simple_stack_entry(&mut state, 30_107, PlayerId(1));
    add_non_mana_activated_artifact(&mut state, PlayerId(0));
    add_non_mana_activated_artifact(&mut state, PlayerId(1));
    let first_contract = AiDecisionContract::issue(&state, PlayerId(0));

    apply_verified_ai_priority_pass(
        &mut state,
        PlayerId(0),
        &first_contract,
        GameAction::PassPriority,
    )
    .expect("the first AI representative starts the session");
    let second_contract = AiDecisionContract::issue(&state, PlayerId(1));

    let second_result = apply_verified_ai_priority_pass(
        &mut state,
        PlayerId(1),
        &second_contract,
        GameAction::PassPriority,
    )
    .expect("a second representative may supply its own fresh AI pass");

    assert!(
        state.stack_resolution_session.is_none(),
        "once every representative has a verified pass, the fenced cohort drains"
    );
    assert!(state.stack.is_empty());
    assert!(second_result.events.iter().any(|event| matches!(
        event,
        GameEvent::StackResolved {
            object_id: ObjectId(30_107)
        }
    )));
}

#[test]
fn explicit_pass_advances_the_recheck_session_cursor_at_one_resolution_boundary() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 30_103, PlayerId(1));
    push_simple_stack_entry(&mut state, 30_104, PlayerId(1));
    add_non_mana_activated_artifact(&mut state, PlayerId(0));
    add_non_mana_activated_artifact(&mut state, PlayerId(1));
    let baseline = state
        .auto_pass
        .iter()
        .map(|(&player, &mode)| (player, mode))
        .collect();
    install_stack_resolution_session(
        &mut state,
        [PlayerId(0)].into_iter().collect(),
        StackResolutionBudget::Unlimited,
        StackResolutionPolicy::RecheckNoMeaningfulPriorityAction,
        baseline,
    );

    apply(&mut state, PlayerId(0), GameAction::PassPriority)
        .expect("the first explicit pass changes priority");
    apply(&mut state, PlayerId(1), GameAction::PassPriority)
        .expect("the all-pass boundary resolves exactly one rechecked entry");

    let session = state
        .stack_resolution_session
        .as_ref()
        .expect("the next fenced entry remains available for a fresh AI recheck");
    assert_eq!(session.cursor, 1);
    assert_eq!(state.stack.len(), 1);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

#[test]
fn until_stack_empty_non_requester_own_stack_shortcut_does_not_hide_action() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 21_000, PlayerId(1));
    add_non_mana_activated_artifact(&mut state, PlayerId(1));
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };
    state.priority_player = PlayerId(1);
    state.auto_pass.insert(
        PlayerId(0),
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
            policy: StackResolutionPolicy::Committed,
        },
    );

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    run_auto_pass_loop(&mut state, &mut result);

    assert_eq!(state.stack.len(), 1);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
}

#[test]
fn until_stack_empty_stops_on_interactive_waiting_for() {
    let mut state = priority_state();
    let spell_id = create_object(
        &mut state,
        CardId(901),
        PlayerId(0),
        "Scry Spell".to_string(),
        Zone::Stack,
    );
    create_object(
        &mut state,
        CardId(902),
        PlayerId(0),
        "Library Card".to_string(),
        Zone::Library,
    );
    let ability = ResolvedAbility::new(
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        spell_id,
        PlayerId(0),
    );
    push_spell(&mut state, spell_id, PlayerId(0), ability);

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::ScryChoice {
            player: PlayerId(0),
            ..
        }
    ));
    assert!(state.stack_resolution_session.is_none());
    assert!(
        !state.auto_pass.contains_key(&PlayerId(0)),
        "the prompt tears down the temporary representative overlay"
    );
}

/// CR 732.2: the halt helper pauses a runaway cascade to a settled Priority
/// for the active player, emits exactly one `ResolutionHalted` carrying the
/// deduped+sorted stack-source ids, and resets consecutive-pass tracking.
#[test]
fn emit_resolution_halt_settles_priority_and_emits_event() {
    let mut state = priority_state();
    state.active_player = PlayerId(0);
    state.priority_passes.insert(PlayerId(1));
    // Two entries share source 7 (must dedup to one), one distinct source 3.
    for (entry_id, source) in [(1u64, 7u64), (2, 7), (3, 3)] {
        state.stack.push_back(StackEntry {
            id: ObjectId(entry_id),
            source_id: ObjectId(source),
            controller: PlayerId(0),
            kind: StackEntryKind::KeywordAction {
                action: KeywordAction::Crew {
                    vehicle_id: ObjectId(entry_id),
                    paid_creature_ids: Vec::new(),
                },
            },
        });
    }

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };
    emit_resolution_halt(&mut state, &mut result);

    // Settled to the active player's priority, pass-tracking reset.
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert_eq!(state.priority_player, PlayerId(0));
    assert!(state.priority_passes.is_empty());

    // Exactly one halt event, involved ids deduped (7 once) and sorted.
    let involved: Vec<Vec<ObjectId>> = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ResolutionHalted { involved } => Some(involved.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(involved.len(), 1);
    assert_eq!(involved[0], vec![ObjectId(3), ObjectId(7)]);
}

/// CR 732.2 regression: a large but TERMINATING stack must resolve fully
/// without tripping the runaway backstop — the growth ceilings are sized
/// far above honest wide play (a 264-deep stack is nowhere near them).
#[test]
fn large_terminating_stack_does_not_halt() {
    let mut state = priority_state();
    for idx in 0..264 {
        push_simple_stack_entry(&mut state, 30_000 + idx, PlayerId(0));
    }

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();

    assert!(state.stack.is_empty());
    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::ResolutionHalted { .. })),
        "a terminating stack must not trip the runaway-resolution backstop"
    );
}

#[test]
fn until_stack_empty_stops_on_stack_growth() {
    let mut state = priority_state();
    let copied_id = create_object(
        &mut state,
        CardId(903),
        PlayerId(0),
        "Copied Spell".to_string(),
        Zone::Stack,
    );
    push_spell(
        &mut state,
        copied_id,
        PlayerId(0),
        draw_ability(copied_id, PlayerId(0)),
    );
    let copy_id = create_object(
        &mut state,
        CardId(904),
        PlayerId(0),
        "Copy Spell".to_string(),
        Zone::Stack,
    );
    let copy_ability = ResolvedAbility::new(
        Effect::CopySpell {
            target: TargetFilter::Any,
            retarget: CopyRetargetPermission::KeepOriginalTargets,
            copier: None,
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
        },
        Vec::new(),
        copy_id,
        PlayerId(0),
    );
    push_spell(&mut state, copy_id, PlayerId(0), copy_ability);

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();

    assert_eq!(state.stack.len(), 2);
    assert!(!state.auto_pass.contains_key(&PlayerId(0)));
    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
}

#[test]
fn until_stack_empty_does_not_advance_phase_after_stack_empties() {
    let mut state = priority_state();
    push_simple_stack_entry(&mut state, 30_000, PlayerId(0));

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .unwrap();

    assert!(state.stack.is_empty());
    assert_eq!(state.phase, Phase::PreCombatMain);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

/// U-gate (CR 732.5): the loop-shortcut gate must probe EVERY living player,
/// not just the current priority holder. Here the NON-priority player P1 holds a
/// meaningful (non-mana activated) ability while the current holder P0 has none.
///
/// - `no_living_player_has_meaningful_priority_action` returns `false` (P1's
///   action blocks the shortcut) — correct.
/// - `priority_player_has_meaningful_action` (current holder P0 only) returns
///   `false`, so a gate built on its negation (`!current_only`) would wrongly be
///   `true` and clear the loop. That contrast proves the all-players
///   generalization is load-bearing (the session-masked victim need not hold
///   priority at the modulo-match iteration).
#[test]
fn loop_gate_probes_all_living_players_not_just_current_holder() {
    let mut state = priority_state();
    // P1 (NOT the current priority holder) has a meaningful action.
    add_non_mana_activated_artifact(&mut state, PlayerId(1));

    assert!(
        !no_living_player_has_meaningful_priority_action(&state),
        "P1 has a loop-ending action, so the all-players gate must refuse to clear"
    );
    assert!(
        !priority_player_has_meaningful_action(&state),
        "the current-holder-only check sees nothing for P0 — its negation would \
             wrongly clear, proving the all-players probe is load-bearing"
    );
}

/// CR 508.1a: "The active player chooses which creatures that they control, IF
/// ANY, will attack." When the candidate set is empty there is no choice to
/// make — the empty declaration is the only legal one — so the engine must
/// submit it rather than park on the prompt.
///
/// Regression: this arm previously auto-submitted ONLY when the player was in
/// `AutoPassMode::UntilTurnBoundary`. A player with no auto-pass configured and
/// no creatures therefore sat on a Declare Attackers prompt whose entire legal
/// action set was a single no-op `DeclareAttackers { attacks: [], bands: [] }`,
/// which had to be clicked through every combat. The `DeclareBlockers` arm has
/// always carried the equivalent "nothing to choose" escape; this pins the
/// attacker side to the same rule.
#[test]
fn empty_attacker_set_auto_submits_without_any_auto_pass_mode() {
    let waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: Vec::new(),
        valid_attack_targets: Vec::new(),
        valid_attack_targets_by_attacker: Some(Default::default()),
        attacker_constraints: Default::default(),
    };
    let mut state = GameState::new_two_player(42);
    state.phase = Phase::DeclareAttackers;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    // Production sets combat before advancing into the declare step.
    state.combat = Some(crate::game::combat::CombatState::default());
    state.waiting_for = waiting_for.clone();

    // The stalling configuration: no auto-pass mode for the declaring player.
    assert!(
        state.auto_pass.is_empty(),
        "fixture must exercise the no-auto-pass case that stalled"
    );

    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for,
        log_entries: Vec::new(),
    };
    let advanced = run_auto_pass_loop(&mut state, &mut result);

    assert!(
        advanced,
        "CR 508.1a: a forced empty attack declaration must not park the game"
    );
    assert!(
        !matches!(result.waiting_for, WaitingFor::DeclareAttackers { .. }),
        "CR 508.1a: the forced empty declaration must be submitted, not re-offered; got {:?}",
        result.waiting_for
    );
}
