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
use engine::game::engine::{apply, EngineError};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, DiscardSelfScope, Effect,
    QuantityExpr, ResolvedAbility, TargetFilter,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoPassMode, CastPaymentMode, GameState, PersistedGameState, PersistedRestoreError,
    StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

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
    // CR 614.17a: the card discarded here is exactly the one drawn during the
    // accepted prefix, not a later or substitute card.
    let drawn = state
        .payment_transaction
        .as_ref()
        .and_then(|transaction| transaction.recovered.as_ref())
        .expect("the recovery checkpoint is live")
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::CardDrawn {
                player_id,
                object_id,
                ..
            } if *player_id == P0 => Some(*object_id),
            _ => None,
        })
        .expect("the accepted prefix published its draw");
    assert!(
        cards.contains(&drawn),
        "the drawn card is one of the discard choices"
    );

    let answer = apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards { cards: vec![drawn] },
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
    let drawn = state
        .payment_transaction
        .as_ref()
        .and_then(|transaction| transaction.recovered.as_ref())
        .expect("the recovery checkpoint is live")
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::CardDrawn {
                player_id,
                object_id,
                ..
            } if *player_id == P0 => Some(*object_id),
            _ => None,
        })
        .expect("the synchronously paid prefix published its draw");
    assert!(cards.contains(&drawn));

    let answer = apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards { cards: vec![drawn] },
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
/// the concession itself. The departure is published with its events and the
/// next ordinary action starts from it.
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
    let (hand_before, _) = hand_and_library(&runner, P0);
    let state = runner.state_mut();
    state.players[P0.0 as usize].life = i32::MAX;
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    assert!(is_dredge_prompt_for_p0(state));
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");

    let published = apply(
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
    assert_eq!(
        hand_and_library(&runner, P0).0 - hand_before,
        1,
        "the accepted prefix draw survives the fallible rider"
    );
    assert!(
        published.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerEliminated { player_id } if *player_id == PlayerId(2)
        )),
        "the departure's events are published with the action"
    );
    let state = runner.state_mut();
    apply(state, P0, GameAction::PassPriority)
        .expect("the next ordinary action starts from the committed departure");
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

/// CR 104.3a + CR 118.12: a submitter-scoped capability change admitted while a
/// recovered payment is parked must survive settlement; the checkpoint must not
/// roll it back.
#[test]
fn a_revoked_debug_capability_survives_payment_settlement() {
    let (mut runner, source) = concession_board_with_hand();
    {
        let state = runner.state_mut();
        state.format_config.allow_debug_actions = true;
        state.debug_mode = true;
        state.debug_permitted = [P0, P1, PlayerId(2)].into_iter().collect();
    }
    let mut root = ResolvedAbility::new(
        pay_cost(AbilityCost::Composite {
            costs: vec![draw_cost(1), discard_cost()],
        }),
        vec![],
        source,
        P0,
    );
    root.targets = Vec::new();
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    assert!(is_dredge_prompt_for_p0(state));
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");
    let WaitingFor::DiscardChoice { cards, .. } = &state.waiting_for else {
        panic!("the discard leg owns the cursor: {:?}", state.waiting_for);
    };
    let cards = cards.clone();

    apply(
        state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("a player can concede at any time");
    assert!(state
        .payment_transaction
        .as_ref()
        .is_some_and(|transaction| transaction.recovered.is_some()));

    apply(
        state,
        P0,
        GameAction::RevokeDebugPermission { player_id: P1 },
    )
    .expect("the host revokes the other seat's debug capability");
    assert!(!state.debug_permitted.contains(&P1));

    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![cards[0]],
        },
    )
    .expect("discard one of the eligible cards");

    let state = runner.state();
    assert!(
        state.payment_transaction.is_none(),
        "the payment settles after the answer"
    );
    assert!(
        !state.debug_permitted.contains(&P1),
        "the revoked capability was not restored by the checkpoint"
    );
    assert!(state.debug_permitted.contains(&P0));
}

/// CR 402.3 + CR 118.12: a hand reorder admitted while a recovered payment is
/// parked names only the live canonical hand; the checkpoint's drawn card must
/// stay in the hand and the player's canonical order must survive settlement.
#[test]
fn a_hand_reorder_between_recovery_and_completion_keeps_the_drawn_card() {
    let (mut runner, source) = concession_board_with_hand();
    let mut root = ResolvedAbility::new(
        pay_cost(AbilityCost::Composite {
            costs: vec![draw_cost(1), discard_cost()],
        }),
        vec![],
        source,
        P0,
    );
    root.sub_ability = None;
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    assert!(is_dredge_prompt_for_p0(state));
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");
    let WaitingFor::DiscardChoice { cards, .. } = &state.waiting_for else {
        panic!("the discard leg owns the cursor: {:?}", state.waiting_for);
    };
    let cards = cards.clone();
    apply(
        state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("a player can concede at any time");
    let canonical_hand: Vec<_> = state.players[P0.0 as usize].hand.iter().copied().collect();
    assert_eq!(canonical_hand.len(), 2);
    let reordered = vec![canonical_hand[1], canonical_hand[0]];
    apply(
        state,
        P0,
        GameAction::ReorderHand {
            order: reordered.clone(),
        },
    )
    .expect("the canonical hand can be reordered while the payment is parked");

    let drawn = state
        .payment_transaction
        .as_ref()
        .and_then(|transaction| transaction.recovered.as_ref())
        .expect("the checkpoint is live")
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::CardDrawn {
                player_id,
                object_id,
                ..
            } if *player_id == P0 => Some(*object_id),
            _ => None,
        })
        .expect("the accepted prefix published its draw");
    assert!(cards.contains(&drawn));
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards { cards: vec![drawn] },
    )
    .expect("discard the prefix-drawn card");

    let state = runner.state();
    assert!(state.payment_transaction.is_none());
    let final_hand: Vec<_> = state.players[P0.0 as usize].hand.iter().copied().collect();
    assert_eq!(
        final_hand, reordered,
        "the accepted canonical order survives and the drawn card was discarded"
    );
}

/// Abundance, verbatim Oracle text: an interactive draw substitute with a
/// land/nonland choice and a bottom-ordering prompt.
const ABUNDANCE_ORACLE: &str = "If you would draw a card, you may instead choose land or \
nonland and reveal cards from the top of your library until you reveal a card \
of the chosen kind. Put that card into your hand and put all other cards \
revealed this way on the bottom of your library in any order.";

/// CR 614.11a + CR 118.11: a bare draw-cost activation resumes after both
/// Abundance choices (the land/nonland choice and the bottom ordering), and the
/// ability reaches the stack.
#[test]
fn a_draw_cost_activation_resumes_after_abundance_bottom_order() {
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
    scenario.add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE_ORACLE);
    let mut runner = scenario.build();
    // Two nonlands above a land so "Land" reveals both misses and asks for the
    // bottom order.
    {
        let state = runner.state_mut();
        let library: Vec<_> = state.players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect();
        for (index, id) in library.iter().enumerate() {
            let object = state.objects.get_mut(id).expect("library card");
            object.card_types.core_types.clear();
            if index == 2 {
                object
                    .card_types
                    .core_types
                    .push(engine::types::card_type::CoreType::Land);
            }
        }
    }
    evaluate_layers(runner.state_mut());

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
        "Abundance offers its optional replacement: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Abundance");
    runner
        .act(GameAction::ChooseOption {
            choice: "Land".to_string(),
        })
        .expect("choose Land");
    let WaitingFor::RevealUntilBottomOrder { cards, .. } = &runner.state().waiting_for else {
        panic!(
            "Abundance asks for the revealed misses' order: {:?}",
            runner.state().waiting_for
        );
    };
    let order = cards.clone();
    runner
        .act(GameAction::SelectCards { cards: order })
        .expect("submit the bottom order");

    let state = runner.state();
    assert!(
        state.pending_cast.is_none(),
        "the bare activation is no longer a pending cast"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "the activation reaches the stack after the whole substitute settles: {:?}",
        state.waiting_for
    );
}

/// CR 402.3 + CR 118.12: the order names the canonical hand; a card the
/// accepted prefix discarded is absent from the checkpoint, so the surviving
/// IDs are reordered and the hidden extras keep their own slots.
#[test]
fn a_reorder_that_names_a_prefix_discarded_card_keeps_the_survivors() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..3 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    scenario.add_card_to_hand(P0, "Hand A");
    scenario.add_card_to_hand(P0, "Hand B");
    scenario.add_card_to_hand(P0, "Hand C");
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    let mut root = ResolvedAbility::new(
        pay_cost(AbilityCost::Composite {
            costs: vec![discard_cost(), draw_cost(1), discard_cost()],
        }),
        vec![],
        source,
        P0,
    );
    root.sub_ability = None;
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    let WaitingFor::DiscardChoice { .. } = &state.waiting_for else {
        panic!(
            "the first discard leg owns the cursor: {:?}",
            state.waiting_for
        );
    };
    let canonical: Vec<_> = state.players[P0.0 as usize].hand.iter().copied().collect();
    assert_eq!(canonical.len(), 3);
    apply(
        state,
        P0,
        GameAction::SelectCards {
            cards: vec![canonical[0]],
        },
    )
    .expect("discard the first canonical card");
    assert!(is_dredge_prompt_for_p0(state));
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining Dredge accepts the draw");
    let WaitingFor::DiscardChoice { .. } = &state.waiting_for else {
        panic!(
            "the second discard leg owns the cursor: {:?}",
            state.waiting_for
        );
    };
    apply(
        state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("a player can concede at any time");

    let checkpoint_hand: Vec<_> = state
        .payment_transaction
        .as_ref()
        .and_then(|transaction| transaction.recovered.as_ref())
        .expect("the checkpoint is live")
        .state
        .players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .collect();
    assert_eq!(checkpoint_hand.len(), 3);
    assert!(
        !checkpoint_hand.contains(&canonical[0]),
        "the accepted prefix discarded the first canonical card"
    );
    let order = vec![canonical[2], canonical[1], canonical[0]];
    apply(state, P0, GameAction::ReorderHand { order })
        .expect("the reorder that names the prefix-discarded card is accepted");

    let after: Vec<_> = state
        .payment_transaction
        .as_ref()
        .and_then(|transaction| transaction.recovered.as_ref())
        .expect("the checkpoint is live")
        .state
        .players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .collect();
    let mut expected = checkpoint_hand.clone();
    let left = expected
        .iter()
        .position(|id| *id == canonical[1])
        .expect("survivor one");
    let right = expected
        .iter()
        .position(|id| *id == canonical[2])
        .expect("survivor two");
    expected.swap(left, right);
    assert_eq!(
        after, expected,
        "the two surviving canonical IDs swap; the hidden prefix card keeps its slot"
    );
    assert!(
        state.payment_transaction.is_some(),
        "the payment is still parked"
    );
}

/// CR 118.12 + CR 614.11a: an activation whose retained composite cost parks
/// a draw and still owes a suffix resumes from that suffix after the
/// replacement subtree retires, then reaches the stack.
#[test]
fn a_composite_draw_cost_activation_resumes_its_retained_suffix() {
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
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::EffectCost {
                        effect: Box::new(draw_effect(1)),
                    },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }),
        )
        .id();
    scenario.add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE_ORACLE);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let library: Vec<_> = state.players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect();
        for (index, id) in library.iter().enumerate() {
            let object = state.objects.get_mut(id).expect("library card");
            object.card_types.core_types.clear();
            if index == 2 {
                object
                    .card_types
                    .core_types
                    .push(engine::types::card_type::CoreType::Land);
            }
        }
    }
    evaluate_layers(runner.state_mut());
    let life_before = runner.state().players[P0.0 as usize].life;

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the composite draw-cost activation begins");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P0
        ),
        "Abundance offers its optional replacement: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Abundance");
    runner
        .act(GameAction::ChooseOption {
            choice: "Land".to_string(),
        })
        .expect("choose Land");
    let WaitingFor::RevealUntilBottomOrder { cards, .. } = &runner.state().waiting_for else {
        panic!(
            "Abundance asks for the revealed misses' order: {:?}",
            runner.state().waiting_for
        );
    };
    let order = cards.clone();
    runner
        .act(GameAction::SelectCards { cards: order })
        .expect("submit the bottom order");

    let state = runner.state();
    assert!(
        state.pending_cast.is_none(),
        "the composite activation is no longer a pending cast"
    );
    assert_eq!(
        state.players[P0.0 as usize].life,
        life_before - 1,
        "the retained PayLife suffix was paid after the replacement"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "the activation reaches the stack after the retained suffix settles: {:?}",
        state.waiting_for
    );
}

/// CR 118.12 + CR 614.11a: the activation retains draw ownership even when the
/// source leaves during the replacement's choice; the resume does not need to
/// rediscover the live ability.
#[test]
fn a_composite_draw_cost_activation_resumes_after_the_source_leaves() {
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
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::EffectCost {
                        effect: Box::new(draw_effect(1)),
                    },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }),
        )
        .id();
    scenario.add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE_ORACLE);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let library: Vec<_> = state.players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect();
        for (index, id) in library.iter().enumerate() {
            let object = state.objects.get_mut(id).expect("library card");
            object.card_types.core_types.clear();
            if index == 2 {
                object
                    .card_types
                    .core_types
                    .push(engine::types::card_type::CoreType::Land);
            }
        }
        state.debug_mode = true;
        state.format_config.allow_debug_actions = true;
    }
    evaluate_layers(runner.state_mut());
    let life_before = runner.state().players[P0.0 as usize].life;

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the composite draw-cost activation begins");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Abundance");
    runner
        .act(GameAction::ChooseOption {
            choice: "Land".to_string(),
        })
        .expect("choose Land");
    let WaitingFor::RevealUntilBottomOrder { cards, .. } = &runner.state().waiting_for else {
        panic!(
            "Abundance asks for the revealed misses' order: {:?}",
            runner.state().waiting_for
        );
    };
    let order = cards.clone();
    runner
        .act(GameAction::Debug(DebugAction::MoveToZone {
            object_id: source,
            to_zone: Zone::Graveyard,
            library_position: None,
            simulate: false,
        }))
        .expect("the debug move removes the source");
    runner
        .act(GameAction::SelectCards { cards: order })
        .expect("submit the bottom order");

    let state = runner.state();
    assert!(
        state.pending_cast.is_none(),
        "the retained draw owner resumes without the source"
    );
    assert_eq!(
        state.players[P0.0 as usize].life,
        life_before - 1,
        "the retained PayLife suffix was paid"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "the activation reaches the stack after the source left: {:?}",
        state.waiting_for
    );
}

/// CR 113.8 + CR 118.12: a nonactive activator keeps its draw ownership and
/// payer through the replacement and the source's removal.
#[test]
fn a_nonactive_composite_draw_cost_activation_resumes_its_retained_suffix() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    scenario.with_library_top(P1, &STAGED_LIBRARY);
    let source = scenario
        .add_creature(P1, "Draw Cost Source", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::EffectCost {
                        effect: Box::new(draw_effect(1)),
                    },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }),
        )
        .id();
    scenario.add_enchantment_from_oracle(P1, "Abundance", ABUNDANCE_ORACLE);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let library: Vec<_> = state.players[P1.0 as usize]
            .library
            .iter()
            .copied()
            .collect();
        for (index, id) in library.iter().enumerate() {
            let object = state.objects.get_mut(id).expect("library card");
            object.card_types.core_types.clear();
            if index == 2 {
                object
                    .card_types
                    .core_types
                    .push(engine::types::card_type::CoreType::Land);
            }
        }
        state.debug_mode = true;
        state.format_config.allow_debug_actions = true;
    }
    evaluate_layers(runner.state_mut());
    let life_before = runner.state().players[P1.0 as usize].life;

    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the nonactive controller activates at instant speed");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept P1's Abundance");
    runner
        .act(GameAction::ChooseOption {
            choice: "Land".to_string(),
        })
        .expect("choose Land");
    let WaitingFor::RevealUntilBottomOrder { cards, .. } = &runner.state().waiting_for else {
        panic!(
            "Abundance asks for P1's revealed misses' order: {:?}",
            runner.state().waiting_for
        );
    };
    let order = cards.clone();
    runner
        .act(GameAction::Debug(DebugAction::RemoveObject {
            object_id: source,
        }))
        .expect("the debug removal takes the nonactive source and its definition away");
    runner
        .act(GameAction::SelectCards { cards: order })
        .expect("submit the bottom order");

    let state = runner.state();
    assert!(
        state.pending_cast.is_none(),
        "the nonactive activation resumes without its source"
    );
    assert_eq!(
        state.players[P1.0 as usize].life,
        life_before - 1,
        "the original controller paid the retained suffix"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "the nonactive activation reaches the stack: {:?}",
        state.waiting_for
    );
}

/// A three-player board at P0's main phase with `stack` on the stack, P0 at
/// `life`, and P0 and P1 auto-passing until the stack is empty.
fn auto_pass_board(life: i32, stack: Vec<StackEntry>) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.players[P0.0 as usize].life = life;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.stack = stack.into();
    for player in [P0, P1] {
        state.auto_pass.insert(
            player,
            AutoPassMode::UntilStackEmpty {
                initial_stack_len: state.stack.len(),
                policy: Default::default(),
            },
        );
    }
    runner
}

/// An activated ability of `controller` on the stack.
fn ability_on_stack(id: u64, controller: PlayerId, effect: Effect) -> StackEntry {
    StackEntry {
        id: ObjectId(id),
        source_id: ObjectId(id),
        controller,
        kind: StackEntryKind::ActivatedAbility {
            source_id: ObjectId(id),
            ability: Box::new(ResolvedAbility::new(
                effect,
                vec![],
                ObjectId(id),
                controller,
            )),
        },
    }
}

/// CR 104.3a: "A player who concedes leaves the game immediately." The
/// concession commits at its action boundary. Its follow-up auto-pass then
/// resolves an unrepresentable life gain; that follow-up is discarded whole and
/// the departure is published with its events. The next ordinary action starts
/// without any concession state: the same failing tail rejects it and restores
/// it exactly.
#[test]
fn a_concession_is_published_when_its_auto_pass_tail_overflows() {
    let gain = Effect::GainLife {
        amount: QuantityExpr::Fixed { value: 2 },
        player: TargetFilter::Controller,
    };
    let mut runner = auto_pass_board(
        i32::MAX - 1,
        vec![
            ability_on_stack(101, P0, gain),
            ability_on_stack(102, P0, Effect::NoOp),
        ],
    );
    let stack_before = runner.state().stack.clone();

    let published = apply(
        runner.state_mut(),
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("CR 104.3a: the concession stands");
    assert!(
        published.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerEliminated { player_id } if *player_id == PlayerId(2)
        )),
        "the departure is published with the action"
    );
    let state = runner.state();
    assert!(state.players[2].is_eliminated, "P2 left the game");
    assert_eq!(
        state.stack, stack_before,
        "the failed auto-pass tail is discarded whole, not kept half-run"
    );
    assert_eq!(state.players[P0.0 as usize].life, i32::MAX - 1);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));

    // The next ordinary action carries no concession: its own failing tail
    // rejects it and restores the state exactly.
    let committed = runner.state().clone();
    let error = apply(runner.state_mut(), P0, GameAction::PassPriority)
        .expect_err("an ordinary action with an unrepresentable tail is rejected");
    assert!(
        matches!(error, EngineError::LifeChangeUnrepresentable { player } if player == P0),
        "expected the unrepresentable life rejection, got {error:?}"
    );
    assert_eq!(runner.state().stack, committed.stack);
    assert_eq!(runner.state().players, committed.players);
    assert_eq!(runner.state().waiting_for, committed.waiting_for);
}

/// The concession board, P0 paying `Composite[draw, discard]`: leg 1's Dredge
/// is declined (the draw is the accepted prefix), the discard prompt is open,
/// and P2 has conceded, so the payment is parked on a recovery checkpoint.
/// Returns the runner and the card the prefix drew.
fn recovered_discard_checkpoint() -> (GameRunner, engine::types::identifiers::ObjectId) {
    let (mut runner, source) = concession_board_with_hand();
    let root = ResolvedAbility::new(
        pay_cost(AbilityCost::Composite {
            costs: vec![draw_cost(1), discard_cost()],
        }),
        vec![],
        source,
        P0,
    );
    let state = runner.state_mut();
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
    .expect("a player can concede at any time");
    let checkpoint = state
        .payment_transaction
        .as_ref()
        .and_then(|transaction| transaction.recovered.as_ref())
        .expect("the concession parks the accepted prefix on a recovery checkpoint");
    assert!(matches!(
        checkpoint.state.waiting_for,
        WaitingFor::DiscardChoice { .. }
    ));
    let drawn = checkpoint
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::CardDrawn {
                player_id,
                object_id,
                ..
            } if *player_id == P0 => Some(*object_id),
            _ => None,
        })
        .expect("the accepted prefix drew a card");
    (runner, drawn)
}

/// Save `state` and load it back through the checked production restore.
fn checked_round_trip(state: &GameState) -> Result<GameState, PersistedRestoreError> {
    let saved = serde_json::to_string(&PersistedGameState::Raw(Box::new(state.clone())))
        .expect("the state serializes");
    serde_json::from_str::<PersistedGameState>(&saved)
        .expect("the save decodes")
        .into_game_state()
}

/// CR 118.12 + CR 614.17a: a payment parked on a recovery checkpoint survives a
/// checked save and restore. The restored discard prompt is answered through
/// the authenticated entry with the card the accepted prefix drew, the payment
/// settles, and the game goes on.
#[test]
fn a_recovered_payment_reopens_through_checked_restore_and_settles() {
    let (runner, drawn) = recovered_discard_checkpoint();
    let mut state = checked_round_trip(runner.state()).expect("the recovered payment restores");
    apply(
        &mut state,
        P0,
        GameAction::SelectCards { cards: vec![drawn] },
    )
    .expect("the restored prompt is answered with the prefix-drawn card");
    assert!(state.payment_transaction.is_none(), "the payment settled");
    assert!(
        state.players[P0.0 as usize].graveyard.contains(&drawn),
        "the drawn card was discarded"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    let priority = match state.waiting_for {
        WaitingFor::Priority { player } => player,
        _ => unreachable!(),
    };
    apply(&mut state, priority, GameAction::PassPriority).expect("the game goes on");
}

/// CR 118.12 + CR 701.9a: a mandatory discard asks for more cards than the
/// player has in hand on the checkpoint. The admission authority accepts no
/// answer, so checked restore refuses the save; the same save uncorrupted
/// restores (reach guard).
#[test]
fn checked_restore_refuses_a_mandatory_discard_larger_than_the_hand() {
    let (runner, _) = recovered_discard_checkpoint();
    checked_round_trip(runner.state()).expect("reach guard: the valid checkpoint restores");
    let mut state = runner.state().clone();
    let checkpoint = state
        .payment_transaction
        .as_mut()
        .and_then(|transaction| transaction.recovered.as_mut())
        .expect("the checkpoint is live");
    let hand = checkpoint.state.players[P0.0 as usize].hand.len();
    let WaitingFor::DiscardChoice { count, .. } = &mut checkpoint.state.waiting_for else {
        unreachable!("the checkpoint holds the discard prompt");
    };
    *count = hand + 1;
    let corrupted = checkpoint.state.waiting_for.clone();
    state.waiting_for = corrupted;
    assert!(
        matches!(
            checked_round_trip(&state),
            Err(PersistedRestoreError::InvalidPaymentTransaction(_))
        ),
        "a discard prompt no answer can satisfy is refused"
    );
}

/// The concession board without the can't-draw pair: a 2-leg draw cost whose
/// leg 2 Dredge prompt is open on a recovery checkpoint after P2 concedes.
fn recovered_dredge_checkpoint() -> GameRunner {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..3 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    let mut runner = scenario.build();
    let root = ResolvedAbility::new(pay_cost(draw_legs(2)), vec![], source, P0);
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");
    assert!(is_dredge_prompt_for_p0(state), "leg 2 offers Dredge");
    apply(
        state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("a player can concede at any time");
    let checkpoint = state
        .payment_transaction
        .as_ref()
        .and_then(|transaction| transaction.recovered.as_ref())
        .expect("the concession parks the prefix on a recovery checkpoint");
    assert!(matches!(
        checkpoint.state.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner
}

/// CR 616.1 + CR 614.1: an optional replacement prompt whose pending
/// replacement lost its only candidate offers no executable accept or
/// decline. The admission authority accepts no answer, so checked restore
/// refuses the save; the uncorrupted save restores and its prompt is answered.
#[test]
fn checked_restore_refuses_an_optional_replacement_without_a_candidate() {
    let runner = recovered_dredge_checkpoint();
    let mut valid =
        checked_round_trip(runner.state()).expect("reach guard: the checkpoint restores");
    apply(&mut valid, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("reach guard: the restored Dredge prompt is answered");
    let mut state = runner.state().clone();
    let pending = state
        .payment_transaction
        .as_mut()
        .and_then(|transaction| transaction.recovered.as_mut())
        .and_then(|checkpoint| checkpoint.state.pending_replacement.as_mut())
        .expect("the checkpoint holds the Dredge choice");
    assert!(pending.is_optional, "Dredge is an optional replacement");
    pending.candidates.clear();
    assert!(
        matches!(
            checked_round_trip(&state),
            Err(PersistedRestoreError::InvalidPaymentTransaction(_))
        ),
        "a replacement prompt with no candidate is refused"
    );
}

/// CR 118.12: while a recovered payment is parked, the shared debug preflight
/// refuses every state-mutating sandbox action, leaving the payment and the
/// board untouched; a zero-count create stays the no-op projection and the
/// read-only permission check still answers.
#[test]
fn sandbox_mutations_are_refused_while_a_recovered_payment_is_parked() {
    let (mut runner, _) = recovered_discard_checkpoint();
    let state = runner.state_mut();
    state.debug_mode = true;
    state.format_config.allow_debug_actions = true;
    let before = state.clone();
    for action in [
        DebugAction::SetLife {
            player_id: P1,
            life: 5,
        },
        DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        },
        DebugAction::GainLife {
            player_id: P0,
            amount: 1,
        },
    ] {
        let error = apply(state, P0, GameAction::Debug(action.clone()))
            .expect_err("a sandbox mutation is refused while the payment is parked");
        assert!(
            matches!(
                &error,
                EngineError::InvalidAction(reason)
                    if reason == "sandbox actions are unavailable while a recovered payment is parked"
            ),
            "{action:?}: {error:?}"
        );
        assert_eq!(state.payment_transaction, before.payment_transaction);
        assert_eq!(state.players, before.players);
    }
    engine::game::engine::require_explicit_debug_permission(state, P0)
        .expect("read-only diagnostics keep their access check");
}
