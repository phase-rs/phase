//! CR 608.2b + CR 608.2c: an unresolved "the owner of target X shuffles it into
//! their library" spell lowers to a `TargetOnly` declaration and a bound
//! `ChangeZoneAll` move. The stack reader (`stack_entry_node_reach`) must name
//! the object that move acts on through the resolver's own selected-object
//! binding, and classify it as a library move through the shared
//! `bound_object_move_destination` authority, for a live target only.

use engine::game::effects::bound_object_move_destination;
use engine::game::effects::stack_reach::stack_entry_node_reach;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Oracle text from the repo's card data export.
const CHAOS_WARP: &str = "The owner of target permanent shuffles it into their library, then reveals the top card of their library. If it's a permanent card, they put it onto the battlefield.";
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";
const TURN_THE_EARTH: &str = "Choose up to three target cards in graveyards. The owners of those cards shuffle them into their libraries. You gain 2 life.\nFlashback {1}{G} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const CREMATE: &str = "Exile target card from a graveyard.\nDraw a card.";

fn main_phase(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    let s = runner.state_mut();
    s.turn_number = 3;
    s.active_player = P0;
    s.phase = Phase::PreCombatMain;
    s.waiting_for = WaitingFor::Priority { player: P0 };
    runner
}

/// What the entry's library-move node acts on. Reach guard: exactly one node
/// is classified as a move to a library, and it is the bound `ChangeZoneAll`.
fn library_move_reach(state: &GameState, entry: ObjectId) -> Vec<TargetRef> {
    let entry = state.stack.iter().find(|e| e.id == entry).expect("entry");
    let moves: Vec<_> = stack_entry_node_reach(state, entry)
        .into_iter()
        .filter(|reach| bound_object_move_destination(&reach.node.effect) == Some(Zone::Library))
        .collect();
    assert_eq!(moves.len(), 1, "reach guard: one library-move node");
    assert!(
        matches!(moves[0].node.effect, Effect::ChangeZoneAll { .. }),
        "reach guard: the move is the bound ChangeZoneAll, not a scalar ChangeZone"
    );
    moves[0].acted_on.clone()
}

/// Every object any node of the entry acts on.
fn all_reached_objects(state: &GameState, entry: ObjectId) -> Vec<ObjectId> {
    let entry = state.stack.iter().find(|e| e.id == entry).expect("entry");
    stack_entry_node_reach(state, entry)
        .into_iter()
        .flat_map(|reach| reach.acted_on)
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(id),
            TargetRef::Player(_) => None,
        })
        .collect()
}

/// Chaos Warp on the stack names its live target as the object its library
/// move acts on; this half fails without the shared binding and
/// classification. After the target is blinked in response (CR 400.7: a new
/// object), the declared target is illegal (CR 608.2b). The stack reader
/// answers nothing for an entry with an illegal declared target
/// (`resolution_board`, which predates this branch), so the stale half
/// documents that short-circuit rather than the selected-object binding.
#[test]
fn unresolved_chaos_warp_reaches_its_live_target_and_not_a_blinked_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let chaos_warp = scenario
        .add_spell_to_hand_from_oracle(P0, "Chaos Warp", true, CHAOS_WARP)
        .id();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P1, "Cloudshift", true, CLOUDSHIFT)
        .id();
    let mut runner = main_phase(scenario);

    runner.cast(chaos_warp).target_object(bear).commit();
    let warp_entry = runner.state().stack.back().expect("Chaos Warp").id;
    assert_eq!(
        library_move_reach(runner.state(), warp_entry),
        vec![TargetRef::Object(bear)],
        "the live target is what the library move acts on"
    );

    // P1 answers with Cloudshift on the targeted creature.
    engine::game::engine::apply_as_current_for_simulation(
        runner.state_mut(),
        GameAction::PassPriority,
    )
    .expect("P0 passes priority");
    runner.cast(cloudshift).target_object(bear).commit();
    runner.resolve_top();
    let state = runner.state();
    assert_eq!(state.stack.len(), 1, "only Chaos Warp remains");
    assert_eq!(state.objects[&bear].zone, Zone::Battlefield);
    assert!(
        all_reached_objects(state, warp_entry).is_empty(),
        "the returned creature is a new object; an entry with an illegal declared target reaches nothing"
    );
}

/// CR 608.2b: Turn the Earth's bound library move reaches each of its declared
/// graveyard targets. Once one is exiled in response it is illegal and is not
/// reached. (The stack reader answers nothing for an entry with an illegal
/// declared target, `resolution_board`; that rule predates this branch, so the
/// live sibling is not asserted after the response.)
#[test]
fn unresolved_turn_the_earth_reaches_only_its_live_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let live = scenario
        .add_creature_to_graveyard(P1, "Live Card", 1, 1)
        .id();
    let stale = scenario
        .add_creature_to_graveyard(P1, "Stale Card", 1, 1)
        .id();
    let turn_the_earth = scenario
        .add_spell_to_hand_from_oracle(P0, "Turn the Earth", true, TURN_THE_EARTH)
        .id();
    let cremate = scenario
        .add_spell_to_hand_from_oracle(P1, "Cremate", true, CREMATE)
        .id();
    scenario.with_library_top(P1, &["P1 Library"]);
    let mut runner = main_phase(scenario);

    runner
        .cast(turn_the_earth)
        .target_objects(&[live, stale])
        .commit();
    let entry = runner.state().stack.back().expect("Turn the Earth").id;
    let mut reached = library_move_reach(runner.state(), entry);
    reached.sort_by_key(|target| match target {
        TargetRef::Object(id) => id.0,
        TargetRef::Player(_) => u64::MAX,
    });
    let mut expected = vec![TargetRef::Object(live), TargetRef::Object(stale)];
    expected.sort_by_key(|target| match target {
        TargetRef::Object(id) => id.0,
        TargetRef::Player(_) => u64::MAX,
    });
    assert_eq!(reached, expected, "both live targets are reached");

    engine::game::engine::apply_as_current_for_simulation(
        runner.state_mut(),
        GameAction::PassPriority,
    )
    .expect("P0 passes priority");
    runner.cast(cremate).target_object(stale).commit();
    runner.resolve_top();
    let state = runner.state();
    assert_eq!(state.objects[&stale].zone, Zone::Exile);
    assert_eq!(state.stack.len(), 1, "only Turn the Earth remains");
    assert!(
        !all_reached_objects(state, entry).contains(&stale),
        "the exiled target is illegal and is not acted on"
    );
}

/// CR 903.9b + CR 614.1: a commander Chaos Warp would shuffle into its owner's
/// library may go to the command zone instead. The library move's proposed
/// event is read by the stack reader's replacement check through the shared
/// classification, so the move answers nothing for a commander target, as a
/// scalar library move does, and still names an ordinary target.
#[test]
fn unresolved_chaos_warp_does_not_claim_a_commander_that_may_be_redirected() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 903.3: the commander designation is an attribute of the card.
    let commander = {
        let mut builder = scenario.add_creature(P1, "Commanding Bear", 2, 2);
        builder.as_legendary().commander();
        builder.id()
    };
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let warp_commander = scenario
        .add_spell_to_hand_from_oracle(P0, "Chaos Warp", true, CHAOS_WARP)
        .id();
    let warp_bear = scenario
        .add_spell_to_hand_from_oracle(P0, "Chaos Warp", true, CHAOS_WARP)
        .id();
    let mut runner = main_phase(scenario);
    runner.state_mut().format_config.command_zone = true;

    runner.cast(warp_bear).target_object(bear).commit();
    let bear_entry = runner
        .state()
        .stack
        .back()
        .expect("Chaos Warp on the bear")
        .id;
    assert_eq!(
        library_move_reach(runner.state(), bear_entry),
        vec![TargetRef::Object(bear)],
        "control: an ordinary target is reached"
    );

    runner
        .cast(warp_commander)
        .target_object(commander)
        .commit();
    let commander_entry = runner
        .state()
        .stack
        .back()
        .expect("Chaos Warp on the commander")
        .id;
    assert!(
        library_move_reach(runner.state(), commander_entry).is_empty(),
        "the commander's own replacement may send it elsewhere, so the move does not claim it"
    );
}
