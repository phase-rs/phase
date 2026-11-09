//! CR 121.2 + CR 121.2a: a draw cost is one instruction of its printed size
//! wherever it is paid; driven through the production chain resolver for
//! `Effect::PayCost` (no printed card carries a multi-card draw cost — the
//! typed-input contract).
//!
//! Every row resolves a real `ResolvedAbility` through
//! `effects::resolve_ability_chain`, and every negative row is paired with a
//! positive reach guard on the same board.
//!
//! CR 614.17b + CR 118.12: a fresh draw cost whose draw can't happen can't be
//! chosen, singleton or `Composite`, while a payment already chosen resumes
//! latched — a later can't-effect clamps each draw but never unpays it.

use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, DiscardSelfScope, Effect,
    QuantityExpr, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Alms Collector, verbatim Oracle text: an instruction-level CR 121.2a
/// replacement that applies only to an instruction to draw two or more cards.
const ALMS_COLLECTOR_ORACLE: &str = "Flash\nIf an opponent would draw two or more cards, \
instead you and that player each draw a card.";

/// Maralen of the Mornsong, verbatim Oracle text: a CR 121.3 can't-draw effect.
const MARALEN_ORACLE: &str = "Players can't draw cards.\nAt the beginning of each player's draw \
step, that player loses 3 life, searches their library for a card, puts it into their hand, then shuffles.";

/// Spirit of the Labyrinth, verbatim Oracle text: a CR 121.2b one-card-per-turn limit.
const SPIRIT_OF_THE_LABYRINTH_ORACLE: &str = "Each player can't draw more than one card each turn.";

/// Stinkweed Imp, verbatim Oracle text: Dredge 5 makes each of P0's draws a replacement choice.
const STINKWEED_IMP_ORACLE: &str = "Flying\nWhenever this creature deals combat damage to a \
creature, destroy that creature.\nDredge 5 (If you would draw a card, you may mill five cards \
instead. If you do, return this card from your graveyard to your hand.)";

/// Humility, verbatim Oracle text: mutes Maralen while it is on the battlefield.
const HUMILITY_ORACLE: &str =
    "All creatures lose all abilities and have base power and toughness 1/1.";

/// Enough cards that every row's draws stay legal for both players.
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

/// One `cards`-card draw instruction by the ability's controller.
fn draw_effect(cards: i32) -> Effect {
    Effect::Draw {
        count: QuantityExpr::Fixed { value: cards },
        target: TargetFilter::Controller,
    }
}

fn draw_cost(cards: i32) -> AbilityCost {
    AbilityCost::EffectCost {
        effect: Box::new(draw_effect(cards)),
    }
}

/// Signed hand and library changes for both players, and whether the payment
/// was reported as failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Deltas {
    p0_hand: i64,
    p1_hand: i64,
    p0_library: i64,
    p1_library: i64,
    payment_failed: bool,
}

fn hand_and_library(runner: &GameRunner, player: PlayerId) -> (i64, i64) {
    let player_state = &runner.state().players[player.0 as usize];
    (
        player_state.hand.len() as i64,
        player_state.library.len() as i64,
    )
}

/// P0 resolves `effect` from a source creature in their main phase, with Alms
/// Collector under P1 when `with_alms` is set. Both libraries are staged.
fn resolve_on_board(effect: Effect, with_alms: bool) -> Deltas {
    resolve_on_prepared_board(effect, |scenario| {
        if with_alms {
            scenario.add_creature_from_oracle(P1, "Alms Collector", 3, 4, ALMS_COLLECTOR_ORACLE);
        }
    })
}

/// P0 resolves `effect` from a source creature in their main phase on a board
/// `prepare` completes. Both libraries are staged, and layers are evaluated
/// after `build()` so every static ability is live.
fn resolve_on_prepared_board(effect: Effect, prepare: impl FnOnce(&mut GameScenario)) -> Deltas {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    scenario.with_library_top(P1, &STAGED_LIBRARY);
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    prepare(&mut scenario);
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    let (p0_hand_before, p0_library_before) = hand_and_library(&runner, P0);
    let (p1_hand_before, p1_library_before) = hand_and_library(&runner, P1);

    let ability = ResolvedAbility::new(effect, vec![], source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("the draw effect or draw cost resolves");

    let (p0_hand_after, p0_library_after) = hand_and_library(&runner, P0);
    let (p1_hand_after, p1_library_after) = hand_and_library(&runner, P1);
    Deltas {
        p0_hand: p0_hand_after - p0_hand_before,
        p1_hand: p1_hand_after - p1_hand_before,
        p0_library: p0_library_after - p0_library_before,
        p1_library: p1_library_after - p1_library_before,
        payment_failed: runner.state().cost_payment_failed_flag,
    }
}

/// CR 121.2 + CR 121.2a: a two-card draw cost is ONE instruction to draw two
/// cards, so Alms Collector ("two or more cards") applies to it: P0 and P1 each
/// draw one. A one-card cost is outside Alms Collector's reach, and CR 702.24a
/// repetition (a `Composite` of one-card legs) stays two one-card instructions,
/// never one summed instruction.
#[test]
fn a_two_card_draw_cost_is_one_instruction() {
    // Reach guard: Alms Collector is live on this board for an ordinary two-card draw.
    let ordinary = resolve_on_board(draw_effect(2), true);
    assert_eq!(
        (ordinary.p0_library, ordinary.p1_library),
        (-1, -1),
        "Alms Collector replaces an ordinary two-card draw, got {ordinary:?}"
    );

    // 0-candidate row: with no replacement the two-card cost draws two cards.
    let unreplaced = resolve_on_board(
        Effect::PayCost {
            cost: draw_cost(2),
            scale: None,
            payer: TargetFilter::Controller,
        },
        false,
    );
    assert_eq!(
        unreplaced,
        Deltas {
            p0_hand: 2,
            p1_hand: 0,
            p0_library: -2,
            p1_library: 0,
            payment_failed: false,
        },
        "an unreplaced two-card draw cost draws two cards"
    );

    let two_card_cost = resolve_on_board(
        Effect::PayCost {
            cost: draw_cost(2),
            scale: None,
            payer: TargetFilter::Controller,
        },
        true,
    );
    assert_eq!(
        two_card_cost,
        Deltas {
            p0_hand: 1,
            p1_hand: 1,
            p0_library: -1,
            p1_library: -1,
            payment_failed: false,
        },
        "Alms Collector applies to a two-card draw cost: one card for each player"
    );

    let one_card_cost = resolve_on_board(
        Effect::PayCost {
            cost: draw_cost(1),
            scale: None,
            payer: TargetFilter::Controller,
        },
        true,
    );
    assert_eq!(
        one_card_cost,
        Deltas {
            p0_hand: 1,
            p1_hand: 0,
            p0_library: -1,
            p1_library: 0,
            payment_failed: false,
        },
        "Alms Collector does not apply to a one-card draw cost"
    );

    let repeated_one_card_cost = resolve_on_board(
        Effect::PayCost {
            cost: AbilityCost::Composite {
                costs: vec![draw_cost(1), draw_cost(1)],
            },
            scale: None,
            payer: TargetFilter::Controller,
        },
        true,
    );
    assert_eq!(
        repeated_one_card_cost,
        Deltas {
            p0_hand: 2,
            p1_hand: 0,
            p0_library: -2,
            p1_library: 0,
            payment_failed: false,
        },
        "CR 702.24a: two one-card legs are two one-card instructions, never one two-card instruction"
    );
}

fn pay_cost(cost: AbilityCost) -> Effect {
    Effect::PayCost {
        cost,
        scale: None,
        payer: TargetFilter::Controller,
    }
}

fn draw_legs(legs: usize) -> AbilityCost {
    AbilityCost::Composite {
        costs: vec![draw_cost(1); legs],
    }
}

fn with_maralen_under(controller: PlayerId) -> impl FnOnce(&mut GameScenario) {
    move |scenario| {
        scenario.add_creature_from_oracle(
            controller,
            "Maralen of the Mornsong",
            2,
            3,
            MARALEN_ORACLE,
        );
    }
}

fn with_spirit(scenario: &mut GameScenario) {
    scenario.add_creature_from_oracle(
        P1,
        "Spirit of the Labyrinth",
        3,
        1,
        SPIRIT_OF_THE_LABYRINTH_ORACLE,
    );
}

/// No library movement and a refused payment.
const REFUSED: Deltas = Deltas {
    p0_hand: 0,
    p1_hand: 0,
    p0_library: 0,
    p1_library: 0,
    payment_failed: true,
};

/// CR 614.17b + CR 121.3: a fresh one-card draw cost under a can't-draw effect can't be chosen, whoever
/// controls the effect. The production `Effect::PayCost` route refuses it at the payment authority's fresh
/// entry, before the Draw arm.
///
/// Revert probe: emptying the authority's `FreshChoice` gate settles both Maralen rows Paid with a clamped
/// draw (flag false).
#[test]
fn a_fresh_draw_cost_under_a_cant_draw_effect_is_refused() {
    // Reach guard: without Maralen the cost is paid and the card drawn.
    let unrestricted = resolve_on_prepared_board(pay_cost(draw_cost(1)), |_| {});
    assert_eq!(
        (unrestricted.p0_hand, unrestricted.payment_failed),
        (1, false)
    );

    for controller in [P1, P0] {
        let deltas =
            resolve_on_prepared_board(pay_cost(draw_cost(1)), with_maralen_under(controller));
        assert_eq!(
            deltas, REFUSED,
            "Maralen under {controller:?}: a draw the drawer can't make can't be chosen"
        );
    }
}

/// CR 614.17b + CR 121.2b: "the player can't pay a cost that includes drawing multiple cards" under a
/// one-card limit. A fresh two-card draw cost is refused whole; a one-card cost on the same board is paid.
///
/// Revert probe: emptying the `FreshChoice` gate pays the two-card cost with one card drawn (flag false).
#[test]
fn a_fresh_two_card_draw_cost_under_a_one_card_limit_is_refused() {
    // Reach guard: Spirit of the Labyrinth admits one card.
    let one_card = resolve_on_prepared_board(pay_cost(draw_cost(1)), with_spirit);
    assert_eq!((one_card.p0_hand, one_card.payment_failed), (1, false));

    let two_cards = resolve_on_prepared_board(pay_cost(draw_cost(2)), with_spirit);
    assert_eq!(
        two_cards, REFUSED,
        "a two-card draw cost can't be chosen under a one-card limit"
    );
}

/// CR 614.17b + CR 121.2b + CR 601.2h: a top-level `Composite` draw cost is staged (its pay.rs pre-gate is
/// bypassed under transaction replay), and the authority's fresh entry judges the whole choice at `begin`:
/// refused before any leg is paid, with no transaction left behind.
///
/// Revert probe: emptying the `FreshChoice` gate pays the Spirit row one card and the Maralen row none,
/// both reported paid.
#[test]
fn a_fresh_composite_draw_cost_is_refused_as_a_whole() {
    // Reach guard: no restriction, both legs are paid.
    let mut deltas = resolve_on_prepared_board(pay_cost(draw_legs(2)), |_| {});
    assert_eq!((deltas.p0_hand, deltas.payment_failed), (2, false));

    deltas = resolve_on_prepared_board(pay_cost(draw_legs(2)), with_spirit);
    assert_eq!(
        deltas, REFUSED,
        "two one-card legs can't be chosen under a one-card limit"
    );

    deltas = resolve_on_prepared_board(pay_cost(draw_legs(2)), with_maralen_under(P1));
    assert_eq!(deltas, REFUSED, "no draw leg can be chosen under Maralen");
}

/// A three-player board for a staged draw cost: P0's draws are Dredge prompts (Stinkweed Imp), Maralen
/// is under P1, and Humility, owned and controlled by P2, mutes her. Returns the runner and P0's source.
fn concession_board() -> (GameRunner, engine::types::identifiers::ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..3 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    scenario.add_creature_from_oracle(P1, "Maralen of the Mornsong", 2, 3, MARALEN_ORACLE);
    scenario.add_enchantment_from_oracle(PlayerId(2), "Humility", HUMILITY_ORACLE);
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    (runner, source)
}

/// The concession board with two cards in P0's hand, so the discard leg of a
/// `Composite` cost has a real choice after the draw leg.
fn concession_board_with_hand() -> (GameRunner, engine::types::identifiers::ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..3 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    scenario.add_creature_from_oracle(P1, "Maralen of the Mornsong", 2, 3, MARALEN_ORACLE);
    scenario.add_enchantment_from_oracle(PlayerId(2), "Humility", HUMILITY_ORACLE);
    scenario.add_card_to_hand(P0, "Hand A");
    scenario.add_card_to_hand(P0, "Hand B");
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    (runner, source)
}

/// One chosen discard by the ability's controller.
fn discard_cost() -> AbilityCost {
    AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    }
}

/// The concession board with two hand cards, but no Dredge source: the draw leg
/// pays synchronously under Humility and the discard leg opens the first prompt.
fn concession_board_draw_pays_synchronously() -> (GameRunner, engine::types::identifiers::ObjectId)
{
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..3 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    scenario.add_creature_from_oracle(P1, "Maralen of the Mornsong", 2, 3, MARALEN_ORACLE);
    scenario.add_enchantment_from_oracle(PlayerId(2), "Humility", HUMILITY_ORACLE);
    scenario.add_card_to_hand(P0, "Hand A");
    scenario.add_card_to_hand(P0, "Hand B");
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    (runner, source)
}

fn is_dredge_prompt_for_p0(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P0)
}

/// P0 resolves `PayCost{Composite[Draw 1; legs]}` with a GainLife 3 rider on the concession board. Without a
/// concession every leg raises a legal Dredge prompt, each declined. With one, P2 concedes at leg 1's prompt.
/// Returns the hand delta, the life delta and the failure flag, after asserting the payment settled with no
/// transaction and, after a concession, that no Dredge was offered or applied.
fn pay_staged_draw_cost_around_a_concession(legs: usize, concede: bool) -> (i64, i32, bool) {
    let (mut runner, source) = concession_board();
    let mut root = ResolvedAbility::new(pay_cost(draw_legs(legs)), vec![], source, P0);
    root.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let life_before = runner.life(P0);
    let (hand_before, library_before) = hand_and_library(&runner, P0);
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");

    // Reach guard: Humility mutes Maralen, so the fresh gate admitted the choice and staged it, and leg 1's
    // draw can happen, so Dredge is a legal replacement of it.
    assert!(state.payment_transaction.is_some(), "the payment is staged");
    assert!(
        is_dredge_prompt_for_p0(state),
        "leg 1 raised its Dredge prompt"
    );

    if concede {
        // CR 104.3a + CR 800.4a: P2 leaves the game and Humility with them, so Maralen's "Players can't draw
        // cards" applies again. The staged payment re-materializes latched on that base: every leg's draw
        // can't happen, so Dredge can't replace it (CR 614.17c) and each draw is clamped. Nothing waits on
        // P0 any more, so the payment commits paid and its rider runs.
        apply(
            state,
            PlayerId(2),
            GameAction::Concede {
                player_id: PlayerId(2),
            },
        )
        .expect("a player can concede at any time");
        assert!(
            !is_dredge_prompt_for_p0(state),
            "no Dredge prompt survives the concession"
        );
        let graveyard = &runner.state().players[P0.0 as usize].graveyard;
        assert_eq!(graveyard.len(), 1, "nothing was milled");
        assert_eq!(
            runner.state().objects[&graveyard[0]].name,
            "Stinkweed Imp",
            "the Imp was not returned"
        );
        let (_, library_after) = hand_and_library(&runner, P0);
        assert_eq!(library_after, library_before, "no card left P0's library");
    } else {
        for leg in 1..=legs {
            assert!(
                is_dredge_prompt_for_p0(state),
                "leg {leg} raised its Dredge prompt"
            );
            apply(state, P0, GameAction::ChooseReplacement { index: 1 })
                .unwrap_or_else(|error| panic!("declining Dredge on leg {leg}: {error:?}"));
        }
    }

    let state = runner.state();
    assert!(state.payment_transaction.is_none(), "the payment committed");
    assert!(state.active_ability_continuation().is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    let failed = state.cost_payment_failed_flag;
    let (hand_after, _) = hand_and_library(&runner, P0);
    (
        hand_after - hand_before,
        runner.life(P0) - life_before,
        failed,
    )
}

/// CR 118.12 + CR 614.17a + CR 118.11 + CR 614.17c: a staged draw cost P0 already chose stays paid when another
/// player's concession (CR 104.3a + CR 800.4a) removes Humility mid-payment and Maralen's can't-draw effect
/// returns. The re-materialized payment is latched: its draws are clamped, never unpaid, no Dredge replaces a
/// draw that can't happen, and the rider runs once. Control: without the concession every leg's Dredge is
/// legal (declined here) and both cards are drawn.
///
/// Revert probes: (A) `replay` not installing `LatchedSuffix` re-gates the root fresh, so the concession aborts
/// the payment (flag true); (B) dropping the can't-happen filter from `find_applicable_replacements` keeps a
/// Dredge prompt pending after the concession; (C) dropping `settle_after_outside_action` from the concession
/// path leaves the stale leg-1 prompt as the canonical `waiting_for`.
#[test]
fn another_players_concession_does_not_unpay_a_staged_draw_cost() {
    assert_eq!(
        pay_staged_draw_cost_around_a_concession(2, false),
        (2, 3, false)
    );

    assert_eq!(
        pay_staged_draw_cost_around_a_concession(2, true),
        (0, 3, false),
        "the chosen payment settles paid, its draws clamped, and the rider runs once"
    );
}

/// CR 118.12 + CR 702.24a: the three-leg form of the concession row (and its control), driven through the
/// production route.
#[test]
fn another_players_concession_does_not_unpay_a_three_leg_draw_cost() {
    assert_eq!(
        pay_staged_draw_cost_around_a_concession(3, false),
        (3, 3, false)
    );
    assert_eq!(
        pay_staged_draw_cost_around_a_concession(3, true),
        (0, 3, false),
        "every nested remainder stays latched"
    );
}

/// CR 104.3a + CR 800.4a + CR 614.17a + CR 614.17c + CR 118.12: P0 already answered leg 1's Dredge
/// prompt (declined it, so the first draw happened and leg 2 opened) when P2 concedes. Humility leaves
/// with P2, so Maralen's can't-draw applies again for the outstanding leg. The accepted prefix must
/// survive the outside action: the performed first draw is retained, no later draw is proposed and no
/// Dredge is offered for it, the payment stays paid, the rider runs once, and the descriptor retires
/// with the canonical and projected waits agreeing.
///
/// Revert probes: (A) replaying the whole root on the departure-adjusted base re-executes the accepted
/// leg under the new prohibition, so the first draw is lost; (B) leaving the replay error in
/// `settle_after_outside_action` keeps the stale descriptor and the pre-concession replacement prompt.
#[test]
fn a_concession_after_one_declined_replacement_keeps_the_performed_draw() {
    let (mut runner, source) = concession_board();
    let mut root = ResolvedAbility::new(pay_cost(draw_legs(2)), vec![], source, P0);
    root.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let life_before = runner.life(P0);
    let (hand_before, library_before) = hand_and_library(&runner, P0);
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");

    // Reach guard: Humility mutes Maralen, so the fresh gate admitted the choice and staged it.
    assert!(state.payment_transaction.is_some(), "the payment is staged");
    assert!(
        is_dredge_prompt_for_p0(state),
        "leg 1 raised its Dredge prompt"
    );

    // Accept the first draw by declining Dredge. The recorded answer opens leg 2's prompt.
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");
    assert!(
        is_dredge_prompt_for_p0(state),
        "leg 2 raised its Dredge prompt"
    );
    assert_eq!(
        state
            .payment_transaction
            .as_ref()
            .expect("the descriptor survives the first answer")
            .transcript
            .len(),
        1,
        "the accepted answer is recorded"
    );

    // CR 104.3a: P2 leaves and takes Humility with them; Maralen applies again for the outstanding leg.
    let concession = apply(
        state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("a player can concede at any time");

    let state = runner.state();
    assert!(
        state.payment_transaction.is_none(),
        "the accepted prefix settles and the descriptor retires"
    );
    assert!(state.active_ability_continuation().is_none());
    assert!(!is_dredge_prompt_for_p0(state), "no Dredge survives");
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(!state.cost_payment_failed_flag, "the payment stays paid");

    let graveyard = &state.players[P0.0 as usize].graveyard;
    assert_eq!(graveyard.len(), 1, "nothing was milled");
    assert_eq!(
        state.objects[&graveyard[0]].name, "Stinkweed Imp",
        "the Imp was not returned"
    );
    let (hand_after, library_after) = hand_and_library(&runner, P0);
    assert_eq!(
        hand_after - hand_before,
        1,
        "the performed first draw is retained and the prohibited second draw is clamped"
    );
    assert_eq!(
        library_after,
        library_before - 1,
        "only the performed draw left P0's library"
    );
    assert_eq!(runner.life(P0) - life_before, 3, "the rider ran once");

    // The committed board is the departure-adjusted one, and the published
    // events describe that same board: P2 is gone with Humility, and the prefix
    // draw and rider each appear exactly once (CR 118.12 + CR 614.17a).
    assert!(state.eliminated_players.contains(&PlayerId(2)));
    assert!(
        !state
            .battlefield
            .iter()
            .any(|id| state.objects[id].name == "Humility"),
        "the conceding player's Humility left the battlefield"
    );
    assert!(concession.events.iter().any(|event| matches!(
        event,
        GameEvent::PlayerEliminated { player_id } if *player_id == PlayerId(2)
    )));
    let p0_draws: u32 = concession
        .events
        .iter()
        .map(|event| match event {
            GameEvent::CardDrawn { player_id, .. } if *player_id == P0 => 1,
            GameEvent::CardsDrawn { player_id, count } if *player_id == P0 => *count,
            _ => 0,
        })
        .sum();
    assert_eq!(
        p0_draws, 1,
        "the performed prefix draw is published exactly once"
    );
    assert_eq!(
        concession
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::LifeChanged { player_id, amount, .. }
                    if *player_id == P0 && *amount == 3
            ))
            .count(),
        1,
        "the rider's life gain is published exactly once"
    );
}

/// CR 104.3a + CR 800.4a + CR 614.17a + CR 118.12: the accepted prefix is
/// followed by another open choice (the discard leg). A concession must still
/// keep the performed draw in canonical state and resume the outstanding cursor,
/// with the printed rider deferred until the payment actually completes.
///
/// Revert probe: dropping the recovered-rider carrier makes the concession fall
/// back to the departure-adjusted replay, which loses the performed draw and
/// cannot answer the recorded Dredge decline.
#[test]
fn a_concession_after_one_declined_replacement_keeps_the_prefix_when_a_discard_is_open() {
    let (mut runner, source) = concession_board_with_hand();
    let mut root = ResolvedAbility::new(
        pay_cost(AbilityCost::Composite {
            costs: vec![draw_cost(1), discard_cost()],
        }),
        vec![],
        source,
        P0,
    );
    root.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let life_before = runner.life(P0);
    let (hand_before, library_before) = hand_and_library(&runner, P0);
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");

    assert!(state.payment_transaction.is_some(), "the payment is staged");
    assert!(
        is_dredge_prompt_for_p0(state),
        "leg 1 raised its Dredge prompt"
    );
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");
    let WaitingFor::DiscardChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "the discard leg owns the outstanding cursor: {:?}",
            state.waiting_for
        );
    };
    let cards = cards.clone();
    assert!(
        cards.len() >= 2,
        "the drawn card and the setup cards are eligible"
    );

    apply(
        state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("a player can concede at any time");

    let state = runner.state();
    assert!(
        state
            .payment_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.recovered.is_some()),
        "only the printed rider remains deferred"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }),
        "the outstanding discard survives the concession"
    );
    assert_eq!(
        runner.life(P0) - life_before,
        0,
        "the rider waits for completion"
    );

    let answer = apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![cards[0]],
        },
    )
    .expect("discard one of the eligible cards");
    assert_eq!(
        answer
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::CardDrawn { player_id, .. } if *player_id == P0
            ))
            .count(),
        1,
        "the preserved prefix draw is published once at completion"
    );

    let state = runner.state();
    assert!(
        state.payment_transaction.is_none(),
        "the descriptor retires after the payment completes"
    );
    assert!(state.active_ability_continuation().is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(!state.cost_payment_failed_flag, "the payment stays paid");
    assert_eq!(
        hand_and_library(&runner, P0).0 - hand_before,
        0,
        "one card was drawn and one discarded"
    );
    assert_eq!(
        hand_and_library(&runner, P0).1,
        library_before - 1,
        "only the performed draw left the library"
    );
    assert_eq!(runner.life(P0) - life_before, 3, "the rider ran once");
}

/// CR 614.17a + CR 118.12: a synchronously paid prefix (no transcript) must also
/// survive a concession when another choice remains open.
#[test]
fn a_concession_keeps_a_synchronously_paid_prefix_when_a_discard_is_open() {
    let (mut runner, source) = concession_board_draw_pays_synchronously();
    let mut root = ResolvedAbility::new(
        pay_cost(AbilityCost::Composite {
            costs: vec![draw_cost(1), discard_cost()],
        }),
        vec![],
        source,
        P0,
    );
    root.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let life_before = runner.life(P0);
    let (hand_before, _library_before) = hand_and_library(&runner, P0);
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    assert!(state.payment_transaction.is_some(), "the payment is staged");
    assert_eq!(
        state
            .payment_transaction
            .as_ref()
            .expect("the descriptor is live")
            .transcript
            .len(),
        0,
        "the draw leg paid synchronously before the first prompt"
    );
    let WaitingFor::DiscardChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "the discard leg owns the first prompt: {:?}",
            state.waiting_for
        );
    };
    let cards = cards.clone();
    assert!(cards.len() >= 2);

    apply(
        state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("a player can concede at any time");

    let state = runner.state();
    assert!(
        state
            .payment_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.recovered.is_some()),
        "the synchronously paid prefix and the open discard are committed"
    );
    assert!(matches!(
        state.waiting_for,
        WaitingFor::DiscardChoice { .. }
    ));
    assert_eq!(runner.life(P0) - life_before, 0);

    let answer = apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![cards[0]],
        },
    )
    .expect("discard one of the eligible cards");
    assert_eq!(
        answer
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::CardDrawn { player_id, .. } if *player_id == P0
            ))
            .count(),
        1,
        "the synchronously performed prefix draw is published once"
    );

    let state = runner.state();
    assert!(state.payment_transaction.is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(runner.life(P0) - life_before, 3, "the rider ran once");
    assert_eq!(
        hand_and_library(&runner, P0).0 - hand_before,
        0,
        "one card was drawn and one discarded"
    );
}

/// CR 614.17c: accepting a parked Dredge after the draw becomes impossible must
/// not mill or return the Imp; the draw settles clamped and the payment resumes.
#[test]
fn accepting_a_parked_dredge_after_the_draw_becomes_impossible_declines_it() {
    let (mut runner, source) = concession_board();
    let mut root = ResolvedAbility::new(pay_cost(draw_cost(1)), vec![], source, P0);
    root.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let life_before = runner.life(P0);
    let (hand_before, library_before) = hand_and_library(&runner, P0);
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    assert!(is_dredge_prompt_for_p0(state));

    // Activate Maralen without a concession: Humility leaves the battlefield.
    let humility = state
        .objects
        .values()
        .find(|object| object.name == "Humility")
        .map(|object| object.id)
        .expect("Humility is on the board");
    state.battlefield.retain(|id| *id != humility);
    state.objects.get_mut(&humility).expect("Humility").zone =
        engine::types::zones::Zone::Graveyard;
    evaluate_layers(state);
    assert!(is_dredge_prompt_for_p0(state), "the parked prompt remains");

    // Accept the stale Dredge. The shared resume boundary rechecks it.
    apply(state, P0, GameAction::ChooseReplacement { index: 0 }).expect("the stale answer settles");

    let state = runner.state();
    assert!(state.payment_transaction.is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    let graveyard = &state.players[P0.0 as usize].graveyard;
    assert_eq!(graveyard.len(), 1, "nothing was milled");
    assert_eq!(state.objects[&graveyard[0]].name, "Stinkweed Imp");
    let (hand_after, library_after) = hand_and_library(&runner, P0);
    assert_eq!(
        hand_after - hand_before,
        0,
        "the impossible draw is clamped"
    );
    assert_eq!(library_after, library_before, "no card left the library");
    assert_eq!(
        runner.life(P0) - life_before,
        3,
        "the payment still resumes"
    );
}

/// CR 104.3a: a fallible payment recovery (an overflowing rider) must not undo
/// the concession itself.
#[test]
fn a_failed_payment_recovery_still_commits_the_concession() {
    let (mut runner, source) = concession_board();
    let mut root = ResolvedAbility::new(pay_cost(draw_legs(2)), vec![], source, P0);
    root.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let state = runner.state_mut();
    state.players[P0.0 as usize].life = i32::MAX;
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    assert!(is_dredge_prompt_for_p0(state));
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");

    apply(
        state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("the concession stands even when the rider overflows");

    let state = runner.state();
    assert!(
        state.eliminated_players.contains(&PlayerId(2)),
        "the conceding player left the game"
    );
    assert!(
        state.payment_transaction.is_none(),
        "the fallible payment state is retired"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the failed recovery publishes a valid priority boundary, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        runner.life(P0),
        i32::MAX,
        "the overflowing rider did not apply"
    );
}

/// Obstinate Familiar, verbatim Oracle text: an optional skip of a single draw.
const OBSTINATE_FAMILIAR_ORACLE: &str = "If you would draw a card, you may skip that draw instead.";

/// CR 118.11 + CR 614.11a: a skipped draw still pays a draw-cost activation, so
/// the activation must reach the stack after the skip is accepted.
#[test]
fn accepting_a_skipped_draw_cost_activation_reaches_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    scenario.with_library_top(P1, &STAGED_LIBRARY);
    let source = scenario
        .add_creature(P0, "Draw Cost Source", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::EffectCost {
                effect: Box::new(draw_effect(1)),
            }),
        )
        .id();
    scenario.add_creature_from_oracle(P0, "Obstinate Familiar", 1, 1, OBSTINATE_FAMILIAR_ORACLE);
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    let (hand_before, library_before) = hand_and_library(&runner, P0);
    let life_before = runner.life(P0);

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the draw-cost activation begins");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P0
        ),
        "the draw cost offers Obstinate Familiar's skip: {:?}",
        runner.state().waiting_for
    );

    // Index 0 accepts the optional skip.
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the skip");

    let state = runner.state();
    assert!(
        state.pending_cast.is_none(),
        "the activation is no longer a bare pending cast: waiting={:?} cast={} stack={}",
        state.waiting_for,
        state.pending_cast.is_some(),
        state.resolution_stack.len()
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(
        state.stack.len(),
        1,
        "the activation reached the stack: waiting={:?}",
        state.waiting_for
    );
    assert_eq!(
        runner.life(P0) - life_before,
        0,
        "the ability is on the stack, not resolved"
    );
    let (hand_after, library_after) = hand_and_library(&runner, P0);
    assert_eq!(hand_after, hand_before, "the skipped draw drew nothing");
    assert_eq!(
        library_after, library_before,
        "the skipped draw milled nothing"
    );
}
