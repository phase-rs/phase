//! Integration tests for Master of Ceremonies's `Effect::Vote` shape:
//! `EachOpponent` voter scope plus `PlayerFilter::VotedFor` per-choice
//! routing, AND end-to-end fan-out of the parser-distributed
//! "you and that player each Y" reward bodies.
//!
//! The full upkeep trigger reads:
//!
//! > At the beginning of your upkeep, each opponent chooses money, friends,
//! > or secrets. For each player who chose money, you and that player each
//! > create a Treasure token. For each player who chose friends, you and that
//! > player each create a 1/1 green and white Citizen creature token. For each
//! > player who chose secrets, you and that player each draw a card.
//!
//! These tests validate the engine round-trip for both halves of the feature:
//! the voter skeleton (voter queue scoping CR 800.4g, ballot recording
//! CR 608.2c, `WaitingFor::VoteChoice` advancement CR 101.4 + CR 701.38d) AND
//! the post-tally distribution (CR 109.5 — first half routes to the
//! original ability controller; CR 608.2c + CR 800.4g — second half routes
//! to the matching voter via `PlayerFilter::VotedFor` + `TargetFilter::ScopedPlayer`).
//!
//! Per-choice bodies are parsed from real Oracle text via
//! `parse_effect_chain`, then tagged with `PlayerFilter::VotedFor { idx }`.
//! The body parser's compound-subject distribution combinator
//! (`try_parse_compound_subject_each`) emits the 2-element chain whose
//! halves carry distinct recipient filters.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, PlayerFilter, ResolvedAbility,
    TargetFilter, VoteTally, VoterScope,
};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// CR 121.1 + CR 111.2: Stack each player's library with one stand-in card so
/// the per-choice draw and token-creation effects have something to act on.
/// Returns nothing — mutates `state` in place.
fn seed_libraries_and_battlefield(state: &mut GameState) {
    for (i, player_id) in state
        .players
        .iter()
        .map(|p| p.id)
        .collect::<Vec<_>>()
        .into_iter()
        .enumerate()
    {
        let card = create_object(
            state,
            CardId(1000 + i as u64),
            player_id,
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .players
            .iter_mut()
            .find(|p| p.id == player_id)
            .expect("player exists")
            .library
            .push_back(card);
    }
}

/// Count battlefield permanents owned by `player` whose name contains the
/// substring `name_contains` (case-insensitive). Used to detect Treasure /
/// Citizen tokens created by the per-choice fan-out.
fn count_battlefield_objects_named(
    state: &GameState,
    player: PlayerId,
    name_contains: &str,
) -> usize {
    let needle = name_contains.to_ascii_lowercase();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.owner == player)
        .filter(|obj| obj.name.to_ascii_lowercase().contains(&needle))
        .count()
}

/// CR 608.2c + CR 701.38: Build the per-choice `AbilityDefinition` for a
/// MoC reward — parse the body text via `parse_effect_chain` and tag the
/// resulting top-level def with `PlayerFilter::VotedFor { choice_index }`.
/// The compound-subject combinator inside the parser produces a 2-element
/// chain whose halves carry `OriginalController` / `ScopedPlayer` recipients,
/// so the per-voter iteration drives both halves correctly.
fn parse_moc_reward_body(body_text: &str, choice_index: u8) -> Box<AbilityDefinition> {
    let mut def = parse_effect_chain(body_text, AbilityKind::Spell);
    def.player_scope = Some(PlayerFilter::VotedFor { choice_index });
    Box::new(def)
}

/// Construct the Master of Ceremonies vote ability from real Oracle bodies:
/// "you and that player each create a Treasure token" (money), "...create a
/// 1/1 green and white Citizen creature token" (friends), "...draw a card"
/// (secrets). Each body is parsed via `parse_effect_chain` and tagged with
/// `PlayerFilter::VotedFor { idx }` so the runtime player_scope iteration
/// drives both halves of the distributed body per voter.
fn make_master_of_ceremonies_vote(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let vote_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Vote {
            choices: vec![
                "money".to_string(),
                "friends".to_string(),
                "secrets".to_string(),
            ],
            per_choice_effect: vec![
                parse_moc_reward_body("you and that player each create a Treasure token", 0),
                parse_moc_reward_body(
                    "you and that player each create a 1/1 green and white Citizen creature token",
                    1,
                ),
                parse_moc_reward_body("you and that player each draw a card", 2),
            ],
            starting_with: ControllerRef::You,
            voter_scope: VoterScope::EachOpponent,
            tally_mode: VoteTally::PerVote,
        },
    );
    build_resolved_from_def(&vote_def, source_id, controller)
}

fn make_threshold_vote(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let vote_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Vote {
            choices: vec!["innocent".to_string(), "guilty".to_string()],
            per_choice_effect: vec![
                Box::new(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
                Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::BecomeMonarch,
                )),
            ],
            starting_with: ControllerRef::You,
            voter_scope: VoterScope::AllPlayers,
            tally_mode: VoteTally::Threshold {
                tie_breaker_index: 0,
            },
        },
    );
    build_resolved_from_def(&vote_def, source_id, controller)
}

/// Pre-flight assertion: each per-choice body has been parsed into the
/// expected 2-element distributed shape — first half targets
/// `OriginalController`, second half targets `ScopedPlayer` via `sub_ability`.
/// This guards the post-tally delta tests against silent regressions in the
/// distribution combinator.
#[test]
fn moc_per_choice_bodies_parse_into_distributed_chain() {
    let bodies = [
        ("money", "you and that player each create a Treasure token"),
        (
            "friends",
            "you and that player each create a 1/1 green and white Citizen creature token",
        ),
        ("secrets", "you and that player each draw a card"),
    ];
    for (label, body) in bodies {
        let def = parse_effect_chain(body, AbilityKind::Spell);
        let top_target = match &*def.effect {
            Effect::Token { owner, .. } => owner.clone(),
            Effect::Draw { target, .. } => target.clone(),
            other => panic!("[{label}] unexpected top-level effect {:?}", other),
        };
        assert_eq!(
            top_target,
            TargetFilter::OriginalController,
            "[{label}] first half must target OriginalController"
        );
        let sub = def
            .sub_ability
            .as_ref()
            .unwrap_or_else(|| panic!("[{label}] expected second-half sub_ability"));
        let sub_target = match &*sub.effect {
            Effect::Token { owner, .. } => owner.clone(),
            Effect::Draw { target, .. } => target.clone(),
            other => panic!("[{label}] unexpected sub-effect {:?}", other),
        };
        assert_eq!(
            sub_target,
            TargetFilter::ScopedPlayer,
            "[{label}] second half must target ScopedPlayer"
        );
    }
}

/// CR 701.38a + CR 608.2c: Threshold vote mode must survive the real
/// `WaitingFor::VoteChoice` → `GameAction::ChooseOption` continuation path. The
/// 1-1 tie routes to index 0 (NoOp), so the controller must NOT become the
/// monarch. If `tally_mode` is dropped in `engine_resolution_choices`, this
/// regresses to per-vote fan-out and the guilty vote executes BecomeMonarch.
#[test]
fn threshold_vote_tie_breaker_survives_choose_option_path() {
    let mut state = GameState::new_two_player(77);
    let controller = state.players[0].id;
    let ability = make_threshold_vote(controller, ObjectId(9100));
    let mut events = Vec::new();

    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    let first_voter = match &state.waiting_for {
        WaitingFor::VoteChoice { player, .. } => *player,
        other => panic!("expected VoteChoice for first voter, got {other:?}"),
    };
    apply(
        &mut state,
        first_voter,
        GameAction::ChooseOption {
            choice: "innocent".to_string(),
        },
    )
    .expect("first ChooseOption must resolve");

    let second_voter = match &state.waiting_for {
        WaitingFor::VoteChoice { player, .. } => *player,
        other => panic!("expected VoteChoice for second voter, got {other:?}"),
    };
    apply(
        &mut state,
        second_voter,
        GameAction::ChooseOption {
            choice: "guilty".to_string(),
        },
    )
    .expect("second ChooseOption must resolve");

    assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));
    assert!(
        state.monarch.is_none(),
        "threshold tie-breaker NoOp must win; per-vote fan-out would make the controller monarch"
    );
}

/// CR 800.4g: In a 2-player game, the controller does NOT vote. The
/// opponent is the only voter; the `WaitingFor::VoteChoice` lands on them.
#[test]
fn master_of_ceremonies_two_player_only_opponent_votes() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let controller = state.players[0].id;
    let opponent = state.players[1].id;
    let ability = make_master_of_ceremonies_vote(controller, ObjectId(9000));

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    match &state.waiting_for {
        WaitingFor::VoteChoice {
            player,
            ref remaining_voters,
            ..
        } => {
            // The opponent is the first (and only) voter.
            assert_eq!(*player, opponent);
            // No further voters queued — controller does not vote.
            assert!(remaining_voters.is_empty());
        }
        other => panic!("expected VoteChoice for opponent, got {:?}", other),
    }
}

/// CR 800.4g: In a 3-player game, the two opponents form the voter queue
/// in APNAP order; the controller never appears.
///
/// CR 109.5 + CR 800.4g: This test additionally drives both opponents to vote
/// "secrets" and verifies the post-tally fan-out: 2 secrets votes → controller
/// draws 2 cards (one per voter iteration), and each voter draws 1 card
/// (their own ScopedPlayer half).
#[test]
fn master_of_ceremonies_apnap_order_with_three_opponents() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    seed_libraries_and_battlefield(&mut state);
    // Each player needs 2 cards in library since the controller will draw twice.
    for i in 0..3usize {
        let pid = state.players[i].id;
        let card = create_object(
            &mut state,
            CardId(2000 + i as u64),
            pid,
            "Forest".to_string(),
            Zone::Library,
        );
        state.players[i].library.push_back(card);
    }
    let controller = state.players[0].id;
    let ability = make_master_of_ceremonies_vote(controller, ObjectId(9000));

    let controller_hand_before = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .unwrap()
        .hand
        .len();

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    let first_voter;
    let second_voter_id;
    match &state.waiting_for {
        WaitingFor::VoteChoice {
            player,
            ref remaining_voters,
            ..
        } => {
            assert_ne!(*player, controller);
            // Two opponents total: one current, one queued.
            assert_eq!(remaining_voters.len(), 1);
            assert_ne!(remaining_voters[0].0, controller);
            assert_ne!(remaining_voters[0].0, *player);
            first_voter = *player;
            second_voter_id = remaining_voters[0].0;
        }
        other => panic!("expected VoteChoice, got {:?}", other),
    }

    // Both opponents vote "secrets".
    apply(
        &mut state,
        first_voter,
        GameAction::ChooseOption {
            choice: "secrets".to_string(),
        },
    )
    .unwrap();
    apply(
        &mut state,
        second_voter_id,
        GameAction::ChooseOption {
            choice: "secrets".to_string(),
        },
    )
    .unwrap();
    assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));

    // CR 109.5: Controller's "you" half fires once per matching voter — the
    // controller drew 2 cards (one for each secrets voter). CR 800.4g + CR
    // 608.2c: Each voter's "that player" half fires once for that voter only,
    // so each opponent drew exactly 1 card.
    let controller_hand_after = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .unwrap()
        .hand
        .len();
    assert_eq!(
        controller_hand_after - controller_hand_before,
        2,
        "controller must draw 2 cards — one per secrets voter"
    );
    let first_hand = state
        .players
        .iter()
        .find(|p| p.id == first_voter)
        .unwrap()
        .hand
        .len();
    let second_hand = state
        .players
        .iter()
        .find(|p| p.id == second_voter_id)
        .unwrap()
        .hand
        .len();
    assert_eq!(first_hand, 1, "first secrets voter must draw 1 card");
    assert_eq!(second_hand, 1, "second secrets voter must draw 1 card");
}

/// CR 800.4g: When every opponent has been eliminated, the `EachOpponent`
/// vote produces an empty queue. The resolver does NOT pause on
/// `WaitingFor::VoteChoice` — chain continues.
#[test]
fn master_of_ceremonies_no_opponents_remaining() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let controller = state.players[0].id;
    state.players[1].is_eliminated = true;
    let ability = make_master_of_ceremonies_vote(controller, ObjectId(9000));

    let initial_waiting_for = state.waiting_for.clone();
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // No VoteChoice was entered — waiting_for is unchanged.
    assert_eq!(state.waiting_for, initial_waiting_for);
}

/// CR 608.2c + CR 701.38 + CR 109.5: Casting a vote in a 2-player game
/// advances the queue, records the ballot, and resolves the per-choice
/// sub-effects. After the opponent votes "money", the parser-distributed
/// "you and that player each create a Treasure token" body must produce
/// EXACTLY ONE Treasure for the controller (CR 109.5: "you" is the printed
/// ability controller, fixed during the player_scope iteration) AND EXACTLY
/// ONE Treasure for the voting opponent (CR 800.4g: "that player" is the
/// iterated voter via `PlayerFilter::VotedFor` + `TargetFilter::ScopedPlayer`).
#[test]
fn master_of_ceremonies_money_path() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    seed_libraries_and_battlefield(&mut state);
    let controller = state.players[0].id;
    let opponent = state.players[1].id;
    let ability = make_master_of_ceremonies_vote(controller, ObjectId(9000));

    let controller_treasures_before =
        count_battlefield_objects_named(&state, controller, "treasure");
    let opponent_treasures_before = count_battlefield_objects_named(&state, opponent, "treasure");

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // Opponent votes for "money".
    apply(
        &mut state,
        opponent,
        GameAction::ChooseOption {
            choice: "money".to_string(),
        },
    )
    .unwrap();

    // The vote has resolved: WaitingFor is no longer VoteChoice.
    assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));

    let controller_treasures_after =
        count_battlefield_objects_named(&state, controller, "treasure");
    let opponent_treasures_after = count_battlefield_objects_named(&state, opponent, "treasure");

    assert_eq!(
        controller_treasures_after - controller_treasures_before,
        1,
        "controller (you) must own +1 Treasure token after the money tally"
    );
    assert_eq!(
        opponent_treasures_after - opponent_treasures_before,
        1,
        "opponent (that player) must own +1 Treasure token after the money tally"
    );
}

/// CR 608.2c + CR 701.38 + CR 109.5: "secrets" body parses to "you and that
/// player each draw a card". After the opponent votes secrets, both the
/// controller (you) AND the voting opponent must each draw exactly one
/// card. Hand size delta = +1 for each player.
#[test]
fn master_of_ceremonies_secrets_path() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    seed_libraries_and_battlefield(&mut state);
    let controller = state.players[0].id;
    let opponent = state.players[1].id;
    let ability = make_master_of_ceremonies_vote(controller, ObjectId(9000));

    let controller_hand_before = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .unwrap()
        .hand
        .len();
    let opponent_hand_before = state
        .players
        .iter()
        .find(|p| p.id == opponent)
        .unwrap()
        .hand
        .len();

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    apply(
        &mut state,
        opponent,
        GameAction::ChooseOption {
            choice: "secrets".to_string(),
        },
    )
    .unwrap();

    assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));

    let controller_hand_after = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .unwrap()
        .hand
        .len();
    let opponent_hand_after = state
        .players
        .iter()
        .find(|p| p.id == opponent)
        .unwrap()
        .hand
        .len();

    assert_eq!(
        controller_hand_after - controller_hand_before,
        1,
        "controller (you) must draw 1 card after the secrets tally"
    );
    assert_eq!(
        opponent_hand_after - opponent_hand_before,
        1,
        "opponent (that player) must draw 1 card after the secrets tally"
    );
}

/// CR 608.2c + CR 701.38 + CR 109.5: "friends" body parses to "you and that
/// player each create a 1/1 green and white Citizen creature token". After
/// the opponent votes friends, both controller and voting opponent must own
/// +1 Citizen token.
#[test]
fn master_of_ceremonies_friends_path() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    seed_libraries_and_battlefield(&mut state);
    let controller = state.players[0].id;
    let opponent = state.players[1].id;
    let ability = make_master_of_ceremonies_vote(controller, ObjectId(9000));

    let controller_citizens_before = count_battlefield_objects_named(&state, controller, "citizen");
    let opponent_citizens_before = count_battlefield_objects_named(&state, opponent, "citizen");

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    apply(
        &mut state,
        opponent,
        GameAction::ChooseOption {
            choice: "friends".to_string(),
        },
    )
    .unwrap();

    assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));

    let controller_citizens_after = count_battlefield_objects_named(&state, controller, "citizen");
    let opponent_citizens_after = count_battlefield_objects_named(&state, opponent, "citizen");

    assert_eq!(
        controller_citizens_after - controller_citizens_before,
        1,
        "controller (you) must own +1 Citizen token after the friends tally"
    );
    assert_eq!(
        opponent_citizens_after - opponent_citizens_before,
        1,
        "opponent (that player) must own +1 Citizen token after the friends tally"
    );
}

/// CR 701.38a: Regression — classic Council's-dilemma vote with default
/// `VoterScope::AllPlayers` continues to enqueue every player (including
/// the controller) and resolves identically to the pre-change shape. This
/// pins that the new `voter_scope` axis hasn't disturbed Tivit / Capital
/// Punishment / Coercive Portal behavior.
#[test]
fn tivit_evidence_bribery_still_resolves_via_default_voter_scope() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let controller = state.players[0].id;
    let opponent = state.players[1].id;

    let make_sub = || -> Box<AbilityDefinition> {
        Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Investigate,
        ))
    };
    let vote_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Vote {
            choices: vec!["evidence".to_string(), "bribery".to_string()],
            per_choice_effect: vec![make_sub(), make_sub()],
            starting_with: ControllerRef::You,
            // Default — this is the Tivit/classic-council shape.
            voter_scope: VoterScope::AllPlayers,
            tally_mode: VoteTally::PerVote,
        },
    );
    let ability = build_resolved_from_def(&vote_def, ObjectId(9001), controller);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // First voter is the controller (CR 701.38a: "starting with you").
    match &state.waiting_for {
        WaitingFor::VoteChoice {
            player,
            ref remaining_voters,
            ..
        } => {
            assert_eq!(*player, controller);
            assert_eq!(remaining_voters.len(), 1);
            assert_eq!(remaining_voters[0].0, opponent);
        }
        other => panic!("expected VoteChoice, got {:?}", other),
    }

    // Controller votes evidence.
    apply(
        &mut state,
        controller,
        GameAction::ChooseOption {
            choice: "evidence".to_string(),
        },
    )
    .unwrap();
    // Opponent votes bribery.
    apply(
        &mut state,
        opponent,
        GameAction::ChooseOption {
            choice: "bribery".to_string(),
        },
    )
    .unwrap();
    // Both votes have resolved.
    assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));
}

/// CR 608.2c + CR 701.38 + CR 109.5: 3-player split — opp1 votes "money",
/// opp2 votes "friends". After both votes resolve, the per-choice fan-out
/// must distribute correctly per voter:
///   * Money tally (opp1 voted): controller gets +1 Treasure, opp1 gets +1
///     Treasure, opp2 gets 0 Treasures.
///   * Friends tally (opp2 voted): controller gets +1 Citizen, opp2 gets +1
///     Citizen, opp1 gets 0 Citizens.
///   * No "secrets" votes were cast → no draws happen.
///
/// Net per-player deltas: controller {+1 Treasure, +1 Citizen}, opp1 {+1
/// Treasure}, opp2 {+1 Citizen}.
#[test]
fn master_of_ceremonies_three_player_split_choices() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    seed_libraries_and_battlefield(&mut state);
    let controller = state.players[0].id;
    let ability = make_master_of_ceremonies_vote(controller, ObjectId(9000));

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // First voter (APNAP order) votes for "money".
    let first_voter = match &state.waiting_for {
        WaitingFor::VoteChoice { player, .. } => *player,
        other => panic!("expected VoteChoice, got {:?}", other),
    };
    let controller_hand_before = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .unwrap()
        .hand
        .len();
    apply(
        &mut state,
        first_voter,
        GameAction::ChooseOption {
            choice: "money".to_string(),
        },
    )
    .unwrap();

    // After the first vote, we should still be in VoteChoice for the second voter.
    let second_voter = match &state.waiting_for {
        WaitingFor::VoteChoice { player, .. } => *player,
        other => panic!("expected VoteChoice for second voter, got {:?}", other),
    };
    assert_ne!(second_voter, first_voter);
    assert_ne!(second_voter, controller);
    apply(
        &mut state,
        second_voter,
        GameAction::ChooseOption {
            choice: "friends".to_string(),
        },
    )
    .unwrap();

    // The vote has resolved: WaitingFor is no longer VoteChoice.
    assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));

    // Treasure deltas: controller +1, the money-voter +1, the other voter 0.
    assert_eq!(
        count_battlefield_objects_named(&state, controller, "treasure"),
        1,
        "controller must own 1 Treasure (from the money tally)"
    );
    assert_eq!(
        count_battlefield_objects_named(&state, first_voter, "treasure"),
        1,
        "money voter must own 1 Treasure"
    );
    assert_eq!(
        count_battlefield_objects_named(&state, second_voter, "treasure"),
        0,
        "friends voter must own 0 Treasures"
    );

    // Citizen deltas: controller +1, the friends-voter +1, the other voter 0.
    assert_eq!(
        count_battlefield_objects_named(&state, controller, "citizen"),
        1,
        "controller must own 1 Citizen (from the friends tally)"
    );
    assert_eq!(
        count_battlefield_objects_named(&state, second_voter, "citizen"),
        1,
        "friends voter must own 1 Citizen"
    );
    assert_eq!(
        count_battlefield_objects_named(&state, first_voter, "citizen"),
        0,
        "money voter must own 0 Citizens"
    );

    // No secrets votes → no draws across the table. Hand size is unchanged
    // for the controller (who never votes under EachOpponent scope) and the
    // controller's pre-vote hand size matches the post-vote hand size.
    let controller_hand_after = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .unwrap()
        .hand
        .len();
    assert_eq!(
        controller_hand_after, controller_hand_before,
        "controller must not draw — no secrets votes were cast"
    );
}

/// CR 109.5 + CR 608.2c: The "you and that player each Y" distribution lives
/// in the body parser, not in the vote pipeline — so it must work in any
/// `player_scope`-driven context. This test wires the distributed body into
/// a synthetic `PlayerFilter::Opponent` ability (no Vote effect involved)
/// and verifies both halves fire correctly per opponent iteration.
///
/// In a 2-player game with the runtime player_scope iterating over the one
/// opponent: controller fires Half A (target = OriginalController), opponent
/// fires Half B (target = ScopedPlayer). Net deltas: controller +1 Treasure,
/// opponent +1 Treasure.
#[test]
fn you_and_that_player_each_distributes_in_non_vote_context() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    seed_libraries_and_battlefield(&mut state);
    let controller = state.players[0].id;
    let opponent = state.players[1].id;

    // Parse the distributed body once and tag it with PlayerFilter::Opponent.
    let mut def = parse_effect_chain(
        "you and that player each create a Treasure token",
        AbilityKind::Spell,
    );
    def.player_scope = Some(PlayerFilter::Opponent);

    let ability = build_resolved_from_def(&def, ObjectId(8000), controller);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        count_battlefield_objects_named(&state, controller, "treasure"),
        1,
        "controller (you = OriginalController) gets +1 Treasure"
    );
    assert_eq!(
        count_battlefield_objects_named(&state, opponent, "treasure"),
        1,
        "opponent (that player = ScopedPlayer) gets +1 Treasure"
    );
}
