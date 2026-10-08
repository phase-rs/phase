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
use std::collections::HashSet;

use engine::game::effects::token::apply_create_token_after_replacement;
use engine::game::layers::evaluate_layers;
use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::game::zones::create_object;
use engine::types::ability::{ContinuousModification, Duration, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::{
    EtbTapState, ProposedEvent, TokenCharacteristics, TokenHostRequest, TokenSpec,
};
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

/// Exercise the production zone-change pipeline, including replacement effects.
fn move_through_pipeline(
    runner: &mut GameRunner,
    object_id: ObjectId,
    destination: Zone,
    events: &mut Vec<GameEvent>,
) {
    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(object_id, destination, object_id),
            events,
        ),
        "the Guardian Project fixture move must not pause for a replacement choice"
    );
}

/// Install the same Layer-2 control effect produced by GainControl, then
/// evaluate layers so the fixture cannot conflate ownership with control.
fn grant_control(
    runner: &mut GameRunner,
    source: ObjectId,
    controller: PlayerId,
    target: ObjectId,
) {
    runner.state_mut().add_transient_continuous_effect(
        source,
        controller,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: target },
        vec![ContinuousModification::ChangeController],
        None,
    );
    evaluate_layers(runner.state_mut());
}

fn guardian_project_trigger_is_pending(runner: &GameRunner, project: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| {
        matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == project
        )
    })
}

/// Move a creature from hand onto the battlefield through the real zone-change
/// path, then run the trigger pass. Returns the net cards drawn by `watch`.
fn enter_creature_and_count_draws(
    runner: &mut GameRunner,
    project: ObjectId,
    owner: PlayerId,
    name: &str,
    is_token: bool,
    watch: PlayerId,
    should_trigger: bool,
) -> i64 {
    let creature = place_creature(runner, owner, name, Zone::Hand, is_token);
    let mut events = Vec::new();
    move_through_pipeline(runner, creature, Zone::Battlefield, &mut events);
    // Baseline AFTER the zone change and BEFORE the triggered ability resolves, so
    // the delta counts only cards drawn by the trigger. Taking it before the move
    // would also have to model the entrant leaving hand, which differs between
    // tokens and nontokens and would measure the harness rather than the engine.
    let before = runner.state().players[watch.0 as usize].hand.len() as i64;
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    assert_eq!(
        guardian_project_trigger_is_pending(runner, project),
        should_trigger,
        "CR 603.4: Guardian Project must {} reach the stack when its \
         intervening-if is {} at trigger creation",
        if should_trigger { "" } else { "not" },
        if should_trigger { "true" } else { "false" },
    );
    runner.advance_until_stack_empty();
    runner.state().players[watch.0 as usize].hand.len() as i64 - before
}

fn creature_token_spec(controller: PlayerId) -> TokenSpec {
    TokenSpec {
        characteristics: TokenCharacteristics {
            display_name: "Zombie".to_string(),
            power: Some(2),
            toughness: Some(2),
            loyalty: None,
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Zombie".to_string()],
            supertypes: Vec::new(),
            colors: Vec::new(),
            keywords: Vec::new(),
        },
        script_name: "Zombie".to_string(),
        static_abilities: Vec::new(),
        enter_with_counters: Vec::new(),
        tapped: false,
        enters_attacking: false,
        sacrifice_at: None,
        source_id: ObjectId(0),
        controller,
        attach_to: TokenHostRequest::NotRequested,
    }
}

/// Create a token through the production replacement and entry pipeline, then
/// measure any Guardian Project draw from its actual battlefield-entry event.
fn create_token_and_count_draws(
    runner: &mut GameRunner,
    project: ObjectId,
    watch: PlayerId,
) -> i64 {
    let mut events = Vec::new();
    let proposed = ProposedEvent::CreateToken {
        owner: P0,
        spec: Box::new(creature_token_spec(P0)),
        copy: None,
        enter_tapped: EtbTapState::Unspecified,
        count: 1,
        applied: HashSet::new(),
    };
    match replace_event(runner.state_mut(), proposed, &mut events) {
        ReplacementResult::Execute(event) => {
            assert!(apply_create_token_after_replacement(
                runner.state_mut(),
                event,
                &mut events,
            ));
        }
        other => panic!("token creation must execute, got {other:?}"),
    }
    let token = runner
        .state()
        .last_created_token_ids
        .last()
        .copied()
        .expect("the production token pipeline must report its created token");
    assert_eq!(runner.state().objects[&token].zone, Zone::Battlefield);

    let before = runner.state().players[watch.0 as usize].hand.len() as i64;
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    assert!(
        !guardian_project_trigger_is_pending(runner, project),
        "CR 111.1: a token entrant must not create a Guardian Project trigger"
    );
    runner.advance_until_stack_empty();
    runner.state().players[watch.0 as usize].hand.len() as i64 - before
}

/// V-B (positive reach-guard): a unique name draws. This proves the trigger is
/// still built and still fires, so the zero-draw rows below are the CONDITION
/// refusing rather than the trigger having been deleted.
#[test]
fn unique_name_entering_draws_a_card() {
    let (mut runner, project) = setup();
    let drawn =
        enter_creature_and_count_draws(&mut runner, project, P0, "Runeclaw Bear", false, P0, true);
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
    let (mut runner, project) = setup();
    place_creature(&mut runner, P0, "Grizzly Bears", Zone::Battlefield, false);
    let drawn =
        enter_creature_and_count_draws(&mut runner, project, P0, "Grizzly Bears", false, P0, false);
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
    let (mut runner, project) = setup();
    let drawn =
        enter_creature_and_count_draws(&mut runner, project, P0, "Llanowar Elves", false, P0, true);
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
    let (mut runner, project) = setup();
    place_creature(&mut runner, P0, "Grizzly Bears", Zone::Graveyard, false);
    let drawn =
        enter_creature_and_count_draws(&mut runner, project, P0, "Grizzly Bears", false, P0, false);
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
    let (mut runner, project) = setup();
    place_creature(&mut runner, P1, "Grizzly Bears", Zone::Battlefield, false);
    let drawn =
        enter_creature_and_count_draws(&mut runner, project, P0, "Grizzly Bears", false, P0, true);
    assert_eq!(
        drawn, 1,
        "the battlefield leg is scoped to creatures you control, so an \
         opponent's same-named creature is not in the reference pool"
    );
}

/// The battlefield leg is controller-scoped, not owner-scoped.
#[test]
fn controlled_opponents_creature_with_same_name_does_not_draw() {
    let (mut runner, project) = setup();
    let creature = place_creature(&mut runner, P1, "Grizzly Bears", Zone::Battlefield, false);
    grant_control(&mut runner, project, P0, creature);
    assert_eq!(runner.state().objects[&creature].owner, P1);
    assert_eq!(runner.state().objects[&creature].controller, P0);

    let drawn =
        enter_creature_and_count_draws(&mut runner, project, P0, "Grizzly Bears", false, P0, false);
    assert_eq!(
        drawn, 0,
        "the battlefield reference is a creature you CONTROL even when its owner is P1"
    );
}

/// H-1 mirror for the graveyard leg: "a creature card in your graveyard"
/// (CR 109.2a) is owner-scoped, so a same-named card in an OPPONENT's
/// graveyard does not block the draw.
#[test]
fn duplicate_name_in_opponents_graveyard_still_draws() {
    let (mut runner, project) = setup();
    place_creature(&mut runner, P1, "Grizzly Bears", Zone::Graveyard, false);
    let drawn =
        enter_creature_and_count_draws(&mut runner, project, P0, "Grizzly Bears", false, P0, true);
    assert_eq!(
        drawn, 1,
        "CR 109.2a: \"a creature card in your graveyard\" is owner-scoped, so \
         a same-named card in an opponent's graveyard is not in the reference pool"
    );
}

/// The graveyard leg is owner-scoped, not scoped to the creature's last
/// battlefield controller.
#[test]
fn your_graveyard_creature_with_opponents_last_control_does_not_draw() {
    let (mut runner, project) = setup();
    let creature = place_creature(&mut runner, P0, "Grizzly Bears", Zone::Battlefield, false);
    grant_control(&mut runner, project, P1, creature);
    assert_eq!(runner.state().objects[&creature].owner, P0);
    assert_eq!(runner.state().objects[&creature].controller, P1);
    let mut events = Vec::new();
    move_through_pipeline(&mut runner, creature, Zone::Graveyard, &mut events);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Graveyard);
    assert_eq!(runner.state().lki_cache[&creature].controller, P1);

    let drawn =
        enter_creature_and_count_draws(&mut runner, project, P0, "Grizzly Bears", false, P0, false);
    assert_eq!(
        drawn, 0,
        "a P0-owned creature card in your graveyard remains in Guardian Project's pool"
    );
}

/// V-D: the "nontoken" head. A token entering never triggers at all, regardless
/// of its name — this guards the head against accidental widening.
#[test]
fn token_entering_does_not_draw() {
    let (mut runner, project) = setup();
    let drawn = create_token_and_count_draws(&mut runner, project, P0);
    assert_eq!(
        drawn, 0,
        "CR 111.1: the trigger watches NONTOKEN creatures, so a token entering \
         does not trigger even with a unique name"
    );
}

/// CR 400.7 + CR 603.4: "an object that moves from one zone to another becomes a
/// new object with no memory of, or relation to, its previous existence."
///
/// The engine REUSES the storage `ObjectId` across such a move and records the
/// discontinuity as an incarnation bump, so an "another" exclusion keyed on the
/// raw id also excludes whatever later occupies that slot. Blink the entrant
/// before the CR 603.4 resolution recheck and the returning permanent — a
/// DIFFERENT object that happens to share the id — was silently dropped from the
/// reference pool, so a same-named board answered "no same name" and drew.
///
/// The re-entry's own Guardian Project trigger is deliberately NOT collected
/// here. The test isolates the original trigger's CR 603.4 recheck; collecting
/// the re-entry event would add a distinct, valid trigger whose draw is outside
/// this regression's assertion.
#[test]
fn reentered_incarnation_counts_as_another_creature() {
    let (mut runner, project) = setup();

    let entrant = place_creature(&mut runner, P0, "Grizzly Bears", Zone::Hand, false);
    let mut events = Vec::new();
    move_through_pipeline(&mut runner, entrant, Zone::Battlefield, &mut events);
    let before = runner.state().players[P0.0 as usize].hand.len() as i64;

    // Put the ETB trigger on the stack; do NOT resolve it yet.
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    assert!(
        runner.state().stack.iter().any(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == project
            )
        }),
        "CR 603.4 reach guard: Guardian Project's original ETB trigger must be \
         pending on the stack before the blink; otherwise zero draws after the \
         blink would not prove the resolution-time recheck"
    );

    let entry_incarnation = runner.state().objects[&entrant].incarnation;

    // CR 400.7: blink the entrant. Same storage id, new object.
    let mut blink_events = Vec::new();
    move_through_pipeline(&mut runner, entrant, Zone::Exile, &mut blink_events);
    move_through_pipeline(&mut runner, entrant, Zone::Battlefield, &mut blink_events);
    let returned = &runner.state().objects[&entrant];
    assert_eq!(
        returned.zone,
        Zone::Battlefield,
        "the blinked permanent must be back on the battlefield"
    );
    assert_ne!(
        returned.incarnation, entry_incarnation,
        "CR 400.7: a leave + re-entry must bump the incarnation, or this test \
         cannot distinguish the two objects and proves nothing"
    );

    runner.advance_until_stack_empty();
    let drawn = runner.state().players[P0.0 as usize].hand.len() as i64 - before;
    assert_eq!(
        drawn, 0,
        "CR 400.7 + CR 603.4: at the resolution recheck the returned permanent is \
         ANOTHER creature you control sharing the entrant's name, so the \
         intervening-if is false and no card is drawn"
    );
}
