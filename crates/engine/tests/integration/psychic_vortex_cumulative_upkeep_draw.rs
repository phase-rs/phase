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

use engine::game::derived_views::{UnlessDrawCostView, UnlessDrawPaymentStage};
use engine::game::ledger::resolve_and_apply_cards_drawn;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, Effect, EffectKind, ManaContribution, ManaProduction, QuantityExpr, TargetFilter,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, PendingCostMoveResume, PersistedGameState, UnpaidCostRemainder,
    UnpaidCostSuffix, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ReplacementId;
use engine::types::resolved_commands::{
    ResolvedLedgerEdit, ResolvedLedgerEditCommand, ResolvedRulesCommand,
};
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
        WaitingFor::UnlessPayment {
            player,
            cost,
            repeat,
            ..
        } => {
            assert_eq!(*player, P0, "Psychic Vortex's controller pays its upkeep");
            // CR 702.24a: a repeated draw upkeep is carried compactly as the
            // printed instruction plus its age-counter count.
            match repeat {
                Some(count) if *count > 0 => AbilityCost::Composite {
                    costs: vec![cost.clone(); *count as usize],
                },
                _ => cost.clone(),
            }
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

/// CR 702.24a: a park that still owes `instructions` printed draw instructions.
fn owed_instructions(instructions: u32) -> Option<UnpaidCostSuffix> {
    std::num::NonZeroU32::new(instructions).map(|owed| UnpaidCostSuffix {
        payer: P0,
        remainder: UnpaidCostRemainder::Repeated(owed),
    })
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
        owed_instructions(1),
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

    // CR 118.11 + CR 614.1: the same mandatory skip over 2^31 instructions.
    // A skipped draw changes nothing and emits nothing, so once one has been
    // performed the rest of the count is paid at once: bounded work, the
    // upkeep paid, nothing drawn and no draw attempted.
    const HUGE: u32 = 1 << 31;
    let (mut runner, vortex, ()) = vortex_board(HUGE - 1, &STAGED_LIBRARY, |scenario| {
        scenario.add_artifact_from_oracle(P1, "Possessed Portal", POSSESSED_PORTAL_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    let before = ZoneSizes::of(&runner, P0);
    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("a skip replacement does not forbid paying");
    let journal_growth = runner.state().resolved_rules_journal.entries().len() - journal_start;
    assert!(
        journal_growth < 200,
        "the skipped instructions are not each journaled: {journal_growth}"
    );
    assert_eq!(ZoneSizes::of(&runner, P0), before, "every draw was skipped");
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(
        upkeep_ability_resolved(&result.events, vortex),
        "the upkeep is paid"
    );
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::PlayerLost { .. })),
        "a skipped draw is not an attempt to draw from the library"
    );
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
        owed_instructions(1),
        "the second instruction must not be issued before the first completes"
    );
    assert_eq!(ZoneSizes::of(&runner, P0).library, before.library);

    let mut events = Vec::new();
    let mut declines = 0;
    for _ in 0..8 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        declines += 1;
        let result = runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("drawing normally is legal");
        events.extend(result.events);
    }
    assert_eq!(
        declines, 3,
        "each of the three remaining draws offers its own skip"
    );

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
    assert_eq!(parked_unpaid_suffix(&runner), owed_instructions(1));

    // Maralen arrives after the choice to pay was made.
    engine::game::zones::move_to_zone(runner.state_mut(), maralen, Zone::Battlefield, &mut events);
    // CR 614.17c: the open choice is answered once; the later draw can't
    // happen, so no Dredge is offered for it and the payment completes.
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: DECLINE_REPLACEMENT,
        })
        .expect("declining Dredge is legal");
    settled_events.extend(result.events);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no further replacement prompt: {:?}",
        runner.state().waiting_for
    );

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
    let expected_suffixes = [owed_instructions(2), owed_instructions(1), None];
    let mut events = Vec::new();
    for (leg, expected_suffix) in expected_suffixes.into_iter().enumerate() {
        assert_replacement_choice(&runner, &format!("Dredge pauses leg {}", leg + 1));
        assert_eq!(
            parked_unpaid_suffix(&runner),
            expected_suffix,
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
    let expected_suffixes = [owed_instructions(1), owed_instructions(1), None, None];
    let mut events = Vec::new();
    for (pause, expected_suffix) in expected_suffixes.into_iter().enumerate() {
        assert_replacement_choice(&runner, &format!("Dredge pauses draw {}", pause + 1));
        assert_eq!(
            parked_unpaid_suffix(&runner),
            expected_suffix,
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
    let expected_suffixes = [owed_instructions(1), None];
    let mut events = Vec::new();
    for (leg, expected_suffix) in expected_suffixes.into_iter().enumerate() {
        assert_replacement_choice(&runner, &format!("Dredge pauses leg {}", leg + 1));
        assert_eq!(
            parked_unpaid_suffix(&runner),
            expected_suffix,
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
                remainder: UnpaidCostRemainder::Legs(AbilityCost::Composite {
                    costs: costs[1..].to_vec(),
                }),
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

/// CR 702.24a: a large age count reaches the decline without expanding one
/// boxed cost per leg; the prompt carries the printed draw instruction and the
/// count.
#[test]
fn a_large_age_count_declines_without_expanding_every_leg() {
    const AGE: u32 = 10_000;
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &STAGED_LIBRARY, |_| ());
    advance_through_upkeep(&mut runner);
    let WaitingFor::UnlessPayment { cost, repeat, .. } = &runner.state().waiting_for else {
        panic!(
            "expected the cumulative-upkeep prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*repeat, Some(AGE));
    assert!(
        matches!(
            cost,
            AbilityCost::EffectCost { effect } if matches!(effect.as_ref(), Effect::Draw { .. })
        ),
        "the prompt keeps the printed instruction instead of {AGE} boxed legs"
    );
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining the upkeep is legal");
    assert_eq!(zone_of(&runner, vortex), Zone::Graveyard);
    assert_eq!(ZoneSizes::of(&runner, P0).hand, before.hand);
}

/// Psychosis Crawler, verbatim Oracle text: one trigger per card drawn.
const PSYCHOSIS_CRAWLER_ORACLE: &str = "Psychosis Crawler's power and toughness are each equal \
to the number of cards in your hand.\nWhenever you draw a card, each opponent loses 1 life.";

/// Desecrated Tomb, verbatim Oracle text: a zone observer of the graveyard a
/// Dredge return leaves.
const DESECRATED_TOMB_ORACLE: &str = "Whenever one or more creature cards leave your graveyard, \
create a 1/1 black Bat creature token with flying.";

/// Psychosis Crawler's power and toughness equal its controller's hand size
/// (CR 604.3); two cards in hand keep it on the battlefield through upkeep.
fn keep_psychosis_crawler_alive(scenario: &mut GameScenario) {
    scenario.add_card_to_hand(P0, "Hand A");
    scenario.add_card_to_hand(P0, "Hand B");
}

/// A library of `size` distinct cards.
fn library_names(size: usize) -> Vec<String> {
    (0..size).map(|index| format!("Library {index}")).collect()
}

/// Triggered abilities from `source` waiting on the stack or in the deferred
/// queue: each occurrence of its trigger event is counted exactly once.
fn queued_triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    let state = runner.state();
    state
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
        + state
            .deferred_triggers
            .iter()
            .filter(|context| context.pending.source_id == source)
            .count()
}

/// The cumulative-upkeep payment is parked on a replacement choice with the
/// remaining instructions owed, and no player has priority.
fn assert_parked_with_owed(runner: &GameRunner, owed: u32, context: &str) {
    assert_replacement_choice(runner, context);
    let remainder = parked_unpaid_suffix(runner).map(|suffix| suffix.remainder);
    let expected = std::num::NonZeroU32::new(owed).map(UnpaidCostRemainder::Repeated);
    assert_eq!(remainder, expected, "{context}: instructions still owed");
}

/// CR 121.4 empty-library edits journaled for `player` after `journal_start`.
fn empty_library_draw_edits(runner: &GameRunner, player: PlayerId, journal_start: usize) -> usize {
    runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter(|entry| {
            matches!(
                &entry.command,
                Some(ResolvedRulesCommand::LedgerEdit(ResolvedLedgerEditCommand {
                    edit: ResolvedLedgerEdit::CardsDrawn {
                        player: drawer,
                        attempted_empty_library: true,
                        ..
                    },
                    ..
                })) if *drawer == player
            )
        })
        .count()
}

/// Index of the first event matching `predicate`, for order assertions.
fn event_index(events: &[GameEvent], predicate: impl Fn(&GameEvent) -> bool) -> Option<usize> {
    events.iter().position(predicate)
}

/// CR 702.24a: accepting a large repeated draw cost pays every printed
/// instruction and parks nothing once the last one is paid.
#[test]
fn a_large_age_count_pays_every_instruction() {
    const AGE: u32 = 100;
    let names = library_names(AGE as usize + 50);
    let library: Vec<&str> = names.iter().map(String::as_str).collect();
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &library, |_| ());
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying a large cumulative upkeep is legal");
    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(after.hand, before.hand + AGE as usize);
    assert_eq!(after.library, before.library - AGE as usize);
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "nothing stays parked after the last instruction"
    );
}

/// CR 702.24a + CR 117.2e: every printed draw pauses on its own Dredge choice,
/// and no player receives priority between two of them; each pause owes
/// exactly the instructions after it.
#[test]
fn each_paused_instruction_keeps_the_rest_owed_without_priority() {
    const AGE: u32 = 100;
    let names = library_names(AGE as usize + 50);
    let library: Vec<&str> = names.iter().map(String::as_str).collect();
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &library, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying a large cumulative upkeep is legal");
    for instruction in 1..=AGE {
        assert_parked_with_owed(
            &runner,
            AGE - instruction,
            &format!("instruction {instruction} pauses on Dredge"),
        );
        runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining Dredge is legal");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the payment completes after its last instruction: {:?}",
        runner.state().waiting_for
    );
    let after = ZoneSizes::of(&runner, P0);
    assert_eq!(after.hand, before.hand + AGE as usize);
    assert_eq!(after.library, before.library - AGE as usize);
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// CR 121.3 + CR 118.11 + CR 121.4 (2008-04-01 ruling: "Psychic Vortex will
/// remain on the battlefield", and the player loses after upkeep finishes): a
/// draw cost chosen on an empty library is paid. The upkeep's paid epilogue
/// resolves, Vortex is not sacrificed, and the player loses only after it. The
/// first empty-library attempt changes the game and the second shows it no
/// longer does; the remaining 63 identical attempts are not journaled.
#[test]
fn a_repeated_draw_cost_on_an_empty_library_is_paid_then_loses() {
    const AGE: u32 = 65;
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &[], |_| ());
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing to draw from an empty library is legal (CR 121.3)");
    let paid = event_index(&result.events, |event| {
        matches!(event, GameEvent::EffectResolved { kind: EffectKind::Sacrifice, source_id, .. }
            if *source_id == vortex)
    })
    .expect("the paid epilogue resolved the upkeep ability");
    assert!(
        !result.events.iter().any(|event| matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == vortex
        )),
        "a paid upkeep does not sacrifice Vortex"
    );
    let lost = event_index(
        &result.events,
        |event| matches!(event, GameEvent::PlayerLost { player_id } if *player_id == P0),
    )
    .expect("CR 121.4: the player who drew from an empty library loses");
    assert!(paid < lost, "the payment completes before the loss");
    assert_eq!(
        empty_library_draw_edits(&runner, P0, journal_start),
        2,
        "the first empty-library attempt changes the game; the second shows it no longer does"
    );
}

/// CR 121.6a + CR 118.11: Obstinate Familiar's skip is offered for every one of
/// the 65 draws on an empty library. Skipping all of them still pays the
/// upkeep; no draw was attempted, so nobody loses.
#[test]
fn skipping_every_draw_on_an_empty_library_pays_without_a_loss() {
    const AGE: u32 = 65;
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &[], |scenario| {
        scenario
            .add_creature(P0, "Obstinate Familiar", 1, 1)
            .from_oracle_text(OBSTINATE_FAMILIAR_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let mut result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    for instruction in 1..=AGE {
        assert_parked_with_owed(
            &runner,
            AGE - instruction,
            &format!("draw {instruction} offers the skip"),
        );
        result = runner
            .act(GameAction::ChooseReplacement {
                index: APPLY_REPLACEMENT,
            })
            .expect("skipping the draw is legal");
    }
    assert!(
        upkeep_ability_resolved(&result.events, vortex),
        "the upkeep is paid"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "no draw was attempted, so P0 keeps playing: {:?}",
        runner.state().waiting_for
    );
    assert!(!runner.state().players[P0.0 as usize].is_eliminated);
}

/// CR 121.6a + CR 117.2e + CR 121.4: declining the skip attempts a draw from the
/// empty library, but the loss waits for the next priority. The second skip
/// prompt still arrives, every one of the 65 is offered, the upkeep's paid
/// epilogue resolves, and only then does P0 lose.
#[test]
fn declining_every_skip_offers_each_prompt_pays_then_loses() {
    const AGE: u32 = 65;
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &[], |scenario| {
        scenario
            .add_creature(P0, "Obstinate Familiar", 1, 1)
            .from_oracle_text(OBSTINATE_FAMILIAR_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let mut result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    for instruction in 1..=AGE {
        assert_parked_with_owed(
            &runner,
            AGE - instruction,
            &format!("draw {instruction} offers the skip"),
        );
        assert!(
            !runner.state().players[P0.0 as usize].is_eliminated,
            "draw {instruction}: the empty-library loss waits for priority"
        );
        result = runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining the skip is legal");
    }
    let paid = event_index(&result.events, |event| {
        matches!(event, GameEvent::EffectResolved { kind: EffectKind::Sacrifice, source_id, .. }
            if *source_id == vortex)
    })
    .expect("the paid epilogue resolved the upkeep ability");
    let lost = event_index(
        &result.events,
        |event| matches!(event, GameEvent::PlayerLost { player_id } if *player_id == P0),
    )
    .expect("CR 121.4: P0 drew from an empty library and loses");
    assert!(paid < lost, "the payment completes before the loss");
    assert!(
        !result.events.iter().any(|event| matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == vortex
        )),
        "a paid upkeep does not sacrifice Vortex"
    );
}

/// CR 702.24a + CR 121.4: a 2^31 age count on a three-card library draws three
/// cards, attempts one empty draw, and settles the remaining identical attempts
/// at once. The journal grows by the four draws, not by 2^31 entries, and a
/// further empty-library attempt is an identity edit, so the kept journal
/// replays to the same game.
#[test]
fn a_huge_age_count_on_a_short_library_is_bounded_and_replayable() {
    const AGE: u32 = 1 << 31;
    let (mut runner, vortex, ()) =
        vortex_board(AGE - 1, &["Library 1", "Library 2", "Library 3"], |_| ());
    advance_through_upkeep(&mut runner);
    let WaitingFor::UnlessPayment { repeat, .. } = &runner.state().waiting_for else {
        panic!(
            "expected the upkeep prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*repeat, Some(AGE));
    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    let journal_growth = runner.state().resolved_rules_journal.entries().len() - journal_start;
    assert!(
        journal_growth < 200,
        "the journal records the draws that changed the game, not 2^31 attempts: {journal_growth}"
    );
    assert_eq!(
        empty_library_draw_edits(&runner, P0, journal_start),
        2,
        "the first empty attempt and the identity attempt that ends the payment"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(
                |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == P0)
            )
            .count(),
        3,
        "the three library cards are drawn"
    );
    assert!(
        upkeep_ability_resolved(&result.events, vortex),
        "the upkeep is paid"
    );
    // The journal replay contract: the attempts not journaled are identity
    // edits. One more empty-library attempt on the final state changes no
    // ledger field, so the kept journal replays to the same game.
    let ledger = |state: &GameState| {
        let player = &state.players[P0.0 as usize];
        (
            player.has_drawn_this_turn,
            player.cards_drawn_this_turn,
            player.cards_drawn_this_step,
            player.drew_from_empty_library,
        )
    };
    let mut further = runner.state().clone();
    resolve_and_apply_cards_drawn(&mut further, P0, None, true)
        .expect("a further empty-library attempt applies on the settled ledger");
    assert_eq!(
        ledger(&further),
        ledger(runner.state()),
        "a further empty-library attempt changes nothing"
    );
}

/// CR 702.24a + CR 118.12: a payment parked between instructions survives a
/// save and the production restore with its remaining count, and the restored
/// game answers the open choice and continues.
#[test]
fn a_parked_repeated_payment_survives_a_checked_restore() {
    const AGE: u32 = 5;
    let names = library_names(20);
    let library: Vec<&str> = names.iter().map(String::as_str).collect();
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &library, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert_parked_with_owed(&runner, AGE - 1, "the first instruction pauses");
    let saved = serde_json::to_string(&PersistedGameState::Raw(Box::new(runner.state().clone())))
        .expect("the parked payment serializes");
    *runner.state_mut() = serde_json::from_str::<PersistedGameState>(&saved)
        .expect("the save decodes")
        .into_game_state()
        .expect("the parked payment restores through the production chokepoint");
    assert_parked_with_owed(&runner, AGE - 1, "the restored payment owes the rest");
    for instruction in 1..=AGE {
        assert_parked_with_owed(
            &runner,
            AGE - instruction,
            "each restored instruction pauses",
        );
        runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("the restored choice is answerable");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// CR 603.2 + CR 603.2c: the draws of an uninterrupted repeated payment queue
/// one Psychosis Crawler trigger each.
#[test]
fn a_repeated_payment_queues_each_draw_trigger_once() {
    const AGE: u32 = 65;
    let names = library_names(AGE as usize + 50);
    let library: Vec<&str> = names.iter().map(String::as_str).collect();
    let (mut runner, vortex, crawler) = vortex_board(AGE - 1, &library, |scenario| {
        let crawler = scenario
            .add_creature(P0, "Psychosis Crawler", 1, 1)
            .from_oracle_text(PSYCHOSIS_CRAWLER_ORACLE)
            .id();
        keep_psychosis_crawler_alive(scenario);
        crawler
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payment is legal");
    assert_eq!(queued_triggers_from(&runner, crawler), AGE as usize);
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// CR 603.2c: a small payment that declines Dredge draws and reopens its next
/// choice under the counter-addition owner. Each declined draw queues exactly
/// one Psychosis Crawler trigger; the graveyard observer sees nothing leave.
#[test]
fn declining_dredge_in_a_small_payment_counts_each_trigger_once() {
    const AGE: u32 = 3;
    let names = library_names(20);
    let library: Vec<&str> = names.iter().map(String::as_str).collect();
    let (mut runner, vortex, (crawler, tomb)) = vortex_board(AGE - 1, &library, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
        let crawler = scenario
            .add_creature(P0, "Psychosis Crawler", 1, 1)
            .from_oracle_text(PSYCHOSIS_CRAWLER_ORACLE)
            .id();
        keep_psychosis_crawler_alive(scenario);
        let tomb = scenario
            .add_enchantment_from_oracle(P0, "Desecrated Tomb", DESECRATED_TOMB_ORACLE)
            .id();
        (crawler, tomb)
    });
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payment is legal");
    for instruction in 1..=AGE {
        assert_parked_with_owed(&runner, AGE - instruction, "each draw offers Dredge");
        assert_eq!(
            queued_triggers_from(&runner, crawler),
            (instruction - 1) as usize,
            "draw {instruction}: each earlier declined draw queued one trigger"
        );
        runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining Dredge is legal");
    }
    assert_eq!(queued_triggers_from(&runner, crawler), AGE as usize);
    assert_eq!(
        queued_triggers_from(&runner, tomb),
        0,
        "no card left the graveyard"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// CR 603.2c + CR 702.52a: accepting Dredge on the first draw mills five and
/// returns Stinkweed Imp, so the other 64 draws pay synchronously inside the
/// same answer. Desecrated Tomb triggers once for the Imp leaving the
/// graveyard, and Psychosis Crawler once per card actually drawn.
#[test]
fn accepting_dredge_then_a_synchronous_suffix_counts_each_trigger_once() {
    const AGE: u32 = 65;
    let names = library_names(AGE as usize + 50);
    let library: Vec<&str> = names.iter().map(String::as_str).collect();
    let (mut runner, vortex, (crawler, tomb)) = vortex_board(AGE - 1, &library, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
        let crawler = scenario
            .add_creature(P0, "Psychosis Crawler", 1, 1)
            .from_oracle_text(PSYCHOSIS_CRAWLER_ORACLE)
            .id();
        keep_psychosis_crawler_alive(scenario);
        let tomb = scenario
            .add_enchantment_from_oracle(P0, "Desecrated Tomb", DESECRATED_TOMB_ORACLE)
            .id();
        (crawler, tomb)
    });
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payment is legal");
    assert_parked_with_owed(&runner, AGE - 1, "the first draw offers Dredge");
    runner
        .act(GameAction::ChooseReplacement {
            index: APPLY_REPLACEMENT,
        })
        .expect("accepting Dredge is legal");
    // CR 603.3b: the payment finished synchronously; P0 now orders the
    // triggers of two sources, each listed exactly once per occurrence.
    let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for else {
        panic!(
            "the rest of the payment is synchronous and its triggers wait to be ordered: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let from = |source: ObjectId| {
        triggers
            .iter()
            .filter(|trigger| trigger.source_id == source)
            .count()
    };
    assert_eq!(
        from(crawler),
        (AGE - 1) as usize,
        "one trigger per card drawn after the dredged instruction"
    );
    assert_eq!(from(tomb), 1, "the Imp left the graveyard once");
    assert_eq!(triggers.len(), AGE as usize, "nothing else was collected");
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "the payment completed"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// CR 800.4a: when the payer of a parked repeated draw payment leaves the
/// game, the instructions still owed leave with them. The parked payment is
/// retired, nothing is drawn for the departed player, and a living player
/// gets priority and can act.
#[test]
fn a_departing_payer_takes_the_owed_instructions_with_them() {
    const AGE: u32 = 5;
    let names = library_names(20);
    let library: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Untap);
    scenario.with_library_top(P0, &library);
    let vortex = scenario
        .add_enchantment_from_oracle(P0, "Psychic Vortex", PSYCHIC_VORTEX_ORACLE)
        .id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&vortex)
        .expect("Psychic Vortex exists")
        .counters
        .insert(CounterType::Age, AGE - 1);
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert_parked_with_owed(&runner, AGE - 1, "the first draw offers Dredge");

    let departure = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("CR 104.3a: a player can concede at any time");
    assert!(
        !departure.events.iter().any(|event| matches!(
            event,
            GameEvent::CardDrawn { player_id, .. } if *player_id == P0
        )),
        "nothing more is drawn for the departed player"
    );

    let state = runner.state();
    assert!(state.players[P0.0 as usize].is_eliminated);
    assert!(
        state.pending_cost_move_resume.is_none(),
        "the departed payer's owed instructions are retired"
    );
    let WaitingFor::Priority { player } = state.waiting_for else {
        panic!("a living player receives priority: {:?}", state.waiting_for);
    };
    assert_ne!(player, P0);
    runner
        .act(GameAction::PassPriority)
        .expect("the game goes on for the living players");
}

/// Notion Thief, verbatim Oracle text: a mandatory draw replacement that
/// changes the game on every draw it replaces.
const NOTION_THIEF_ORACLE: &str = "Flash\nIf an opponent would draw a card except the first one \
they draw in each of their draw steps, instead that player skips that draw and you draw a card.";

/// Hullbreacher, verbatim Oracle text.
const HULLBREACHER_ORACLE: &str = "Flash\nIf an opponent would draw a card except the first one \
they draw in each of their draw steps, instead you create a Treasure token. (It's an artifact \
with \"{T}, Sacrifice this token: Add one mana of any color.\")";

/// CR 118.11 + CR 614.1: a mandatory replacement that changes the game on each
/// draw is performed for each instruction, never collapsed. Each of P0's draws
/// becomes a draw by Notion Thief's controller: ten real draws from P1's
/// ten-card library, then P1 attempts to draw from the empty library, and only
/// once an attempt changes nothing is the rest of the count paid at once.
#[test]
fn a_replacement_that_changes_the_game_is_performed_for_each_instruction() {
    const AGE: u32 = 65;
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &STAGED_LIBRARY, |scenario| {
        scenario.with_library_top(P1, &STAGED_LIBRARY[..10]);
        scenario.add_creature_from_oracle(P1, "Notion Thief", 3, 1, NOTION_THIEF_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let before = ZoneSizes::of(&runner, P0);
    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("a mandatory substitute does not forbid paying");
    let drawn_by = |player: PlayerId| {
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == player))
            .count()
    };
    assert_eq!(
        drawn_by(P1),
        10,
        "each replaced draw is performed: P1 draws its library"
    );
    assert_eq!(drawn_by(P0), 0, "P0 skips every draw");
    assert_eq!(ZoneSizes::of(&runner, P0).hand, before.hand);
    assert_eq!(
        empty_library_draw_edits(&runner, P1, journal_start),
        2,
        "P1's first empty attempt changes the game and the second ends the payment"
    );
    assert!(
        upkeep_ability_resolved(&result.events, vortex),
        "the upkeep is paid"
    );
}

/// Treasures under `player`'s control.
fn treasures_of(runner: &GameRunner, player: PlayerId) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects[*id].name == "Treasure" && state.objects[*id].controller == player
        })
        .count()
}

/// One work slice of a repeated draw payment (the engine's
/// `WORK_SLICE_INSTRUCTIONS`).
const WORK_SLICE: u32 = 1_024;

/// How many times `events` report Psychic Vortex's upkeep ability resolving.
fn upkeep_resolutions(events: &[GameEvent], vortex: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Sacrifice,
                    source_id,
                    ..
                } if *source_id == vortex
            )
        })
        .count()
}

/// The game waits on P0's continuation of the payment: the unless-payment
/// prompt for exactly `owed` instructions, with that cursor parked under the
/// payment's owner. Continuing is the only answer: declining is refused and
/// leaves the game unchanged, and no other player may act (CR 117.2e).
fn assert_continuation_boundary(runner: &mut GameRunner, owed: u32, context: &str) {
    let WaitingFor::UnlessPayment { player, repeat, .. } = &runner.state().waiting_for else {
        panic!(
            "{context}: the payer is asked to continue, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        (*player, *repeat),
        (P0, Some(owed)),
        "{context}: owed cursor"
    );
    assert_eq!(
        parked_unpaid_suffix(runner),
        owed_instructions(owed),
        "{context}: the owner holds the same cursor"
    );
    let answers: Vec<GameAction> = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter(|action| {
            matches!(
                action,
                GameAction::PayUnlessCost { .. } | GameAction::PassPriority
            )
        })
        .collect();
    assert_eq!(
        answers,
        vec![GameAction::PayUnlessCost { pay: true }],
        "{context}: continuing is the only answer"
    );
    let (flat, _, by_object) = engine::ai_support::legal_actions_full(runner.state());
    let mana_actions: Vec<&GameAction> = flat
        .iter()
        .chain(by_object.values().flatten())
        .filter(|action| {
            matches!(
                action,
                GameAction::TapLandForMana { .. }
                    | GameAction::UntapLandForMana { .. }
                    | GameAction::ActivateManaSource { .. }
                    | GameAction::ActivateAbility { .. }
            )
        })
        .collect();
    assert!(
        mana_actions.is_empty(),
        "{context}: no mana action is offered: {mana_actions:?}"
    );
    // The payer's client and an opponent's are both told the payment only
    // continues, through the same viewer projection the transports use.
    for viewer in [P0, P1] {
        let filtered = engine::game::visibility::filter_state_for_viewer(runner.state(), viewer);
        let views = engine::game::derived_views::derive_filtered_views(
            runner.state(),
            &filtered,
            Some(viewer),
        );
        assert_eq!(
            views.unless_draw_cost,
            Some(UnlessDrawCostView {
                cards: 1,
                instructions: owed,
                stage: UnlessDrawPaymentStage::Continuing,
            }),
            "{context}: viewer {viewer:?} sees a continuation"
        );
    }
    let before = runner.state().clone();
    let refused = runner.act(GameAction::PayUnlessCost { pay: false });
    assert!(
        matches!(
            &refused,
            Err(engine::game::engine::EngineError::InvalidAction(reason))
                if reason.contains("partial payments aren't allowed")
        ),
        "{context}: declining is refused: {refused:?}"
    );
    assert_eq!(
        runner.state().waiting_for,
        before.waiting_for,
        "{context}: the refused decline leaves the prompt"
    );
    assert_eq!(parked_unpaid_suffix(runner), owed_instructions(owed));
    engine::game::engine::apply(runner.state_mut(), P1, GameAction::PassPriority)
        .expect_err("no other player has priority inside the payment");
}

/// CR 702.24a + CR 117.2e + CR 118.11: a replacement that creates an object
/// for every draw (Hullbreacher) changes the game on every instruction and
/// never reaches a fixed point. A payment of two work slices and one more
/// instruction is performed one slice per dispatch: at each boundary the exact
/// owed cursor stays with the payment's owner, the payer can only continue,
/// and no player receives priority. Every draw becomes a Treasure for P1 and
/// the upkeep resolves exactly once.
#[test]
fn a_payment_past_a_work_slice_continues_at_a_boundary_without_priority() {
    const AGE: u32 = 2 * WORK_SLICE + 1;
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(P1, "Hullbreacher", 3, 2, HULLBREACHER_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let filtered = engine::game::visibility::filter_state_for_viewer(runner.state(), P0);
    assert_eq!(
        engine::game::derived_views::derive_filtered_views(runner.state(), &filtered, Some(P0))
            .unless_draw_cost,
        Some(UnlessDrawCostView {
            cards: 1,
            instructions: AGE,
            stage: UnlessDrawPaymentStage::Offered,
        }),
        "the fresh choice can be declined"
    );
    let before = ZoneSizes::of(&runner, P0);
    let mut resolutions = 0;
    for slice in 1..=2 {
        let result = runner
            .act(GameAction::PayUnlessCost { pay: true })
            .expect("paying is never refused");
        resolutions += upkeep_resolutions(&result.events, vortex);
        assert_eq!(
            treasures_of(&runner, P1),
            (slice * WORK_SLICE) as usize,
            "slice {slice}: one Treasure per instruction performed"
        );
        assert_continuation_boundary(
            &mut runner,
            AGE - slice * WORK_SLICE,
            &format!("after slice {slice}"),
        );
    }
    assert_eq!(resolutions, 0, "the upkeep waits for the whole payment");
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the last instruction is paid");
    assert_eq!(
        treasures_of(&runner, P1),
        AGE as usize,
        "every age counter is paid"
    );
    assert_eq!(
        upkeep_resolutions(&result.events, vortex),
        1,
        "the upkeep resolves exactly once"
    );
    assert_eq!(ZoneSizes::of(&runner, P0), before, "P0 draws nothing");
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "nothing is owed"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "priority follows the paid upkeep: {:?}",
        runner.state().waiting_for
    );
}

/// Sol Ring, verbatim Oracle text.
const SOL_RING_ORACLE: &str = "{T}: Add {C}{C}.";

/// CR 605.3a + CR 702.24a: the continuation asks for no mana payment, so the
/// payer's mana abilities are refused at a work-slice boundary as they are at
/// the offer: P0 can neither tap a land nor activate Sol Ring there, and the
/// prompt and its owed cursor are unchanged.
#[test]
fn the_payer_cannot_activate_mana_abilities_at_a_work_slice_boundary() {
    const AGE: u32 = WORK_SLICE + 2;
    let (mut runner, _, (ring, island)) = vortex_board(AGE - 1, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(P1, "Hullbreacher", 3, 2, HULLBREACHER_ORACLE);
        let ring = scenario
            .add_artifact_from_oracle(P0, "Sol Ring", SOL_RING_ORACLE)
            .id();
        (ring, scenario.add_basic_land(P0, ManaColor::Blue))
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let selection =
        engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0)
            .into_iter()
            .find(|selection| selection.source.object_id == island)
            .expect("reach guard: the Island's mana ability is activatable");
    let tap_island = GameAction::TapLandForMana { selection };
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is never refused");
    assert_continuation_boundary(&mut runner, 2, "before the mana actions");
    let pool_before = runner.state().players[P0.0 as usize].mana_pool.clone();
    let mut mana_actions = vec![GameAction::ActivateAbility {
        source_id: ring,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    }];
    mana_actions.push(tap_island);
    for action in mana_actions {
        let refused = runner.act(action.clone());
        assert!(
            matches!(
                &refused,
                Err(engine::game::engine::EngineError::InvalidAction(reason))
                    if reason.contains("asks for no mana")
            ),
            "{action:?} is refused at the boundary: {refused:?}"
        );
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool,
        pool_before,
        "no mana was produced"
    );
    assert!(!runner.state().objects[&ring].tapped);
    assert!(!runner.state().objects[&island].tapped);
    assert_continuation_boundary(&mut runner, 2, "after the mana actions");
}

/// CR 702.24a: a payment parked at a work-slice boundary survives a checked
/// save and restore with its exact owed cursor, and continues to the end. A
/// save whose prompt disagrees with the cursor its owner holds is refused.
#[test]
fn a_payment_at_a_work_slice_boundary_survives_a_checked_restore() {
    const AGE: u32 = WORK_SLICE + 2;
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(P1, "Hullbreacher", 3, 2, HULLBREACHER_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is never refused");
    assert_continuation_boundary(&mut runner, 2, "before the save");

    let restore = |state: &GameState| {
        let saved = serde_json::to_string(&PersistedGameState::Raw(Box::new(state.clone())))
            .expect("the boundary serializes");
        serde_json::from_str::<PersistedGameState>(&saved)
            .expect("the save decodes")
            .into_game_state()
    };
    let mut forged = runner.state().clone();
    let WaitingFor::UnlessPayment { repeat, .. } = &mut forged.waiting_for else {
        panic!("reach guard: the continuation prompt is open");
    };
    *repeat = Some(1);
    let refused = restore(&forged);
    assert!(
        refused.as_ref().is_err_and(|error| error
            .to_string()
            .contains("repeated payment parked under it")),
        "a prompt that disagrees with its owner's cursor is refused: {:?}",
        refused.as_ref().err()
    );

    // The same unsupported draw (its drawer is not the controller) written to
    // the prompt and to the owner agrees with itself, and is refused for what
    // it repeats: the payment authority would fail every owed instruction.
    let unsupported_draw = AbilityCost::EffectCost {
        effect: Box::new(Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        }),
    };
    let mut forged = runner.state().clone();
    let WaitingFor::UnlessPayment { cost, .. } = &mut forged.waiting_for else {
        panic!("reach guard: the continuation prompt is open");
    };
    *cost = unsupported_draw.clone();
    let Some(PendingCostMoveResume::CounterAdditionUnlessPayment { cost, .. }) =
        forged.pending_cost_move_resume.as_mut()
    else {
        panic!("reach guard: the payment is parked");
    };
    *cost = unsupported_draw;
    let refused = restore(&forged);
    assert!(
        refused.as_ref().is_err_and(|error| error
            .to_string()
            .contains("not a supported draw instruction")),
        "a repeated cost the authority does not pay is refused: {:?}",
        refused.as_ref().err()
    );

    *runner.state_mut() = restore(runner.state()).expect("the boundary restores");
    assert_continuation_boundary(&mut runner, 2, "after the restore");
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the restored payment continues");
    assert_eq!(treasures_of(&runner, P1), AGE as usize);
    assert_eq!(upkeep_resolutions(&result.events, vortex), 1);
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "nothing is owed"
    );
}

/// Three players: P0's Psychic Vortex at one work slice plus two age counters
/// and P1's Hullbreacher. P0 pays and the first slice ends at its boundary.
fn three_player_boundary() -> (GameRunner, ObjectId) {
    const AGE: u32 = WORK_SLICE + 2;
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Untap);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    let vortex = scenario
        .add_enchantment_from_oracle(P0, "Psychic Vortex", PSYCHIC_VORTEX_ORACLE)
        .id();
    scenario.add_creature_from_oracle(P1, "Hullbreacher", 3, 2, HULLBREACHER_ORACLE);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&vortex)
        .expect("Psychic Vortex exists")
        .counters
        .insert(CounterType::Age, AGE - 1);
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is never refused");
    assert_continuation_boundary(&mut runner, 2, "three players");
    (runner, vortex)
}

fn concede(runner: &mut GameRunner, player: PlayerId) {
    engine::game::engine::apply(
        runner.state_mut(),
        player,
        GameAction::Concede { player_id: player },
    )
    .expect("a player can concede at any time");
}

/// CR 104.3a + CR 702.24a: another player's concession at a work-slice
/// boundary leaves the payment where it is: the same continuation, the same
/// owed cursor, and the payment completes.
#[test]
fn another_players_concession_at_a_work_slice_boundary_keeps_the_payment() {
    let (mut runner, vortex) = three_player_boundary();
    concede(&mut runner, PlayerId(2));
    assert_continuation_boundary(&mut runner, 2, "after the concession");
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payment continues");
    assert_eq!(treasures_of(&runner, P1), (WORK_SLICE + 2) as usize);
    assert_eq!(upkeep_resolutions(&result.events, vortex), 1);
}

/// CR 104.3a + CR 800.4a: the payer's own concession at a work-slice boundary
/// takes the owed instructions with it: no payment and no continuation prompt
/// remain, and a living player has priority.
#[test]
fn the_payers_concession_at_a_work_slice_boundary_retires_the_payment() {
    let (mut runner, _) = three_player_boundary();
    concede(&mut runner, P0);
    let state = runner.state();
    assert!(state.players[P0.0 as usize].is_eliminated);
    assert!(state.pending_cost_move_resume.is_none(), "nothing is owed");
    let WaitingFor::Priority { player } = state.waiting_for else {
        panic!("a living player has priority: {:?}", state.waiting_for);
    };
    assert_ne!(player, P0);
    engine::game::engine::apply(runner.state_mut(), player, GameAction::PassPriority)
        .expect("the game goes on");
}

/// CR 603.2c: a payment that crosses a work-slice boundary queues each draw's
/// trigger exactly once: one work slice's Psychosis Crawler triggers at the
/// boundary, and one per age counter when the payment ends.
#[test]
fn a_payment_across_a_work_slice_boundary_queues_each_draw_trigger_once() {
    const AGE: u32 = WORK_SLICE + 6;
    let names = library_names(AGE as usize + 50);
    let library: Vec<&str> = names.iter().map(String::as_str).collect();
    let (mut runner, vortex, crawler) = vortex_board(AGE - 1, &library, |scenario| {
        let crawler = scenario
            .add_creature(P0, "Psychosis Crawler", 1, 1)
            .from_oracle_text(PSYCHOSIS_CRAWLER_ORACLE)
            .id();
        keep_psychosis_crawler_alive(scenario);
        crawler
    });
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payment is legal");
    assert_continuation_boundary(&mut runner, 6, "after the first slice");
    assert_eq!(
        queued_triggers_from(&runner, crawler),
        WORK_SLICE as usize,
        "one trigger per draw of the first slice"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payment continues");
    assert_eq!(
        queued_triggers_from(&runner, crawler),
        AGE as usize,
        "one trigger per draw of the whole payment"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// CR 603.2c: a work-slice boundary reached after a resumed replacement answer
/// queues each draw's trigger exactly once. The first draw is replaced by
/// Stinkweed Imp's Dredge (accepted, so the Imp leaves the graveyard and no
/// later draw is replaced); the resumed payment then draws one work slice of
/// cards to its boundary and the rest after the payer continues.
#[test]
fn a_boundary_after_a_resumed_dredge_answer_queues_each_draw_trigger_once() {
    const AGE: u32 = WORK_SLICE + 3;
    let names = library_names(AGE as usize + 50);
    let library: Vec<&str> = names.iter().map(String::as_str).collect();
    let (mut runner, vortex, crawler) = vortex_board(AGE - 1, &library, |scenario| {
        let crawler = scenario
            .add_creature(P0, "Psychosis Crawler", 1, 1)
            .from_oracle_text(PSYCHOSIS_CRAWLER_ORACLE)
            .id();
        keep_psychosis_crawler_alive(scenario);
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
        crawler
    });
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payment is legal");
    assert_replacement_choice(&runner, "the first draw offers Dredge");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting Dredge resumes the payment");
    assert_continuation_boundary(&mut runner, 2, "after the resumed slice");
    assert_eq!(
        queued_triggers_from(&runner, crawler),
        WORK_SLICE as usize,
        "one trigger per draw of the resumed slice"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payment continues");
    assert_eq!(
        queued_triggers_from(&runner, crawler),
        (AGE - 1) as usize,
        "one trigger per card drawn; the Dredge drew none"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// Laboratory Maniac, verbatim Oracle text.
const LABORATORY_MANIAC_ORACLE: &str =
    "If you would draw a card while your library has no cards in it, you win the game instead.";

/// CR 104.1 + CR 702.24a: a mandatory replacement that performs work of its own
/// is bounded when it ends the game. With 1,025 age counters and an empty
/// library, paying is admitted and Laboratory Maniac wins the game on the first
/// instruction.
#[test]
fn laboratory_maniac_wins_a_payment_past_a_thousand_instructions() {
    const AGE: u32 = 1_025;
    let (mut runner, _vortex, ()) = vortex_board(AGE - 1, &[], |scenario| {
        scenario.add_creature_from_oracle(P0, "Laboratory Maniac", 2, 2, LABORATORY_MANIAC_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is admitted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(winner) } if winner == P0
        ),
        "P0 wins instead of drawing: {:?}",
        runner.state().waiting_for
    );
}

/// Words of Worship and Platinum Emperion, verbatim Oracle text.
const WORDS_OF_WORSHIP_ORACLE: &str =
    "{1}: The next time you would draw a card this turn, you gain 5 life instead.";
const PLATINUM_EMPERION_ORACLE: &str = "Your life total can't change. (You can't gain or lose \
life. You can't pay any amount of life except 0.)";

/// CR 118.11 + CR 614.6: in response to Psychic Vortex's upkeep trigger, P0
/// activates Words of Worship. The first draw instruction is replaced by a gain
/// that Platinum Emperion makes impossible: it emits nothing a trigger keys on,
/// yet it uses up the shield, so the game changed and it is not a fixed point.
/// The two later instructions are real draws.
#[test]
fn a_quiet_instruction_that_uses_up_a_shield_is_not_a_fixed_point() {
    const AGE: u32 = 3;
    let (mut runner, vortex, words) = vortex_board(AGE - 1, &STAGED_LIBRARY, |scenario| {
        scenario.add_creature_from_oracle(P0, "Platinum Emperion", 8, 8, PLATINUM_EMPERION_ORACLE);
        scenario
            .add_enchantment_from_oracle(P0, "Words of Worship", WORDS_OF_WORSHIP_ORACLE)
            .id()
    });
    runner.advance_to_upkeep();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the upkeep trigger waits"
    );
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    runner
        .act(GameAction::ActivateAbility {
            source_id: words,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Words of Worship is activated in response");
    runner.resolve_top();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the shield is made"
    );
    runner.resolve_top();
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let before = ZoneSizes::of(&runner, P0);
    let life_before = runner.life(P0);
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is admitted");
    assert_eq!(
        ZoneSizes::of(&runner, P0).hand,
        before.hand + 2,
        "the instructions after the shield are real draws"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "Platinum Emperion keeps the life total"
    );
    assert!(
        upkeep_ability_resolved(&result.events, vortex),
        "the upkeep is paid"
    );
}

/// Humility, verbatim Oracle text.
const HUMILITY_ORACLE: &str =
    "All creatures lose all abilities and have base power and toughness 1/1.";

/// Three players: P0's Psychic Vortex at one work slice plus two age counters
/// with Stinkweed Imp in P0's graveyard, P1's Hullbreacher muted by P2's
/// Humility. P0 pays, and
/// the first draw parks on Dredge (the only replacement that applies). P2
/// concedes, so Humility leaves the game (CR 800.4a) and Hullbreacher applies
/// again while the payment is parked: the parked choice is rebuilt, and P0 is
/// asked which of Dredge and Hullbreacher applies first (CR 616.1).
fn hullbreacher_unmuted_mid_payment() -> (GameRunner, ObjectId, ObjectId) {
    const AGE: u32 = WORK_SLICE + 2;
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Untap);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    let vortex = scenario
        .add_enchantment_from_oracle(P0, "Psychic Vortex", PSYCHIC_VORTEX_ORACLE)
        .id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    let hullbreacher = scenario
        .add_creature_from_oracle(P1, "Hullbreacher", 3, 2, HULLBREACHER_ORACLE)
        .id();
    scenario.add_enchantment_from_oracle(PlayerId(2), "Humility", HUMILITY_ORACLE);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&vortex)
        .expect("Psychic Vortex exists")
        .counters
        .insert(CounterType::Age, AGE - 1);
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is admitted");
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("reach guard: the first draw parks on a replacement choice");
    assert_eq!(
        pending.candidates.len(),
        1,
        "reach guard: only Dredge applies while Humility mutes Hullbreacher"
    );
    engine::game::engine::apply(
        runner.state_mut(),
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("a player can concede at any time");
    assert_replacement_choice(&runner, "the rebuilt choice is still P0's");
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the first draw is still parked");
    assert_eq!(
        pending.candidates.len(),
        2,
        "CR 616.1: Dredge and Hullbreacher both apply once Humility is gone"
    );
    (runner, vortex, hullbreacher)
}

/// The position of Hullbreacher (`chosen`) or of the other candidate, Dredge,
/// in the parked replacement choice.
fn candidate_position(runner: &GameRunner, hullbreacher: ObjectId, chosen: bool) -> usize {
    runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("a replacement choice is parked")
        .candidates
        .iter()
        .position(|rid| (rid.source == hullbreacher) == chosen)
        .expect("the candidate is offered")
}

/// CR 702.24a + CR 117.2e + CR 800.4a: after the departure, choosing Dredge
/// replaces the parked draw, and every later instruction is replaced by
/// Hullbreacher. The resumed suffix is one work slice and one more instruction:
/// it runs to the slice boundary, where the payer continues, and the payment
/// is performed in full.
#[test]
fn accepting_dredge_after_a_departure_unmutes_hullbreacher_completes_the_payment() {
    let (mut runner, vortex, hullbreacher) = hullbreacher_unmuted_mid_payment();
    // CR 616.1: choosing Dredge's turn asks its own "may"; accepting it mills.
    let index = candidate_position(&runner, hullbreacher, false);
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choosing Dredge's turn is never refused");
    assert!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .is_some_and(|pending| pending.is_optional && pending.candidates.len() == 1),
        "Dredge's own choice is asked while Hullbreacher still applies"
    );
    let resumed = runner
        .act(GameAction::ChooseReplacement {
            index: APPLY_REPLACEMENT,
        })
        .expect("accepting Dredge is never refused");
    assert_eq!(
        treasures_of(&runner, P1),
        WORK_SLICE as usize,
        "the resumed suffix performs one work slice"
    );
    assert_eq!(upkeep_resolutions(&resumed.events, vortex), 0);
    assert_continuation_boundary(&mut runner, 1, "the resumed suffix");
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payment continues");
    assert_eq!(
        treasures_of(&runner, P1),
        (WORK_SLICE + 1) as usize,
        "every draw after the Dredge is a Treasure"
    );
    assert_eq!(upkeep_resolutions(&result.events, vortex), 1);
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "nothing is owed"
    );
}

/// CR 616.1 + CR 800.4a: after the same departure, choosing Dredge's turn and
/// then declining its "may" leaves the draw to Hullbreacher, the effect that
/// was passed over: one Treasure, the Imp still in the graveyard, and the next
/// instruction asks for the order of both effects again.
#[test]
fn declining_dredge_after_choosing_its_turn_leaves_the_draw_to_hullbreacher() {
    let (mut runner, vortex, hullbreacher) = hullbreacher_unmuted_mid_payment();
    let graveyard = ZoneSizes::of(&runner, P0).graveyard;
    let index = candidate_position(&runner, hullbreacher, false);
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choosing Dredge's turn is never refused");
    let declined = runner
        .act(GameAction::ChooseReplacement {
            index: DECLINE_REPLACEMENT,
        })
        .expect("declining Dredge is never refused");
    assert_eq!(
        treasures_of(&runner, P1),
        1,
        "Hullbreacher replaces the draw"
    );
    assert_eq!(
        ZoneSizes::of(&runner, P0).graveyard,
        graveyard,
        "nothing was milled and the Imp stays in the graveyard"
    );
    assert_eq!(upkeep_resolutions(&declined.events, vortex), 0);
    assert_replacement_choice(&runner, "the next instruction asks again");
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the next draw is parked");
    assert!(
        !pending.is_optional && pending.candidates.len() == 2,
        "the next draw offers the order of Dredge and Hullbreacher"
    );
    assert_eq!(parked_unpaid_suffix(&runner), owed_instructions(WORK_SLICE));
}

/// CR 616.1 + CR 702.24a + CR 800.4a: after the same departure, choosing
/// Hullbreacher replaces the parked draw; every later draw again offers
/// Dredge and Hullbreacher, and choosing Hullbreacher each time pays every
/// instruction with no answer refused.
#[test]
fn choosing_hullbreacher_after_a_departure_unmutes_it_completes_the_payment() {
    let (mut runner, vortex, hullbreacher) = hullbreacher_unmuted_mid_payment();
    let mut events = Vec::new();
    let mut orders = 0;
    while runner.state().pending_replacement.is_some() {
        let index = candidate_position(&runner, hullbreacher, true);
        events = runner
            .act(GameAction::ChooseReplacement { index })
            .expect("choosing Hullbreacher is never refused")
            .events;
        orders += 1;
    }
    assert_eq!(orders, WORK_SLICE + 2, "each draw asks for an order");
    assert_eq!(
        treasures_of(&runner, P1),
        (WORK_SLICE + 2) as usize,
        "every draw is a Treasure"
    );
    assert!(
        upkeep_ability_resolved(&events, vortex),
        "the upkeep is paid"
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// CR 616.1 + CR 800.4a: a parked skip prompt whose replacement's source has
/// left is answered by re-proposing the draw: only the effects that would now
/// apply are considered, so the draw happens, the payment goes on, and no
/// answer is refused. The source is removed by a sandbox action, which normal
/// play cannot take while the prompt is parked; the public-action form of
/// this class is a concession that removes a candidate
/// (`draw_cost_payment_authority.rs`, Notion Thief).
#[test]
fn a_skip_prompt_whose_source_left_reproposes_the_draw() {
    let (mut runner, vortex, familiar) = vortex_board(1, &STAGED_LIBRARY, |scenario| {
        scenario
            .add_creature(P0, "Obstinate Familiar", 1, 1)
            .from_oracle_text(OBSTINATE_FAMILIAR_ORACLE)
            .id()
    });
    advance_through_upkeep(&mut runner);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert_parked_with_owed(&runner, 1, "the first draw offers the skip");
    let state = runner.state_mut();
    state.debug_mode = true;
    state.format_config.allow_debug_actions = true;
    runner
        .act(GameAction::Debug(DebugAction::RemoveObject {
            object_id: familiar,
        }))
        .expect("the sandbox removes Obstinate Familiar");
    assert_replacement_choice(&runner, "the skip prompt is still parked");
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: APPLY_REPLACEMENT,
        })
        .expect("an answer to a prompt whose source left is not refused");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no skip is offered any more: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        ZoneSizes::of(&runner, P0).hand,
        before.hand + 2,
        "both draws happen once the skip no longer applies"
    );
    assert!(upkeep_ability_resolved(&result.events, vortex));
}

/// Thought Reflection, verbatim Oracle text.
const THOUGHT_REFLECTION_ORACLE: &str = "If you would draw a card, draw two cards instead.";

/// `player` concedes; returns the events of the concession.
fn concede_with_events(runner: &mut GameRunner, player: PlayerId) -> Vec<GameEvent> {
    engine::game::engine::apply(
        runner.state_mut(),
        player,
        GameAction::Concede { player_id: player },
    )
    .expect("a player can concede at any time")
    .events
}

/// Three players at P0's untap step with Psychic Vortex carrying
/// `age_counters_before` age counters, plus whatever `setup` adds.
fn three_player_vortex_board<Fixture>(
    age_counters_before: u32,
    setup: impl FnOnce(&mut GameScenario) -> Fixture,
) -> (GameRunner, ObjectId, Fixture) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Untap);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    let vortex = scenario
        .add_enchantment_from_oracle(P0, "Psychic Vortex", PSYCHIC_VORTEX_ORACLE)
        .id();
    let fixture = setup(&mut scenario);
    let mut runner = scenario.build();
    if age_counters_before > 0 {
        runner
            .state_mut()
            .objects
            .get_mut(&vortex)
            .expect("Psychic Vortex exists")
            .counters
            .insert(CounterType::Age, age_counters_before);
    }
    (runner, vortex, fixture)
}

/// CR 800.4a: the payer of a draw payment who leaves at the replacement prompt
/// of the last instruction takes the payment with them. Nothing is owed after
/// that instruction, and the parked payment is still retired: no owner is left
/// behind for a later replacement answer to settle, and the living players go
/// on.
#[test]
fn a_payer_departing_at_the_last_instructions_prompt_retires_the_payment() {
    let (mut runner, vortex, ()) = three_player_vortex_board(0, |scenario| {
        scenario.add_creature_from_oracle(
            P0,
            "Obstinate Familiar",
            1,
            1,
            OBSTINATE_FAMILIAR_ORACLE,
        );
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 1, 1);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert_replacement_choice(&runner, "the only draw offers the skip");
    assert_eq!(
        parked_unpaid_suffix(&runner),
        None,
        "reach guard: the pause came on the last instruction"
    );

    let departure = concede_with_events(&mut runner, P0);
    assert!(
        !departure
            .iter()
            .any(|event| matches!(event, GameEvent::CardDrawn { .. })),
        "nothing is drawn for the departed player"
    );
    assert_eq!(upkeep_resolutions(&departure, vortex), 0);
    let state = runner.state();
    assert!(state.players[P0.0 as usize].is_eliminated);
    assert!(
        state.pending_cost_move_resume.is_none(),
        "the departed payer's payment is retired: {:?}",
        state.pending_cost_move_resume
    );
    assert!(state.pending_replacement.is_none());
    let WaitingFor::Priority { player } = state.waiting_for else {
        panic!("a living player receives priority: {:?}", state.waiting_for);
    };
    assert_ne!(player, P0);
    let next = engine::game::engine::apply(runner.state_mut(), player, GameAction::PassPriority)
        .expect("the game goes on for the living players");
    assert_eq!(upkeep_resolutions(&next.events, vortex), 0);
}

/// CR 800.4a + CR 702.24a + CR 117.2e: P1's Notion Thief turns the first of
/// P0's two upkeep draws into a draw by P1, which parks on P1's own Obstinate
/// Familiar. P1 concedes: the interrupted substitute leaves with P1, and P0,
/// who chose to pay and is still in the game, performs the instruction still
/// owed before any player receives priority, and Psychosis Crawler sees that
/// draw once.
#[test]
fn a_departing_substitute_chooser_leaves_the_surviving_payer_to_finish_the_payment() {
    let (mut runner, vortex, crawler) = three_player_vortex_board(1, |scenario| {
        scenario.with_library_top(P1, &STAGED_LIBRARY[..6]);
        scenario.add_creature_from_oracle(P1, "Notion Thief", 3, 1, NOTION_THIEF_ORACLE);
        scenario.add_creature_from_oracle(
            P1,
            "Obstinate Familiar",
            1,
            1,
            OBSTINATE_FAMILIAR_ORACLE,
        );
        let crawler = scenario
            .add_creature(P0, "Psychosis Crawler", 1, 1)
            .from_oracle_text(PSYCHOSIS_CRAWLER_ORACLE)
            .id();
        keep_psychosis_crawler_alive(scenario);
        crawler
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ),
        "reach guard: Notion Thief's draw offers P1 the skip: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        parked_unpaid_suffix(&runner),
        owed_instructions(1),
        "reach guard: P0 still owes the second instruction"
    );

    let departure = concede_with_events(&mut runner, P1);
    let drawn_by = |player: PlayerId| {
        departure
            .iter()
            .filter(|event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == player))
            .count()
    };
    assert_eq!(
        drawn_by(P1),
        0,
        "the departed player's draw does not happen"
    );
    assert_eq!(
        drawn_by(P0),
        1,
        "the instruction still owed is performed, with Notion Thief gone"
    );
    assert_eq!(ZoneSizes::of(&runner, P0).hand, before.hand + 1);
    assert_eq!(
        upkeep_resolutions(&departure, vortex),
        1,
        "the upkeep is paid inside the same resolution"
    );
    assert_eq!(
        queued_triggers_from(&runner, crawler),
        1,
        "CR 603.2c: the resumed draw's trigger is collected exactly once"
    );
    let state = runner.state();
    assert!(
        state.pending_cost_move_resume.is_none(),
        "nothing is owed: {:?}",
        state.pending_cost_move_resume
    );
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player: P0 }),
        "priority only once the payment is over: {:?}",
        state.waiting_for
    );
}

/// CR 616.1 + CR 800.4a: a parked ordering choice is rebuilt when a departure
/// makes another replacement applicable. P0's only upkeep draw parks on the
/// order of two Thought Reflections while P2's Humility mutes P1's
/// Hullbreacher. P2 concedes, Hullbreacher applies again, and P0 may choose it
/// first: one Treasure, no draw, and the payment is complete.
#[test]
fn a_departure_that_makes_a_replacement_applicable_rebuilds_the_parked_choice() {
    let (mut runner, vortex, hullbreacher) = three_player_vortex_board(0, |scenario| {
        for _ in 0..2 {
            scenario.add_enchantment_from_oracle(
                P0,
                "Thought Reflection",
                THOUGHT_REFLECTION_ORACLE,
            );
        }
        let hullbreacher = scenario
            .add_creature_from_oracle(P1, "Hullbreacher", 3, 2, HULLBREACHER_ORACLE)
            .id();
        scenario.add_enchantment_from_oracle(PlayerId(2), "Humility", HUMILITY_ORACLE);
        hullbreacher
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 1, 1);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    let offered = |runner: &GameRunner| {
        runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("a replacement choice is parked")
            .candidates
            .clone()
    };
    assert_replacement_choice(&runner, "the draw asks for the Reflections' order");
    assert_eq!(offered(&runner).len(), 2, "reach guard: two Reflections");
    assert!(
        offered(&runner)
            .iter()
            .all(|rid| rid.source != hullbreacher),
        "reach guard: Humility mutes Hullbreacher"
    );

    concede_with_events(&mut runner, PlayerId(2));
    assert_replacement_choice(&runner, "the choice is still P0's");
    let candidates = offered(&runner);
    assert_eq!(
        candidates.len(),
        3,
        "CR 616.1: every replacement that now applies is offered"
    );
    let index = candidates
        .iter()
        .position(|rid| rid.source == hullbreacher)
        .expect("Hullbreacher is offered");
    let result = runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choosing Hullbreacher first is legal");
    assert_eq!(treasures_of(&runner, P1), 1, "exactly one Treasure");
    assert_eq!(ZoneSizes::of(&runner, P0), before, "no card is drawn");
    assert!(
        upkeep_ability_resolved(&result.events, vortex),
        "the upkeep is paid"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
}

/// CR 702.24a: checked restore validates a parked draw payment's owner where
/// no continuation prompt is open. A save of a payment parked on a Dredge
/// choice whose only change is a payer who is not a seat in the game is
/// refused, and so is one that polls a player who is not a seat, or that
/// repeats a cost which is not a draw instruction; the untouched save
/// restores and the payment completes.
#[test]
fn checked_restore_refuses_a_parked_payment_whose_payer_is_not_in_the_game() {
    const AGE: u32 = 3;
    let (mut runner, vortex, ()) = vortex_board(AGE - 1, &STAGED_LIBRARY, |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert_parked_with_owed(&runner, AGE - 1, "the first draw offers Dredge");

    let restore = |saved: &serde_json::Value| {
        serde_json::from_value::<PersistedGameState>(saved.clone())
            .expect("the save decodes")
            .into_game_state()
    };
    let saved = serde_json::to_value(PersistedGameState::Raw(Box::new(runner.state().clone())))
        .expect("the parked payment serializes");
    let mut forged = saved.clone();
    let mut replaced = 0;
    replace_parked_payer(&mut forged, 99, &mut replaced);
    assert_eq!(replaced, 1, "reach guard: exactly the parked payer changed");
    let refused = restore(&forged);
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("parked unless payment")),
        "a payer who is not a seat is refused: {:?}",
        refused.as_ref().err()
    );

    let refused_with = |forge: &dyn Fn(&mut PendingCostMoveResume), reason: &str| {
        let mut forged = runner.state().clone();
        forge(
            forged
                .pending_cost_move_resume
                .as_mut()
                .expect("reach guard: the payment is parked"),
        );
        let saved = serde_json::to_value(PersistedGameState::Raw(Box::new(forged)))
            .expect("the forged state serializes");
        let refused = restore(&saved);
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.to_string().contains(reason)),
            "expected a refusal naming {reason:?}: {:?}",
            refused.as_ref().err()
        );
    };
    refused_with(
        &|owner| {
            let PendingCostMoveResume::CounterAdditionUnlessPayment { remaining, .. } = owner
            else {
                panic!("reach guard: an unless payment is parked");
            };
            remaining.push(PlayerId(99));
        },
        "who is not a seat",
    );
    refused_with(
        &|owner| {
            let PendingCostMoveResume::CounterAdditionUnlessPayment { cost, .. } = owner else {
                panic!("reach guard: an unless payment is parked");
            };
            *cost = draw_counter_mana_cost();
        },
        "not a supported draw instruction",
    );
    // A draw the payment authority does not pay (its drawer is not the
    // controller) is refused as well, before any instruction is attempted.
    refused_with(
        &|owner| {
            let PendingCostMoveResume::CounterAdditionUnlessPayment { cost, .. } = owner else {
                panic!("reach guard: an unless payment is parked");
            };
            *cost = AbilityCost::EffectCost {
                effect: Box::new(Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                }),
            };
        },
        "not a supported draw instruction",
    );

    *runner.state_mut() = restore(&saved).expect("the untouched save restores");
    assert_parked_with_owed(&runner, AGE - 1, "after the restore");
    let mut events = Vec::new();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        events = runner
            .act(GameAction::ChooseReplacement {
                index: DECLINE_REPLACEMENT,
            })
            .expect("declining Dredge is legal")
            .events;
    }
    assert!(
        upkeep_ability_resolved(&events, vortex),
        "the restored payment completes"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

/// Sets the payer of the parked unless payment's remainder in a serialized
/// game to seat `payer`, counting the records changed.
fn replace_parked_payer(value: &mut serde_json::Value, payer: u64, replaced: &mut usize) {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(suffix) = fields
                .get_mut("CounterAdditionUnlessPayment")
                .and_then(|owner| owner.get_mut("unpaid_suffix"))
            {
                assert!(suffix["payer"].is_u64(), "reach guard: the payer is a seat");
                suffix["payer"] = serde_json::json!(payer);
                *replaced += 1;
            }
            for field in fields.values_mut() {
                replace_parked_payer(field, payer, replaced);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                replace_parked_payer(item, payer, replaced);
            }
        }
        _ => {}
    }
}

/// CR 605.3a + CR 702.24a: Psychic Vortex's upkeep asks for draws, not mana,
/// so the payer's mana abilities are not admitted at its initial offer either:
/// tapping an Island and activating Sol Ring are refused and offered by no
/// legal-action list, the sources and the pool are unchanged, and the payment
/// is then paid in full.
#[test]
fn the_payer_cannot_activate_mana_abilities_at_a_draw_only_offer() {
    const AGE: u32 = 2;
    let (mut runner, vortex, (ring, island)) = vortex_board(AGE - 1, &STAGED_LIBRARY, |scenario| {
        let ring = scenario
            .add_artifact_from_oracle(P0, "Sol Ring", SOL_RING_ORACLE)
            .id();
        (ring, scenario.add_basic_land(P0, ManaColor::Blue))
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let before = ZoneSizes::of(&runner, P0);
    let (flat, _, by_object) = engine::ai_support::legal_actions_full(runner.state());
    let offered: Vec<&GameAction> = flat
        .iter()
        .chain(by_object.values().flatten())
        .filter(|action| {
            matches!(
                action,
                GameAction::TapLandForMana { .. }
                    | GameAction::UntapLandForMana { .. }
                    | GameAction::ActivateAbility { .. }
            )
        })
        .collect();
    assert!(
        offered.is_empty(),
        "no mana action is offered at a draw-only offer: {offered:?}"
    );
    let selection =
        engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0)
            .into_iter()
            .find(|selection| selection.source.object_id == island)
            .expect("reach guard: the Island's mana ability is activatable");
    let pool_before = runner.state().players[P0.0 as usize].mana_pool.clone();
    for action in [
        GameAction::ActivateAbility {
            source_id: ring,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
        GameAction::TapLandForMana { selection },
    ] {
        let refused = runner.act(action.clone());
        assert!(
            matches!(
                &refused,
                Err(engine::game::engine::EngineError::InvalidAction(reason))
                    if reason.contains("asks for no mana")
            ),
            "{action:?} is refused at the offer: {refused:?}"
        );
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool,
        pool_before,
        "no mana was produced"
    );
    assert!(!runner.state().objects[&ring].tapped);
    assert!(!runner.state().objects[&island].tapped);
    assert_draw_cost_prompt(&runner, AGE as usize, 1);
    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the draw payment is still open");
    assert_eq!(ZoneSizes::of(&runner, P0).hand, before.hand + AGE as usize);
    assert!(upkeep_ability_resolved(&result.events, vortex));
}

/// CR 121.2 + CR 121.6b + CR 800.4a: a departing substitute drawer does not
/// take the payer's own draw sequence with them. P0's one upkeep draw is
/// replaced by Thought Reflection ("draw two cards instead"); P1's Notion Thief
/// turns the first of those two draws into P1's draw, which parks on P1's own
/// Obstinate Familiar. P1 concedes: P1's draw can no longer happen, P0's second
/// draw of the sequence is still drawn (Notion Thief is gone), and only then is
/// the upkeep complete. The departed board passes a checked save and restore.
#[test]
fn a_departing_substitute_chooser_leaves_the_payers_own_draw_sequence_running() {
    let (mut runner, vortex, reflection) = three_player_vortex_board(0, |scenario| {
        scenario.with_library_top(P1, &STAGED_LIBRARY[..6]);
        scenario.add_creature_from_oracle(P1, "Notion Thief", 3, 1, NOTION_THIEF_ORACLE);
        scenario.add_creature_from_oracle(
            P1,
            "Obstinate Familiar",
            1,
            1,
            OBSTINATE_FAMILIAR_ORACLE,
        );
        scenario
            .add_enchantment_from_oracle(P0, "Thought Reflection", THOUGHT_REFLECTION_ORACLE)
            .id()
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 1, 1);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert_replacement_choice(&runner, "P0 orders Thought Reflection and Notion Thief");
    let index = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the order is parked")
        .candidates
        .iter()
        .position(|rid| rid.source == reflection)
        .expect("Thought Reflection is offered");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choosing Thought Reflection first is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ),
        "reach guard: the first of P0's two draws became P1's, parked on the skip: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        parked_unpaid_suffix(&runner),
        None,
        "reach guard: the only instruction is the one in flight"
    );
    assert_eq!(
        ZoneSizes::of(&runner, P0),
        before,
        "reach guard: P0 drew nothing yet"
    );

    let departure = concede_with_events(&mut runner, P1);
    let drawn_by_p0 = event_index(
        &departure,
        |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == P0),
    )
    .expect("P0's second draw of the sequence is performed");
    let resolved = event_index(&departure, |event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Sacrifice,
                source_id,
                ..
            } if *source_id == vortex
        )
    })
    .expect("the upkeep is paid");
    assert!(
        drawn_by_p0 < resolved,
        "the remaining draw comes before the upkeep completes"
    );
    assert_eq!(
        departure
            .iter()
            .filter(|event| matches!(event, GameEvent::CardDrawn { .. }))
            .count(),
        1,
        "exactly one card is drawn, by P0"
    );
    assert_eq!(ZoneSizes::of(&runner, P0).hand, before.hand + 1);
    assert_eq!(upkeep_resolutions(&departure, vortex), 1);
    let state = runner.state();
    assert!(state.pending_cost_move_resume.is_none(), "nothing is owed");
    assert!(state.pending_replacement.is_none());
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player: P0 }),
        "priority only once the payment is over: {:?}",
        state.waiting_for
    );
    let saved = serde_json::to_string(&PersistedGameState::Raw(Box::new(state.clone())))
        .expect("the departed board serializes");
    let mut restored = serde_json::from_str::<PersistedGameState>(&saved)
        .expect("the save decodes")
        .into_game_state()
        .expect("nothing of the abandoned substitute is left behind");
    engine::game::engine::apply(&mut restored, P0, GameAction::PassPriority)
        .expect("the game goes on");
}

/// CR 121.2 + CR 121.6b + CR 800.4a: the Thought Reflection board of
/// `a_departing_substitute_chooser_leaves_the_payers_own_draw_sequence_running`
/// with Abundance in place of Obstinate Familiar, so the leaver's open
/// question is a later choice of their substitute, not their draw. P1 accepts
/// Abundance for the draw Notion Thief gave them and concedes at its
/// land-or-nonland choice. Only P1's part of the draw is abandoned: P0's
/// second draw of the two-card sequence is still drawn, and only then is the
/// upkeep complete. The departed board passes a checked save and restore.
#[test]
fn a_departing_substitute_chooser_at_a_later_choice_leaves_the_payers_sequence_running() {
    let (mut runner, vortex, reflection) = three_player_vortex_board(0, |scenario| {
        scenario.with_library_top(P1, &STAGED_LIBRARY[..6]);
        scenario.add_creature_from_oracle(P1, "Notion Thief", 3, 1, NOTION_THIEF_ORACLE);
        scenario.add_enchantment_from_oracle(P1, "Abundance", ABUNDANCE_ORACLE);
        scenario
            .add_enchantment_from_oracle(P0, "Thought Reflection", THOUGHT_REFLECTION_ORACLE)
            .id()
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 1, 1);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert_replacement_choice(&runner, "P0 orders Thought Reflection and Notion Thief");
    let index = offered_candidates(&runner)
        .iter()
        .position(|rid| rid.source == reflection)
        .expect("Thought Reflection is offered");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choosing Thought Reflection first is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ),
        "reach guard: the first of P0's two draws became P1's, offered Abundance: {:?}",
        runner.state().waiting_for
    );
    engine::game::engine::apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseReplacement {
            index: APPLY_REPLACEMENT,
        },
    )
    .expect("P1 accepts Abundance");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice { player: P1, .. }
        ),
        "reach guard: Abundance asks P1 for land or nonland: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        parked_unpaid_suffix(&runner),
        None,
        "reach guard: the only instruction is the one in flight"
    );
    assert_eq!(
        ZoneSizes::of(&runner, P0),
        before,
        "reach guard: P0 drew nothing yet"
    );

    let departure = concede_with_events(&mut runner, P1);
    let drawn_by_p0 = event_index(
        &departure,
        |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == P0),
    )
    .expect("P0's second draw of the sequence is performed");
    let resolved = event_index(&departure, |event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Sacrifice,
                source_id,
                ..
            } if *source_id == vortex
        )
    })
    .expect("the upkeep is paid");
    assert!(
        drawn_by_p0 < resolved,
        "the remaining draw comes before the upkeep completes"
    );
    assert_eq!(
        departure
            .iter()
            .filter(|event| matches!(event, GameEvent::CardDrawn { .. }))
            .count(),
        1,
        "exactly one card is drawn, by P0"
    );
    assert_eq!(ZoneSizes::of(&runner, P0).hand, before.hand + 1);
    assert_eq!(upkeep_resolutions(&departure, vortex), 1);
    let state = runner.state();
    assert!(state.pending_cost_move_resume.is_none(), "nothing is owed");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player: P0 }),
        "priority only once the payment is over: {:?}",
        state.waiting_for
    );
    let saved = serde_json::to_string(&PersistedGameState::Raw(Box::new(state.clone())))
        .expect("the departed board serializes");
    let mut restored = serde_json::from_str::<PersistedGameState>(&saved)
        .expect("the save decodes")
        .into_game_state()
        .expect("nothing of the abandoned substitute is left behind");
    engine::game::engine::apply(&mut restored, P0, GameAction::PassPriority)
        .expect("the game goes on");
}

/// CR 800.4a + CR 614.17c: the departed chooser's parked draw is settled as a
/// draw that can't happen even when every effect offered for it belongs to a
/// player still in the game. P1's Notion Thief turns the first of P0's two
/// upkeep draws into P1's draw, and P1 is asked to order P2's Hullbreacher and
/// P2's Possessed Portal on it. P1 concedes: both effects still apply to a
/// draw, but a player who left draws nothing, so neither is applied (no
/// Treasure), and P0's next instruction asks P0 for the same order.
#[test]
fn a_departed_players_parked_draw_is_settled_though_survivors_effects_still_apply() {
    let (mut runner, vortex, portal) = three_player_vortex_board(1, |scenario| {
        scenario.with_library_top(P1, &STAGED_LIBRARY[..6]);
        scenario.add_creature_from_oracle(P1, "Notion Thief", 3, 1, NOTION_THIEF_ORACLE);
        scenario.add_creature_from_oracle(PlayerId(2), "Hullbreacher", 3, 2, HULLBREACHER_ORACLE);
        scenario
            .add_artifact_from_oracle(PlayerId(2), "Possessed Portal", POSSESSED_PORTAL_ORACLE)
            .id()
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert_replacement_choice(&runner, "P0 orders the three effects on the first draw");
    let thief = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the order is parked")
        .candidates
        .iter()
        .position(|rid| runner.state().objects[&rid.source].name == "Notion Thief")
        .expect("Notion Thief is offered");
    runner
        .act(GameAction::ChooseReplacement { index: thief })
        .expect("choosing Notion Thief first is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ),
        "reach guard: P1 orders Hullbreacher and Possessed Portal on their own draw: {:?}",
        runner.state().waiting_for
    );
    let offered = offered_candidates(&runner);
    assert!(
        offered.len() == 2
            && offered
                .iter()
                .all(|rid| runner.state().objects[&rid.source].controller == PlayerId(2)),
        "reach guard: both effects offered to P1 are P2's"
    );

    let departure = concede_with_events(&mut runner, P1);
    assert!(
        !departure
            .iter()
            .any(|event| matches!(event, GameEvent::CardDrawn { .. })),
        "no card is drawn at the departure"
    );
    assert_eq!(
        treasures_of(&runner, PlayerId(2)),
        0,
        "Hullbreacher replaces nothing: the departed player's draw does not happen"
    );
    assert_replacement_choice(&runner, "P0's second instruction asks P0");
    assert_eq!(
        offered_candidates(&runner).len(),
        2,
        "Hullbreacher and Possessed Portal apply to P0's draw"
    );
    assert_eq!(upkeep_resolutions(&departure, vortex), 0);
    let index = offered_candidates(&runner)
        .iter()
        .position(|rid| rid.source == portal)
        .expect("Possessed Portal is offered");
    let result = runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choosing Possessed Portal is legal");
    assert_eq!(
        ZoneSizes::of(&runner, P0),
        before,
        "both of P0's draws were replaced"
    );
    assert_eq!(treasures_of(&runner, PlayerId(2)), 0);
    assert!(upkeep_ability_resolved(&result.events, vortex));
    assert!(runner.state().pending_cost_move_resume.is_none());
}

/// Abundance, verbatim Oracle text: an interactive draw substitute.
const ABUNDANCE_ORACLE: &str = "If you would draw a card, you may instead choose land or \
nonland and reveal cards from the top of your library until you reveal a card \
of the chosen kind. Put that card into your hand and put all other cards \
revealed this way on the bottom of your library in any order.";

/// CR 800.4a + CR 117.2e: the departed chooser's open question is not their
/// own draw here. P1's Notion Thief turns the first of P0's two upkeep draws
/// into P1's draw, P1 accepts Abundance for it, and P1 concedes at Abundance's
/// land-or-nonland choice. That choice is abandoned with P1, the payment has
/// no prompt left, and P0 performs the instruction still owed before any
/// player receives priority.
#[test]
fn a_departing_substitute_chooser_at_a_later_choice_leaves_the_payer_to_finish() {
    let (mut runner, vortex, ()) = three_player_vortex_board(1, |scenario| {
        scenario.with_library_top(P1, &STAGED_LIBRARY[..6]);
        scenario.add_creature_from_oracle(P1, "Notion Thief", 3, 1, NOTION_THIEF_ORACLE);
        scenario.add_enchantment_from_oracle(P1, "Abundance", ABUNDANCE_ORACLE);
    });
    advance_through_upkeep(&mut runner);
    assert_draw_cost_prompt(&runner, 2, 1);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ),
        "reach guard: Notion Thief's draw offers P1 Abundance: {:?}",
        runner.state().waiting_for
    );
    engine::game::engine::apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseReplacement {
            index: APPLY_REPLACEMENT,
        },
    )
    .expect("P1 accepts Abundance");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice { player: P1, .. }
        ),
        "reach guard: Abundance asks P1 for land or nonland: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_replacement.is_none(),
        "reach guard: the open question is not a parked draw"
    );
    assert_eq!(parked_unpaid_suffix(&runner), owed_instructions(1));
    let departure = concede_with_events(&mut runner, P1);
    let drawn_by = |player: PlayerId| {
        departure
            .iter()
            .filter(|event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == player))
            .count()
    };
    assert_eq!(drawn_by(P1), 0, "nothing is drawn for the departed player");
    assert_eq!(drawn_by(P0), 1, "the instruction still owed is performed");
    assert_eq!(ZoneSizes::of(&runner, P0).hand, before.hand + 1);
    assert_eq!(upkeep_resolutions(&departure, vortex), 1);
    let state = runner.state();
    assert!(state.pending_cost_move_resume.is_none(), "nothing is owed");
    assert_eq!(zone_of(&runner, vortex), Zone::Battlefield);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player: P0 }),
        "priority only once the payment is over: {:?}",
        state.waiting_for
    );
}

/// CR 800.4a: the same board as
/// `a_departing_substitute_chooser_leaves_the_surviving_payer_to_finish_the_payment`,
/// with the payer leaving instead. Psychic Vortex's upkeep ability ceases to
/// exist with its controller, so the payment is retired whole and nothing more
/// is drawn for P0. P1's own draw, which Notion Thief gave them, is still
/// theirs to take or skip, and the game goes on.
#[test]
fn a_departing_payer_leaves_another_players_substitute_draw_to_its_chooser() {
    let (mut runner, vortex, ()) = three_player_vortex_board(1, |scenario| {
        scenario.with_library_top(P1, &STAGED_LIBRARY[..6]);
        scenario.add_creature_from_oracle(P1, "Notion Thief", 3, 1, NOTION_THIEF_ORACLE);
        scenario.add_creature_from_oracle(
            P1,
            "Obstinate Familiar",
            1,
            1,
            OBSTINATE_FAMILIAR_ORACLE,
        );
    });
    advance_through_upkeep(&mut runner);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ),
        "reach guard: Notion Thief's draw offers P1 the skip"
    );
    assert_eq!(parked_unpaid_suffix(&runner), owed_instructions(1));

    let departure = concede_with_events(&mut runner, P0);
    assert!(
        !departure
            .iter()
            .any(|event| matches!(event, GameEvent::CardDrawn { .. })),
        "the departure draws nothing"
    );
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "the departed controller's payment is retired"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ),
        "P1's own draw is still P1's to answer: {:?}",
        runner.state().waiting_for
    );
    let p1_hand = ZoneSizes::of(&runner, P1).hand;
    let answered = engine::game::engine::apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseReplacement {
            index: DECLINE_REPLACEMENT,
        },
    )
    .expect("P1 declines the skip");
    assert_eq!(ZoneSizes::of(&runner, P1).hand, p1_hand + 1, "P1 draws");
    assert!(
        !answered.events.iter().any(
            |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == P0)
        ),
        "nothing is drawn for the departed player"
    );
    assert_eq!(upkeep_resolutions(&answered.events, vortex), 0);
    let WaitingFor::Priority { player } = runner.state().waiting_for else {
        panic!(
            "a living player has priority: {:?}",
            runner.state().waiting_for
        );
    };
    assert_ne!(player, P0);
    engine::game::engine::apply(runner.state_mut(), player, GameAction::PassPriority)
        .expect("the game goes on");
}

/// The candidates of the parked replacement choice.
fn offered_candidates(runner: &GameRunner) -> Vec<ReplacementId> {
    runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("a replacement choice is parked")
        .candidates
        .clone()
}

/// Three players: P0's one-draw Psychic Vortex upkeep is paid and parks on the
/// order of P0's two Thought Reflections while P2's Humility mutes P1's
/// Hullbreacher. A sandbox action then removes Humility, which re-settles
/// nothing (the public-action form is a concession), so the two-Reflection
/// prompt is still parked while Hullbreacher applies again. Returns the
/// runner, Psychic Vortex, Hullbreacher and P0's zones before the payment.
fn reflections_prompt_after_humility_is_removed() -> (GameRunner, ObjectId, ObjectId, ZoneSizes) {
    let (mut runner, vortex, (hullbreacher, humility)) = three_player_vortex_board(0, |scenario| {
        for _ in 0..2 {
            scenario.add_enchantment_from_oracle(
                P0,
                "Thought Reflection",
                THOUGHT_REFLECTION_ORACLE,
            );
        }
        let hullbreacher = scenario
            .add_creature_from_oracle(P1, "Hullbreacher", 3, 2, HULLBREACHER_ORACLE)
            .id();
        let humility = scenario
            .add_enchantment_from_oracle(PlayerId(2), "Humility", HUMILITY_ORACLE)
            .id();
        (hullbreacher, humility)
    });
    advance_through_upkeep(&mut runner);
    let before = ZoneSizes::of(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    let state = runner.state_mut();
    state.debug_mode = true;
    state.format_config.allow_debug_actions = true;
    runner
        .act(GameAction::Debug(DebugAction::RemoveObject {
            object_id: humility,
        }))
        .expect("the sandbox removes Humility");
    assert_eq!(
        offered_candidates(&runner).len(),
        2,
        "reach guard: the sandbox action leaves the two-Reflection prompt parked"
    );
    (runner, vortex, hullbreacher, before)
}

/// CR 616.1: an answer to an ordering prompt that leaves out an effect which
/// now applies is not applied; the event is re-proposed first, also after a
/// checked save and restore. P0 answers the two-Reflection prompt left parked
/// by `reflections_prompt_after_humility_is_removed`: the answer re-proposes
/// the draw, all three effects are offered, and Hullbreacher can be chosen
/// first.
#[test]
fn an_answer_to_an_ordering_prompt_missing_an_applicable_replacement_reproposes_it() {
    let (mut runner, vortex, hullbreacher, before) = reflections_prompt_after_humility_is_removed();
    let offered = offered_candidates;

    let saved = serde_json::to_string(&PersistedGameState::Raw(Box::new(runner.state().clone())))
        .expect("the parked payment serializes");
    *runner.state_mut() = serde_json::from_str::<PersistedGameState>(&saved)
        .expect("the save decodes")
        .into_game_state()
        .expect("the save restores");
    assert_eq!(
        offered(&runner).len(),
        2,
        "the restored prompt is the saved one"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("an answer to a prompt that changed is not refused");
    assert_replacement_choice(&runner, "the draw is re-proposed, not replaced");
    assert_eq!(ZoneSizes::of(&runner, P0), before, "no Reflection applied");
    let candidates = offered(&runner);
    assert_eq!(candidates.len(), 3, "every effect that applies is offered");
    let index = candidates
        .iter()
        .position(|rid| rid.source == hullbreacher)
        .expect("Hullbreacher is offered");
    let result = runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choosing Hullbreacher first is legal");
    assert_eq!(treasures_of(&runner, P1), 1, "exactly one Treasure");
    assert_eq!(ZoneSizes::of(&runner, P0), before, "no card is drawn");
    assert!(upkeep_ability_resolved(&result.events, vortex));
}

/// CR 616.1: the candidates an optional prompt passed over
/// (`PendingReplacement::ordering_peers`) are read only on that optional
/// prompt. NOT a record the game produces: the same parked ordering prompt
/// with Hullbreacher forged into its passed-over list. The record cannot hide
/// Hullbreacher from the ordering: the answer still re-proposes the draw and
/// offers all three effects.
#[test]
fn a_forged_passed_over_list_cannot_hide_a_replacement_from_an_ordering_prompt() {
    let (mut runner, _, hullbreacher, before) = reflections_prompt_after_humility_is_removed();
    runner
        .state_mut()
        .pending_replacement
        .as_mut()
        .expect("reach guard: the ordering prompt is parked")
        .ordering_peers = (0..4)
        .map(|index| ReplacementId {
            source: hullbreacher,
            index,
        })
        .collect();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("an answer to a prompt that changed is not refused");
    assert_replacement_choice(&runner, "the draw is re-proposed, not replaced");
    assert_eq!(ZoneSizes::of(&runner, P0), before, "no Reflection applied");
    let candidates = offered_candidates(&runner);
    assert_eq!(candidates.len(), 3, "every effect that applies is offered");
    assert!(
        candidates.iter().any(|rid| rid.source == hullbreacher),
        "Hullbreacher is offered"
    );
}

/// Mystic Remora, verbatim Oracle text.
const MYSTIC_REMORA_ORACLE: &str = "Cumulative upkeep {1} (At the beginning of your upkeep, put \
an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age \
counter on it.)\nWhenever an opponent casts a noncreature spell, you may draw a card unless that \
player pays {4}.";

/// CR 605.3a: the control for the draw-only refusal. Mystic Remora's upkeep
/// asks for a mana payment, so its prompt keeps the mana window: the Island is
/// offered and tapped at the prompt, and the mana it adds pays the upkeep.
#[test]
fn a_mana_upkeep_keeps_its_mana_window() {
    let (mut runner, remora, island) = upkeep_board(
        "Mystic Remora",
        MYSTIC_REMORA_ORACLE,
        0,
        &STAGED_LIBRARY,
        |scenario| scenario.add_basic_land(P0, ManaColor::Blue),
    );
    advance_through_upkeep(&mut runner);
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::UnlessPayment {
                player: P0,
                cost: AbilityCost::Mana { .. },
                ..
            }
        ),
        "reach guard: Mystic Remora's upkeep asks for mana: {:?}",
        runner.state().waiting_for
    );
    let (flat, _, by_object) = engine::ai_support::legal_actions_full(runner.state());
    assert!(
        flat.iter()
            .chain(by_object.values().flatten())
            .any(|action| matches!(action, GameAction::TapLandForMana { .. })),
        "the Island's mana ability is offered at a mana payment"
    );
    let selection =
        engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0)
            .into_iter()
            .find(|selection| selection.source.object_id == island)
            .expect("reach guard: the Island's mana ability is activatable");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("CR 605.3a: a mana ability may be activated when an effect asks for mana");
    assert!(runner.state().objects[&island].tapped);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the tapped mana pays the upkeep");
    assert_eq!(zone_of(&runner, remora), Zone::Battlefield);
}
