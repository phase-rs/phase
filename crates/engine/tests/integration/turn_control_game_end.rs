//! CR 104.1 + CR 723.1: a game that ends takes no further turn and no further
//! combat phase, so every player-control effect over it is over too. Driven from
//! the CR 104.4b mandatory-loop draw, a game end that eliminates nobody and
//! therefore never reaches CR 800.4a's leave-game teardown, and from the
//! CR 732.2a loop crown.

use std::sync::Arc;

use engine::ai_support::legal_actions_for_viewer;
use engine::analysis::decision_template::IterationCount;
use engine::analysis::resource::ResourceAxis;
use engine::database::card_db::CardDatabase;
use engine::game::casting::can_cast_object_now;
use engine::game::deck_loading::DeckEntry;
use engine::game::engine::apply;
use engine::game::interaction::{bind_interaction_authority, derive_viewer_interaction};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::turn_control::{authorized_submitters, turn_decision_maker};
use engine::game::turns::advance_phase;
use engine::game::visibility::filter_state_for_viewer;
use engine::game::EngineError;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::format::{FormatConfig, SideboardPolicy};
use engine::types::game_state::{
    GameEnd, GameState, LoopDetectionMode, PlayerDeckPool, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionResponseSpec, InteractionSessionId,
};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::match_config::{DeckCardCount, MatchConfig, MatchPhase, MatchType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);

const WORST_FEARS: &str = "You control target player during that player's next turn. Exile Worst Fears. (You see all cards that player could see and make all decisions for them.)";
const TAINTED_PACT: &str = "Exile the top card of your library. You may put that card into your hand unless it has the same name as another card exiled this way. Repeat this process until you put a card into your hand or you exile two cards with the same name, whichever comes first.";

const MAIN_CARD: &str = "Registered Main Card";
const SIDE_CARD: &str = "Registered Sideboard Card";

fn entry(name: &str, count: u32) -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: name.to_string(),
            ..Default::default()
        },
        count,
    }
}

/// Register the same four-plus-three pool for every seat and put the game in a
/// match, so the game's end has a between-games step to route.
fn install_match(state: &mut GameState, match_type: MatchType, seats: u8) {
    state.set_match_config(MatchConfig {
        match_type,
        ..Default::default()
    });
    state.match_phase = MatchPhase::InGame;
    state.game_number = 1;
    state.deck_pools = (0..seats)
        .map(|seat| PlayerDeckPool {
            player: PlayerId(seat),
            registered_main: Arc::new(vec![entry(MAIN_CARD, 4)]),
            registered_sideboard: Arc::new(vec![entry(SIDE_CARD, 3)]),
            current_main: Arc::new(vec![entry(MAIN_CARD, 4)]),
            current_sideboard: Arc::new(vec![entry(SIDE_CARD, 3)]),
            ..Default::default()
        })
        .collect();
}

/// The whole registered pool in the main deck: legal under every sideboard
/// policy, including one that permits no sideboard at all.
fn submit_whole_pool() -> GameAction {
    GameAction::SubmitSideboard {
        main: vec![
            DeckCardCount {
                name: MAIN_CARD.to_string(),
                count: 4,
            },
            DeckCardCount {
                name: SIDE_CARD.to_string(),
                count: 3,
            },
        ],
        sideboard: vec![],
    }
}

/// `controller` casts Worst Fears at `target` (CR 723.1), the game advances into
/// the controlled turn, and the controller's draw from an empty library ends the
/// game as a CR 104.4b draw with nobody eliminated. Returns the post-draw state.
fn game_ended_by_a_loop_draw_under_control(
    format: FormatConfig,
    seats: u8,
    match_type: MatchType,
    controller: PlayerId,
    target: PlayerId,
) -> GameState {
    let mut scenario = GameScenario::new_with_format(format, seats, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let worst_fears = scenario
        .add_spell_to_hand_from_oracle(controller, "Worst Fears", false, WORST_FEARS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let pact = scenario
        .add_spell_to_hand_from_oracle(controller, "Tainted Pact", true, TAINTED_PACT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    install_match(runner.state_mut(), match_type, seats);
    {
        let state = runner.state_mut();
        state.active_player = controller;
        state.priority_player = controller;
        state.waiting_for = WaitingFor::Priority { player: controller };
    }

    runner.cast(worst_fears).target_player(target).resolve();

    // CR 723.1: control activates when the affected player's turn begins. Stop at
    // a main phase, the first window of that turn a spell can be cast in.
    let mut events = Vec::new();
    for _ in 0..60 {
        advance_phase(runner.state_mut(), &mut events);
        let state = runner.state();
        if state.turn_decision_controller.is_some() && state.phase == Phase::PreCombatMain {
            break;
        }
    }
    assert_eq!(
        turn_decision_maker(runner.state()),
        controller,
        "reach guard: CR 723.1 control is active over the turn the draw ends"
    );
    assert_eq!(
        runner.state().scheduled_turn_controls.len(),
        1,
        "reach guard: the control effect is still scheduled when the game ends"
    );
    assert!(
        runner.state().players[controller.0 as usize]
            .library
            .is_empty(),
        "reach guard: the CR 104.4b loop needs a draw with no card to draw"
    );

    let outcome = runner.cast(pact).accept_optional().resolve();
    let state = outcome.state().clone();
    assert!(
        state.game_end.is_some(),
        "reach guard: the mandatory-loop draw ended the game"
    );
    assert!(
        state.eliminated_players.is_empty(),
        "reach guard: nobody was eliminated, so CR 800.4a's teardown never ran"
    );
    state
}

/// CR 104.1 + CR 723.1: no player-control effect survives the game, in any of the
/// places one is recorded. `clause` names the ending or teardown clause the
/// calling row pins.
fn assert_no_control_survives(state: &GameState, clause: &str) {
    assert_eq!(
        state.turn_decision_controller, None,
        "a decision controller outlived the game (clause: {clause})"
    );
    assert_eq!(
        state.turn_decision_control_timestamp, None,
        "a control timestamp outlived the game (clause: {clause})"
    );
    assert_eq!(
        state.active_full_turn_control, None,
        "a full-turn control window outlived the game (clause: {clause})"
    );
    assert_eq!(
        state.active_combat_phase_control, None,
        "a combat-phase control window outlived the game (clause: {clause})"
    );
    assert!(
        state.scheduled_turn_controls.is_empty(),
        "a scheduled control outlived the game (clause: {clause})"
    );
}

/// The interaction projection is only populated behind a bound session, so a
/// prompt read without one publishes nothing for anybody.
fn with_interaction_session(state: &GameState) -> GameState {
    let mut bound = state.clone();
    bind_interaction_authority(
        &mut bound,
        InteractionSessionId("turn-control-game-end".to_string()),
    )
    .expect("interaction authority binds");
    bound
}

/// `(min_main_total, max_main_total, candidate count)` of a viewer's sideboard
/// opportunity, or `None` when the viewer is offered nothing to answer.
fn sideboard_partition(state: &GameState, viewer: PlayerId) -> Option<(u32, u32, usize)> {
    let filtered = filter_state_for_viewer(state, viewer);
    let view = derive_viewer_interaction(state, &filtered, viewer);
    let opportunity = view.opportunities.first()?;
    let InteractionOpportunityResponse::Schema { spec, candidates } = &opportunity.response else {
        return None;
    };
    let InteractionResponseSpec::DeckPartition {
        min_main_total,
        max_main_total,
        ..
    } = spec
    else {
        return None;
    };
    Some((*min_main_total, *max_main_total, candidates.len()))
}

fn can_submit(state: &GameState, viewer: PlayerId) -> bool {
    let filtered = filter_state_for_viewer(state, viewer);
    derive_viewer_interaction(state, &filtered, viewer).can_submit
}

#[test]
fn control_ends_when_a_loop_draw_ends_the_game() {
    let state = game_ended_by_a_loop_draw_under_control(
        FormatConfig::standard(),
        2,
        MatchType::Bo3,
        P1,
        P0,
    );

    assert_eq!(
        state.waiting_for.variant_name(),
        "BetweenGamesSideboard",
        "reach guard: the draw moved the match to its between-games step"
    );
    assert_no_control_survives(&state, "a CR 104.4b draw that eliminates nobody");
}

#[test]
fn the_sideboard_prompt_routes_to_the_seat_that_owns_the_cards() {
    let state = game_ended_by_a_loop_draw_under_control(
        FormatConfig::standard(),
        2,
        MatchType::Bo3,
        P1,
        P0,
    );

    assert_eq!(
        authorized_submitters(&state),
        vec![P0],
        "CR 100.4: the sideboarding seat answers its own prompt"
    );
    let bound = with_interaction_session(&state);
    assert!(can_submit(&bound, P0));
    let (_, max_main_total, candidates) = sideboard_partition(&bound, P0)
        .expect("the owner's prompt publishes a deck partition to answer");
    assert!(max_main_total > 0);
    assert!(candidates > 0);
}

#[test]
fn the_former_controller_cannot_answer_the_owners_prompt() {
    let state = game_ended_by_a_loop_draw_under_control(
        FormatConfig::standard(),
        2,
        MatchType::Bo3,
        P1,
        P0,
    );

    let mut by_owner = state.clone();
    apply(&mut by_owner, P0, submit_whole_pool()).expect("the owner submits its own sideboard");

    let mut by_controller = state.clone();
    assert!(
        matches!(
            apply(&mut by_controller, P1, submit_whole_pool()),
            Err(EngineError::WrongPlayer)
        ),
        "the CR 723 controller has no say once the game is over"
    );

    let bound = with_interaction_session(&state);
    // Paired with the owner's populated projection: without it, an empty
    // projection for the controller would read the same whether the prompt is
    // routed correctly or published to nobody at all.
    assert!(sideboard_partition(&bound, P0).is_some());
    assert!(sideboard_partition(&bound, P1).is_none());
    assert!(!can_submit(&bound, P1));
}

#[test]
fn the_play_draw_prompt_routes_to_its_own_seat() {
    let mut state = game_ended_by_a_loop_draw_under_control(
        FormatConfig::standard(),
        2,
        MatchType::Bo3,
        P1,
        P0,
    );

    apply(&mut state, P0, submit_whole_pool()).expect("the first seat submits");
    apply(&mut state, P1, submit_whole_pool()).expect("the second seat submits");

    assert_eq!(
        state.waiting_for.variant_name(),
        "BetweenGamesChoosePlayDraw",
        "reach guard: both seats submitted, so the match moved to the CR 103.1 choice"
    );
    assert_eq!(
        authorized_submitters(&state),
        vec![P0],
        "CR 103.1: the chooser answers its own prompt"
    );
    assert_no_control_survives(
        &state,
        "a CR 104.4b draw that eliminates nobody, sibling prompt",
    );
}

/// CR 100.4: sideboard restrictions are per-format, and a format that permits
/// none pins the whole pool in the main deck — degenerate bounds that must still
/// route to their seat rather than read as "nobody may act".
#[test]
fn a_format_without_a_sideboard_still_routes_to_the_owner() {
    let state = game_ended_by_a_loop_draw_under_control(
        FormatConfig {
            sideboard_policy: SideboardPolicy::Forbidden,
            ..FormatConfig::standard()
        },
        2,
        MatchType::Bo3,
        P1,
        P0,
    );

    let WaitingFor::BetweenGamesSideboard {
        max_sideboard_size, ..
    } = state.waiting_for
    else {
        panic!("reach guard: the draw opened a between-games sideboard prompt");
    };
    assert_eq!(
        max_sideboard_size,
        Some(0),
        "reach guard: the row rides on the `Forbidden` bounds arm"
    );

    assert_eq!(authorized_submitters(&state), vec![P0]);
    let bound = with_interaction_session(&state);
    let (min_main_total, max_main_total, _) = sideboard_partition(&bound, P0)
        .expect("the owner's prompt publishes a deck partition to answer");
    assert_eq!(
        min_main_total, max_main_total,
        "with no sideboard the whole pool is pinned in the main deck"
    );
    let mut submitted = state.clone();
    apply(&mut submitted, P0, submit_whole_pool()).expect("the owner submits the pinned pool");
}

/// CR 805.8 + CR 723.5: control over one seat of a shared-turn team delegates
/// every teammate's decisions, so a multi-seat match has more prompts to strand
/// than a duel does. The teardown names no seat, so each answers its own.
#[test]
fn every_seat_answers_its_own_prompt_in_a_multi_seat_match() {
    let mut state = game_ended_by_a_loop_draw_under_control(
        FormatConfig::archenemy(),
        4,
        MatchType::Bo3,
        P0,
        P2,
    );

    assert_eq!(
        state.waiting_for.variant_name(),
        "BetweenGamesSideboard",
        "reach guard: the draw moved the match to its between-games step"
    );
    assert_no_control_survives(
        &state,
        "a CR 104.4b draw that eliminates nobody, four seats",
    );
    for seat in [P0, P1, P2, PlayerId(3)] {
        assert_eq!(
            authorized_submitters(&state),
            vec![seat],
            "each seat in turn answers its own sideboard prompt"
        );
        apply(&mut state, seat, submit_whole_pool()).expect("the prompted seat submits");
    }
}

/// A best-of-one draw completes the match rather than opening between-games
/// prompts, so its terminal snapshot is the only place a surviving control
/// could show: it carries none.
#[test]
fn control_ends_when_a_bo1_game_ends_in_a_draw() {
    let state = game_ended_by_a_loop_draw_under_control(
        FormatConfig::standard(),
        2,
        MatchType::Bo1,
        P1,
        P0,
    );

    assert_eq!(
        state.match_phase,
        MatchPhase::Completed,
        "reach guard: a best-of-one draw completes the match"
    );
    assert_eq!(state.waiting_for.variant_name(), "GameOver");
    assert_no_control_survives(
        &state,
        "a CR 104.4b draw that eliminates nobody, past the match",
    );
}

const REVITALIZE: &str = "You gain 3 life.\nDraw a card.";
const BEDEVIL: &str = "Destroy target artifact, creature, or planeswalker.";

fn card_db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration card fixture")
}

/// Passes each window and declares no attackers or blockers until `reached` holds.
fn pass_until(runner: &mut GameRunner, reached: impl Fn(&GameState) -> bool) {
    for _ in 0..400 {
        if reached(runner.state()) {
            return;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            other => panic!("unexpected prompt on the way: {other:?}"),
        };
        runner.act(action).expect("pass the turn along");
    }
    panic!("never reached; at {:?}", runner.state().waiting_for);
}

fn main_phase_of(seat: PlayerId) -> impl Fn(&GameState) -> bool {
    move |state| {
        state.active_player == seat
            && state.phase == Phase::PreCombatMain
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
    }
}

/// A board at P0's precombat main where P1 holds Worst Fears and eight Swamps to cast it, and
/// every seat has cards to draw.
fn worst_fears_board(seats: u8) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(seats, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..seats {
        for _ in 0..5 {
            scenario.add_card_to_library_top(PlayerId(seat), MAIN_CARD);
        }
    }
    for _ in 0..8 {
        scenario.add_basic_land(P1, ManaColor::Black);
    }
    let worst_fears = scenario.add_real_card(P1, "Worst Fears", Zone::Hand, card_db());
    (scenario, worst_fears)
}

/// CR 723.1: P1 casts Worst Fears at P0 in its own main phase, and play passes into P0's next
/// turn, which P1 controls.
fn into_worst_fears_turn(runner: &mut GameRunner, worst_fears: ObjectId) {
    pass_until(runner, main_phase_of(P1));
    runner.cast(worst_fears).target_player(P0).resolve();
    pass_until(runner, |state| {
        main_phase_of(P0)(state) && state.turn_decision_controller == Some(P1)
    });
    assert_eq!(
        authorized_submitters(runner.state()),
        vec![P1],
        "reach guard: CR 723.1 control is live and routes away from the active seat"
    );
}

/// P0's Revitalize, whose 3 life starts its controller's drain.
fn add_revitalize(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Revitalize", true, REVITALIZE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .id()
}

/// Passes priority and orders trigger groups as listed, answering every trigger target with
/// `aim`, until a prompt outside those.
fn drive(runner: &mut GameRunner, aim: Option<PlayerId>) {
    for _ in 0..2000 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::OrderTriggers { triggers, .. } => runner.act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            }),
            WaitingFor::TriggerTargetSelection { .. } if aim.is_some() => {
                runner.act(GameAction::SelectTargets {
                    targets: aim.map(TargetRef::Player).into_iter().collect(),
                })
            }
            _ => return,
        };
        if result.is_err() {
            return;
        }
    }
}

/// CR 104.1 + CR 723.1 + CR 723.5 + CR 100.4 through the production entry point: P1 casts
/// Worst Fears at P0 and, on P0's controlled turn, casts P0's Revitalize beside Marauding
/// Blight-Priest ("Whenever you gain life, each opponent loses 1 life.") and Exquisite Blood
/// ("Whenever an opponent loses life, you gain that much life."). The mandatory drain is decided
/// by the loop detector, whose replay submits P0's choices as P1, so the CR 732.2a crown ends
/// the game — and player control with it — and the best-of-three goes on to sideboarding.
#[test]
fn a_loop_crown_ends_player_control_and_reaches_sideboarding() {
    let (mut scenario, worst_fears) = worst_fears_board(2);
    scenario.with_life(P1, 6);
    scenario.add_real_card(P0, "Marauding Blight-Priest", Zone::Battlefield, card_db());
    scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, card_db());
    let revitalize = add_revitalize(&mut scenario);
    let mut runner = scenario.build();
    // `install_match` projects `MatchConfig::loop_detection` (default `Off`) onto the state, so
    // the detector is armed after it.
    install_match(runner.state_mut(), MatchType::Bo3, 2);
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;

    into_worst_fears_turn(&mut runner, worst_fears);
    assert_eq!(runner.state().turn_decision_controller, Some(P1));
    runner.cast(revitalize).resolve();
    drive(&mut runner, None);

    let state = runner.state();
    assert!(
        state.players[1].life > 0,
        "reach guard: the CR 732.2a crown ended it early, not a natural CR 704.5a death"
    );
    assert_eq!(state.game_end, Some(GameEnd { winner: Some(P0) }));
    let marked = state
        .unbounded_resources
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    assert!(
        marked.contains(&ResourceAxis::Life(P0)) && marked.contains(&ResourceAxis::Life(P1)),
        "{marked:?}"
    );
    assert!(
        state
            .unbounded_rising
            .get(&P0)
            .is_some_and(|rising| rising.contains(&ResourceAxis::Life(P0))),
        "{:?}",
        state.unbounded_rising
    );
    assert_eq!(state.match_phase, MatchPhase::BetweenGames);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::BetweenGamesSideboard {
                player: P0,
                game_number: 2,
                ..
            }
        ),
        "{:?}",
        state.waiting_for
    );
    assert_no_control_survives(state, "a CR 732.2a loop crown");

    runner
        .act(submit_whole_pool())
        .expect("the winner submits its own sideboard");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::BetweenGamesSideboard { player: P1, .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
}

/// CR 723.5 + CR 732.2a: on P0's turn under P1's Worst Fears, P1 casts P0's Revitalize beside
/// Sanguine Bond ("Whenever you gain life, target opponent loses that much life.") and Exquisite
/// Blood, aiming the drain at P2. The loop is offered to P0 as its controller is the one who
/// may break it: each repetition takes 3, so P2 at 17 falls in 6 and P1 at 20 in 7 more, 13.
#[test]
fn a_breakable_loop_during_a_controlled_turn_is_offered() {
    let (mut scenario, worst_fears) = worst_fears_board(3);
    scenario.add_real_card(P0, "Sanguine Bond", Zone::Battlefield, card_db());
    scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, card_db());
    let revitalize = add_revitalize(&mut scenario);
    let mut runner = scenario.build();
    install_match(runner.state_mut(), MatchType::Bo3, 3);
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;

    into_worst_fears_turn(&mut runner, worst_fears);
    runner.cast(revitalize).target_player(P2).resolve();
    drive(&mut runner, Some(P2));

    let state = runner.state();
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        schema,
        ..
    } = &state.waiting_for
    else {
        panic!("no offer: {:?}", state.waiting_for);
    };
    assert_eq!((*proposer, *predicted_winner), (P0, Some(P0)));
    assert_eq!(schema.iteration_count, IterationCount::Fixed(13));
    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        [26, 20, 17]
    );
}

/// CR 723.5 + CR 605.3a: on P0's turn under P1's Worst Fears, P0's Bedevil off two Swamps and
/// Graven Cairns ("{T}: Add {C}. / {B/R}, {T}: Add {B}{B}, {B}{R}, or {R}{R}.") is listed for P1
/// and castable: the filter-land payment witness submits P0's mana abilities as P1.
#[test]
fn a_filter_land_cast_is_listed_for_the_controller_of_the_turn() {
    let (mut scenario, worst_fears) = worst_fears_board(2);
    let bedevil = add_bedevil_board(&mut scenario);
    let mut runner = scenario.build();
    into_worst_fears_turn(&mut runner, worst_fears);
    assert_bedevil_listed(runner.state(), P1, bedevil);
}

/// CR 601.2g + CR 605.3a: the same Bedevil board on P0's own uncontrolled turn. Listing the cast
/// runs the filter-land witness on a clone taken inside the simulated cast's action.
#[test]
fn a_filter_land_cast_is_listed_on_an_uncontrolled_turn() {
    let (mut scenario, _) = worst_fears_board(2);
    let bedevil = add_bedevil_board(&mut scenario);
    let runner = scenario.build();
    assert_eq!(authorized_submitters(runner.state()), vec![P0]);
    assert_bedevil_listed(runner.state(), P0, bedevil);
}

fn add_bedevil_board(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_real_card(P0, "Graven Cairns", Zone::Battlefield, card_db());
    scenario.add_vanilla(P1, 2, 2);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Bedevil", true, BEDEVIL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Red,
            ],
            generic: 0,
        })
        .id()
}

fn assert_bedevil_listed(state: &GameState, viewer: PlayerId, bedevil: ObjectId) {
    let (actions, _, _) = legal_actions_for_viewer(state, viewer);
    assert!(
        actions.iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == bedevil)
        ),
        "Bedevil is listed for {viewer:?}"
    );
    assert!(can_cast_object_now(state, P0, bedevil));
}
