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
use engine::game::staged_payment_shadow_for_test;
use engine::types::ability::{AbilityCost, Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::ResolutionFrame;

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

fn is_dredge_prompt_for_p0(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P0)
}

/// The head of the topmost ability continuation on the staged shadow, if any.
fn shadow_continuation_head(state: &GameState) -> Option<Effect> {
    staged_payment_shadow_for_test(state)
        .resolution_stack
        .iter()
        .filter_map(|frame| match frame {
            ResolutionFrame::AbilityContinuation(frame) => Some(frame.pending.chain.effect.clone()),
            _ => None,
        })
        .last()
}

/// P0 resolves `PayCost{Composite[Draw 1; legs]}` with a GainLife 3 rider on the concession board; when
/// `concede`, P2 concedes at the first Dredge prompt. Every Dredge prompt is declined. Returns the hand
/// delta, the life delta and the failure flag, after asserting the payment settled with no transaction.
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
    let (hand_before, _) = hand_and_library(&runner, P0);
    let state = runner.state_mut();
    resolve_ability_chain(state, &root, &mut Vec::new(), 0).expect("the staged payment resolves");

    // Reach guard: Humility mutes Maralen, so the fresh gate admitted the choice and staged it.
    assert!(state.payment_transaction.is_some(), "the payment is staged");
    assert!(
        is_dredge_prompt_for_p0(state),
        "leg 1 raised its Dredge prompt"
    );

    if concede {
        // CR 104.3a + CR 800.4a: P2 leaves the game and Humility with them, so Maralen's
        // "Players can't draw cards" applies again on canonical state.
        apply(
            state,
            PlayerId(2),
            GameAction::Concede {
                player_id: PlayerId(2),
            },
        )
        .expect("a player can concede at any time");
        assert!(
            state.payment_transaction.is_some(),
            "the transaction survives the concession"
        );
        assert!(is_dredge_prompt_for_p0(&staged_payment_shadow_for_test(
            state
        )));
        if legs == 3 {
            // Reach guard: the parked remainder exists after the first pause.
            assert_eq!(
                shadow_continuation_head(state),
                Some(pay_cost(draw_legs(2)))
            );
        }
    }

    for leg in 1..=legs {
        assert!(
            is_dredge_prompt_for_p0(state),
            "leg {leg} raised its Dredge prompt"
        );
        apply(state, P0, GameAction::ChooseReplacement { index: 1 })
            .unwrap_or_else(|error| panic!("declining Dredge on leg {leg}: {error:?}"));
        if concede && legs == 3 {
            match leg {
                // The latched remainder re-prepends its own latched remainder.
                1 => assert_eq!(
                    shadow_continuation_head(state),
                    Some(pay_cost(draw_cost(1)))
                ),
                2 => assert_eq!(shadow_continuation_head(state), None),
                _ => {}
            }
        }
    }

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

/// CR 118.12 + CR 614.17a + CR 118.11: a staged draw cost P0 already chose stays paid when another player's
/// concession (CR 104.3a + CR 800.4a) removes Humility mid-payment and Maralen's can't-draw effect returns.
/// Every replay re-materializes the choice latched: the remaining draws are clamped, never unpaid, and the
/// rider runs once.
///
/// Revert probes: (A) `replay` not installing `LatchedSuffix` re-gates the root fresh, so the replay after
/// the concession refuses the payment and the post-concession reach guard
/// `is_dredge_prompt_for_p0(&staged_payment_shadow_for_test(state))` fails; (B) the prepend helpers queueing
/// `FreshChoice` re-gate the remainder head, which is refused before the Draw arm, so the per-leg
/// "leg 2 raised its Dredge prompt" assertion fails.
#[test]
fn another_players_concession_does_not_unpay_a_staged_draw_cost() {
    // Control: without the concession both legs draw.
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

/// CR 118.12 + CR 702.24a: the three-leg form of the concession row, where the latched remainder pauses
/// again and re-prepends its own latched remainder, driven through the production route.
#[test]
fn another_players_concession_does_not_unpay_a_three_leg_draw_cost() {
    assert_eq!(
        pay_staged_draw_cost_around_a_concession(3, true),
        (0, 3, false),
        "every nested remainder stays latched"
    );
}
