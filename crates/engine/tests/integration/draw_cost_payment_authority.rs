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
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, ControllerRef,
    DiscardSelfScope, Effect, PlayerScope, QuantityExpr, QuantityRef, ReplacementDefinition,
    ResolvedAbility, TapCreaturesRequirement, TargetFilter, TypedFilter,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoPassMode, CastPaymentMode, GameState, PayCostKind, PendingCostMoveResume,
    PersistedGameState, PersistedRestoreError, StackEntry, StackEntryKind, UnpaidCostRemainder,
    UnpaidCostSuffix, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ReplacementId;
use engine::types::replacements::ReplacementEvent;
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
/// Synthetic: the staged costs paid on it have draw legs, which no printed resolution-time composite
/// cost has (the five printed ones are all `[Mana, PayLife]`); the nearest printed draw cost is
/// Psychic Vortex's "Cumulative upkeep—Draw a card." (see `concession_board_with_hand`).
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
/// `Composite` cost has a real choice after the draw leg. Synthetic: no
/// printed card has a resolution-time composite cost with a draw or a discard
/// leg (the five printed ones are all `[Mana, PayLife]`); the nearest printed
/// draw cost is Psychic Vortex's "Cumulative upkeep—Draw a card."
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
/// next ordinary action starts from it. The rider's unrepresentable gain is
/// recorded in the concession's own ingress window, so the payment is retired
/// whole: neither its staged draw nor a skipped gain is published.
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
        0,
        "the retired payment's staged draw is not committed"
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

/// P0's life gain equal to the cards in P0's hand: harmless on the canonical
/// board, where the staged payment's drawn card is not yet in hand, and
/// unrepresentable at `i32::MAX` life once the payment has drawn. Union of
/// the Third Path prints the same gain after a draw ("Draw a card, then you
/// gain life equal to the number of cards in your hand."); no printed card
/// makes it an unless-payment's rider. It probes the engine's numeric
/// capacity, which is where a recovery can fail.
fn gain_hand_size() -> Effect {
    Effect::GainLife {
        amount: QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
        },
        player: TargetFilter::Controller,
    }
}

/// Four players: P0 pays `PayCost{Composite[Draw 1; Draw 1]}` with `rider`, its
/// draws Dredge prompts (Stinkweed Imp); Maralen is under P1, muted by P2's
/// Humility; P3 has nothing. P0 is at `i32::MAX` life with `stack` on the
/// stack and P0 and P1 auto-passing. P0 declines leg 1's Dredge and P3
/// concedes, so leg 2's Dredge prompt is parked on a recovery checkpoint.
/// Synthetic, as `concession_board`: no printed staged cost has a draw leg.
fn second_concession_board(rider: Option<Effect>, stack: Vec<StackEntry>) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..4 {
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
    let mut root = ResolvedAbility::new(pay_cost(draw_legs(2)), vec![], source, P0);
    root.sub_ability =
        rider.map(|effect| Box::new(ResolvedAbility::new(effect, vec![], source, P0)));
    let state = runner.state_mut();
    state.players[P0.0 as usize].life = i32::MAX;
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
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");
    assert!(is_dredge_prompt_for_p0(state), "leg 2 offers Dredge");
    apply(
        state,
        PlayerId(3),
        GameAction::Concede {
            player_id: PlayerId(3),
        },
    )
    .expect("a player can concede at any time");
    assert!(
        state
            .payment_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.recovered.is_some()),
        "the first concession parks the prefix on a recovery checkpoint"
    );
    runner
}

/// The second departure, its published event, the retired checkpoint, a
/// checked save and restore, and the next ordinary action. The published
/// board is the departure's: none of the staged draws is in P0's hand and no
/// life changed, so a payment completed on the departed checkpoint (with its
/// failed life change skipped) was not kept.
fn assert_second_departure_retires_the_checkpoint(runner: &mut GameRunner) {
    let hand_before = runner.state().players[P0.0 as usize].hand.clone();
    let published = apply(
        runner.state_mut(),
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("CR 104.3a: the second concession stands");
    assert!(
        published.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerEliminated { player_id } if *player_id == PlayerId(2)
        )),
        "the second departure is published"
    );
    let state = runner.state();
    assert!(state.players[2].is_eliminated);
    assert!(
        state.payment_transaction.is_none(),
        "a recovery that cannot include the departure is retired, not kept stale"
    );
    assert_eq!(
        state.players[P0.0 as usize].hand, hand_before,
        "the retired payment's staged draws are not committed"
    );
    assert_eq!(
        state.players[P0.0 as usize].life,
        i32::MAX,
        "no life changed"
    );
    let mut restored =
        checked_round_trip(state).expect("the published state passes checked restore");
    let WaitingFor::Priority { player } = restored.waiting_for else {
        panic!("a living player has priority: {:?}", restored.waiting_for);
    };
    apply(&mut restored, player, GameAction::PassPriority)
        .expect("the next ordinary action starts from the departure");
}

/// CR 104.3a + CR 614.17c: P2's concession removes Humility, so leg 2's
/// parked draw can no longer happen and settles on the departed checkpoint.
/// That settlement completes the payment and its auto-pass resolves a life
/// gain equal to the drawn card, which is unrepresentable at `i32::MAX`: the
/// recovery fails. The failed-recovery boundary retires the checkpoint
/// instead of keeping one from before P2 left.
#[test]
fn a_second_concession_whose_parked_draw_fails_to_settle_retires_the_checkpoint() {
    let mut runner =
        second_concession_board(None, vec![ability_on_stack(201, P0, gain_hand_size())]);
    assert_second_departure_retires_the_checkpoint(&mut runner);
}

/// CR 104.3a: the same second concession, with the failure in the payment's
/// rider instead. The rider resolves as ordinary resolution on the departed
/// checkpoint, so its unrepresentable life gain is recorded in the action's
/// ingress window and the settlement itself returns `Ok`: this reaches the
/// Concede arm's life-failure branch, which publishes the departure and
/// retires the payment rather than the completed payment with the gain
/// skipped.
#[test]
fn a_second_concession_whose_rider_is_unrepresentable_retires_the_payment() {
    let mut runner = second_concession_board(Some(gain_hand_size()), Vec::new());
    assert_second_departure_retires_the_checkpoint(&mut runner);
}

/// CR 616.1 + CR 702.52a: a saved Dredge choice whose stored
/// candidate no longer applies is refused by the candidate check, like an
/// empty candidate list; the valid save restores.
#[test]
fn checked_restore_refuses_a_stale_replacement_candidate() {
    let runner = recovered_dredge_checkpoint();
    checked_round_trip(runner.state()).expect("reach guard: the checkpoint restores");
    let mut state = runner.state().clone();
    let pending = state
        .payment_transaction
        .as_mut()
        .and_then(|transaction| transaction.recovered.as_mut())
        .and_then(|checkpoint| checkpoint.state.pending_replacement.as_mut())
        .expect("the checkpoint holds the Dredge choice");
    let stale = pending
        .candidates
        .first_mut()
        .expect("Dredge is the candidate");
    stale.source = ObjectId(9_999);
    let refusal = checked_round_trip(&state);
    assert!(
        matches!(
            &refusal,
            Err(PersistedRestoreError::InvalidPaymentTransaction(reason))
                if reason.contains("replacement choice is not the one the game offers")
        ),
        "a candidate whose definition is gone is not admitted: {refusal:?}"
    );
}

/// A recovery checkpoint's state is read through the persistence ingress, as
/// the saved game that carries it is. NOT a state the game produces: the valid
/// Dredge checkpoint with its nested state marked as one player's view. A
/// projection holds only what that player was shown, so the save is refused;
/// the untouched save restores (`checked_restore_refuses_a_stale_replacement_candidate`
/// and its neighbours restore the same fixture).
#[test]
fn checked_restore_refuses_a_checkpoint_that_is_a_viewer_projection() {
    let runner = recovered_dredge_checkpoint();
    checked_round_trip(runner.state()).expect("reach guard: the checkpoint restores");
    let mut state = runner.state().clone();
    state
        .payment_transaction
        .as_mut()
        .and_then(|transaction| transaction.recovered.as_mut())
        .expect("the departure parked a recovery checkpoint")
        .state
        .viewer_projection = Some(P0);
    let saved = serde_json::to_string(&PersistedGameState::Raw(Box::new(state)))
        .expect("the forged state serializes");
    let refused = serde_json::from_str::<PersistedGameState>(&saved).map(|_| "decoded");
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("recovery checkpoint")),
        "a projection-marked checkpoint is refused where it is decoded: {refused:?}"
    );
}

/// CR 615.5 + CR 614.6: a recovery checkpoint has arenas of its own, so a
/// handle inside it names the checkpoint's arena, not the enclosing game's.
/// NOT a state the game produces: a shield installed only in the valid Dredge
/// checkpoint (already used, so it is not a candidate), whose continuation is
/// entry 0 of the checkpoint's arena while the enclosing game's arena is
/// empty. The save restores; the same handle naming no entry of the
/// checkpoint's arena is refused where the checkpoint is decoded.
#[test]
fn checked_restore_reads_a_checkpoints_handles_against_its_own_arenas() {
    let runner = recovered_dredge_checkpoint();
    let forge = |handle_offset: u64| {
        let mut state = runner.state().clone();
        assert!(
            state.replacement_continuations.is_empty(),
            "reach guard: the enclosing game's arena is empty"
        );
        let source = state
            .payment_transaction
            .as_ref()
            .expect("reach guard: the payment is staged")
            .root
            .source_id;
        let nested = &mut state
            .payment_transaction
            .as_mut()
            .and_then(|transaction| transaction.recovered.as_mut())
            .expect("the departure parked a recovery checkpoint")
            .state;
        let installed = nested.install_replacement_continuation(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        ));
        let mut used = ReplacementDefinition::new(ReplacementEvent::Draw);
        used.is_consumed = true;
        used.runtime_execute = Some(engine::types::identifiers::ReplacementContinuationId(
            installed.0 + handle_offset,
        ));
        nested.pending_damage_replacements.push(used);
        state
    };
    checked_round_trip(&forge(0))
        .expect("a handle into the checkpoint's own arena is not read against the outer arena");
    let saved = serde_json::to_string(&PersistedGameState::Raw(Box::new(forge(7))))
        .expect("the forged state serializes");
    let refused = serde_json::from_str::<PersistedGameState>(&saved).map(|_| "decoded");
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("recovery checkpoint")),
        "a handle that names no entry of the checkpoint's arena is refused: {refused:?}"
    );
}

/// CR 702.24a + CR 118.12: a recovery checkpoint is a second executable state,
/// so a parked unless payment inside it is held to the outer state's rule. NOT
/// a state the game produces: a staged payment's checkpoint never parks an
/// unless payment, so one is forged into the valid Dredge checkpoint, owing a
/// repeated draw to a payer who is not a seat. The save is refused.
#[test]
fn checked_restore_refuses_a_checkpoint_whose_parked_payment_names_no_seat() {
    let runner = recovered_dredge_checkpoint();
    checked_round_trip(runner.state()).expect("reach guard: the checkpoint restores");
    let mut state = runner.state().clone();
    let source = state
        .payment_transaction
        .as_ref()
        .expect("reach guard: the payment is staged")
        .root
        .source_id;
    let checkpoint = state
        .payment_transaction
        .as_mut()
        .and_then(|transaction| transaction.recovered.as_mut())
        .expect("the departure parked a recovery checkpoint");
    checkpoint.state.pending_cost_move_resume =
        Some(PendingCostMoveResume::CounterAdditionUnlessPayment {
            cost: draw_cost(1),
            pending_effect: Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
                vec![],
                source,
                P0,
            )),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
            unpaid_suffix: Some(Box::new(UnpaidCostSuffix {
                payer: PlayerId(99),
                remainder: UnpaidCostRemainder::Repeated(
                    std::num::NonZeroU32::new(2).expect("two is not zero"),
                ),
            })),
        });
    let refusal = checked_round_trip(&state);
    assert!(
        matches!(
            &refusal,
            Err(PersistedRestoreError::InvalidPaymentTransaction(reason))
                if reason.contains("recovery checkpoint: parked unless payment")
        ),
        "a checkpoint owing a remainder to no seat is not admitted: {refusal:?}"
    );
}

/// Notion Thief, verbatim Oracle text.
const NOTION_THIEF_ORACLE: &str = "Flash\nIf an opponent would draw a card except the first one \
they draw in each of their draw steps, instead that player skips that draw and you draw a card.";

/// Hullbreacher, verbatim Oracle text.
const HULLBREACHER_ORACLE: &str = "Flash\nIf an opponent would draw a card except the first one \
they draw in each of their draw steps, instead you create a Treasure token. (It's an artifact \
with \"{T}, Sacrifice this token: Add one mana of any color.\")";

/// CR 616.1 + CR 800.4a + CR 104.3a: the staged form of a departure that makes
/// a replacement applicable. P0's draw leg parks on Dredge alone while P3's
/// Humility mutes P1's Hullbreacher. P3 concedes, so Hullbreacher applies
/// again. The departure re-settles the parked prompt on the checkpoint, which
/// stores the order among both effects; the save passes checked restore, and
/// choosing Hullbreacher for each leg pays the cost with two Treasures and no
/// card drawn. NOT a printed cost: `PayCost{Composite[Draw 1; Draw 1]}` is the
/// staged shape nearest the five printed `[Mana, PayLife]` composites.
#[test]
fn a_departure_that_adds_a_parked_candidate_keeps_a_restorable_checkpoint() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..4 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    let hullbreacher = scenario
        .add_creature_from_oracle(P1, "Hullbreacher", 3, 2, HULLBREACHER_ORACLE)
        .id();
    scenario.add_enchantment_from_oracle(PlayerId(3), "Humility", HUMILITY_ORACLE);
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    let root = ResolvedAbility::new(pay_cost(draw_legs(2)), vec![], source, P0);
    let state = runner.state_mut();
    let hand_before = state.players[P0.0 as usize].hand.len();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    assert!(
        is_dredge_prompt_for_p0(state)
            && matches!(
                &state.waiting_for,
                WaitingFor::ReplacementChoice {
                    kind: engine::types::game_state::ReplacementChoiceKind::OptionalBranch,
                    ..
                }
            ),
        "reach guard: only Dredge applies while Humility mutes Hullbreacher: {:?}",
        state.waiting_for
    );

    apply(
        state,
        PlayerId(3),
        GameAction::Concede {
            player_id: PlayerId(3),
        },
    )
    .expect("a player can concede at any time");
    let pending = state
        .payment_transaction
        .as_ref()
        .and_then(|transaction| transaction.recovered.as_ref())
        .expect("the departure keeps the accepted prefix on a checkpoint")
        .state
        .pending_replacement
        .as_ref()
        .expect("the checkpoint parks a replacement choice");
    assert!(
        !pending.is_optional && pending.candidates.len() == 2,
        "the checkpoint stores the order among Dredge and Hullbreacher: {:?}",
        pending.candidates
    );

    let mut restored =
        checked_round_trip(runner.state()).expect("the checkpoint passes checked restore");
    for leg in 1..=2 {
        let WaitingFor::ReplacementChoice { candidates, .. } = &restored.waiting_for else {
            panic!(
                "leg {leg} asks for a replacement choice: {:?}",
                restored.waiting_for
            );
        };
        let index = candidates
            .iter()
            .position(|candidate| candidate.source_id == hullbreacher)
            .unwrap_or_else(|| panic!("leg {leg} offers Hullbreacher: {candidates:?}"));
        apply(&mut restored, P0, GameAction::ChooseReplacement { index })
            .unwrap_or_else(|error| panic!("choosing Hullbreacher on leg {leg}: {error:?}"));
    }
    assert!(
        restored.payment_transaction.is_none(),
        "the payment settled"
    );
    let treasures = restored
        .battlefield
        .iter()
        .filter(|id| restored.objects[*id].name == "Treasure")
        .count();
    assert_eq!(treasures, 2, "each leg became a Treasure");
    assert_eq!(
        restored.players[P0.0 as usize].hand.len(),
        hand_before,
        "no card is drawn"
    );
}

/// CR 616.1 + CR 800.4a + CR 104.3a: P0's draw leg parks a choice between
/// P0's Dredge and P3's Notion Thief. P3 concedes, so Notion Thief leaves the
/// game. The departure re-settles the parked prompt on the checkpoint: only
/// Dredge would now apply, so the checkpoint stores that prompt, the save
/// passes checked restore, and the restored payment is answered to the end.
#[test]
fn a_departure_that_removes_a_parked_candidate_keeps_a_restorable_checkpoint() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..4 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    scenario.add_creature_from_oracle(PlayerId(3), "Notion Thief", 3, 1, NOTION_THIEF_ORACLE);
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    let root = ResolvedAbility::new(pay_cost(draw_legs(2)), vec![], source, P0);
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "leg 1 asks for a replacement choice: {:?}",
            state.waiting_for
        );
    };
    assert_eq!(
        (*player, *candidate_count),
        (P0, 2),
        "reach guard: Dredge and Notion Thief both apply to leg 1"
    );

    apply(
        state,
        PlayerId(3),
        GameAction::Concede {
            player_id: PlayerId(3),
        },
    )
    .expect("a player can concede at any time");
    let checkpoint = state
        .payment_transaction
        .as_ref()
        .and_then(|transaction| transaction.recovered.as_ref())
        .expect("the departure keeps the accepted prefix on a checkpoint");
    let pending = checkpoint
        .state
        .pending_replacement
        .as_ref()
        .expect("the checkpoint parks a replacement choice");
    assert!(
        pending.is_optional,
        "only Dredge, an optional replacement, still applies"
    );
    assert_eq!(pending.candidates.len(), 1);

    let mut restored =
        checked_round_trip(runner.state()).expect("the checkpoint passes checked restore");
    for leg in 1..=2 {
        assert!(
            is_dredge_prompt_for_p0(&restored),
            "leg {leg} offers Dredge"
        );
        apply(
            &mut restored,
            P0,
            GameAction::ChooseReplacement { index: 1 },
        )
        .unwrap_or_else(|error| panic!("declining Dredge on leg {leg}: {error:?}"));
    }
    assert!(
        restored.payment_transaction.is_none(),
        "the payment settled"
    );
    assert!(matches!(restored.waiting_for, WaitingFor::Priority { .. }));
}

/// `recovered_dredge_checkpoint` with `second` as the cost's second leg: P2
/// concedes during leg 1's Dredge prompt, which parks the payment on a
/// recovery checkpoint; P0 then declines Dredge, and leg 2 opens its own
/// prompt on that checkpoint. P0 has a second creature and a second
/// graveyard card, so tapping a creature or a graveyard exile asks which.
/// Synthetic, as `concession_board`: no printed staged cost has a draw, tap
/// or exile leg.
fn recovered_second_leg_checkpoint(second: AbilityCost) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..3 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    scenario.add_creature(P0, "Second Creature", 1, 1);
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    scenario.add_creature_to_graveyard(P0, "Graveyard Creature", 1, 1);
    let mut runner = scenario.build();
    let root = ResolvedAbility::new(
        pay_cost(AbilityCost::Composite {
            costs: vec![draw_cost(1), second],
        }),
        vec![],
        source,
        P0,
    );
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    apply(
        state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("a player can concede at any time");
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");
    assert!(
        state
            .payment_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.recovered.is_some()),
        "leg 2's prompt is parked on the recovery checkpoint"
    );
    runner
}

/// The live save with every object its checkpoint's prompt lists moved to
/// exile inside the checkpoint, as an edited save holding objects that left
/// would have them (CR 400.7: each is a new object).
fn save_with_listed_objects_exiled(
    runner: &GameRunner,
    listed: fn(&WaitingFor) -> Vec<ObjectId>,
) -> GameState {
    let mut state = runner.state().clone();
    let checkpoint = state
        .payment_transaction
        .as_mut()
        .and_then(|transaction| transaction.recovered.as_mut())
        .expect("the save holds a recovery checkpoint");
    let ids = listed(&checkpoint.state.waiting_for);
    assert!(
        ids.len() >= 2,
        "reach guard: the prompt lists a choice: {ids:?}"
    );
    for id in ids {
        engine::game::zones::move_to_zone(&mut checkpoint.state, id, Zone::Exile, &mut Vec::new());
    }
    state
}

fn refused_with(refusal: Result<GameState, PersistedRestoreError>, fragment: &str) -> bool {
    matches!(
        refusal,
        Err(PersistedRestoreError::InvalidPaymentTransaction(reason)) if reason.contains(fragment)
    )
}

/// CR 701.26a + CR 118.12: a saved prompt to tap creatures as a resolution
/// cost (the shape of Meanders Guide's "you may tap another untapped Merfolk
/// you control") names creatures on the battlefield. With every listed
/// creature exiled in the save, the prompt names objects that left their zone
/// (CR 400.7) and checked restore refuses it before probing an answer, so a
/// decline the prompt also admits cannot carry the save; the unedited save
/// restores.
#[test]
fn checked_restore_refuses_a_tap_creatures_prompt_whose_creatures_left() {
    let runner = recovered_second_leg_checkpoint(AbilityCost::TapCreatures {
        requirement: TapCreaturesRequirement::Count { count: 1 },
        filter: TypedFilter::creature()
            .controller(ControllerRef::You)
            .into(),
    });
    checked_round_trip(runner.state()).expect("reach guard: the tap prompt restores");
    let edited = save_with_listed_objects_exiled(&runner, |waiting| match waiting {
        WaitingFor::PayCost {
            kind: PayCostKind::TapCreatures { .. },
            choices,
            ..
        } => choices.clone(),
        other => panic!("reach guard: leg 2 asks which creature to tap: {other:?}"),
    });
    let refusal = checked_round_trip(&edited);
    assert!(
        refused_with(refusal.clone(), "an object that left its zone"),
        "{refusal:?}"
    );
}

/// CR 701.13a + CR 118.12: a saved graveyard-exile prompt names cards in the
/// graveyard. With every listed card exiled in the save, the prompt names
/// objects that left their zone (CR 400.7) and checked restore refuses it; the
/// unedited save restores.
#[test]
fn checked_restore_refuses_a_zone_choice_whose_cards_left() {
    let runner = recovered_second_leg_checkpoint(AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Graveyard),
        filter: None,
    });
    checked_round_trip(runner.state()).expect("reach guard: the zone choice restores");
    let edited = save_with_listed_objects_exiled(&runner, |waiting| match waiting {
        WaitingFor::EffectZoneChoice { cards, .. } => cards.clone(),
        other => panic!("reach guard: leg 2 asks which card to exile: {other:?}"),
    });
    let refusal = checked_round_trip(&edited);
    assert!(
        refused_with(refusal.clone(), "an object that left its zone"),
        "{refusal:?}"
    );
}

/// CR 616.1 + CR 614.1: a saved replacement prompt whose candidate is a
/// floating one-shot draw shield already used up names an effect discovery no
/// longer offers, so checked restore refuses it like any stale candidate.
/// Synthetic: the floating shield stands in for Words of Worship's "the next
/// time you would draw a card this turn" shield after it applied.
#[test]
fn checked_restore_refuses_a_used_floating_shield_candidate() {
    let runner = recovered_dredge_checkpoint();
    let mut state = runner.state().clone();
    let checkpoint = state
        .payment_transaction
        .as_mut()
        .and_then(|transaction| transaction.recovered.as_mut())
        .expect("the save holds a recovery checkpoint");
    let floating = &mut checkpoint.state.pending_damage_replacements;
    let mut used = ReplacementDefinition::new(ReplacementEvent::Draw);
    used.is_consumed = true;
    let candidate = ReplacementId {
        source: ObjectId(0),
        index: floating.len(),
    };
    floating.push(used);
    checkpoint
        .state
        .pending_replacement
        .as_mut()
        .expect("the checkpoint holds the Dredge choice")
        .candidates = vec![candidate];
    let refusal = checked_round_trip(&state);
    assert!(
        refused_with(
            refusal.clone(),
            "replacement choice is not the one the game offers"
        ),
        "{refusal:?}"
    );
}

/// Words of Worship, verbatim Oracle text.
const WORDS_OF_WORSHIP_ORACLE: &str =
    "{1}: The next time you would draw a card this turn, you gain 5 life instead.";

/// P0 activates Words of Worship and, at one less than the largest life the
/// engine represents, holds a staged `PayCost{Composite[Draw 1; Discard 1]}`
/// whose draw leg the shield replaces with a gain of 5.
fn words_of_worship_board() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    let words = scenario
        .add_enchantment_from_oracle(P0, "Words of Worship", WORDS_OF_WORSHIP_ORACLE)
        .id();
    scenario.add_card_to_hand(P0, "Hand A");
    scenario.add_card_to_hand(P0, "Hand B");
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
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
        .expect("Words of Worship's ability is activated");
    runner.advance_until_stack_empty();
    runner.state_mut().players[P0.0 as usize].life = i32::MAX - 1;
    (runner, source)
}

fn draw_then_discard() -> Effect {
    pay_cost(AbilityCost::Composite {
        costs: vec![draw_cost(1), discard_cost()],
    })
}

/// CR 614.1 + CR 118.12: the staged payment begins when its ability resolves
/// through priority passes. Its draw leg becomes a gain no engine life total
/// can hold, and the discard leg pauses: the pause does not drop that failure,
/// so the resolving pass is rejected and the board restored.
#[test]
fn an_unrepresentable_gain_before_a_payment_pauses_rejects_the_resolving_pass() {
    let (mut runner, _) = words_of_worship_board();
    let state = runner.state_mut();
    state.stack = vec![ability_on_stack(301, P0, draw_then_discard())].into();
    apply(state, P0, GameAction::PassPriority).expect("P0 passes");
    let before = state.clone();
    let error = apply(state, P1, GameAction::PassPriority)
        .expect_err("the resolving pass reaches the unrepresentable gain");
    assert!(
        matches!(error, EngineError::LifeChangeUnrepresentable { player } if player == P0),
        "{error:?}"
    );
    assert_eq!(state.players[P0.0 as usize].life, i32::MAX - 1);
    assert_eq!(
        state.stack, before.stack,
        "the ability is still on the stack"
    );
    assert!(state.payment_transaction.is_none(), "no payment is staged");
}

/// CR 614.1 + CR 118.12: the same payment, begun outside an action boundary as
/// a resolution test harness does, pauses on the discard leg. Answering it
/// replays the accepted prefix, whose gain cannot be represented, and runs the
/// answer in a boundary of its own: the failure the prefix recorded survives
/// that boundary, so the answer is rejected and the board restored instead of
/// completing the payment with the gain dropped.
#[test]
fn an_unrepresentable_gain_in_the_replayed_prefix_rejects_the_completing_answer() {
    let (mut runner, source) = words_of_worship_board();
    let root = ResolvedAbility::new(draw_then_discard(), vec![], source, P0);
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    let WaitingFor::DiscardChoice { cards, .. } = &state.waiting_for else {
        panic!("reach guard: the discard leg asks: {:?}", state.waiting_for);
    };
    let card = cards[0];
    let before = state.clone();
    let error = apply(state, P0, GameAction::SelectCards { cards: vec![card] })
        .expect_err("the replayed prefix's gain cannot be represented");
    assert!(
        matches!(error, EngineError::LifeChangeUnrepresentable { player } if player == P0),
        "{error:?}"
    );
    assert_eq!(state.players[P0.0 as usize].life, i32::MAX - 1);
    assert_eq!(
        state.players[P0.0 as usize].hand,
        before.players[P0.0 as usize].hand
    );
    assert_eq!(
        state.waiting_for, before.waiting_for,
        "the prompt is restored"
    );
    assert_eq!(state.payment_transaction, before.payment_transaction);
}

/// P0's staged `PayCost{Composite[Draw 1; tap a creature you control]}`
/// declines leg 1's Dredge and parks leg 2's prompt, which lists a creature P2
/// owns but P0 controls. Synthetic: no printed card has a resolution-time
/// composite cost with a draw or a tap-creatures leg (the five printed ones
/// are all `[Mana, PayLife]`); the nearest printed control effect is Control
/// Magic ("You control enchanted creature."). With `own_creature`, P0 also
/// controls a creature of its own. With `checkpoint`, P3 concedes during the
/// Dredge prompt first, so leg 2's prompt is parked on a recovery checkpoint.
/// Returns the runner, the lent creature, and P0's creature if any.
fn lent_creature_tap_prompt(
    own_creature: bool,
    checkpoint: bool,
) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..4 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario
        .add_artifact_from_oracle(P0, "Draw Cost Source", "")
        .id();
    let own = own_creature.then(|| scenario.add_creature(P0, "Own Creature", 1, 1).id());
    let lent = scenario
        .add_creature(PlayerId(2), "Lent Creature", 1, 1)
        .id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&lent)
        .expect("the lent creature exists")
        .base_controller = Some(P0);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&lent].controller,
        P0,
        "reach guard: P0 controls the lent creature"
    );
    let root = ResolvedAbility::new(
        pay_cost(AbilityCost::Composite {
            costs: vec![
                draw_cost(1),
                AbilityCost::TapCreatures {
                    requirement: TapCreaturesRequirement::Count { count: 1 },
                    filter: TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .into(),
                },
            ],
        }),
        vec![],
        source,
        P0,
    );
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    if checkpoint {
        apply(
            state,
            PlayerId(3),
            GameAction::Concede {
                player_id: PlayerId(3),
            },
        )
        .expect("a player can concede at any time");
    }
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");
    let WaitingFor::PayCost { choices, .. } = &state.waiting_for else {
        panic!(
            "reach guard: leg 2 asks which creature to tap: {:?}",
            state.waiting_for
        );
    };
    assert!(
        choices.contains(&lent),
        "reach guard: the lent creature is listed"
    );
    assert_eq!(
        state
            .payment_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.recovered.is_some()),
        checkpoint,
        "reach guard: the prompt is parked on a checkpoint only after a concession"
    );
    (runner, lent, own)
}

fn concede_lender(runner: &mut GameRunner) {
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
        "the departure is published"
    );
}

/// The lender has left: the payment holds no checkpoint, its prompt was derived
/// afresh by the cost authority and lists only P0's own creature, the save
/// restores, and tapping that creature completes the payment with the card
/// drawn through the declined Dredge.
fn assert_tap_prompt_was_rederived(runner: &GameRunner, lent: ObjectId, own: ObjectId) {
    let state = runner.state();
    let hand_before = state.players[P0.0 as usize].hand.len();
    assert!(
        state
            .payment_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.recovered.is_none()),
        "the payment is kept without a checkpoint"
    );
    let WaitingFor::PayCost { choices, .. } = &state.waiting_for else {
        panic!("leg 2 still asks: {:?}", state.waiting_for);
    };
    assert_eq!(choices, &vec![own], "only P0's own creature is listed");
    assert_ne!(state.objects[&lent].zone, Zone::Battlefield);
    let mut restored = checked_round_trip(state).expect("the re-derived prompt restores");
    apply(
        &mut restored,
        P0,
        GameAction::SelectCards { cards: vec![own] },
    )
    .expect("tapping P0's own creature pays leg 2");
    assert!(
        restored.payment_transaction.is_none(),
        "the payment completed"
    );
    assert!(restored.objects[&own].tapped, "the creature paid the cost");
    assert_eq!(
        restored.players[P0.0 as usize].hand.len(),
        hand_before + 1,
        "the card drawn through the declined Dredge is in hand"
    );
}

/// CR 800.4a + CR 400.7: a tap-creatures selection is outside the checkpoint
/// contract, because what it may list depends on control another player's
/// effect grants. When the lender concedes, the guard keeps no checkpoint: the
/// payment re-materializes on the departed board, where the cost authority
/// lists only P0's own creature, and the payment completes.
#[test]
fn a_concession_that_removes_a_listed_creature_rederives_the_tap_prompt() {
    let (mut runner, lent, own) = lent_creature_tap_prompt(true, false);
    concede_lender(&mut runner);
    assert_tap_prompt_was_rederived(&runner, lent, own.expect("P0 controls its own creature"));
}

/// The same departure when the prompt is parked on a recovery checkpoint from
/// an earlier concession: the guard drops the checkpoint and the payment
/// re-materializes on the departed board in the same way.
#[test]
fn a_concession_that_removes_a_listed_creature_drops_the_checkpoint() {
    let (mut runner, lent, own) = lent_creature_tap_prompt(true, true);
    concede_lender(&mut runner);
    assert_tap_prompt_was_rederived(&runner, lent, own.expect("P0 controls its own creature"));
}

/// CR 800.4a + CR 118.3: the same departure when the lent creature was the only
/// one listed. Re-materialized on the departed board, the cost can no longer be
/// paid in full, and it fails whole, as a replay of the transcript on that
/// board finds it: nothing of the payment is committed, no prompt remains, and
/// the save restores.
#[test]
fn a_concession_that_removes_the_only_listed_creature_fails_the_payment_whole() {
    let (mut runner, _, _) = lent_creature_tap_prompt(false, false);
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    concede_lender(&mut runner);
    let state = runner.state();
    assert!(
        state.payment_transaction.is_none(),
        "the payment is settled"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::PayCost { .. }),
        "no prompt remains: {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.players[P0.0 as usize].hand.len(),
        hand_before,
        "a failed staged cost commits none of its legs"
    );
    checked_round_trip(state).expect("the settled state restores");
}

/// CR 800.4a + CR 400.7: a zone choice among cards in graveyards is outside the
/// checkpoint contract too: it may list another player's card. Synthetic: no
/// printed card has a resolution-time composite cost with an exile leg; the
/// nearest printed cost is The Mimeoplasm's "exile two creature cards from
/// graveyards". P0's staged `PayCost{Composite[Draw 1; exile a card from a
/// graveyard]}` lists P3's graveyard card. When P3 concedes, the guard keeps no
/// checkpoint: the payment re-materializes on the departed board, where the
/// cost authority no longer lists P3's card, the save restores, and exiling
/// P0's own card completes the payment.
#[test]
fn a_concession_that_removes_a_listed_graveyard_card_rederives_the_zone_choice() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..4 {
        scenario.with_library_top(PlayerId(player), &STAGED_LIBRARY);
    }
    let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    let theirs = scenario
        .add_creature_to_graveyard(PlayerId(3), "Their Graveyard Creature", 1, 1)
        .id();
    let mut runner = scenario.build();
    let root = ResolvedAbility::new(
        pay_cost(AbilityCost::Composite {
            costs: vec![
                draw_cost(1),
                AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Graveyard),
                    filter: None,
                },
            ],
        }),
        vec![],
        source,
        P0,
    );
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");
    apply(state, P0, GameAction::ChooseReplacement { index: 1 })
        .expect("declining leg 1's Dredge accepts the draw");
    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "reach guard: leg 2 asks which card to exile: {:?}",
            state.waiting_for
        );
    };
    assert!(
        cards.contains(&theirs),
        "reach guard: P3's graveyard card is listed: {cards:?}"
    );
    let hand_before = state.players[P0.0 as usize].hand.len();

    apply(
        state,
        PlayerId(3),
        GameAction::Concede {
            player_id: PlayerId(3),
        },
    )
    .expect("a player can concede at any time");
    assert!(
        state
            .payment_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.recovered.is_none()),
        "the payment is kept without a checkpoint"
    );
    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!("leg 2 still asks: {:?}", state.waiting_for);
    };
    assert!(
        !cards.is_empty() && !cards.contains(&theirs),
        "P3's card is no longer listed: {cards:?}"
    );
    let mine = cards[0];
    let mut restored = checked_round_trip(state).expect("the re-derived prompt restores");
    apply(
        &mut restored,
        P0,
        GameAction::SelectCards { cards: vec![mine] },
    )
    .expect("exiling P0's own card pays leg 2");
    assert!(
        restored.payment_transaction.is_none(),
        "the payment completed"
    );
    assert_eq!(restored.objects[&mine].zone, Zone::Exile);
    assert_eq!(
        restored.players[P0.0 as usize].hand.len(),
        hand_before + 1,
        "the card drawn through the declined Dredge is in hand"
    );
}
