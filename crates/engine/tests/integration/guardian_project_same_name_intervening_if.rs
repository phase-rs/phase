//! Runtime regression tests for Guardian Project's same-name intervening-`if`.
//!
//! Guardian Project (RVR/PIP/RNA) reads:
//!   "Whenever a nontoken creature you control enters, if it doesn't have the
//!    same name as another creature you control or a creature card in your
//!    graveyard, draw a card."
//!
//! Before this change the intervening-`if` was swallowed entirely (coverage
//! reported `Swallow:Condition_If`), so the card drew a card on EVERY nontoken
//! creature entering. Each negative row below therefore fails if the change is
//! reverted, which is what makes them discriminating rather than shape tests.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 201.2a: "Two or more objects have the same name if they
//!     have at least one name in common. An object with no name doesn't have the
//!     same name as any other object, including another object with no name."
//!     This is the authorizing rule for the name-equality test the card negates.
//!   - CR 603.4: the intervening-`if` rule — the condition is checked
//!     when the event occurs AND again as the ability resolves.
//!   - CR 603.6a: enters-the-battlefield abilities; the anaphor "it"
//!     is the entering permanent, not the permanent that owns the ability.
//!   - CR 109.2 / CR 109.2a: a zone-less type description
//!     means a permanent on the battlefield, while a description naming a zone
//!     ("a creature card in your graveyard") keeps that zone — the two legs of
//!     the reference pool.
//!   - CR 111.1: the "nontoken" head.
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::game::zones::{create_object, move_to_zone};
use engine::types::card_type::CoreType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (confirmed against the Scryfall API this session).
const GUARDIAN_PROJECT_ORACLE: &str = "Whenever a nontoken creature you control enters, if it doesn't have the same name as another creature you control or a creature card in your graveyard, draw a card.";

/// Put Guardian Project onto the battlefield under P0 and return the runner.
fn setup() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let project_id = {
        let mut builder = scenario.add_creature(P0, "Guardian Project", 0, 0);
        builder.as_enchantment();
        builder.from_oracle_text(GUARDIAN_PROJECT_ORACLE);
        builder.id()
    };
    // Library padding so drawing (and advancing) never decks anyone.
    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }
    let mut runner = scenario.build();
    reindex_object_triggers(runner.state_mut(), project_id);
    (runner, project_id)
}

/// Create a creature directly in `zone` without firing any trigger.
fn place_creature(
    runner: &mut GameRunner,
    owner: PlayerId,
    name: &str,
    zone: Zone,
    is_token: bool,
) -> ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), zone);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.is_token = is_token;
    id
}

/// Move a creature from hand onto the battlefield through the real zone-change
/// path, then run the trigger pass. Returns the net cards drawn by `watch`.
fn enter_creature_and_count_draws(
    runner: &mut GameRunner,
    owner: PlayerId,
    name: &str,
    is_token: bool,
    watch: PlayerId,
) -> i64 {
    let creature = place_creature(runner, owner, name, Zone::Hand, is_token);
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), creature, Zone::Battlefield, &mut events);
    // Baseline AFTER the zone change and BEFORE the triggered ability resolves, so
    // the delta counts only cards drawn by the trigger. Taking it before the move
    // would also have to model the entrant leaving hand, which differs between
    // tokens and nontokens and would measure the harness rather than the engine.
    let before = runner.state().players[watch.0 as usize].hand.len() as i64;
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    runner.advance_until_stack_empty();
    runner.state().players[watch.0 as usize].hand.len() as i64 - before
}

/// V-B (positive reach-guard): a unique name draws. This proves the trigger is
/// still built and still fires, so the zero-draw rows below are the CONDITION
/// refusing rather than the trigger having been deleted.
#[test]
fn unique_name_entering_draws_a_card() {
    let (mut runner, _project) = setup();
    let drawn = enter_creature_and_count_draws(&mut runner, P0, "Runeclaw Bear", false, P0);
    assert_eq!(
        drawn, 1,
        "CR 201.2a: a creature sharing no name with anything must satisfy the \
         intervening-if and draw"
    );
}

/// V-A: a duplicate name already on the battlefield must NOT draw.
/// Fails on revert — the swallowed clause drew unconditionally.
#[test]
fn duplicate_name_on_battlefield_does_not_draw() {
    let (mut runner, _project) = setup();
    place_creature(&mut runner, P0, "Grizzly Bears", Zone::Battlefield, false);
    let drawn = enter_creature_and_count_draws(&mut runner, P0, "Grizzly Bears", false, P0);
    assert_eq!(
        drawn, 0,
        "CR 201.2a: the entrant shares a name with another creature you control, \
         so the CR 603.4 intervening-if is false and no card is drawn"
    );
}

/// V-E: the self-exclusion is real. The entrant is the ONLY creature anywhere,
/// so it can only fail by matching itself — which is what "another" forbids.
/// This is the row that fails if the trigger-object exclusion is inert.
#[test]
fn entrant_does_not_match_itself() {
    let (mut runner, _project) = setup();
    let drawn = enter_creature_and_count_draws(&mut runner, P0, "Llanowar Elves", false, P0);
    assert_eq!(
        drawn, 1,
        "CR 201.2a + CR 603.6a: every object shares a name with itself, so \
         \"another creature you control\" must exclude the entering creature"
    );
}

/// V-C: the graveyard leg. Nothing on the battlefield shares the name; the match
/// is against a creature CARD in your graveyard (CR 109.2a).
#[test]
fn duplicate_name_in_graveyard_does_not_draw() {
    let (mut runner, _project) = setup();
    place_creature(&mut runner, P0, "Grizzly Bears", Zone::Graveyard, false);
    let drawn = enter_creature_and_count_draws(&mut runner, P0, "Grizzly Bears", false, P0);
    assert_eq!(
        drawn, 0,
        "CR 109.2a: \"a creature card in your graveyard\" is the second reference \
         leg, so a graveyard name match also fails the intervening-if"
    );
}

/// H-1: the reference is scoped to creatures YOU control, so an opponent's
/// same-named creature does not block the draw.
#[test]
fn opponent_creature_with_same_name_still_draws() {
    let (mut runner, _project) = setup();
    place_creature(&mut runner, P1, "Grizzly Bears", Zone::Battlefield, false);
    let drawn = enter_creature_and_count_draws(&mut runner, P0, "Grizzly Bears", false, P0);
    assert_eq!(
        drawn, 1,
        "the battlefield leg is scoped to creatures you control, so an \
         opponent's same-named creature is not in the reference pool"
    );
}

/// H-1 mirror for the graveyard leg: "a creature card in your graveyard"
/// (CR 109.2a) is owner-scoped, so a same-named card in an OPPONENT's
/// graveyard does not block the draw.
#[test]
fn duplicate_name_in_opponents_graveyard_still_draws() {
    let (mut runner, _project) = setup();
    place_creature(&mut runner, P1, "Grizzly Bears", Zone::Graveyard, false);
    let drawn = enter_creature_and_count_draws(&mut runner, P0, "Grizzly Bears", false, P0);
    assert_eq!(
        drawn, 1,
        "CR 109.2a: \"a creature card in your graveyard\" is owner-scoped, so \
         a same-named card in an opponent's graveyard is not in the reference pool"
    );
}

/// V-D: the "nontoken" head. A token entering never triggers at all, regardless
/// of its name — this guards the head against accidental widening.
#[test]
fn token_entering_does_not_draw() {
    let (mut runner, _project) = setup();
    let drawn = enter_creature_and_count_draws(&mut runner, P0, "Zombie", true, P0);
    assert_eq!(
        drawn, 0,
        "CR 111.1: the trigger watches NONTOKEN creatures, so a token entering \
         does not trigger even with a unique name"
    );
}
