//! Gush: "You may return two Islands you control to their owner's hand rather
//! than pay this spell's mana cost." (CR 118.9, CR 601.2h), through the real
//! cast pipeline with the verbatim Oracle text.
//!
//! Before the counted return-to-hand cost class, the alternative cost lowered
//! to a free `EffectCost { Bounce }`: the spell was cast for nothing and no Island
//! returned. The rows below drive the production cast path (`CastSpell`,
//! `DecideOptionalCost`, `SelectCards`, `ChooseReplacement`) and assert the
//! derived reading: exactly two Islands you control are returned together to
//! their owners' hands, tapped Islands are valid, nothing else is paid, mana
//! already in the pool stays, and Gush then draws two cards.
//!
//! The Oracle text equals the `data/card-data.json` export (MTGJSON-derived) and
//! Scryfall's text for Gush.
//!
//! CR 118.9: alternative costs. CR 601.2h: the total cost is paid. CR 118.3: a
//! player can't pay a cost without the necessary resources to pay it fully.
//! CR 400.3: an object put into a hand goes to its owner's hand. CR 106.4: mana
//! stays in the pool as unspent mana until the end of the step. CR 603.10a:
//! permanents that leave the battlefield simultaneously "look back" at each
//! other. CR 616.1: the affected object's controller chooses among competing
//! replacement effects.

use engine::ai_support::{legal_actions, legal_actions_full};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition,
    TargetFilter, TriggerDefinition, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    ActionResult, CastPaymentMode, GameState, PayCostKind, PendingCostMoveResume, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};
use std::sync::Arc;

const GUSH: &str = "You may return two Islands you control to their owner's hand rather than pay this spell's mana cost.\nDraw two cards.";

fn gush_mana_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 4,
    }
}

/// Gush in P0's hand, `islands` P0 Islands, `plains` P0 Plains, `opp_islands`
/// opposing Islands, and four filler cards on top of P0's library.
struct GushBoard {
    runner: GameRunner,
    gush: ObjectId,
    islands: Vec<ObjectId>,
    plains: Vec<ObjectId>,
    opp_islands: Vec<ObjectId>,
}

fn gush_scenario(
    islands: usize,
    plains: usize,
    opp_islands: usize,
) -> (GameScenario, GushBoardIds) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gush = scenario
        .add_spell_to_hand_from_oracle(P0, "Gush", true, GUSH)
        .with_mana_cost(gush_mana_cost())
        .id();
    let islands: Vec<ObjectId> = (0..islands)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Blue))
        .collect();
    let plains: Vec<ObjectId> = (0..plains)
        .map(|_| scenario.add_basic_land(P0, ManaColor::White))
        .collect();
    let opp_islands: Vec<ObjectId> = (0..opp_islands)
        .map(|_| scenario.add_basic_land(engine::game::scenario::P1, ManaColor::Blue))
        .collect();
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }
    (
        scenario,
        GushBoardIds {
            gush,
            islands,
            plains,
            opp_islands,
        },
    )
}

struct GushBoardIds {
    gush: ObjectId,
    islands: Vec<ObjectId>,
    plains: Vec<ObjectId>,
    opp_islands: Vec<ObjectId>,
}

fn gush_board(islands: usize, plains: usize, opp_islands: usize) -> GushBoard {
    let (scenario, ids) = gush_scenario(islands, plains, opp_islands);
    GushBoard {
        runner: scenario.build(),
        gush: ids.gush,
        islands: ids.islands,
        plains: ids.plains,
        opp_islands: ids.opp_islands,
    }
}

fn cast(
    runner: &mut GameRunner,
    spell: ObjectId,
) -> Result<ActionResult, engine::game::engine::EngineError> {
    let card_id = runner.state().objects[&spell].card_id;
    runner.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
}

/// Cast Gush, accept the alternative cost, and return the `PayCost` prompt's
/// (count, choices). Asserts every step reached its prompt.
fn cast_and_accept_alt(runner: &mut GameRunner, gush: ObjectId) -> (usize, Vec<ObjectId>) {
    let result = cast(runner, gush).expect("cast Gush");
    assert!(
        matches!(result.waiting_for, WaitingFor::OptionalCostChoice { .. }),
        "two Islands must offer the alternative cost, got {:?}",
        result.waiting_for
    );
    let result = runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept the alternative cost");
    match result.waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ReturnToHand,
            choices,
            count,
            ..
        } => (count, choices),
        other => panic!("expected PayCost ReturnToHand, got {other:?}"),
    }
}

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[0].hand.len()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn blue_in_pool(runner: &GameRunner) -> usize {
    runner.state().players[0]
        .mana_pool
        .count_color(ManaType::Blue)
}

fn tap_land_for_mana(runner: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the land offers a mana selection");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tap the land for mana");
}

// ---------------------------------------------------------------------------
// G1: parse. The alternative cost is the counted return, not a free bounce.
// ---------------------------------------------------------------------------

#[test]
fn gush_parses_counted_island_return_alt_cost() {
    let board = gush_board(0, 0, 0);
    let gush = &board.runner.state().objects[&board.gush];
    match gush.casting_options.first().and_then(|o| o.cost.as_ref()) {
        Some(AbilityCost::ReturnToHand {
            count: 2,
            filter: Some(TargetFilter::Typed(tf)),
            from_zone: None,
        }) => {
            assert_eq!(tf.get_subtype(), Some("Island"));
            assert_eq!(
                tf.controller,
                Some(engine::types::ability::ControllerRef::You)
            );
        }
        other => panic!("expected ReturnToHand count 2 Island alt cost, got {other:?}"),
    }
    // Vacuity guard: the draw clause is parsed, not a parser gap.
    assert!(
        !format!("{:?}", gush.abilities).contains("Unimplemented"),
        "Gush's abilities must not contain a parser gap: {:?}",
        gush.abilities
    );
}

// ---------------------------------------------------------------------------
// G2: two Islands you control pay; an opponent's Islands are not choices.
// ---------------------------------------------------------------------------

#[test]
fn gush_returns_two_islands_you_control_and_draws_two() {
    let mut board = gush_board(2, 0, 2);
    let (a, b) = (board.islands[0], board.islands[1]);
    let hand0 = hand_len(&board.runner);
    let library0 = board.runner.state().players[0].library.len();

    let (count, choices) = cast_and_accept_alt(&mut board.runner, board.gush);
    assert_eq!(count, 2);
    let mut sorted = choices.clone();
    sorted.sort();
    assert_eq!(sorted, vec![a, b], "only the two Islands you control");
    for opp in &board.opp_islands {
        assert!(
            !choices.contains(opp),
            "an opponent's Island is not a choice"
        );
    }

    board
        .runner
        .act(GameAction::SelectCards { cards: vec![a, b] })
        .expect("two Islands pay the alternative cost");
    assert_eq!(zone(&board.runner, a), Zone::Hand);
    assert_eq!(zone(&board.runner, b), Zone::Hand);
    assert!(board.runner.state().players[0].hand.contains(&a));
    assert!(board.runner.state().players[0].hand.contains(&b));
    assert_eq!(zone(&board.runner, board.gush), Zone::Stack);
    for opp in &board.opp_islands {
        assert_eq!(zone(&board.runner, *opp), Zone::Battlefield);
    }

    board.runner.advance_until_stack_empty();
    assert_eq!(zone(&board.runner, board.gush), Zone::Graveyard);
    assert_eq!(
        hand_len(&board.runner),
        hand0 - 1 + 2 + 2,
        "Gush left the hand, two Islands returned, two cards drawn"
    );
    assert_eq!(board.runner.state().players[0].library.len(), library0 - 2);
}

// ---------------------------------------------------------------------------
// G3: fewer than two eligible Islands: the alternative cost is not available.
// ---------------------------------------------------------------------------

#[test]
fn gush_with_fewer_than_two_islands_is_refused() {
    // Reach guard: the same cast with two Islands reaches the optional-cost prompt.
    let mut reach = gush_board(2, 0, 0);
    let result = cast(&mut reach.runner, reach.gush).expect("cast with two Islands");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ));

    for (islands, opp_islands) in [(0, 0), (1, 0), (1, 2)] {
        let mut board = gush_board(islands, 0, opp_islands);
        let hand0 = hand_len(&board.runner);
        let refused = cast(&mut board.runner, board.gush);
        assert!(
            matches!(
                refused,
                Err(engine::game::engine::EngineError::ActionNotAllowed(_))
            ),
            "{islands} Island(s) + {opp_islands} opposing: Gush is not free, got {refused:?}"
        );
        assert_eq!(zone(&board.runner, board.gush), Zone::Hand);
        assert_eq!(hand_len(&board.runner), hand0);
        for island in &board.islands {
            assert_eq!(zone(&board.runner, *island), Zone::Battlefield);
        }
    }
}

// ---------------------------------------------------------------------------
// G4: a nonbasic land with the Island subtype counts; a Mountain does not.
// ---------------------------------------------------------------------------

#[test]
fn gush_counts_nonbasic_islands_but_not_other_lands() {
    let (mut scenario, ids) = gush_scenario(1, 0, 0);
    let basic = ids.islands[0];
    let dual = scenario
        .add_land_from_oracle(P0, "{T}: Add {U} or {R}.", "Scratch Dual")
        .with_subtypes(vec!["Island", "Mountain"])
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    let hand0 = hand_len(&runner);

    let (count, choices) = cast_and_accept_alt(&mut runner, ids.gush);
    assert_eq!(count, 2);
    assert!(choices.contains(&basic) && choices.contains(&dual));
    assert!(
        !choices.contains(&mountain),
        "a land without the Island subtype is not a choice"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![basic, dual],
        })
        .expect("a basic and a nonbasic Island pay");
    assert_eq!(zone(&runner, basic), Zone::Hand);
    assert_eq!(zone(&runner, dual), Zone::Hand);
    assert_eq!(zone(&runner, mountain), Zone::Battlefield);
    runner.advance_until_stack_empty();
    assert_eq!(hand_len(&runner), hand0 - 1 + 2 + 2);
}

// ---------------------------------------------------------------------------
// G5: tapped Islands are valid; mana already in the pool stays.
// ---------------------------------------------------------------------------

/// Cast Gush returning both Islands, optionally after tapping them for mana.
/// Returns (blue in pool before the cast, after the payment, after resolution,
/// whether the third land stayed untapped, hand growth).
fn gush_with_islands(tapped: bool) -> (usize, usize, usize, bool, usize) {
    let mut board = gush_board(2, 1, 0);
    let (a, b) = (board.islands[0], board.islands[1]);
    let plains = board.plains[0];
    if tapped {
        tap_land_for_mana(&mut board.runner, a);
        tap_land_for_mana(&mut board.runner, b);
        assert!(board.runner.state().objects[&a].tapped);
        assert!(board.runner.state().objects[&b].tapped);
    }
    let before = blue_in_pool(&board.runner);
    let hand0 = hand_len(&board.runner);

    let (count, choices) = cast_and_accept_alt(&mut board.runner, board.gush);
    assert_eq!(count, 2);
    assert!(
        choices.contains(&a) && choices.contains(&b),
        "tapped or not, both Islands are choices"
    );
    board
        .runner
        .act(GameAction::SelectCards { cards: vec![a, b] })
        .expect("return both Islands");
    assert_eq!(zone(&board.runner, a), Zone::Hand);
    assert_eq!(zone(&board.runner, b), Zone::Hand);
    let after_payment = blue_in_pool(&board.runner);
    board.runner.advance_until_stack_empty();
    let after_resolution = blue_in_pool(&board.runner);
    let plains_untapped = !board.runner.state().objects[&plains].tapped;
    (
        before,
        after_payment,
        after_resolution,
        plains_untapped,
        hand_len(&board.runner) - hand0 + 1,
    )
}

#[test]
fn gush_returns_tapped_islands_and_keeps_the_pooled_mana() {
    let (before, after_payment, after_resolution, plains_untapped, growth) =
        gush_with_islands(true);
    assert_eq!(before, 2, "tapping both Islands pooled two blue mana");
    assert_eq!(after_payment, 2, "the alternative cost owes no mana");
    assert_eq!(after_resolution, 2);
    assert!(plains_untapped, "no mana was auto-tapped for the free cast");
    assert_eq!(growth, 4, "two Islands returned, two cards drawn");

    // Sibling: untapped Islands return the same way and the pool stays empty,
    // so the pooled-mana assertions above can differ.
    let (before, after_payment, after_resolution, plains_untapped, growth) =
        gush_with_islands(false);
    assert_eq!((before, after_payment, after_resolution), (0, 0, 0));
    assert!(plains_untapped);
    assert_eq!(growth, 4);
}

// ---------------------------------------------------------------------------
// G6: declining the alternative cost pays {4}{U}; the Islands stay.
// ---------------------------------------------------------------------------

#[test]
fn gush_declined_alt_cost_pays_the_mana_cost() {
    let mut board = gush_board(2, 3, 0);
    let hand0 = hand_len(&board.runner);
    let result = cast(&mut board.runner, board.gush).expect("cast Gush");
    assert!(
        matches!(result.waiting_for, WaitingFor::OptionalCostChoice { .. }),
        "reach guard: the alternative cost was offered, got {:?}",
        result.waiting_for
    );
    board
        .runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("decline the alternative cost");
    board.runner.advance_until_stack_empty();

    assert_eq!(zone(&board.runner, board.gush), Zone::Graveyard);
    for island in &board.islands {
        assert_eq!(zone(&board.runner, *island), Zone::Battlefield);
        assert!(board.runner.state().objects[island].tapped);
    }
    for plains in &board.plains {
        assert!(board.runner.state().objects[plains].tapped);
    }
    assert_eq!(hand_len(&board.runner), hand0 - 1 + 2);
}

// ---------------------------------------------------------------------------
// G7: one Island cannot pay a count-2 cost twice.
// ---------------------------------------------------------------------------

#[test]
fn gush_refuses_the_same_island_twice() {
    let mut board = gush_board(2, 0, 0);
    let (a, b) = (board.islands[0], board.islands[1]);
    let (count, _) = cast_and_accept_alt(&mut board.runner, board.gush);
    assert_eq!(count, 2);

    let refused = board
        .runner
        .act(GameAction::SelectCards { cards: vec![a, a] });
    assert!(refused.is_err(), "[a, a] must not pay a count-2 cost");
    assert_eq!(zone(&board.runner, a), Zone::Battlefield);
    assert_eq!(zone(&board.runner, b), Zone::Battlefield);
    assert_ne!(
        zone(&board.runner, board.gush),
        Zone::Stack,
        "the unpaid cast has not put Gush on the stack"
    );
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::ReturnToHand,
            count: 2,
            ..
        }
    ));

    // Same prompt, distinct Islands: the payment goes through.
    board
        .runner
        .act(GameAction::SelectCards { cards: vec![a, b] })
        .expect("two distinct Islands pay");
    assert_eq!(zone(&board.runner, a), Zone::Hand);
    assert_eq!(zone(&board.runner, b), Zone::Hand);
    assert_eq!(zone(&board.runner, board.gush), Zone::Stack);
}

// ---------------------------------------------------------------------------
// G8: the AI / multiplayer candidate generator offers the two-Island selection.
// ---------------------------------------------------------------------------

#[test]
fn gush_count_two_prompt_has_one_two_island_candidate() {
    let mut board = gush_board(2, 0, 0);
    let (a, b) = (board.islands[0], board.islands[1]);
    let (count, _) = cast_and_accept_alt(&mut board.runner, board.gush);
    assert_eq!(count, 2);

    let selections: Vec<Vec<ObjectId>> = legal_actions(board.runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::SelectCards { cards } => Some(cards),
            _ => None,
        })
        .collect();
    assert!(
        !selections.is_empty(),
        "reach guard: the prompt yields SelectCards candidates"
    );
    for cards in &selections {
        assert_eq!(cards.len(), 2, "no SelectCards of another length");
    }
    assert!(selections.iter().any(|cards| {
        let mut sorted = cards.clone();
        sorted.sort();
        sorted == vec![a, b]
    }));
}

// ---------------------------------------------------------------------------
// G10: the group stays one group across a replacement-choice pause.
// ---------------------------------------------------------------------------

/// "If a land would be put into a hand from anywhere, exile it instead",
/// scoped to one object by the caller.
fn redirect_land_to_exile(id: ObjectId) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Hand)
        .valid_card(TargetFilter::SpecificObject { id })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
}

/// A leaves-the-battlefield observer on `host`: whenever a land leaves the
/// battlefield, its controller gains 1 life. The host departs with the group,
/// so it sees its co-departed sibling only through the CR 603.10a look-back.
fn attach_land_departure_observer(runner: &mut GameRunner, host: ObjectId) {
    let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)))
        .origin(Zone::Battlefield)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ));
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&host)
        .expect("observer host exists");
    obj.trigger_definitions.push(trigger.clone());
    Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
}

/// The battlefield departures of `ids` in the persisted per-turn records, as
/// (object, co_departed), ordered by object id.
fn departure_records(state: &GameState, ids: &[ObjectId]) -> Vec<(ObjectId, Vec<ObjectId>)> {
    let mut records: Vec<(ObjectId, Vec<ObjectId>)> = state
        .zone_changes_this_turn
        .iter()
        .filter(|record| {
            record.from_zone == Some(Zone::Battlefield) && ids.contains(&record.object_id)
        })
        .map(|record| (record.object_id, record.co_departed.clone()))
        .collect();
    records.sort();
    records
}

/// (object, co_departed) of every battlefield departure of `ids` in `events`.
fn event_departures(events: &[GameEvent], ids: &[ObjectId]) -> Vec<(ObjectId, Vec<ObjectId>)> {
    let mut departures: Vec<(ObjectId, Vec<ObjectId>)> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                record,
                ..
            } if ids.contains(object_id) => Some((*object_id, record.co_departed.clone())),
            _ => None,
        })
        .collect();
    departures.sort();
    departures
}

#[derive(Clone, Copy)]
enum Redirected {
    None,
    First,
    Second,
    Both,
}

struct PauseRun {
    runner: GameRunner,
    a: ObjectId,
    b: ObjectId,
    gush: ObjectId,
    life0: i32,
    hand0: usize,
    pauses: usize,
    last_action_events: Vec<GameEvent>,
}

/// Gush with two Islands; each redirected Island carries two competing
/// replacements, so returning it pauses for a CR 616.1 choice. Stops right
/// after the first `ChooseReplacement` of the run when `stop_at_second_pause`
/// and a second pause follows.
fn run_gush_pause(redirected: Redirected, stop_at_second_pause: bool) -> PauseRun {
    let (mut scenario, ids) = gush_scenario(2, 0, 0);
    let (a, b) = (ids.islands[0], ids.islands[1]);
    let scoped: Vec<ObjectId> = match redirected {
        Redirected::None => vec![],
        Redirected::First => vec![a],
        Redirected::Second => vec![b],
        Redirected::Both => vec![a, b],
    };
    for id in &scoped {
        for name in ["First Redirect", "Second Redirect"] {
            scenario
                .add_creature(P0, &format!("{name} {}", id.0), 0, 0)
                .as_enchantment()
                .with_replacement_definition(redirect_land_to_exile(*id));
        }
    }
    let mut runner = scenario.build();
    attach_land_departure_observer(&mut runner, a);
    let life0 = runner.life(P0);
    let hand0 = hand_len(&runner);

    let (count, _) = cast_and_accept_alt(&mut runner, ids.gush);
    assert_eq!(count, 2);
    let mut last_action_events = runner
        .act(GameAction::SelectCards { cards: vec![a, b] })
        .expect("select both Islands")
        .events;
    let mut pauses = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        pauses += 1;
        // Reach guard: the paused payment is parked on the typed Cast root.
        assert!(
            matches!(
                runner.state().pending_cost_move_resume,
                Some(PendingCostMoveResume::Cast { .. })
            ),
            "pause {pauses}: the payment must be parked on a Cast root, got {:?}",
            runner.state().pending_cost_move_resume
        );
        if stop_at_second_pause && pauses == 2 {
            break;
        }
        last_action_events = runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("choose the replacement")
            .events;
    }
    PauseRun {
        runner,
        a,
        b,
        gush: ids.gush,
        life0,
        hand0,
        pauses,
        last_action_events,
    }
}

fn assert_group_survived_pause(mut run: PauseRun, expected_pauses: usize, redirected: Redirected) {
    let (a, b) = (run.a, run.b);
    assert_eq!(run.pauses, expected_pauses, "pause count (reach guard)");
    assert_eq!(zone(&run.runner, run.gush), Zone::Stack);

    // The delivered departures of the final action carry the whole group.
    let final_departures = event_departures(&run.last_action_events, &[a, b]);
    assert!(
        !final_departures.is_empty(),
        "reach guard: the final action delivered a departure"
    );
    for (id, co_departed) in &final_departures {
        let other = if *id == a { b } else { a };
        assert_eq!(co_departed, &vec![other], "{id:?} names the other Island");
    }

    // The persisted per-turn records are mutual too.
    assert_eq!(
        departure_records(run.runner.state(), &[a, b]),
        vec![(a, vec![b]), (b, vec![a])]
    );

    run.runner.advance_until_stack_empty();
    let (a_zone, b_zone) = match redirected {
        Redirected::None => (Zone::Hand, Zone::Hand),
        Redirected::First => (Zone::Exile, Zone::Hand),
        Redirected::Second => (Zone::Hand, Zone::Exile),
        Redirected::Both => (Zone::Exile, Zone::Exile),
    };
    assert_eq!(zone(&run.runner, a), a_zone);
    assert_eq!(zone(&run.runner, b), b_zone);
    assert_eq!(
        run.runner.life(P0) - run.life0,
        2,
        "CR 603.10a: the observer on a sees both Islands leave"
    );
    assert_eq!(zone(&run.runner, run.gush), Zone::Graveyard);
    let returned_to_hand = [a_zone, b_zone]
        .iter()
        .filter(|zone| **zone == Zone::Hand)
        .count();
    assert_eq!(
        hand_len(&run.runner),
        run.hand0 - 1 + returned_to_hand + 2,
        "Gush resolved and drew two"
    );
}

#[test]
fn gush_group_survives_a_pause_on_the_first_island() {
    assert_group_survived_pause(
        run_gush_pause(Redirected::First, false),
        1,
        Redirected::First,
    );
}

#[test]
fn gush_group_survives_a_pause_on_the_second_island() {
    assert_group_survived_pause(
        run_gush_pause(Redirected::Second, false),
        1,
        Redirected::Second,
    );
}

#[test]
fn gush_group_survives_a_pause_on_both_islands() {
    assert_group_survived_pause(run_gush_pause(Redirected::Both, false), 2, Redirected::Both);
}

/// G10d: the second pause is serialized and restored mid-payment.
#[test]
fn gush_group_survives_a_serialized_second_pause() {
    let run = run_gush_pause(Redirected::Both, true);
    assert_eq!(run.pauses, 2, "stopped at the second pause");
    let (a, b, gush, life0) = (run.a, run.b, run.gush, run.life0);

    let json = serde_json::to_string(run.runner.state()).expect("serialize the paused state");
    assert!(
        json.contains("deferred_cost_events"),
        "reach guard: the Cast root carries a non-empty ledger"
    );
    let restored: GameState = serde_json::from_str(&json).expect("deserialize the paused state");
    let mut runner = GameRunner::from_state(restored);
    let events = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("finish the restored payment")
        .events;
    for (id, co_departed) in event_departures(&events, &[a, b]) {
        let other = if id == a { b } else { a };
        assert_eq!(co_departed, vec![other]);
    }
    assert_eq!(
        departure_records(runner.state(), &[a, b]),
        vec![(a, vec![b]), (b, vec![a])]
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0) - life0, 2);
    assert_eq!(zone(&runner, gush), Zone::Graveyard);
}

/// G10e: the unpaused sibling reaches the same totals.
#[test]
fn gush_group_without_a_pause_reaches_the_same_totals() {
    let run = run_gush_pause(Redirected::None, false);
    assert_group_survived_pause(run, 0, Redirected::None);
}

// ---------------------------------------------------------------------------
// U-B: preservation of the unless-side "sacrifice it unless you return N lands".
// Bull Elephant and Ovinomancer keep their prior behavior: paying returns the
// lands one prompt at a time (CR 118.12), declining or being unable to pay
// sacrifices the creature. A preservation row (green at BASE and here).
// CR 118.12: "unless" a player pays a cost. CR 701.21a: sacrifice.
// ---------------------------------------------------------------------------

const BULL_ELEPHANT: &str = "When this creature enters, sacrifice it unless you return two Forests you control to their owner's hand.";
const OVINOMANCER: &str = "When this creature enters, sacrifice it unless you return three basic lands you control to their owner's hand.\n{T}, Return this creature to its owner's hand: Destroy target creature. It can't be regenerated. That creature's controller creates a 0/1 green Sheep creature token.";

struct UnlessReturnBoard {
    runner: GameRunner,
    creature: ObjectId,
    lands: Vec<ObjectId>,
}

/// Cast `name` from P0's hand with its cost pooled (so the lands stay on the
/// battlefield, untouched) and `lands` basic lands of `land_color`, then pass
/// priority until the enters trigger parks the `UnlessPayment` prompt.
fn unless_return_board(
    name: &str,
    oracle: &str,
    (power, toughness): (i32, i32),
    (shard, generic): (ManaCostShard, u32),
    lands: usize,
    land_color: ManaColor,
) -> UnlessReturnBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, name, power, toughness, oracle)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![shard],
            generic,
        })
        .id();
    let lands: Vec<ObjectId> = (0..lands)
        .map(|_| scenario.add_basic_land(P0, land_color))
        .collect();
    let mut runner = scenario.build();
    let pooled = if shard == ManaCostShard::Green {
        ManaType::Green
    } else {
        ManaType::Blue
    };
    for _ in 0..=generic {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(P0, ManaUnit::new(pooled, ObjectId(0), false, vec![]));
    }
    cast(&mut runner, creature).expect("cast the creature");
    for _ in 0..6 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority toward the enters trigger");
    }
    // Reach guard: the prompt is parked, the creature is on the battlefield and
    // every land is still there.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the enters trigger must park the unless-payment prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone(&runner, creature), Zone::Battlefield);
    for land in &lands {
        assert_eq!(zone(&runner, *land), Zone::Battlefield);
    }
    UnlessReturnBoard {
        runner,
        creature,
        lands,
    }
}

fn bull_elephant_board(forests: usize) -> UnlessReturnBoard {
    unless_return_board(
        "Bull Elephant",
        BULL_ELEPHANT,
        (4, 4),
        (ManaCostShard::Green, 3),
        forests,
        ManaColor::Green,
    )
}

/// Answer one `UnlessBounceChoice` prompt with `pick`; returns the `remaining`
/// count the prompt carried and the choices it offered.
fn answer_unless_bounce(runner: &mut GameRunner, pick: ObjectId) -> (u32, Vec<ObjectId>) {
    let (remaining, permanents) = match &runner.state().waiting_for {
        WaitingFor::UnlessBounceChoice {
            remaining,
            permanents,
            ..
        } => (*remaining, permanents.clone()),
        other => panic!("expected UnlessBounceChoice, got {other:?}"),
    };
    runner
        .act(GameAction::SelectCards { cards: vec![pick] })
        .expect("return one land");
    (remaining, permanents)
}

#[test]
fn bull_elephant_pays_by_returning_two_forests_one_prompt_at_a_time() {
    let mut board = bull_elephant_board(2);
    let (a, b) = (board.lands[0], board.lands[1]);
    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choose to pay");
    // First prompt: both Forests offered, two still owed.
    let (remaining, offered) = answer_unless_bounce(&mut board.runner, a);
    assert_eq!(remaining, 2);
    assert!(offered.contains(&a) && offered.contains(&b));
    assert_eq!(zone(&board.runner, a), Zone::Hand);
    assert_eq!(zone(&board.runner, b), Zone::Battlefield);
    // Second, sequential prompt: the returned Forest is gone from the choices.
    match &board.runner.state().waiting_for {
        WaitingFor::UnlessBounceChoice {
            remaining,
            permanents,
            ..
        } => {
            assert_eq!(*remaining, 1);
            assert_eq!(permanents, &vec![b]);
        }
        other => panic!("expected a second UnlessBounceChoice, got {other:?}"),
    }
    answer_unless_bounce(&mut board.runner, b);
    board.runner.advance_until_stack_empty();
    assert_eq!(zone(&board.runner, a), Zone::Hand);
    assert_eq!(zone(&board.runner, b), Zone::Hand);
    assert_eq!(zone(&board.runner, board.creature), Zone::Battlefield);
}

#[test]
fn bull_elephant_declined_is_sacrificed_and_keeps_the_forests() {
    let mut board = bull_elephant_board(2);
    board
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline to pay");
    board.runner.advance_until_stack_empty();
    assert_eq!(zone(&board.runner, board.creature), Zone::Graveyard);
    for forest in &board.lands {
        assert_eq!(zone(&board.runner, *forest), Zone::Battlefield);
    }
}

#[test]
fn bull_elephant_with_one_forest_cannot_pay_and_is_sacrificed() {
    let mut board = bull_elephant_board(1);
    let forest = board.lands[0];
    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempt to pay");
    assert!(
        !matches!(
            board.runner.state().waiting_for,
            WaitingFor::UnlessBounceChoice { .. }
        ),
        "one Forest cannot pay a two-land cost: no return prompt"
    );
    board.runner.advance_until_stack_empty();
    assert_eq!(zone(&board.runner, board.creature), Zone::Graveyard);
    assert_eq!(zone(&board.runner, forest), Zone::Battlefield);
}

#[test]
fn ovinomancer_pay_is_optional_and_returns_three_basic_lands() {
    let ovinomancer = || {
        unless_return_board(
            "Ovinomancer",
            OVINOMANCER,
            (0, 1),
            (ManaCostShard::Blue, 2),
            3,
            ManaColor::Green,
        )
    };
    // Decline: sacrificed, lands stay.
    let mut declined = ovinomancer();
    declined
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline to pay");
    declined.runner.advance_until_stack_empty();
    assert_eq!(zone(&declined.runner, declined.creature), Zone::Graveyard);
    for land in &declined.lands {
        assert_eq!(zone(&declined.runner, *land), Zone::Battlefield);
    }
    // Pay: three sequential prompts (remaining 3, 2, 1), all three to hand.
    let mut paid = ovinomancer();
    paid.runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choose to pay");
    let mut remainings = Vec::new();
    for land in paid.lands.clone() {
        remainings.push(answer_unless_bounce(&mut paid.runner, land).0);
    }
    assert_eq!(remainings, vec![3, 2, 1]);
    paid.runner.advance_until_stack_empty();
    for land in &paid.lands {
        assert_eq!(zone(&paid.runner, *land), Zone::Hand);
    }
    assert_eq!(zone(&paid.runner, paid.creature), Zone::Battlefield);
}
