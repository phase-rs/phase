//! Psychic Vortex — cumulative upkeep paid by drawing cards.
//!
//! "Cumulative upkeep—Draw a card." CR 702.24a: at the beginning of the
//! controller's upkeep an age counter goes on the permanent, then the controller
//! may pay "draw a card" once for each age counter, or sacrifice it. CR 702.24a
//! supplies the repetition — one payment of the printed "draw a card" per age
//! counter — and CR 121.2a governs instruction-level replacement of each such
//! instruction (Alms Collector's ruling: count the word "draw"). The second
//! ability, "At the beginning of your end step, sacrifice a land and discard your
//! hand", keeps working.
//!
//! Every row drives the real upkeep trigger through `PayUnlessCost` /
//! `ChooseReplacement`, and every negative row carries a positive reach guard in
//! the same test.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, Effect, EffectKind, ManaContribution, ManaProduction, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, PendingCostMoveResume, UnpaidCostSuffix, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PSYCHIC_VORTEX_ORACLE: &str = "Cumulative upkeep\u{2014}Draw a card. (At the beginning of \
your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep \
cost for each age counter on it.)\nAt the beginning of your end step, sacrifice a land and \
discard your hand.";

const ALMS_COLLECTOR_ORACLE: &str = "Flash\nIf an opponent would draw two or more cards, \
instead you and that player each draw a card.";

const QUANTUM_RIDDLER_ORACLE: &str = "Flying\nWhen this creature enters, draw a card.\nAs long \
as you have one or fewer cards in hand, if you would draw one or more cards, you draw that many \
cards plus one instead.\nWarp {1}{U}";

const MARALEN_ORACLE: &str = "Players can't draw cards.\nAt the beginning of each player's draw \
step, that player loses 3 life, searches their library for a card, puts it into their hand, \
then shuffles.";

const SPIRIT_OF_THE_LABYRINTH_ORACLE: &str = "Each player can't draw more than one card each turn.";

const OBSTINATE_FAMILIAR_ORACLE: &str = "If you would draw a card, you may skip that draw instead.";

const STINKWEED_IMP_ORACLE: &str = "Flying\nWhenever this creature deals combat damage to a \
creature, destroy that creature.\nDredge 5 (If you would draw a card, you may mill five cards \
instead. If you do, return this card from your graveyard to your hand.)";

const POSSESSED_PORTAL_ORACLE: &str = "If a player would draw a card, that player skips that draw \
instead.\nAt the beginning of each end step, each player sacrifices a permanent of their choice \
unless they discard a card.";

const DIVINATION_ORACLE: &str = "Draw two cards.";

const SOLEMNITY_ORACLE: &str = "Players can't get counters.\n\
Counters can't be put on artifacts, creatures, enchantments, or lands.";

/// NOT a printed card: the parser-produced Draw-N cumulative-upkeep base, used to pin
/// instruction size separately from repetition.
const DRAW_TWO_VORTEX_ORACLE: &str =
    "Cumulative upkeep\u{2014}Draw two cards. (At the beginning of \
your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep \
cost for each age counter on it.)";

/// Enough cards that every row's draws, and Dredge 5, stay legal.
const STAGED_LIBRARY: [&str; 12] = [
    "Library 1",
    "Library 2",
    "Library 3",
    "Library 4",
    "Library 5",
    "Library 6",
    "Library 7",
    "Library 8",
    "Library 9",
    "Library 10",
    "Library 11",
    "Library 12",
];

/// ReplacementChoice index convention shared with the draw-replacement tests:
/// `0` applies the replacement (Dredge, Obstinate Familiar's skip), `1` declines.
const APPLY_REPLACEMENT: usize = 0;
const DECLINE_REPLACEMENT: usize = 1;

/// One player's hand, library and graveyard sizes at a point in a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ZoneSizes {
    hand: usize,
    library: usize,
    graveyard: usize,
}

impl ZoneSizes {
    fn of(runner: &GameRunner, player: PlayerId) -> Self {
        let player_state = &runner.state().players[player.0 as usize];
        Self {
            hand: player_state.hand.len(),
            library: player_state.library.len(),
            graveyard: player_state.graveyard.len(),
        }
    }
}

/// One `draws`-card draw instruction by the controller.
fn draw_cost(draws: i32) -> AbilityCost {
    AbilityCost::EffectCost {
        effect: Box::new(Effect::Draw {
            count: QuantityExpr::Fixed { value: draws },
            target: TargetFilter::Controller,
        }),
    }
}

/// CR 702.24a: the upkeep cost after expansion — the printed `size`-card instruction once per age counter.
fn upkeep_draw_cost(age_counters: usize, size: i32) -> AbilityCost {
    AbilityCost::Composite {
        costs: vec![draw_cost(size); age_counters],
    }
}

/// P0's untap step with the cumulative-upkeep enchantment `name` (Oracle text
/// `oracle`) on the battlefield carrying `age_counters_before` age counters, P0's
/// library staged with `library`, and whatever `setup` adds. The upkeep tick adds
/// one more age counter.
fn upkeep_board<Fixture>(
    name: &str,
    oracle: &str,
    age_counters_before: u32,
    library: &[&str],
    setup: impl FnOnce(&mut GameScenario) -> Fixture,
) -> (GameRunner, ObjectId, Fixture) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.with_library_top(P0, library);
    let enchantment = scenario.add_enchantment_from_oracle(P0, name, oracle).id();
    let fixture = setup(&mut scenario);
    let mut runner = scenario.build();
    if age_counters_before > 0 {
        runner
            .state_mut()
            .objects
            .get_mut(&enchantment)
            .expect("the cumulative-upkeep enchantment exists")
            .counters
            .insert(CounterType::Age, age_counters_before);
    }
    (runner, enchantment, fixture)
}

/// `upkeep_board` with Psychic Vortex itself.
fn vortex_board<Fixture>(
    age_counters_before: u32,
    library: &[&str],
    setup: impl FnOnce(&mut GameScenario) -> Fixture,
) -> (GameRunner, ObjectId, Fixture) {
    upkeep_board(
        "Psychic Vortex",
        PSYCHIC_VORTEX_ORACLE,
        age_counters_before,
        library,
        setup,
    )
}

/// Advances from the untap step through the cumulative-upkeep trigger. A
/// payment prompt stops the advance, so a board that never shows one has
/// already settled the trigger.
fn advance_through_upkeep(runner: &mut GameRunner) {
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();
}

/// Reach guard: the upkeep is waiting on P0's unless payment; returns the offered cost so a test can compare it
/// after its behavioural rows.
fn unless_prompt_cost(runner: &GameRunner) -> AbilityCost {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, P0, "Psychic Vortex's controller pays its upkeep");
            cost.clone()
        }
        other => panic!("expected Psychic Vortex's cumulative-upkeep prompt, got {other:?}"),
    }
}

/// Reach guard: the upkeep offered P0 the expanded draw cost (CR 702.24a: the printed instruction once per age
/// counter).
fn assert_draw_cost_prompt(runner: &GameRunner, age_counters: usize, size: i32) {
    assert_eq!(
        unless_prompt_cost(runner),
        upkeep_draw_cost(age_counters, size),
        "CR 702.24a: the printed instruction once per age counter"
    );
}

/// CR 118.12: the paid epilogue ran — the cumulative-upkeep ability (whose
/// guarded effect is the sacrifice) finished resolving.
fn upkeep_ability_resolved(events: &[GameEvent], vortex: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Sacrifice,
                source_id,
                ..
            } if *source_id == vortex
        )
    })
}

/// The unpaid suffix of the parked unless-payment. Panics when none is parked.
fn parked_unpaid_suffix(runner: &GameRunner) -> Option<UnpaidCostSuffix> {
    match &runner.state().pending_cost_move_resume {
        Some(PendingCostMoveResume::CounterAdditionUnlessPayment { unpaid_suffix, .. }) => {
            unpaid_suffix.as_deref().cloned()
        }
        other => panic!("expected a parked unless-payment, got {other:?}"),
    }
}

fn assert_replacement_choice(runner: &GameRunner, context: &str) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P0, .. }
        ),
        "{context}, got {:?}",
        runner.state().waiting_for
    );
}

fn zone_of(runner: &GameRunner, object: ObjectId) -> Zone {
    runner.state().objects[&object].zone
}

/// NOT a printed card: the hostile shape the deterministic unless contract admits (a draw, then a counter
/// on the source, then fixed red mana), used to pin that a counter leg prevented after the choice to pay
/// still lets every later leg pay.
fn draw_counter_mana_cost() -> AbilityCost {
    AbilityCost::Composite {
        costs: vec![
            draw_cost(1),
            AbilityCost::EffectCost {
                effect: Box::new(Effect::PutCounter {
                    counter_type: CounterType::Generic("probe".to_string()),
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                }),
            },
            AbilityCost::EffectCost {
                effect: Box::new(Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Red],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                }),
            },
        ],
    }
}

/// Replaces the synthesized cumulative-upkeep unless cost of `source` with `cost`. Reach guard: exactly one
/// unless slot exists, so the install cannot silently miss the trigger the test drives.
fn install_upkeep_cost(runner: &mut GameRunner, source: ObjectId, cost: AbilityCost) {
    let object = runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("the cumulative-upkeep source exists");
    let mut installed = 0;
    let unless_slots = Arc::make_mut(&mut object.base_trigger_definitions)
        .iter_mut()
        .filter_map(|trigger| trigger.execute.as_mut())
        .filter_map(|execute| execute.sub_ability.as_mut())
        .filter_map(|branch| branch.unless_pay.as_mut());
    for unless in unless_slots {
        unless.cost = cost.clone();
        installed += 1;
    }
    assert_eq!(
        installed, 1,
        "exactly one synthesized cumulative-upkeep unless cost"
    );
    object.materialize_base_trigger_definitions();
}

/// Psychic Vortex with the mixed cost installed, Stinkweed Imp in P0's graveyard (its Dredge pauses the
/// draw leg), and Solemnity staged in exile. Advanced to the upkeep prompt, which offers the installed cost.
fn mixed_cost_board() -> (GameRunner, ObjectId, ObjectId) {
    let (mut runner, vortex, solemnity) = vortex_board(0, &STAGED_LIBRARY, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
        scenario
            .add_enchantment_from_oracle(P1, "Solemnity", SOLEMNITY_ORACLE)
            .id()
    });
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), solemnity, Zone::Exile, &mut events);
    install_upkeep_cost(&mut runner, vortex, draw_counter_mana_cost());
    advance_through_upkeep(&mut runner);
    assert_eq!(
        unless_prompt_cost(&runner),
        draw_counter_mana_cost(),
        "the upkeep offers the installed mixed cost"
    );
    (runner, vortex, solemnity)
}

/// CR 702.24a (AC2): with N age counters, paying draws N cards and keeps the
/// permanent. Rows: N = 2 (one age counter already on it) and N = 1.
#[test]
fn paying_draws_a_card_per_age_counter_and_keeps_vortex() {
    for (age_counters_before, draws) in [(1u32, 2usize), (0, 1)] {
        let (mut runner, vortex, ()) = vortex_board(age_counters_before, &STAGED_LIBRARY, |_| ());
        advance_through_upkeep(&mut runner);
        assert_draw_cost_prompt(&runner, draws, 1);
        let before = ZoneSizes::of(&runner, P0);

        let result = runner
            .act(GameAction::PayUnlessCost { pay: true })
            .expect("drawing to pay the cumulative upkeep is legal");

        let after = ZoneSizes::of(&runner, P0);
        assert_eq!(after.hand, before.hand + draws, "one card per age counter");
        assert_eq!(after.library, before.library - draws);
        assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&vortex]
                .counters
                .get(&CounterType::Age),
            Some(&(draws as u32))
        );
        assert!(
            upkeep_ability_resolved(&result.events, vortex),
            "the paid upkeep must finish resolving, got {:?}",
            result.events
        );
        assert!(runner.state().pending_cost_move_resume.is_none());
    }
}

/// CR 702.24a (AC2): declining the payment sacrifices Psychic Vortex.
#[test]
fn declining_sacrifices_vortex_without_drawing() {
    let (mut runner, vortex, ()) = vortex_board(1, &STAGED_LIBRARY, |_| ());
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining is legal");

    assert_eq!(zone_of(&runner, vortex), Zone::Graveyard);
    assert_eq!(ZoneSizes::of(&runner, P0).hand, before.hand);
}

/// CR 121.3 + CR 121.4 + CR 704.5b: an empty library does not stop the draw
/// payment (Psychic Vortex ruling). The payment is made, so Psychic Vortex is
/// not sacrificed, and P0 loses the game at the state-based action check after
/// the ability finishes resolving. Rows: N = 1 from an empty library, and N = 2
/// with one card left (draws it, then attempts the second draw).
///
/// The engine's elimination cleanup then exiles the losing player's objects, so
/// Psychic Vortex's survival is read from the event order rather than its final
/// zone: the paid ability finishes resolving while it is still on the
/// battlefield, and it never moves to the graveyard.
#[test]
fn paying_from_a_short_library_keeps_vortex_and_loses_the_game() {
    for (age_counters_before, library) in [(0u32, &[][..]), (1, &["Only Card"][..])] {
        let (mut runner, vortex, ()) = vortex_board(age_counters_before, library, |_| ());
        advance_through_upkeep(&mut runner);
        assert_draw_cost_prompt(&runner, age_counters_before as usize + 1, 1);

        let result = runner
            .act(GameAction::PayUnlessCost { pay: true })
            .expect("CR 121.3: an empty library does not forbid choosing to draw");

        let cards_drawn = result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
            .count();
        assert_eq!(
            cards_drawn,
            library.len(),
            "every card the library held is drawn"
        );
        let paid_epilogue = result
            .events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Sacrifice,
                        source_id,
                        ..
                    } if *source_id == vortex
                )
            })
            .expect("the paid cumulative upkeep must finish resolving");
        let vortex_moves: Vec<(usize, Zone)> = result
            .events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                GameEvent::ZoneChanged { object_id, to, .. } if *object_id == vortex => {
                    Some((index, *to))
                }
                _ => None,
            })
            .collect();
        assert!(
            vortex_moves
                .iter()
                .all(|(index, to)| *index > paid_epilogue && *to != Zone::Graveyard),
            "Psychic Vortex must stay on the battlefield through the paid upkeep, got {vortex_moves:?}"
        );
        assert!(
            runner.state().players[P0.0 as usize].is_eliminated,
            "CR 704.5b: drawing from an empty library loses the game, got {:?}",
            runner.state().waiting_for
        );
    }
}

/// CR 616.1 + CR 118.11 + CR 118.12: Dredge pauses the first of two draw
/// instructions. The second instruction stays owed across the pause, is issued
/// once the first completes, and parks again on its own Dredge choice. Replaced
/// draws still pay the cost (Psychic Vortex ruling).
#[test]
fn a_dredge_pause_carries_the_unissued_draw_and_parks_it_again() {
    let (mut runner, vortex, imp) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE)
            .id()
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");
    assert_replacement_choice(&runner, "Dredge must pause the first draw instruction");
    assert_eq!(
        parked_unpaid_suffix(&runner),
        Some(UnpaidCostSuffix {
            payer: P0,
            cost: draw_cost(1),
        }),
        "the second instruction is owed, not issued"
    );

    runner
        .act(GameAction::ChooseReplacement {
            index: DECLINE_REPLACEMENT,
        })
        .expect("declining Dredge is legal");
    assert_replacement_choice(
        &runner,
        "the second instruction must be issued and pause on its own Dredge choice",
    );
    assert_eq!(
        parked_unpaid_suffix(&runner),
        None,
        "the park now owes nothing beyond the paused instruction"
    );

    let result = runner
        .act(GameAction::ChooseReplacement {
            index: APPLY_REPLACEMENT,
        })
        .expect("applying Dredge is legal");

    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(
        zone_of(&runner, imp),
        Zone::Hand,
        "Dredge returned Stinkweed Imp"
    );
    assert_eq!(
        after.hand,
        before.hand + 2,
        "one drawn card plus Stinkweed Imp"
    );
    assert_eq!(
        after.library,
        before.library - 1 - 5,
        "one draw plus Dredge 5"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(upkeep_ability_resolved(&result.events, vortex));
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().active_draw_sequence().is_none());
}

/// Sub-case of the row above: declining Dredge on both instructions draws both.
#[test]
fn declining_dredge_on_both_draws_draws_two_cards() {
    let (mut runner, vortex, ()) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");
    for instruction in 1..=2 {
        assert_replacement_choice(&runner, &format!("Dredge pauses draw {instruction}"));
        runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining Dredge is legal");
    }

    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(after.hand, before.hand + 2);
    assert_eq!(after.library, before.library - 2);
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(runner.state().pending_cost_move_resume.is_none());
}

/// CR 614.1b + CR 118.11 + CR 118.12: Obstinate Familiar's optional skip settles
/// through the replacement handler's prevented boundary. A skipped draw still
/// pays, the next instruction is issued after it, and the park settles PAID only
/// once the last instruction completes.
#[test]
fn a_skipped_draw_still_pays_and_the_next_draw_follows() {
    let (mut runner, vortex, ()) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(
            P0,
            "Obstinate Familiar",
            1,
            1,
            OBSTINATE_FAMILIAR_ORACLE,
        );
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");
    assert_replacement_choice(&runner, "the skip is offered on the first draw");
    assert!(parked_unpaid_suffix(&runner).is_some());

    runner
        .act(GameAction::ChooseReplacement {
            index: APPLY_REPLACEMENT,
        })
        .expect("skipping the draw is legal");
    assert_eq!(
        ZoneSizes::of(&runner, P0).library,
        before.library,
        "the first draw was skipped"
    );
    assert_replacement_choice(&runner, "the second draw is issued and offers its skip");
    assert_eq!(parked_unpaid_suffix(&runner), None);

    let result = runner
        .act(GameAction::ChooseReplacement {
            index: DECLINE_REPLACEMENT,
        })
        .expect("drawing normally is legal");

    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(after.hand, before.hand + 1);
    assert_eq!(after.library, before.library - 1);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(upkeep_ability_resolved(&result.events, vortex));
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(runner.state().active_draw_sequence().is_none());
}

/// Sub-case: skipping both draws draws nothing, yet the cost is paid (CR 118.11).
/// Possessed Portal's mandatory skip pays the same way without any prompt.
#[test]
fn skipping_every_draw_still_pays_the_cost() {
    let (mut runner, vortex, ()) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(
            P0,
            "Obstinate Familiar",
            1,
            1,
            OBSTINATE_FAMILIAR_ORACLE,
        );
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");
    let mut events = Vec::new();
    for draw in 1..=2 {
        assert_replacement_choice(&runner, &format!("draw {draw} offers its skip"));
        let result = runner
            .act(GameAction::ChooseReplacement {
                index: APPLY_REPLACEMENT,
            })
            .expect("skipping is legal");
        events.extend(result.events);
    }

    assert_eq!(ZoneSizes::of(&runner, P0), before);
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(upkeep_ability_resolved(&events, vortex));
    assert!(runner.state().pending_cost_move_resume.is_none());

    let (mut runner, vortex, ()) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario.add_artifact_from_oracle(P1, "Possessed Portal", POSSESSED_PORTAL_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);

    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("a skip replacement does not forbid paying");

    assert_eq!(
        ZoneSizes::of(&runner, P0),
        before,
        "both draws were skipped"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(upkeep_ability_resolved(&result.events, vortex));
    assert!(runner.state().pending_cost_move_resume.is_none());
}

/// CR 121.2 + CR 614.11a: a skipped draw inside a two-draw instruction finishes
/// that instruction before the next instruction is issued. Quantum Riddler turns
/// the first one-card instruction into two draws (P0's hand is empty), so
/// skipping its first draw leaves its second still to come — and the second
/// instruction must still be owed, not issued on top of the unfinished one.
#[test]
fn a_skip_inside_an_instruction_finishes_it_before_the_next_instruction() {
    let (mut runner, vortex, ()) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(P0, "Quantum Riddler", 4, 6, QUANTUM_RIDDLER_ORACLE);
        scenario.add_creature_from_oracle(
            P0,
            "Obstinate Familiar",
            1,
            1,
            OBSTINATE_FAMILIAR_ORACLE,
        );
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);
    assert_eq!(
        before.hand, 0,
        "Quantum Riddler applies with one or fewer cards in hand"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");
    assert_replacement_choice(&runner, "the skip is offered on the first draw");
    runner
        .act(GameAction::ChooseReplacement {
            index: APPLY_REPLACEMENT,
        })
        .expect("skipping is legal");

    assert_replacement_choice(
        &runner,
        "the first instruction's second draw offers its skip",
    );
    assert_eq!(
        parked_unpaid_suffix(&runner),
        Some(UnpaidCostSuffix {
            payer: P0,
            cost: draw_cost(1),
        }),
        "the second instruction must not be issued before the first completes"
    );
    assert_eq!(ZoneSizes::of(&runner, P0).library, before.library);

    let mut events = Vec::new();
    for _ in 0..8 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        let result = runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("drawing normally is legal");
        events.extend(result.events);
    }

    // First instruction: its second draw (hand 1). Second instruction: hand 1
    // is still "one or fewer", so Quantum Riddler makes it two draws (hand 3).
    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(after.hand, 3);
    assert_eq!(after.library, before.library - 3);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(upkeep_ability_resolved(&events, vortex));
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// CR 121.3 + CR 614.17b: a can't-draw effect forbids choosing the draw
/// payment, so the payment is never offered and Psychic Vortex is sacrificed.
/// The prohibition applies whoever controls it.
#[test]
fn a_cant_draw_effect_refuses_the_payment_and_sacrifices_vortex() {
    let (mut runner, _vortex, ()) = vortex_board(0, &STAGED_LIBRARY, |_| ());
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 1, 1);

    for maralen_controller in [P0, P1] {
        let (mut runner, vortex, ()) = vortex_board(0, &STAGED_LIBRARY, |scenario| {
            scenario.add_creature_from_oracle(
                maralen_controller,
                "Maralen of the Mornsong",
                2,
                3,
                MARALEN_ORACLE,
            );
        });
        advance_through_upkeep(&mut runner);

        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
            "Maralen ({maralen_controller:?}) must stop the payment being offered"
        );
        assert_eq!(zone_of(&runner, vortex), Zone::Graveyard);
    }
}

/// CR 614.17a + CR 614.17b: a can't-draw effect that arrives while the prompt is
/// open refuses `pay: true` on the live board; declining stays legal.
#[test]
fn a_cant_draw_effect_arriving_at_the_prompt_refuses_payment() {
    let (mut runner, vortex, maralen) = vortex_board(0, &STAGED_LIBRARY, |scenario| {
        scenario
            .add_creature_from_oracle(P0, "Maralen of the Mornsong", 2, 3, MARALEN_ORACLE)
            .id()
    });
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), maralen, Zone::Exile, &mut events);
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 1, 1);

    engine::game::zones::move_to_zone(runner.state_mut(), maralen, Zone::Battlefield, &mut events);

    assert!(
        runner.act(GameAction::PayUnlessCost { pay: true }).is_err(),
        "a player can't choose to pay a cost that includes a draw that can't happen"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining stays legal");
    assert_eq!(zone_of(&runner, vortex), Zone::Graveyard);
}

/// CR 118.12 + CR 614.17a + CR 118.11: a can't-draw effect that arrives after P0 chose to pay does not unpay
/// the choice. The parked suffix resumes through the authority's latched entry: each remaining draw is
/// clamped as it happens, the upkeep settles paid, and Psychic Vortex stays.
///
/// Revert probe: resuming the suffix through the fresh entry fails it under Maralen, which trips the resume
/// root's "a resumed unless-cost suffix failed at resolution" `debug_assert!`.
#[test]
fn a_cant_draw_effect_arriving_mid_payment_does_not_unpay_the_chosen_upkeep() {
    let (mut runner, vortex, maralen) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
        scenario
            .add_creature_from_oracle(P0, "Maralen of the Mornsong", 2, 3, MARALEN_ORACLE)
            .id()
    });
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), maralen, Zone::Exile, &mut events);
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);

    let mut settled_events = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal while no can't-draw effect exists")
        .events;
    assert_replacement_choice(&runner, "Dredge must pause the first draw instruction");
    // Reach guard: the second instruction is parked as the latched suffix.
    assert_eq!(
        parked_unpaid_suffix(&runner),
        Some(UnpaidCostSuffix {
            payer: P0,
            cost: draw_cost(1),
        })
    );

    // Maralen arrives after the choice to pay was made.
    engine::game::zones::move_to_zone(runner.state_mut(), maralen, Zone::Battlefield, &mut events);
    for _ in 0..3 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        let result = runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining Dredge is legal");
        settled_events.extend(result.events);
    }

    assert_eq!(
        zone_of(&runner, vortex),
        Zone::Battlefield,
        "the upkeep was paid"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(upkeep_ability_resolved(&settled_events, vortex));
    assert_eq!(
        ZoneSizes::of(&runner, P0).hand,
        before.hand,
        "every draw after Maralen arrived is clamped"
    );
}

/// CR 121.2b: a one-card-per-turn limit forbids paying a cost that includes
/// drawing two cards, but not one that includes drawing one.
#[test]
fn a_per_turn_draw_limit_refuses_two_draws_but_not_one() {
    let (mut runner, vortex, ()) = vortex_board(0, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(
            P1,
            "Spirit of the Labyrinth",
            3,
            1,
            SPIRIT_OF_THE_LABYRINTH_ORACLE,
        );
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 1, 1);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("one draw is within the limit");
    assert_eq!(ZoneSizes::of(&runner, P0).hand, before.hand + 1);
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);

    let (mut runner, vortex, ()) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(
            P1,
            "Spirit of the Labyrinth",
            3,
            1,
            SPIRIT_OF_THE_LABYRINTH_ORACLE,
        );
    });
    advance_through_upkeep(&mut runner);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "a two-draw cost can't be chosen under a one-card limit"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Graveyard);
}

/// CR 702.24a + CR 121.2a (Alms Collector ruling: count the word "draw"): two
/// age counters mean two instructions to draw one card, never one instruction
/// to draw two, so Alms Collector ("two or more cards") never applies. The
/// Divination cast on the same board is the reach guard that Alms Collector is
/// live: its one two-card instruction becomes one card for each player.
#[test]
fn each_age_counter_is_its_own_one_card_instruction() {
    let (mut runner, vortex, divination) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario.with_library_top(P1, &["Opponent 1", "Opponent 2", "Opponent 3"]);
        scenario.add_creature_from_oracle(P1, "Alms Collector", 3, 4, ALMS_COLLECTOR_ORACLE);
        scenario
            .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            })
            .id()
    });
    advance_through_upkeep(&mut runner);
    let prompt = unless_prompt_cost(&runner);
    let before = ZoneSizes::of(&runner, P0);
    let opponent_before = ZoneSizes::of(&runner, P1);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");

    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(after.hand, before.hand + 2);
    assert_eq!(after.library, before.library - 2);
    assert_eq!(
        ZoneSizes::of(&runner, P1).hand,
        opponent_before.hand,
        "Alms Collector must not apply to one-card instructions"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);

    // Reach guard: Alms Collector applies to an ordinary two-card instruction on this board. The paid upkeep
    // leaves P0 with priority in the upkeep; pass through the draw step to this turn's main phase.
    for _ in 0..4 {
        if runner.state().phase == Phase::PreCombatMain {
            break;
        }
        runner.pass_both_players();
    }
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    let outcome = runner.cast(divination).resolve();
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_hand_drawn(P1, 1);

    assert_eq!(
        prompt,
        upkeep_draw_cost(2, 1),
        "CR 702.24a: the printed instruction once per age counter"
    );
}

/// CR 702.24a + CR 121.2a: instruction size and repetition are independent. A
/// "Draw two cards." upkeep at two age counters is two two-card instructions,
/// and Alms Collector applies to each (one card for each player per instruction).
#[test]
fn a_draw_two_upkeep_repeats_a_two_card_instruction() {
    let (mut runner, enchantment, ()) = upkeep_board(
        "Draw-Two Vortex",
        DRAW_TWO_VORTEX_ORACLE,
        1,
        &STAGED_LIBRARY,
        |scenario| {
            scenario.with_library_top(P1, &STAGED_LIBRARY);
            scenario.add_creature_from_oracle(P1, "Alms Collector", 3, 4, ALMS_COLLECTOR_ORACLE);
        },
    );
    advance_through_upkeep(&mut runner);
    let prompt = unless_prompt_cost(&runner);
    let before = ZoneSizes::of(&runner, P0);
    let opponent_before = ZoneSizes::of(&runner, P1);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");

    let after = ZoneSizes::of(&runner, P0);
    let opponent_after = ZoneSizes::of(&runner, P1);
    assert_eq!(
        (
            after.hand - before.hand,
            opponent_after.hand - opponent_before.hand
        ),
        (2, 2),
        "Alms Collector turns each two-card instruction into one card for each player"
    );
    assert_eq!(after.library, before.library - 2);
    assert_eq!(opponent_after.library, opponent_before.library - 2);
    assert_eq!(zone_of(&runner, enchantment), Zone::Battlefield);

    assert_eq!(
        prompt,
        upkeep_draw_cost(2, 2),
        "CR 702.24a: the two-card instruction once per age counter"
    );
}

/// CR 121.2b: a one-card-per-turn limit forbids a cost that includes one
/// instruction to draw two cards, even at a single age counter.
#[test]
fn a_per_turn_draw_limit_refuses_a_two_card_instruction() {
    let add_spirit = |scenario: &mut GameScenario| {
        scenario.add_creature_from_oracle(
            P1,
            "Spirit of the Labyrinth",
            3,
            1,
            SPIRIT_OF_THE_LABYRINTH_ORACLE,
        );
    };

    // Reach guard: the printed one-card upkeep is offered under the same limit.
    let (mut runner, _vortex, ()) = vortex_board(0, &STAGED_LIBRARY, add_spirit);
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 1, 1);

    let (mut runner, enchantment, ()) = upkeep_board(
        "Draw-Two Vortex",
        DRAW_TWO_VORTEX_ORACLE,
        0,
        &STAGED_LIBRARY,
        add_spirit,
    );
    advance_through_upkeep(&mut runner);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "a two-card draw instruction can't be chosen under a one-card limit, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone_of(&runner, enchantment), Zone::Graveyard);
}

/// CR 702.24a + CR 614.11a + CR 616.1: at three age counters, Dredge pauses each
/// one-card leg in turn. Each park carries only the legs not yet paid (a
/// one-leg tail collapses to its leaf); a paused leg's own draw belongs to its
/// draw frame, never to the parked suffix.
#[test]
fn a_dredge_pause_on_each_of_three_legs_parks_only_later_legs() {
    let (mut runner, vortex, ()) = vortex_board(2, &STAGED_LIBRARY, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    let prompt = unless_prompt_cost(&runner);
    let before = ZoneSizes::of(&runner, P0);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");
    let expected_suffixes = [
        Some(AbilityCost::Composite {
            costs: vec![draw_cost(1), draw_cost(1)],
        }),
        Some(draw_cost(1)),
        None,
    ];
    let mut events = Vec::new();
    for (leg, expected_suffix) in expected_suffixes.into_iter().enumerate() {
        assert_replacement_choice(&runner, &format!("Dredge pauses leg {}", leg + 1));
        assert_eq!(
            parked_unpaid_suffix(&runner),
            expected_suffix.map(|cost| UnpaidCostSuffix { payer: P0, cost }),
            "the park after leg {} carries only the later legs",
            leg + 1
        );
        let result = runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining Dredge is legal");
        events.extend(result.events);
    }

    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(after.hand, before.hand + 3);
    assert_eq!(after.library, before.library - 3);
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().active_draw_sequence().is_none());
    assert!(upkeep_ability_resolved(&events, vortex));

    assert_eq!(
        prompt,
        upkeep_draw_cost(3, 1),
        "CR 702.24a: the printed instruction once per age counter"
    );
}

/// CR 702.24a + CR 121.2a + CR 614.11a + CR 616.1: instruction size survives a
/// pause. A "Draw two cards." upkeep at two age counters pays two two-card legs;
/// Dredge pauses each card in turn. While the first leg is unfinished the park
/// owes the whole second leg at its printed size, and once the second leg is
/// issued the park owes nothing — that leg's own second draw belongs to its draw
/// frame.
#[test]
fn a_dredge_pause_inside_a_two_card_leg_parks_the_later_leg_whole() {
    let (mut runner, enchantment, ()) = upkeep_board(
        "Draw-Two Vortex",
        DRAW_TWO_VORTEX_ORACLE,
        1,
        &STAGED_LIBRARY,
        |scenario| {
            scenario
                .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
                .from_oracle_text(STINKWEED_IMP_ORACLE);
        },
    );
    advance_through_upkeep(&mut runner);
    let prompt = unless_prompt_cost(&runner);
    let before = ZoneSizes::of(&runner, P0);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");
    // One Dredge pause per card: two in the first leg, two in the second.
    let expected_suffixes = [Some(draw_cost(2)), Some(draw_cost(2)), None, None];
    let mut events = Vec::new();
    for (pause, expected_suffix) in expected_suffixes.into_iter().enumerate() {
        assert_replacement_choice(&runner, &format!("Dredge pauses draw {}", pause + 1));
        assert_eq!(
            parked_unpaid_suffix(&runner),
            expected_suffix.map(|cost| UnpaidCostSuffix { payer: P0, cost }),
            "the park at draw {} owes only the later leg, at its printed size",
            pause + 1
        );
        let result = runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining Dredge is legal");
        events.extend(result.events);
    }

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "four declined draws settle the payment, got {:?}",
        runner.state().waiting_for
    );
    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(after.hand, before.hand + 4, "two legs of two cards each");
    assert_eq!(after.library, before.library - 4);
    assert_eq!(zone_of(&runner, enchantment), Zone::Battlefield);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().active_draw_sequence().is_none());
    assert!(upkeep_ability_resolved(&events, enchantment));

    assert_eq!(
        prompt,
        upkeep_draw_cost(2, 2),
        "CR 702.24a: the two-card instruction once per age counter"
    );
}

/// CR 121.2a + CR 614.11a: the later leg parked across a Dredge pause is still
/// a two-card instruction when it is issued. Alms Collector turns each two-card
/// leg into one card for each player, so Dredge pauses once per leg, and the
/// second leg — issued from the parked suffix — is replaced exactly like the
/// first. A suffix that had shrunk to one card would escape Alms Collector and
/// draw P0 an extra card.
#[test]
fn a_two_card_leg_resumed_from_a_dredge_park_keeps_its_instruction_size() {
    let (mut runner, enchantment, ()) = upkeep_board(
        "Draw-Two Vortex",
        DRAW_TWO_VORTEX_ORACLE,
        1,
        &STAGED_LIBRARY,
        |scenario| {
            scenario.with_library_top(P1, &STAGED_LIBRARY);
            scenario.add_creature_from_oracle(P1, "Alms Collector", 3, 4, ALMS_COLLECTOR_ORACLE);
            scenario
                .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
                .from_oracle_text(STINKWEED_IMP_ORACLE);
        },
    );
    advance_through_upkeep(&mut runner);
    let prompt = unless_prompt_cost(&runner);
    let before = ZoneSizes::of(&runner, P0);
    let opponent_before = ZoneSizes::of(&runner, P1);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");
    // One Dredge pause per leg: Alms Collector leaves P0 one card per leg.
    let expected_suffixes = [Some(draw_cost(2)), None];
    let mut events = Vec::new();
    for (leg, expected_suffix) in expected_suffixes.into_iter().enumerate() {
        assert_replacement_choice(&runner, &format!("Dredge pauses leg {}", leg + 1));
        assert_eq!(
            parked_unpaid_suffix(&runner),
            expected_suffix.map(|cost| UnpaidCostSuffix { payer: P0, cost }),
            "the park at leg {} owes only the later leg, at its printed size",
            leg + 1
        );
        let result = runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining Dredge is legal");
        events.extend(result.events);
    }

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "two declined draws settle the payment, got {:?}",
        runner.state().waiting_for
    );
    let after = ZoneSizes::of(&runner, P0);
    let opponent_after = ZoneSizes::of(&runner, P1);
    assert_eq!(
        (
            after.hand - before.hand,
            opponent_after.hand - opponent_before.hand
        ),
        (2, 2),
        "Alms Collector turns each two-card leg, parked or not, into one card for each player"
    );
    assert_eq!(after.library, before.library - 2);
    assert_eq!(opponent_after.library, opponent_before.library - 2);
    assert_eq!(zone_of(&runner, enchantment), Zone::Battlefield);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().active_draw_sequence().is_none());
    assert!(upkeep_ability_resolved(&events, enchantment));

    assert_eq!(
        prompt,
        upkeep_draw_cost(2, 2),
        "CR 702.24a: the two-card instruction once per age counter"
    );
}

/// CR 121.2a: every instruction gets its own instruction-level replacement
/// consult. Quantum Riddler adds one card to an instruction drawn with one or
/// fewer cards in hand: the first instruction (hand 0) draws two, the second
/// (hand 2) draws one, totalling 3. A path that skipped the instruction-level
/// consult would draw only 2.
#[test]
fn each_instruction_is_consulted_for_instruction_replacements() {
    let (mut runner, vortex, ()) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(P0, "Quantum Riddler", 4, 6, QUANTUM_RIDDLER_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);
    assert_eq!(before.hand, 0);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");

    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(after.hand, 3);
    assert_eq!(after.library, before.library - 3);
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// AC3: "At the beginning of your end step, sacrifice a land and discard your
/// hand." The controller's land goes; the opponent's land stays.
#[test]
fn end_step_sacrifices_a_land_and_discards_the_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    scenario.with_cards_in_hand(P0, &["Hand 1", "Hand 2"]);
    let vortex = scenario
        .add_enchantment_from_oracle(P0, "Psychic Vortex", PSYCHIC_VORTEX_ORACLE)
        .id();
    let own_land = scenario.add_basic_land(P0, ManaColor::Blue);
    let opponent_land = scenario.add_basic_land(P1, ManaColor::Blue);
    let mut runner = scenario.build();
    let before = ZoneSizes::of(&runner, P0);

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(zone_of(&runner, own_land), Zone::Graveyard);
    assert_eq!(zone_of(&runner, opponent_land), Zone::Battlefield);
    assert_eq!(after.hand, 0, "the whole hand is discarded");
    assert_eq!(
        after.graveyard,
        before.graveyard + 3,
        "two cards and one land"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// Preservation: an ordinary effect's skipped draw keeps its existing
/// priority-boundary resume. The prevented-boundary resume added for draw costs
/// is gated to a parked unless-payment, so Divination's second draw still waits
/// on its own skip prompt with the frame active.
#[test]
fn a_skipped_draw_of_an_ordinary_effect_is_unchanged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.add_creature_from_oracle(P0, "Obstinate Familiar", 1, 1, OBSTINATE_FAMILIAR_ORACLE);
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    let library_before = ZoneSizes::of(&runner, P0).library;

    runner
        .act(GameAction::CastSpell {
            object_id: divination,
            card_id: runner.state().objects[&divination].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Divination");
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pay mana");
    }
    runner.advance_until_stack_empty();
    assert_replacement_choice(&runner, "the skip is offered on Divination's first draw");

    runner
        .act(GameAction::ChooseReplacement {
            index: APPLY_REPLACEMENT,
        })
        .expect("skipping is legal");

    assert_replacement_choice(&runner, "the second draw offers its own skip");
    assert_eq!(ZoneSizes::of(&runner, P0).library, library_before);
    assert!(runner.state().active_draw_sequence().is_some());
}

/// CR 118.12 + CR 118.11 + CR 614.17a: a counter prohibition that arrives after P0 chose to pay does not
/// unpay the choice, and it does not drop the legs after the prevented one. The draw leg pauses on Dredge,
/// parking `[counter, mana]` as the latched suffix; Solemnity then arrives; declining Dredge resumes the
/// suffix through the authority's latched entry. The prevented counter completes as paid and the red mana
/// leg still pays.
///
/// Rows: `solemnity_arrives = false` is the positive control (both later legs execute on this exact
/// path); `true` is the regression.
///
/// Revert probe: restoring the latched counter refusal fails the suffix, which trips the resume root's
/// "a resumed unless-cost suffix failed at resolution" `debug_assert!` (debug) or leaves the red mana
/// unpaid (release).
#[test]
fn a_counter_prohibition_arriving_mid_payment_still_pays_the_later_legs() {
    let probe_counter = CounterType::Generic("probe".to_string());
    for solemnity_arrives in [false, true] {
        let (mut runner, vortex, solemnity) = mixed_cost_board();
        let hand_before = ZoneSizes::of(&runner, P0).hand;

        let mut settled_events = runner
            .act(GameAction::PayUnlessCost { pay: true })
            .expect("paying is legal while no counter prohibition exists")
            .events;
        assert_replacement_choice(&runner, "Dredge must pause the draw leg");
        // Reach guard: the counter and mana legs are parked as the latched suffix.
        let AbilityCost::Composite { costs } = draw_counter_mana_cost() else {
            unreachable!("the fixture is a composite");
        };
        assert_eq!(
            parked_unpaid_suffix(&runner),
            Some(UnpaidCostSuffix {
                payer: P0,
                cost: AbilityCost::Composite {
                    costs: costs[1..].to_vec(),
                },
            })
        );

        if solemnity_arrives {
            let mut events = Vec::new();
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                solemnity,
                Zone::Battlefield,
                &mut events,
            );
        }
        let declined = runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining Dredge is legal");
        settled_events.extend(declined.events);

        let row = format!("solemnity_arrives={solemnity_arrives}");
        let red_mana = runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red);
        assert_eq!(
            red_mana, 1,
            "{row}: the mana leg after the counter leg is paid"
        );
        let probe_counters = runner.state().objects[&vortex]
            .counters
            .get(&probe_counter)
            .copied()
            .unwrap_or(0);
        let expected_probe_counters = if solemnity_arrives { 0 } else { 1 };
        assert_eq!(
            probe_counters, expected_probe_counters,
            "{row}: the counter is placed unless Solemnity prevents it"
        );
        assert_eq!(
            ZoneSizes::of(&runner, P0).hand,
            hand_before + 1,
            "{row}: the draw leg drew its card once Dredge was declined"
        );
        assert_eq!(
            zone_of(&runner, vortex),
            Zone::Battlefield,
            "{row}: the upkeep was paid"
        );
        assert!(
            runner.state().pending_cost_move_resume.is_none(),
            "{row}: nothing is left parked"
        );
        assert!(
            upkeep_ability_resolved(&settled_events, vortex),
            "{row}: the paid epilogue ran"
        );
    }
}

/// CR 614.17b: when Solemnity is already on the battlefield at the prompt, P0 can't choose to pay the mixed
/// cost, because it includes a counter placement that can't happen; nothing is paid, and declining
/// sacrifices Psychic Vortex.
#[test]
fn a_counter_prohibition_at_the_prompt_refuses_the_mixed_payment() {
    let (mut runner, vortex, solemnity) = mixed_cost_board();
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        solemnity,
        Zone::Battlefield,
        &mut events,
    );
    let hand_before = ZoneSizes::of(&runner, P0).hand;

    assert!(
        runner.act(GameAction::PayUnlessCost { pay: true }).is_err(),
        "a player can't choose to pay a cost that includes a counter placement that can't happen"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        0,
        "a refused choice pays no mana"
    );
    assert_eq!(
        ZoneSizes::of(&runner, P0).hand,
        hand_before,
        "a refused choice draws nothing"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining stays legal");
    assert_eq!(zone_of(&runner, vortex), Zone::Graveyard);
}
