//! Final card integration for Opposition Agent over the previously merged
//! search-control, SearchFound, and bound exile-permission foundations.

use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CastingPermission, ControlWindow, ControllerRef, Effect,
    ManaSpendPermission, QuantityExpr, ResolvedAbility, SearchSelectionConstraint,
    StaticDefinition, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{ScheduledTurnControl, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const OPPOSITION_AGENT: &str = "Flash\n\
You control your opponents while they're searching their libraries.\n\
While an opponent is searching their library, they exile each card they find. You may play those cards for as long as they remain exiled, and you may spend mana as though it were mana of any color to cast them.";
const TEST_TUTOR: &str =
    "Search your library for a card, put that card into your hand, then shuffle.";

fn setup_two_player() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let agent = scenario
        .add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(P1, "Found Card", true)
        .id();
    scenario.add_card_to_library_top(P1, "Library Filler");
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, agent, tutor, found)
}

fn resolve_to_search(runner: &mut GameRunner, tutor: ObjectId) {
    let outcome = runner.cast(tutor).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { player: P1, .. }
    ));
}

fn has_agent_permission(runner: &GameRunner, found: ObjectId, grantee: PlayerId) -> bool {
    runner.state().objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| {
            matches!(
                permission,
                CastingPermission::PlayFromExile {
                    granted_to,
                    mana_spend_permission: Some(ManaSpendPermission::AnyColor),
                    ..
                } if *granted_to == grantee
            )
        })
}

fn setup_two_headed_search(searcher: PlayerId) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(searcher, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(searcher, "Found Card", true)
        .id();
    scenario.add_card_to_library_top(searcher, "Library Filler");
    let mut runner = scenario.build();
    runner.state_mut().format_config = FormatConfig::two_headed_giant();
    runner.state_mut().active_player = searcher;
    runner.state_mut().priority_player = searcher;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: searcher };
    (runner, tutor, found)
}

/// CR 102.3: teammates are not opponents in team games. Opposition Agent must
/// neither control a teammate's own-library search nor replace that teammate's
/// SearchFound event, while both production paths still apply to the opposing
/// team.
#[test]
fn two_headed_giant_teammate_search_is_not_an_opponent_search() {
    let (mut teammate, tutor, found) = setup_two_headed_search(P1);
    let outcome = teammate.cast(tutor).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { player: P1, .. }
    ));
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(teammate.state(), P1),
        P1,
        "P0 must not control their teammate P1's search"
    );
    teammate
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the teammate submits their own search choice");
    assert_eq!(
        teammate.state().objects[&found].zone,
        Zone::Hand,
        "the teammate's found card follows the original search destination"
    );
    assert!(!has_agent_permission(&teammate, found, P0));

    let (mut opponent, tutor, found) = setup_two_headed_search(P2);
    let outcome = opponent.cast(tutor).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { player: P2, .. }
    ));
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(opponent.state(), P2),
        P0,
        "P0 must still control an opposing-team player's search"
    );
    assert!(apply(
        opponent.state_mut(),
        P2,
        GameAction::SelectCards { cards: vec![found] }
    )
    .is_err());
    opponent
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("Opposition Agent's controller submits the opponent's search choice");
    assert_eq!(opponent.state().objects[&found].zone, Zone::Exile);
    assert!(has_agent_permission(&opponent, found, P0));
}

/// CR 723.5 + CR 701.23a + CR 614.1 + CR 611.2b + CR 609.4b: the semantic
/// searcher remains P1, P0 makes the latched decision, and the canonical
/// SearchFound replacement exiles the selected card with P0's exact any-color
/// persistent play permission.
#[test]
fn opponent_own_library_search_controls_exiles_and_grants_any_color_permission() {
    let (mut runner, _agent, tutor, found) = setup_two_player();
    resolve_to_search(&mut runner, tutor);

    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(runner.state(), P1),
        P0
    );
    assert!(apply(
        runner.state_mut(),
        P1,
        GameAction::SelectCards { cards: vec![found] }
    )
    .is_err());
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the snapshotted controller submits the search choice");

    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert!(has_agent_permission(&runner, found, P0));
}

fn direct_search_definition(
    target_player: Option<TargetFilter>,
    source_zones: Vec<Zone>,
) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::default()),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
            source_zones,
        },
    )
}

/// Negative siblings for the production preparation seam: searching another
/// player's library is outside the static's own-library condition, and a
/// library removed by CantSearchLibrary never creates a controlled search.
#[test]
fn cross_library_and_muzzled_searches_do_not_activate_agent_control() {
    let mut cross = GameScenario::new();
    cross.at_phase(Phase::PreCombatMain);
    cross.add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    let cross_search = cross
        .add_spell_to_hand(P1, "Cross-Library Search", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(direct_search_definition(
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
            vec![Zone::Library],
        ))
        .id();
    cross.add_card_to_library_top(P0, "Opponent Library Card");
    let mut cross = cross.build();
    cross.state_mut().active_player = P1;
    cross.state_mut().priority_player = P1;
    cross.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let outcome = cross.cast(cross_search).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice {
            player: P1,
            library_owner: Some(P0),
            ..
        }
    ));
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(outcome.state(), P1),
        P1,
        "Opposition Agent is restricted to opponents searching their own libraries"
    );

    let mut muzzled = GameScenario::new();
    muzzled.at_phase(Phase::PreCombatMain);
    muzzled.add_creature_from_oracle(P0, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    muzzled
        .add_creature(P0, "Search Muzzle", 1, 1)
        .with_static_definition(StaticDefinition::new(StaticMode::CantSearchLibrary {
            cause: ProhibitionScope::Opponents,
        }));
    let tutor = muzzled
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    muzzled.add_card_to_library_top(P1, "Muzzled Card");
    let mut muzzled = muzzled.build();
    muzzled.state_mut().active_player = P1;
    muzzled.state_mut().priority_player = P1;
    muzzled.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let outcome = muzzled.cast(tutor).resolve();
    assert!(!matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice { .. }
    ));
    assert!(outcome.state().active_search_decision_controls.is_empty());
}

fn precedence_runner(
    newer_agent_timestamp: u64,
    turn_timestamp: u64,
    turn_controller: PlayerId,
) -> (GameRunner, ObjectId, ObjectId) {
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let older_agent = scenario
        .add_creature_from_oracle(P0, "Older Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let newer_agent = scenario
        .add_creature_from_oracle(p3, "Newer Opposition Agent", 3, 2, OPPOSITION_AGENT)
        .id();
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_card_to_library_top(P1, "Found Card");
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&older_agent)
        .unwrap()
        .timestamp = 25;
    runner
        .state_mut()
        .objects
        .get_mut(&newer_agent)
        .unwrap()
        .timestamp = newer_agent_timestamp;
    runner.state_mut().active_player = P1;
    runner.state_mut().turn_decision_controller = Some(turn_controller);
    runner.state_mut().turn_decision_control_timestamp = Some(turn_timestamp);
    runner
        .state_mut()
        .scheduled_turn_controls
        .push(ScheduledTurnControl {
            target_player: P1,
            controller: turn_controller,
            timestamp: turn_timestamp,
            grant_extra_turn_after: false,
            window: engine::types::ability::ControlWindow::NextTurn,
        });
    runner.state_mut().priority_player = turn_controller;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, tutor, newer_agent)
}

/// CR 723.1a + CR 723.5: scheduling a future control during an already
/// controlled turn must not lend the future effect's timestamp to the active
/// control when search authority is selected.
#[test]
fn future_scheduled_control_cannot_replace_active_control_provenance() {
    let active_controller = P2;
    let (mut runner, tutor, _agent) = precedence_runner(100, 50, active_controller);
    runner.state_mut().next_timestamp = 150;
    let future_control = ResolvedAbility::new(
        Effect::ControlNextTurn {
            target: TargetFilter::Player,
            grant_extra_turn_after: false,
            window: ControlWindow::NextTurn,
        },
        vec![TargetRef::Player(P1)],
        ObjectId(9_999),
        active_controller,
    );
    let mut events = Vec::new();
    engine::game::effects::control_next_turn::resolve(
        runner.state_mut(),
        &future_control,
        &mut events,
    )
    .expect("schedule the future control");

    assert_eq!(runner.state().turn_decision_control_timestamp, Some(50));
    assert!(runner
        .state()
        .scheduled_turn_controls
        .iter()
        .any(|scheduled| scheduled.timestamp == 150));

    resolve_to_search(&mut runner, tutor);

    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(runner.state(), P1),
        PlayerId(3),
        "the Agent between the active and future timestamps must control the search"
    );
    let search = runner
        .state()
        .active_library_searches
        .get(&P1)
        .expect("own-library search exposes a hidden library view");
    assert!(search.learned_audience().contains(&PlayerId(3)));
    assert!(
        !search.learned_audience().contains(&active_controller),
        "future scheduled control must not expose the library to its controller"
    );

    let found = match &runner.state().waiting_for {
        WaitingFor::SearchChoice { cards, .. } => *cards
            .first()
            .expect("the prepared search has a selectable card"),
        other => panic!("expected prepared search choice, got {other:?}"),
    };
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("the Agent controller completes the search before turns advance");

    // Finish P1's current controlled turn, then advance P2, P3, and P0. The
    // newly scheduled effect must survive that first boundary and activate when
    // P1's next turn begins.
    let mut turn_events = Vec::new();
    for _ in 0..4 {
        engine::game::turns::start_next_turn(runner.state_mut(), &mut turn_events);
    }
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(
        runner.state().turn_decision_controller,
        Some(active_controller),
        "the future control must govern P1's next turn"
    );
    assert_eq!(runner.state().turn_decision_control_timestamp, Some(150));
}

/// CR 723.1a + CR 723.5: multiple Agents and an active turn-control effect are
/// compared by creation time at search preparation. Each ordering is exercised,
/// then the winning Agent leaves to prove the already-prepared authority is a
/// snapshot rather than a live static rescan.
#[test]
fn newest_player_control_effect_wins_and_search_authority_remains_latched() {
    let (mut newer_turn, tutor, _agent) = precedence_runner(50, 100, P2);
    resolve_to_search(&mut newer_turn, tutor);
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(newer_turn.state(), P1),
        P2,
        "newer active turn control must beat the older Agent"
    );

    let (mut newer_agent, tutor, winning_agent) = precedence_runner(100, 50, P2);
    resolve_to_search(&mut newer_agent, tutor);
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(newer_agent.state(), P1),
        PlayerId(3),
        "newer Agent must beat the older active turn control"
    );
    let mut events = Vec::new();
    move_to_zone(
        newer_agent.state_mut(),
        winning_agent,
        Zone::Graveyard,
        &mut events,
    );
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(newer_agent.state(), P1),
        PlayerId(3),
        "source departure after preparation must not rebind the exposed search"
    );

    let (mut newest_self_control, tutor, _agent) = precedence_runner(100, 150, P1);
    resolve_to_search(&mut newest_self_control, tutor);
    assert_eq!(
        engine::game::turn_control::authorized_submitter_for_player(
            newest_self_control.state(),
            P1
        ),
        P1,
        "a newer active self-control effect still overwrites an older Agent"
    );
}
