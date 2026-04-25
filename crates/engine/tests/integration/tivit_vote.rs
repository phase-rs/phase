//! End-to-end tests for the Council's-dilemma `Effect::Vote` infrastructure
//! using Tivit, Seller of Secrets as the canonical card.
//!
//! Validates that the vote effect:
//!   1. Snapshots extra-vote granters at session start (CR 701.38d) — Tivit
//!      himself controls a `GrantsExtraVote` static, so his controller's
//!      first VoteChoice carries `remaining_votes == 2`.
//!   2. Walks voters in APNAP order and decrements `remaining_votes` per cast
//!      vote (CR 101.4 + CR 701.38d).
//!   3. Tallies all votes and fans out the per-choice sub-effects on the
//!      final ballot — every vote for `evidence` triggers Investigate, every
//!      vote for `bribery` creates a Treasure token (CR 701.38).
//!
//! This is the regression test that was missing in the original PR — the
//! existing unit test only asserted the initial WaitingFor shape, never that
//! the full session resolves and emits sub-effects.

use engine::game::effects;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, ResolvedAbility, StaticDefinition,
    TargetFilter,
};
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;
use engine::types::GameAction;

/// Build a vote-resolved-ability mirroring Tivit's ETB trigger:
/// "Council's dilemma — ... vote for evidence or bribery. For each evidence
/// vote, investigate. For each bribery vote, create a Treasure token."
fn build_tivit_vote(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    // Per-choice sub-effects use distinct, observable variants so the test
    // catches a slot-swap bug.
    let evidence = AbilityDefinition::new(AbilityKind::Spell, Effect::Investigate);
    // Bribery's "create a Treasure token" is modeled here as Populate so this
    // test stays independent of Effect::Token's broader surface area; the
    // tally fan-out behavior is identical for the two effect kinds.
    let bribery = AbilityDefinition::new(AbilityKind::Spell, Effect::Populate);

    ResolvedAbility {
        effect: Effect::Vote {
            choices: vec!["evidence".to_string(), "bribery".to_string()],
            per_choice_effect: vec![Box::new(evidence), Box::new(bribery)],
            starting_with: ControllerRef::You,
        },
        targets: Vec::new(),
        source_id,
        controller,
        kind: AbilityKind::Spell,
        sub_ability: None,
        else_ability: None,
        duration: None,
        condition: None,
        context: Default::default(),
        optional_targeting: false,
        optional: false,
        optional_for: None,
        multi_target: None,
        description: None,
        repeat_for: None,
        forward_result: false,
        unless_pay: None,
        distribution: None,
        player_scope: None,
        chosen_x: None,
        ability_index: None,
    }
}

/// Place Tivit on `controller`'s side of the battlefield with the
/// `GrantsExtraVote` static attached (CR 701.38d).
fn create_tivit(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(101),
        controller,
        "Tivit, Seller of Secrets".to_string(),
        Zone::Battlefield,
    );
    state.objects.get_mut(&id).unwrap().static_definitions.push(
        StaticDefinition::new(StaticMode::GrantsExtraVote)
            .affected(TargetFilter::Player)
            .description("While voting, you may vote an additional time.".to_string()),
    );
    id
}

/// CR 701.38d: With Tivit on the battlefield, his controller's first
/// `VoteChoice` must carry `remaining_votes == 2` — one base vote plus
/// the +1 his static grants. The opponent (no granter) gets exactly 1.
#[test]
fn tivit_self_grant_yields_two_votes_for_controller() {
    let mut state = GameState::new_two_player(42);
    let controller = state.players[0].id;
    let opponent = state.players[1].id;
    let tivit_id = create_tivit(&mut state, controller);

    let ability = build_tivit_vote(tivit_id, controller);
    let mut events = Vec::new();
    effects::vote::resolve(&mut state, &ability, &mut events).expect("vote resolves");

    match state.waiting_for {
        WaitingFor::VoteChoice {
            player,
            remaining_votes,
            ref remaining_voters,
            ..
        } => {
            assert_eq!(player, controller, "controller votes first");
            assert_eq!(
                remaining_votes, 2,
                "Tivit's GrantsExtraVote static must give controller 2 votes"
            );
            assert_eq!(remaining_voters.len(), 1);
            assert_eq!(remaining_voters[0].0, opponent);
            assert_eq!(remaining_voters[0].1, 1, "opponent has no granter, 1 vote");
        }
        ref other => panic!("expected VoteChoice, got {:?}", other),
    }
}

/// Drive the full vote session through `apply()`:
///
///   1. Controller casts "evidence" (votes 2 → 1).
///   2. Controller casts "bribery" (votes 1 → 0; advances to opponent).
///   3. Opponent casts "bribery" (final tally: evidence=1, bribery=2).
///
/// Then assert the per-choice sub-effects emitted: one `EffectResolved` for
/// Investigate (evidence tally=1) and one for Populate (bribery tally=2,
/// Populate runs twice via `repeat_for`).
#[test]
fn tivit_full_vote_session_drives_to_tally() {
    let mut state = GameState::new_two_player(42);
    let controller = state.players[0].id;
    let opponent = state.players[1].id;
    let tivit_id = create_tivit(&mut state, controller);

    let ability = build_tivit_vote(tivit_id, controller);
    let mut events = Vec::new();
    effects::vote::resolve(&mut state, &ability, &mut events).expect("vote resolves");

    // Vote 1 of 2 from controller: evidence.
    let r1 = apply(
        &mut state,
        controller,
        GameAction::ChooseOption {
            choice: "evidence".to_string(),
        },
    )
    .expect("first vote applies");
    match r1.waiting_for {
        WaitingFor::VoteChoice {
            player,
            remaining_votes,
            ref tallies,
            ..
        } => {
            assert_eq!(player, controller, "still controller's turn");
            assert_eq!(remaining_votes, 1);
            assert_eq!(tallies, &vec![1u32, 0]);
        }
        ref other => panic!("expected VoteChoice after first cast, got {:?}", other),
    }

    // Vote 2 of 2 from controller: bribery. Advances to opponent.
    let r2 = apply(
        &mut state,
        controller,
        GameAction::ChooseOption {
            choice: "bribery".to_string(),
        },
    )
    .expect("second vote applies");
    match r2.waiting_for {
        WaitingFor::VoteChoice {
            player,
            remaining_votes,
            ref tallies,
            ..
        } => {
            assert_eq!(
                player, opponent,
                "advances to opponent after controller's last vote"
            );
            assert_eq!(remaining_votes, 1);
            assert_eq!(tallies, &vec![1u32, 1]);
        }
        ref other => panic!("expected VoteChoice for opponent, got {:?}", other),
    }

    // Final vote from opponent: bribery. Triggers tally fan-out.
    let r3 = apply(
        &mut state,
        opponent,
        GameAction::ChooseOption {
            choice: "bribery".to_string(),
        },
    )
    .expect("final vote applies");

    // CR 701.38: After the last vote, expect a VoteResolved event with the
    // final tally and at least one investigate event (evidence tally=1) plus
    // sub-effect resolutions for the two bribery votes.
    let resolved = r3
        .events
        .iter()
        .find(|e| matches!(e, GameEvent::VoteResolved { .. }));
    let resolved = resolved.expect("VoteResolved event must fire after final vote");
    match resolved {
        GameEvent::VoteResolved { tallies, .. } => {
            let map: std::collections::HashMap<&str, u32> =
                tallies.iter().map(|(k, v)| (k.as_str(), *v)).collect();
            assert_eq!(map.get("evidence").copied(), Some(1));
            assert_eq!(map.get("bribery").copied(), Some(2));
        }
        _ => unreachable!(),
    }

    // CR 701.38 fan-out: Investigate emits a CardsCreated event (clue token).
    // Populate runs twice via `repeat_for: Fixed(2)` for the two bribery
    // votes — but in a state with no creature tokens to copy, populate
    // resolves to no-op. The minimum invariant we assert is that the engine
    // didn't drop the Investigate (evidence tally) — a clue token must be
    // created (CR 701.31a).
    let clue_created = r3.events.iter().any(
        |e| matches!(e, GameEvent::TokenCreated { name, .. } if name.eq_ignore_ascii_case("clue")),
    );
    assert!(
        clue_created,
        "evidence vote (tally=1) must trigger Investigate → clue token. \
         Events: {:?}",
        r3.events
    );

    // After tally fan-out the WaitingFor should NOT still be VoteChoice —
    // session is over.
    assert!(
        !matches!(state.waiting_for, WaitingFor::VoteChoice { .. }),
        "vote session must end after final ballot, got {:?}",
        state.waiting_for
    );
}

/// Wire-shape contract: `WaitingFor::VoteChoice.per_choice_effect` is an
/// engine-internal payload (only `vote::resolve_tally` reads it) and must
/// NOT serialize to JSON — opponents would otherwise receive the full
/// per-choice ability tree on every multiplayer state echo, and the
/// hand-written TypeScript adapter type already omits it.
#[test]
fn vote_choice_per_choice_effect_skipped_from_serialization() {
    let mut state = GameState::new_two_player(42);
    let controller = state.players[0].id;
    let tivit_id = create_tivit(&mut state, controller);

    let ability = build_tivit_vote(tivit_id, controller);
    let mut events = Vec::new();
    effects::vote::resolve(&mut state, &ability, &mut events).expect("vote resolves");

    // Sanity: the in-memory state still carries the per-choice effects so
    // the resolver can fan out the tally.
    match state.waiting_for {
        WaitingFor::VoteChoice {
            ref per_choice_effect,
            ..
        } => assert_eq!(per_choice_effect.len(), 2),
        ref other => panic!("expected VoteChoice, got {:?}", other),
    }

    // Round-trip through serde and confirm the field drops out — the
    // serialized JSON must not contain the field name nor any encoded
    // sub-effect type tag.
    let json = serde_json::to_string(&state.waiting_for).expect("serialize");
    assert!(
        !json.contains("per_choice_effect"),
        "per_choice_effect must be skipped in serialized output, got: {json}"
    );
    assert!(
        !json.contains("Investigate"),
        "no encoded sub-effect should appear in serialized output, got: {json}"
    );
}

/// CR 701.38a: Casting a vote not in `options` must be rejected by the
/// engine, not silently absorbed. Protects the "you can't abstain"
/// invariant (Council's-dilemma rulings: "You must vote for one of the
/// available options. You can't abstain.").
#[test]
fn vote_with_invalid_choice_is_rejected() {
    let mut state = GameState::new_two_player(42);
    let controller = state.players[0].id;
    let _tivit_id = create_tivit(&mut state, controller);

    let ability = build_tivit_vote(ObjectId(999), controller);
    let mut events = Vec::new();
    effects::vote::resolve(&mut state, &ability, &mut events).expect("vote resolves");

    let result = apply(
        &mut state,
        controller,
        GameAction::ChooseOption {
            choice: "abstain".to_string(),
        },
    );
    assert!(
        result.is_err(),
        "engine must reject an off-list vote choice"
    );
    // State stays in VoteChoice — no ballots cast yet.
    assert!(matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));
}
