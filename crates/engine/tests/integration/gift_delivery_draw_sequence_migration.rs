//! Integration coverage for Gift a card using the shared draw-sequence path.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{EffectKind, StaticDefinition, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::Zone;
use engine::types::ObjectId;

use crate::support::shared_card_db as load_db;

const COILING_REBIRTH: &str =
    "Gift a card (You may promise an opponent a gift as you cast this spell. \
If you do, they draw a card before its other effects.)\n\
Return target creature card from your graveyard to the battlefield. Then if the gift was promised \
and that creature isn't legendary, create a token that's a copy of that creature, except it's 1/1.";

const STINKWEED_IMP_ORACLE: &str = "Flying\n\
Whenever this creature deals combat damage to a creature, destroy that creature.\n\
Dredge 5 (If you would draw a card, you may mill five cards instead. If you do, return this card from your graveyard to your hand.)";

#[derive(Debug, PartialEq, Eq)]
enum GiftResolution {
    ReplacementChoice,
    Resolved,
}

fn cast_promised_coiling(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
    events: &mut Vec<GameEvent>,
) -> GiftResolution {
    let card_id = runner.state().objects[&spell].card_id;
    events.extend(
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast Coiling Rebirth")
            .events,
    );

    for _ in 0..200 {
        let (action, why) = match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => (GameAction::PassPriority, "complete mana payment"),
            WaitingFor::OptionalCostChoice { .. } => (
                GameAction::DecideOptionalCost { index: 0 },
                "promise Gift a card",
            ),
            WaitingFor::ChooseGiftRecipient { candidates, .. } => (
                GameAction::ChooseGiftRecipient {
                    opponent: candidates[0],
                },
                "choose gift recipient",
            ),
            WaitingFor::TargetSelection { .. } => (
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                },
                "choose Coiling Rebirth target",
            ),
            WaitingFor::OrderTriggers { .. } | WaitingFor::Priority { .. }
                if !runner.state().stack.is_empty() =>
            {
                (GameAction::PassPriority, "advance promised Gift spell")
            }
            WaitingFor::ReplacementChoice { .. } => return GiftResolution::ReplacementChoice,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return GiftResolution::Resolved;
            }
            other => panic!("unexpected Gift resolution prompt: {other:?}"),
        };
        events.extend(runner.act(action).expect(why).events);
    }

    panic!("promised Gift spell did not resolve within 200 actions");
}

fn finish_resolving_stack(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..200 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("finish resolving Gift spell")
                        .events,
                );
            }
            other => panic!("unexpected prompt while finishing Gift spell: {other:?}"),
        }
    }

    panic!("Gift spell did not finish within 200 actions");
}

fn gift_resolved(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::GiftDelivery,
                    ..
                }
            )
        })
        .count()
}

fn position(events: &[GameEvent], what: &str, pred: impl Fn(&GameEvent) -> bool) -> usize {
    events
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("no {what} event among {events:?}"))
}

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn library_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == player)
        .expect("player exists")
        .library
        .len()
}

fn object_zone(runner: &GameRunner, object_id: ObjectId) -> Zone {
    runner.state().objects[&object_id].zone
}

/// CR 117.2e: no player has priority while the spell resolves, so once the
/// last choice of its resolution is answered the spell finishes inside that
/// answer: the stack and every parked resolution frame are empty and a player
/// has priority.
fn assert_settled_by_the_answer(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        state.stack.is_empty(),
        "the spell finished resolving inside the answer, stack {:?}",
        state.stack
    );
    assert!(
        state.resolution_stack.is_empty(),
        "no resolution work is left parked after the answer: {:?}",
        state.resolution_stack
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "a player has priority after the answer, got {:?}",
        state.waiting_for
    );
}

/// Coiling Rebirth promised to P1 and paused on P1's Dredge prompt.
struct DredgeBoard {
    runner: GameRunner,
    events: Vec<GameEvent>,
    /// Coiling Rebirth's target in P0's graveyard.
    target: ObjectId,
    /// The top card of P1's six-card library.
    drawn_card: ObjectId,
    /// Stinkweed Imp in P1's graveyard.
    imp: ObjectId,
}

/// P0 casts Coiling Rebirth with the gift promised to P1, who has six Plains
/// in library and Stinkweed Imp in graveyard. The gift's draw pauses on P1's
/// Dredge 5 prompt, and the gift has not completed while it waits.
fn coiling_rebirth_at_dredge_prompt(db: &CardDatabase) -> DredgeBoard {
    let (mut scenario, spell, target) = promised_gift_scenario();
    let drawn_card = scenario.add_real_card(P1, "Plains", Zone::Library, db);
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let imp = scenario
        .add_creature_to_graveyard(P1, "Stinkweed Imp", 1, 1)
        .from_oracle_text(STINKWEED_IMP_ORACLE)
        .id();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let mut events = Vec::new();
    assert_eq!(
        cast_promised_coiling(&mut runner, spell, target, &mut events),
        GiftResolution::ReplacementChoice,
        "Dredge must pause the Gift draw for its optional replacement choice"
    );
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        unreachable!("cast_promised_coiling stopped on a replacement prompt");
    };
    assert_eq!(*player, P1, "CR 616.1: the drawing player chooses");
    assert!(
        candidates.iter().any(|c| c.source_id == imp),
        "Stinkweed Imp's Dredge is offered: {candidates:?}"
    );
    assert_eq!(
        gift_resolved(&events),
        0,
        "the gift has not completed while its draw waits for the Dredge choice"
    );
    DredgeBoard {
        runner,
        events,
        target,
        drawn_card,
        imp,
    }
}

/// Index of the `Decline` answer of the optional Dredge prompt.
fn decline_index(runner: &GameRunner) -> usize {
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!("expected the Dredge prompt");
    };
    candidates
        .iter()
        .position(|c| c.description == "Decline")
        .unwrap_or_else(|| panic!("no Decline among {candidates:?}"))
}

fn promised_gift_scenario() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature_to_graveyard(P0, "Coiling Target", 3, 3)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coiling Rebirth", false, COILING_REBIRTH)
        .id();
    (scenario, spell, target)
}

/// CR 702.52a + CR 121.6b: A Dredge replacement on the Gift recipient's draw
/// pauses the Gift resolution; declining it resumes and performs the normal draw.
/// CR 702.174j: the gift completes once the draw has settled, before the
/// spell's own return-to-battlefield effect.
#[test]
fn gift_card_draw_pauses_on_dredge_and_resumes() {
    let Some(db) = load_db() else {
        return;
    };
    let mut board = coiling_rebirth_at_dredge_prompt(db);
    let (runner, events) = (&mut board.runner, &mut board.events);
    let (target, drawn_card) = (board.target, board.drawn_card);
    assert_eq!(
        decline_index(runner),
        1,
        "the optional prompt declines at index 1"
    );
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("decline Dredge")
            .events,
    );
    assert_settled_by_the_answer(runner);
    // A no-op once the answer settled the spell.
    finish_resolving_stack(runner, events);

    assert_eq!(gift_resolved(events), 1, "the gift completes exactly once");
    let gift = position(events, "gift completion", |e| {
        matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::GiftDelivery,
                ..
            }
        )
    });
    let drawn = position(
        events,
        "the gift draw",
        |e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == drawn_card),
    );
    let returned = position(
        events,
        "the return to the battlefield",
        |e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == target),
    );
    assert!(drawn < gift, "the gift completes after its card is drawn");
    assert!(
        gift < returned,
        "CR 702.174j: the gift completes before the spell's other effects"
    );

    assert_eq!(object_zone(runner, drawn_card), Zone::Hand);
    assert_eq!(
        hand_count(runner, P1),
        1,
        "declined Dredge must draw the card"
    );
}

/// R23 — CR 702.52a + CR 701.17a: accepting Dredge mills five cards instead
/// of the gift's draw and returns Stinkweed Imp to P1's hand. The gift
/// completes once that settles, inside the answering action, and before the
/// spell's own return-to-battlefield effect (CR 702.174j).
#[test]
fn gift_card_draw_accepting_dredge_completes_the_gift_before_the_return() {
    let Some(db) = load_db() else {
        return;
    };
    let mut board = coiling_rebirth_at_dredge_prompt(db);
    let decline = decline_index(&board.runner);
    let accept = usize::from(decline == 0);
    board.events.extend(
        board
            .runner
            .act(GameAction::ChooseReplacement { index: accept })
            .expect("accept Dredge")
            .events,
    );
    assert_settled_by_the_answer(&board.runner);

    let p1 = board
        .runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == P1)
        .expect("player exists");
    assert_eq!(p1.graveyard.len(), 5, "CR 701.17a: five cards were milled");
    assert!(
        p1.graveyard.contains(&board.drawn_card),
        "CR 701.17a: the library top is among the milled cards"
    );
    assert!(
        p1.graveyard
            .iter()
            .all(|id| board.runner.state().objects[id].name == "Plains"),
        "the five milled cards are the Plains"
    );
    assert_eq!(p1.library.len(), 1, "one of the six Plains is left");
    assert_eq!(
        p1.hand.iter().copied().collect::<Vec<_>>(),
        vec![board.imp],
        "CR 614.6: no card was drawn; Dredge returned Stinkweed Imp"
    );

    let events = &board.events;
    assert_eq!(gift_resolved(events), 1, "the gift completes exactly once");
    let gift = position(events, "gift completion", |e| {
        matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::GiftDelivery,
                ..
            }
        )
    });
    let imp_returned = position(
        events,
        "Stinkweed Imp's return to hand",
        |e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == board.imp),
    );
    let returned = position(
        events,
        "the return to the battlefield",
        |e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == board.target),
    );
    assert!(
        imp_returned < gift,
        "the gift completes after Dredge returned the Imp"
    );
    assert!(
        gift < returned,
        "CR 702.174j: the gift completes before the spell's other effects"
    );
}

/// CR 121.1: Pins the `start_draw_sequence` migration bug fix. Gift used to
/// call `select_cards_to_draw` directly, skipping `allowed_draw_count` and
/// incorrectly drawing through a `CantDraw` static.
#[test]
fn gift_card_draw_respects_cant_draw_static() {
    let (mut scenario, spell, target) = promised_gift_scenario();
    let gift_card = scenario.add_card_to_library_top(P1, "Gift Recipient Card");
    let cant_draw_source = scenario.add_creature(P1, "Cant Draw Source", 1, 1).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&cant_draw_source)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(StaticMode::CantDraw {
            who: ProhibitionScope::AllPlayers,
        }));

    let hand_before = hand_count(&runner, P1);
    let library_before = library_count(&runner, P1);
    let mut events = Vec::new();
    assert_eq!(
        cast_promised_coiling(&mut runner, spell, target, &mut events),
        GiftResolution::Resolved
    );
    assert_eq!(
        gift_resolved(&events),
        1,
        "with no pause the gift completes inline, once"
    );

    assert_eq!(hand_count(&runner, P1), hand_before);
    assert_eq!(library_count(&runner, P1), library_before);
    assert_eq!(object_zone(&runner, gift_card), Zone::Library);
}
