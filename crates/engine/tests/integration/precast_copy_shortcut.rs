//! Production-path regression coverage for the finite, pre-cast Witherbloom
//! Apprentice + Chain of Smog shortcut.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::engine::apply;
use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{add_to_zone, create_object, remove_from_zone};
use engine::game::{filter_state_for_viewer, normalize_untrusted_restore};
use engine::types::ability::{
    CardSelectionMode, ContinuousModification, CopyRetargetPermission, Effect, QuantityExpr,
    QuantityModification, ReplacementDefinition, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::{GameAction, PrecastCopyShortcutResponse};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, GameState, TrustedGameStateEnvelope, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const CHAIN_OF_SMOG: &str =
    "Target player discards two cards. That player may copy this spell and may choose a new target for that copy.";

fn witherbloom_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        CardDatabase::from_mtgjson(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json"),
        )
        .expect("parser fixture must contain Witherbloom Apprentice")
    })
}

fn setup_shortcut(player_count: u8) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(player_count, 4_242);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 20);
    scenario.add_real_card(
        P0,
        "Witherbloom Apprentice",
        Zone::Battlefield,
        witherbloom_db(),
    );
    let chain = scenario
        .add_spell_to_hand_from_oracle(P0, "Chain of Smog", false, CHAIN_OF_SMOG)
        .id();
    let reserve_chain = scenario
        .add_spell_to_library_top(P0, "Chain of Smog", false)
        .from_oracle_text(CHAIN_OF_SMOG)
        .id();
    scenario.add_bolt_to_hand(P1);
    (scenario.build(), chain, reserve_chain)
}

fn cast_to_offer(runner: &mut GameRunner, chain: ObjectId) -> u64 {
    let card_id = runner.state().objects[&chain].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: chain,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Chain through the normal pipeline");

    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P0)),
                })
                .expect("target Chain at its caster"),
            WaitingFor::PrecastCopyShortcutOffer { epoch, .. } => return epoch,
            other => panic!("expected target selection or pre-cast offer, got {other:?}"),
        };
    }
    panic!("cast pipeline did not reach the pre-cast offer")
}

fn cast_to_target_selection(runner: &mut GameRunner, chain: ObjectId) {
    let card_id = runner.state().objects[&chain].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: chain,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Chain through the normal pipeline");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
}

fn add_copy_count_replacement(
    runner: &mut GameRunner,
    controller: PlayerId,
    zone: Zone,
    modification: Option<QuantityModification>,
) {
    let state = runner.state_mut();
    let staff_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let mut staff = GameObject::new(
        staff_id,
        CardId(staff_id.0),
        controller,
        "Twinning Staff".to_string(),
        zone,
    );
    let definition = ReplacementDefinition::new(ReplacementEvent::CopySpell);
    staff.replacement_definitions = vec![match modification {
        Some(modification) => definition.quantity_modification(modification),
        None => definition,
    }]
    .into();
    state.objects.insert(staff_id, staff);
    if zone == Zone::Battlefield {
        state.battlefield.push_back(staff_id);
    }
}

fn add_twinning_staff_replacement(runner: &mut GameRunner) {
    add_copy_count_replacement(
        runner,
        P0,
        Zone::Battlefield,
        Some(QuantityModification::Plus { value: 1 }),
    );
}

fn assert_ordinary_priority_without_offer(runner: &mut GameRunner, chain: ObjectId) {
    cast_to_target_selection(runner, chain);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("hostile route remains a normal cast");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::PrecastCopyShortcutOffer { .. }
                | WaitingFor::RespondToPrecastCopyShortcut { .. }
        ),
        "an unproven route must not enter a shortcut protocol wait"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("ordinary priority remains passable after the rejected route");
}

fn assert_noncanonical_chain_is_not_offered(mutator: impl FnOnce(&mut ResolvedAbility)) {
    let (mut runner, chain, _) = setup_shortcut(2);
    cast_to_target_selection(&mut runner, chain);
    let WaitingFor::TargetSelection { pending_cast, .. } = &mut runner.state_mut().waiting_for
    else {
        panic!("target selection retains the announced spell");
    };
    mutator(&mut pending_cast.ability);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("the mutated spell remains castable through target selection");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::PrecastCopyShortcutOffer { .. }
        ),
        "only the exact deterministic Chain route may receive an offer"
    );
}

fn resolve_declined_chain_to_empty_priority(runner: &mut GameRunner) {
    for _ in 0..32 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
        {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("normal priority pass while resolving declined Chain"),
            WaitingFor::DiscardChoice { cards, count, .. } => runner
                .act(GameAction::SelectCards {
                    cards: cards.iter().take(count).copied().collect(),
                })
                .expect("submit the only legal discard selection"),
            WaitingFor::OptionalEffectChoice { .. } => runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .expect("decline the ordinary Chain copy"),
            other => panic!("declined Chain reached an unexpected prompt: {other:?}"),
        };
    }
    panic!("declined Chain did not settle at ordinary P0 priority")
}

fn precast_shortcut_response_state() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::RespondToPrecastCopyShortcut {
        player: P1,
        epoch: 7,
        breakpoint_ids: vec![99],
        remaining_players: Vec::new(),
    };
    state
}

/// A responder with no meaningful priority action accepts the engine-proved
/// route instead of shortening merely because it has a breakpoint.
#[test]
fn precast_responder_candidate_accepts_without_meaningful_priority_action() {
    let state = precast_shortcut_response_state();

    assert!(engine::ai_support::candidate_actions(&state)
        .iter()
        .any(|candidate| {
            matches!(
                candidate.action,
                GameAction::PrecastCopyShortcut {
                    epoch: 7,
                    response: PrecastCopyShortcutResponse::Accept,
                }
            )
        }));
}

/// Only a responder with a meaningful action may shorten, and it may name only
/// the breakpoint issued to that responder.
#[test]
fn precast_responder_candidate_shortens_for_meaningful_priority_action() {
    let mut state = precast_shortcut_response_state();
    let land = create_object(
        &mut state,
        CardId(999),
        P1,
        "Forest".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&land)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    assert!(engine::ai_support::candidate_actions(&state)
        .iter()
        .any(|candidate| {
            matches!(
                candidate.action,
                GameAction::PrecastCopyShortcut {
                    epoch: 7,
                    response: PrecastCopyShortcutResponse::Shorten { breakpoint_id: 99 },
                }
            )
        }));
}

/// CR 117.3c: `run_post_action_pipeline_from` leaves the first post-cast
/// priority window with the caster, even during another player's turn.
#[test]
fn precast_offer_uses_nonactive_casters_semantic_priority_after_cast_triggers() {
    let (mut runner, chain, _) = setup_shortcut(2);
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state
            .objects
            .get_mut(&chain)
            .unwrap()
            .keywords
            .push(Keyword::Flash);
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }

    let epoch = cast_to_offer(&mut runner, chain);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PrecastCopyShortcutOffer { proposer, epoch: offered, .. }
            if proposer == P0 && offered == epoch
    ));
    assert_eq!(
        runner.state().stack.len(),
        2,
        "the Witherbloom cast trigger remains above the original Chain"
    );
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().priority_player, P0);
}

/// CR 117.3c + CR 723.5: P1 submits P0's controlled cast, but the semantic
/// offer owner remains P0.
#[test]
fn precast_offer_keeps_controlled_caster_as_semantic_owner() {
    let (mut runner, chain, _) = setup_shortcut(2);
    {
        let state = runner.state_mut();
        state.turn_decision_controller = Some(P1);
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }

    let epoch = cast_to_offer(&mut runner, chain);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PrecastCopyShortcutOffer { proposer, epoch: offered, .. }
            if proposer == P0 && offered == epoch
    ));
    assert_eq!(runner.state().priority_player, P1);
}

/// The authorized turn controller submits the controlled caster's proposal or
/// decline through the public action boundary; an unauthorized semantic
/// identity cannot bypass that authority.
#[test]
fn precast_controlled_caster_uses_authorized_submitter_for_protocol_actions() {
    let (mut proposed, chain, _) = setup_shortcut(2);
    {
        let state = proposed.state_mut();
        state.turn_decision_controller = Some(P1);
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }
    let epoch = cast_to_offer(&mut proposed, chain);

    apply(
        proposed.state_mut(),
        P1,
        GameAction::PrecastCopyShortcut {
            epoch,
            response: PrecastCopyShortcutResponse::Propose { route_id: epoch },
        },
    )
    .expect("the controller submits P0's proposal through the normal action boundary");
    assert!(matches!(
        proposed.state().waiting_for,
        WaitingFor::RespondToPrecastCopyShortcut { player, .. } if player == P1
    ));
    apply(
        proposed.state_mut(),
        P1,
        GameAction::PrecastCopyShortcut {
            epoch,
            response: PrecastCopyShortcutResponse::Accept,
        },
    )
    .expect("the responder's normal action-boundary response remains authorized");

    let (mut declined, chain, _) = setup_shortcut(2);
    {
        let state = declined.state_mut();
        state.turn_decision_controller = Some(P1);
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }
    let epoch = cast_to_offer(&mut declined, chain);
    let decline = GameAction::PrecastCopyShortcut {
        epoch,
        response: PrecastCopyShortcutResponse::Decline,
    };
    assert!(
        apply(declined.state_mut(), P0, decline.clone()).is_err(),
        "the controlled semantic owner cannot bypass its turn controller"
    );
    apply(declined.state_mut(), P1, decline)
        .expect("the controller submits P0's decline through the normal action boundary");
    assert!(matches!(
        declined.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(declined.state().priority_player, P1);
}

/// CR 732.2b-c: every responder answers after a shorten and the selected
/// prefix replays exactly once at the end.
#[test]
fn precast_three_seat_shorten_waits_for_every_responder_and_replays_once() {
    let (mut runner, chain, _) = setup_shortcut(3);
    let epoch = cast_to_offer(&mut runner, chain);
    runner
        .act(GameAction::PrecastCopyShortcut {
            epoch,
            response: PrecastCopyShortcutResponse::Propose { route_id: epoch },
        })
        .expect("proposer declares the issued route");

    let p1_breakpoint = match runner.state().waiting_for.clone() {
        WaitingFor::RespondToPrecastCopyShortcut {
            player,
            epoch: response_epoch,
            breakpoint_ids,
            remaining_players,
        } => {
            assert_eq!(player, P1);
            assert_eq!(response_epoch, epoch);
            assert_eq!(remaining_players, vec![P2]);
            assert_eq!(breakpoint_ids.len(), 1);
            breakpoint_ids[0]
        }
        other => panic!("expected P1 shortcut response, got {other:?}"),
    };

    assert!(runner
        .act(GameAction::PrecastCopyShortcut {
            epoch,
            response: PrecastCopyShortcutResponse::Shorten {
                breakpoint_id: p1_breakpoint.wrapping_add(10_000),
            },
        })
        .is_err());

    runner
        .act(GameAction::PrecastCopyShortcut {
            epoch,
            response: PrecastCopyShortcutResponse::Shorten {
                breakpoint_id: p1_breakpoint,
            },
        })
        .expect("P1 may select its own engine-issued breakpoint");
    let (p2_epoch, p2_breakpoint) = match runner.state().waiting_for.clone() {
        WaitingFor::RespondToPrecastCopyShortcut {
            player,
            epoch,
            breakpoint_ids,
            remaining_players,
        } => {
            assert_eq!(player, P2, "a shorter does not skip later responders");
            assert!(remaining_players.is_empty());
            assert_eq!(breakpoint_ids.len(), 1);
            (epoch, breakpoint_ids[0])
        }
        other => panic!("expected P2 response after P1 shortens, got {other:?}"),
    };
    assert!(runner
        .act(GameAction::PrecastCopyShortcut {
            epoch: p2_epoch,
            response: PrecastCopyShortcutResponse::Shorten {
                breakpoint_id: p1_breakpoint,
            },
        })
        .is_err());

    let result = runner
        .act(GameAction::PrecastCopyShortcut {
            epoch: p2_epoch,
            response: PrecastCopyShortcutResponse::Shorten {
                breakpoint_id: p2_breakpoint,
            },
        })
        .expect("the final responder may choose its own later boundary");
    assert!(result
        .events
        .iter()
        .all(|event| !matches!(event, GameEvent::SpellCopied { .. })));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    assert!(runner.state().priority_passes.contains(&P0));
    assert!(!runner.state().priority_passes.contains(&P1));
    assert_eq!(runner.state().stack.len(), 2);
    assert!(runner
        .act(GameAction::PrecastCopyShortcut {
            epoch: p2_epoch,
            response: PrecastCopyShortcutResponse::Accept,
        })
        .is_err());
}

/// CR 732.2c + CR 117.3d: manual and auto passes remain blocked until a real,
/// different action is accepted by the normal reducer.
#[test]
fn precast_shorten_requires_meaningful_divergence_before_manual_or_auto_pass() {
    let (mut runner, chain, _) = setup_shortcut(2);
    let bolt = runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P1)
        .and_then(|player| player.hand.front().copied())
        .expect("P1 has an alternate instant");
    let epoch = cast_to_offer(&mut runner, chain);
    runner
        .act(GameAction::PrecastCopyShortcut {
            epoch,
            response: PrecastCopyShortcutResponse::Propose { route_id: epoch },
        })
        .expect("declare shortcut");
    let breakpoint = match runner.state().waiting_for.clone() {
        WaitingFor::RespondToPrecastCopyShortcut { breakpoint_ids, .. } => breakpoint_ids[0],
        other => panic!("expected responder window, got {other:?}"),
    };
    runner
        .act(GameAction::PrecastCopyShortcut {
            epoch,
            response: PrecastCopyShortcutResponse::Shorten {
                breakpoint_id: breakpoint,
            },
        })
        .expect("shorten at the issued boundary");

    assert!(runner.act(GameAction::PassPriority).is_err());
    assert!(runner
        .act(GameAction::SetAutoPass {
            mode: engine::types::game_state::AutoPassRequest::UntilStackEmpty,
        })
        .is_err());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    runner
        .act(GameAction::CancelAutoPass)
        .expect("preference actions remain available at the shortened boundary");
    assert!(runner.act(GameAction::PassPriority).is_err());

    let card_id = runner.state().objects[&bolt].card_id;
    assert!(runner
        .act(GameAction::CastSpell {
            object_id: ObjectId(99_999),
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .is_err());
    assert!(
        runner.act(GameAction::PassPriority).is_err(),
        "a rejected cast must not discharge MustDiverge"
    );
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("a different instant is a meaningful divergence");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("complete the ordinary Bolt cast");
    runner
        .act(GameAction::PassPriority)
        .expect("the owner may pass after a meaningful divergence");
}

/// Trusted codec restores rekey opaque capabilities; raw/public state cannot
/// carry private transcript authority and therefore normalizes to priority.
#[test]
fn precast_epochs_rekey_at_trusted_restore_and_raw_public_state_fails_closed() {
    let (mut runner, chain, _) = setup_shortcut(2);
    let stale_epoch = cast_to_offer(&mut runner, chain);
    let raw_json = serde_json::to_value(runner.state()).expect("raw state serializes");
    assert!(raw_json.get("precast_shortcut_runtime").is_none());
    assert!(raw_json.get("route_id").is_none());
    assert!(raw_json.get("breakpoints").is_none());
    assert!(raw_json.get("transcript").is_none());
    assert!(
        serde_json::from_value::<TrustedGameStateEnvelope>(raw_json.clone()).is_err(),
        "a raw GameState must not be decoded as a trusted envelope"
    );

    let controller_view = filter_state_for_viewer(runner.state(), P0);
    let viewer_json = serde_json::to_value(controller_view).expect("viewer state serializes");
    assert!(viewer_json.get("precast_shortcut_runtime").is_none());
    assert!(viewer_json.get("route_id").is_none());

    let envelope_json =
        serde_json::to_string(&TrustedGameStateEnvelope::capture(runner.state().clone()))
            .expect("trusted state serializes");
    let restored: TrustedGameStateEnvelope =
        serde_json::from_str(&envelope_json).expect("trusted state decodes");
    let mut trusted = restored.into_game_state();
    let fresh_epoch = match trusted.waiting_for {
        WaitingFor::PrecastCopyShortcutOffer { epoch, .. } => epoch,
        ref other => panic!("trusted restore must reissue the offer, got {other:?}"),
    };
    assert_ne!(fresh_epoch, stale_epoch);
    assert!(apply(
        &mut trusted,
        P0,
        GameAction::PrecastCopyShortcut {
            epoch: stale_epoch,
            response: PrecastCopyShortcutResponse::Propose {
                route_id: stale_epoch,
            },
        },
    )
    .is_err());
    assert!(apply(
        &mut trusted,
        P0,
        GameAction::PrecastCopyShortcut {
            epoch: stale_epoch,
            response: PrecastCopyShortcutResponse::Decline,
        },
    )
    .is_err());

    apply(
        &mut trusted,
        P0,
        GameAction::PrecastCopyShortcut {
            epoch: fresh_epoch,
            response: PrecastCopyShortcutResponse::Propose {
                route_id: fresh_epoch,
            },
        },
    )
    .expect("fresh proposer capability remains valid");
    assert!(apply(
        &mut trusted,
        P1,
        GameAction::PrecastCopyShortcut {
            epoch: stale_epoch,
            response: PrecastCopyShortcutResponse::Accept,
        },
    )
    .is_err());

    let mut raw: engine::types::game_state::GameState =
        serde_json::from_value(raw_json).expect("raw public state decodes");
    normalize_untrusted_restore(&mut raw);
    assert!(matches!(raw.waiting_for, WaitingFor::Priority { player } if player == P0));
}

/// Raw state has no route authority. A response prompt returns control to its
/// semantic responder even when turn control authorized someone else to submit.
#[test]
fn raw_precast_response_normalizes_to_the_semantic_prompt_owner() {
    let (mut runner, chain, _) = setup_shortcut(2);
    {
        let state = runner.state_mut();
        state.turn_decision_controller = Some(P0);
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }
    let epoch = cast_to_offer(&mut runner, chain);
    runner
        .act(GameAction::PrecastCopyShortcut {
            epoch,
            response: PrecastCopyShortcutResponse::Propose { route_id: epoch },
        })
        .expect("the controlled proposer declares the route");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToPrecastCopyShortcut { player, .. } if player == P1
    ));
    assert_eq!(runner.state().priority_player, P1);

    let raw_json = serde_json::to_value(runner.state()).expect("raw response state serializes");
    let mut raw = serde_json::from_value(raw_json).expect("raw response state decodes");
    normalize_untrusted_restore(&mut raw);
    assert!(matches!(raw.waiting_for, WaitingFor::Priority { player } if player == P1));
}

/// The finite route is a strict proof over one exact Chain shape. Every
/// mutation below would add a choice, alter copied characteristics, or change
/// the route's deterministic continuation, so the offer must fail closed.
#[test]
fn precast_offer_rejects_noncanonical_chain_prompts_and_modifications() {
    assert_noncanonical_chain_is_not_offered(|chain| {
        let Effect::Discard { selection, .. } = &mut chain.effect else {
            panic!("fixture must start with Chain's discard effect");
        };
        *selection = CardSelectionMode::Random;
    });
    assert_noncanonical_chain_is_not_offered(|chain| {
        let Effect::Discard { count, .. } = &mut chain.effect else {
            panic!("fixture must start with Chain's discard effect");
        };
        *count = QuantityExpr::up_to(QuantityExpr::Fixed { value: 2 });
    });
    assert_noncanonical_chain_is_not_offered(|chain| {
        let Effect::Discard { unless_filter, .. } = &mut chain.effect else {
            panic!("fixture must start with Chain's discard effect");
        };
        *unless_filter = Some(TargetFilter::Controller);
    });
    assert_noncanonical_chain_is_not_offered(|chain| {
        let Effect::Discard { filter, .. } = &mut chain.effect else {
            panic!("fixture must start with Chain's discard effect");
        };
        *filter = Some(TargetFilter::Any);
    });
    assert_noncanonical_chain_is_not_offered(|chain| {
        chain.optional = true;
    });
    assert_noncanonical_chain_is_not_offered(|chain| {
        let copy = chain
            .sub_ability
            .as_mut()
            .expect("fixture must start with Chain's copy continuation");
        let Effect::CopySpell { retarget, .. } = &mut copy.effect else {
            panic!("fixture must start with Chain's copy effect");
        };
        *retarget = CopyRetargetPermission::KeepOriginalTargets;
    });
    assert_noncanonical_chain_is_not_offered(|chain| {
        let copy = chain
            .sub_ability
            .as_mut()
            .expect("fixture must start with Chain's copy continuation");
        let Effect::CopySpell {
            additional_modifications,
            ..
        } = &mut copy.effect
        else {
            panic!("fixture must start with Chain's copy effect");
        };
        additional_modifications.push(ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        });
    });
    assert_noncanonical_chain_is_not_offered(|chain| {
        let copy = chain
            .sub_ability
            .as_mut()
            .expect("fixture must start with Chain's copy continuation");
        copy.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            copy.source_id,
            copy.controller,
        )));
    });
}

/// An active Twinning Staff changes the number of copies, so the finite
/// one-copy transcript must never be offered.
#[test]
fn precast_offer_rejects_twinning_staff_copy_replacement_before_protocol_wait() {
    let (mut runner, chain, _) = setup_shortcut(2);
    add_twinning_staff_replacement(&mut runner);
    assert_ordinary_priority_without_offer(&mut runner, chain);
}

/// CR 614.1a: preflight uses the normal copy-count authority. A CopySpell
/// replacement can affect the finite route only when it functions for the
/// Chain copier and changes the count; an opponent's Staff, inert definition,
/// or off-zone source leaves the one-copy transcript intact.
#[test]
fn precast_offer_uses_effective_copy_count_replacements_only() {
    let cases = [
        (
            "opponent staff",
            P1,
            Zone::Battlefield,
            Some(QuantityModification::Plus { value: 1 }),
        ),
        ("inert definition", P0, Zone::Battlefield, None),
        (
            "prevent definition",
            P0,
            Zone::Battlefield,
            Some(QuantityModification::Prevent),
        ),
        (
            "off-zone staff",
            P0,
            Zone::Hand,
            Some(QuantityModification::Plus { value: 1 }),
        ),
    ];

    for (label, controller, zone, modification) in cases {
        let (mut runner, chain, _) = setup_shortcut(2);
        add_copy_count_replacement(&mut runner, controller, zone, modification);
        cast_to_offer(&mut runner, chain);
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::PrecastCopyShortcutOffer { proposer, .. } if proposer == P0
            ),
            "{label} must not block the one-copy shortcut route"
        );
    }
}

/// A second caster-controlled Magecraft trigger makes the post-cast transcript
/// non-canonical. It remains an ordinary post-cast priority sequence rather than
/// a shortcut wait that cannot be materialized.
#[test]
fn precast_offer_rejects_extra_cast_trigger_before_protocol_wait() {
    let mut scenario = GameScenario::new_n_player(2, 4_242);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(
        P0,
        "Witherbloom Apprentice",
        Zone::Battlefield,
        witherbloom_db(),
    );
    scenario.add_real_card(
        P0,
        "Witherbloom Apprentice",
        Zone::Battlefield,
        witherbloom_db(),
    );
    let chain = scenario
        .add_spell_to_hand_from_oracle(P0, "Chain of Smog", false, CHAIN_OF_SMOG)
        .id();
    let mut runner = scenario.build();

    cast_to_target_selection(&mut runner, chain);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("target the Chain at its caster");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(
        runner.state().stack.len(),
        3,
        "the ordinary stack contains Chain plus both Magecraft triggers"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::PrecastCopyShortcutOffer { .. }
                | WaitingFor::RespondToPrecastCopyShortcut { .. }
        ),
        "extra cast triggers must remain in the ordinary post-cast pipeline"
    );
}

/// A decline is scoped to the current stack object. A later Chain-shaped cast
/// goes through the normal cast pipeline and receives a fresh offer.
#[test]
fn precast_decline_suppresses_only_the_current_cast() {
    let (mut runner, chain, reserve_chain) = setup_shortcut(2);
    let epoch = cast_to_offer(&mut runner, chain);
    runner
        .act(GameAction::PrecastCopyShortcut {
            epoch,
            response: PrecastCopyShortcutResponse::Decline,
        })
        .expect("decline the current offer");
    runner
        .act(GameAction::PassPriority)
        .expect("immediate same-cast pass remains ordinary priority");
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::PrecastCopyShortcutOffer { .. }
    ));
    resolve_declined_chain_to_empty_priority(&mut runner);

    {
        let state = runner.state_mut();
        remove_from_zone(state, reserve_chain, Zone::Library, P0);
        add_to_zone(state, reserve_chain, Zone::Hand, P0);
        state.objects.get_mut(&reserve_chain).unwrap().zone = Zone::Hand;
    }
    let later_epoch = cast_to_offer(&mut runner, reserve_chain);
    assert_ne!(later_epoch, epoch);
}

/// `maybe_offer`: shared-team turns fail closed rather than assigning one
/// teammate's shortcut policy to the whole team.
#[test]
fn precast_shortcut_is_not_offered_in_shared_team_formats() {
    let (mut runner, chain, _) = setup_shortcut(4);
    runner.state_mut().format_config = FormatConfig::two_headed_giant();
    let card_id = runner.state().objects[&chain].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: chain,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Chain in a shared-team fixture");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("target Chain at P0");
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::PrecastCopyShortcutOffer { .. }
    ));
}
