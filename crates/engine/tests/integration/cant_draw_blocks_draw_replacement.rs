//! CR 614.17c: "If an event can't happen, it can only be replaced by a
//! self-replacement effect (see rule 614.15). Other replacement and/or
//! prevention effects can't modify or replace it." A draw that no effect lets
//! its player make is such an event, so Stinkweed Imp's Dredge (CR 702.52a, a
//! static ability of a graveyard card, never a self-replacement) is not offered
//! for it: no prompt, no mill, no return.
//!
//! Maralen of the Mornsong's rulings: "replacement effects that instruct a
//! player to do something instead of drawing a card won't work." Spirit of the
//! Labyrinth's: "If a replacement effect would try to replace a card that you
//! can't draw, that effect can't apply."
//!
//! CR 614.17b + CR 121.2b: a fresh activation whose cost includes more draws
//! than a per-turn limit allows is refused as a whole before its first draw leg
//! can raise a replacement prompt (no printed card carries a multi-draw
//! activation cost — the typed-input contract).

use engine::game::effects::resolve_ability_chain;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, ResolvedAbility,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

/// Maralen of the Mornsong, verbatim Oracle text: "Players can't draw cards."
const MARALEN_ORACLE: &str = "Players can't draw cards.\nAt the beginning of each player's draw \
step, that player loses 3 life, searches their library for a card, puts it into their hand, then shuffles.";

/// Spirit of the Labyrinth, verbatim Oracle text: a CR 121.2b one-card-per-turn limit.
const SPIRIT_OF_THE_LABYRINTH_ORACLE: &str = "Each player can't draw more than one card each turn.";

/// Stinkweed Imp, verbatim Oracle text: Dredge 5 makes each of P0's draws a replacement choice.
const STINKWEED_IMP_ORACLE: &str = "Flying\nWhenever this creature deals combat damage to a \
creature, destroy that creature.\nDredge 5 (If you would draw a card, you may mill five cards \
instead. If you do, return this card from your graveyard to your hand.)";

const STAGED_LIBRARY: [&str; 10] = [
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
];

#[derive(Clone, Copy)]
enum Restriction {
    None,
    Maralen,
    Spirit,
}

fn draw_cost(cards: i32) -> AbilityCost {
    AbilityCost::EffectCost {
        effect: Box::new(Effect::Draw {
            count: QuantityExpr::Fixed { value: cards },
            target: TargetFilter::Controller,
        }),
    }
}

/// P0's main phase: a staged library, Stinkweed Imp in P0's graveyard, the
/// restriction under P1, and a P0 creature carrying `ability` when given.
/// Layers are evaluated after `build()` so every static is live.
fn board(
    restriction: Restriction,
    ability: Option<AbilityDefinition>,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    let mut source = scenario.add_creature(P0, "Draw Cost Source", 1, 1);
    if let Some(ability) = ability {
        source.with_ability_definition(ability);
    }
    let source = source.id();
    let imp = scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE)
        .id();
    match restriction {
        Restriction::None => {}
        Restriction::Maralen => {
            scenario.add_creature_from_oracle(P1, "Maralen of the Mornsong", 2, 3, MARALEN_ORACLE);
        }
        Restriction::Spirit => {
            scenario.add_creature_from_oracle(
                P1,
                "Spirit of the Labyrinth",
                3,
                1,
                SPIRIT_OF_THE_LABYRINTH_ORACLE,
            );
        }
    }
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    (runner, source, imp)
}

fn is_dredge_prompt_for_p0(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P0)
}

/// No Dredge was offered or applied: no prompt, and P0's graveyard is still
/// exactly Stinkweed Imp (nothing milled, the Imp not returned).
fn assert_no_dredge(state: &GameState, imp: ObjectId, label: &str) {
    let p0 = &state.players[P0.0 as usize];
    assert!(!is_dredge_prompt_for_p0(state), "{label}: no Dredge prompt");
    assert_eq!(
        p0.graveyard.iter().copied().collect::<Vec<_>>(),
        vec![imp],
        "{label}: nothing milled and the Imp stays in the graveyard"
    );
}

/// P0 resolves a plain "draw a card" from the source.
fn resolve_draw(runner: &mut GameRunner, source: ObjectId) {
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0)
        .expect("the draw resolves");
}

/// CR 614.17c: under Maralen a plain draw can't happen, so Dredge is
/// not offered and the draw is clamped — no prompt, no mill, no return.
///
/// Revert probe: dropping the can't-happen filter from
/// `replacement::find_applicable_replacements` raises the Dredge prompt on the
/// Maralen row.
#[test]
fn maralen_keeps_dredge_from_replacing_a_draw() {
    // Control: without Maralen the same draw is a legal Dredge choice.
    let (mut runner, source, _) = board(Restriction::None, None);
    resolve_draw(&mut runner, source);
    assert!(
        is_dredge_prompt_for_p0(runner.state()),
        "an ordinary draw offers Dredge"
    );

    let (mut runner, source, imp) = board(Restriction::Maralen, None);
    let p0 = &runner.state().players[P0.0 as usize];
    let (hand_before, library_before) = (p0.hand.len(), p0.library.len());
    resolve_draw(&mut runner, source);
    assert_no_dredge(runner.state(), imp, "Maralen");
    let p0 = &runner.state().players[P0.0 as usize];
    assert_eq!(p0.hand.len(), hand_before, "the draw is clamped");
    assert_eq!(p0.library.len(), library_before, "no card left the library");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 614.17c + CR 121.2b: Spirit of the Labyrinth lets P0 draw one card a
/// turn. The first draw is a legal Dredge choice (accepted: five milled, the
/// Imp returned). Once P0 has drawn (Dredge declined), a second draw can't
/// happen, so Dredge is not offered for it.
///
/// Revert probe: dropping the can't-happen filter raises a Dredge prompt on the
/// second draw.
#[test]
fn a_per_turn_draw_limit_keeps_dredge_from_replacing_a_forbidden_draw() {
    // Control: the first draw this turn offers a legal Dredge.
    let (mut runner, source, imp) = board(Restriction::Spirit, None);
    let library_before = runner.state().players[P0.0 as usize].library.len();
    resolve_draw(&mut runner, source);
    assert!(
        is_dredge_prompt_for_p0(runner.state()),
        "the first draw this turn offers Dredge"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting Dredge is legal");
    let p0 = &runner.state().players[P0.0 as usize];
    assert!(p0.hand.contains(&imp), "Dredge returned the Imp");
    assert_eq!(p0.library.len(), library_before - 5, "Dredge milled five");

    let (mut runner, source, imp) = board(Restriction::Spirit, None);
    resolve_draw(&mut runner, source);
    assert!(is_dredge_prompt_for_p0(runner.state()));
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining Dredge is legal");
    let p0 = &runner.state().players[P0.0 as usize];
    assert_eq!(p0.cards_drawn_this_turn, 1, "P0 drew the first card");
    let (hand_before, library_before) = (p0.hand.len(), p0.library.len());

    resolve_draw(&mut runner, source);
    assert_no_dredge(runner.state(), imp, "Spirit, second draw");
    let p0 = &runner.state().players[P0.0 as usize];
    assert_eq!(p0.hand.len(), hand_before, "the second draw is ignored");
    assert_eq!(p0.library.len(), library_before, "no mill, no draw");
}

/// A P0 creature ability "`cost`: draw a card".
fn draw_ability_costing(cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(cost)
}

fn activate(runner: &mut GameRunner, source: ObjectId) -> bool {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .is_ok()
}

/// CR 614.17b + CR 121.2b: "the player can't pay a cost that includes drawing
/// multiple cards" under Spirit of the Labyrinth. A fresh activation costing
/// `Composite[draw a card, draw a card]` is refused before its first leg can
/// raise Dredge, though each leg alone is drawable. Controls: the single-draw
/// cost under Spirit and the two-draw cost without Spirit are both admitted and
/// reach the first leg's legal Dredge prompt.
///
/// Revert probe: dropping the whole-cost draw admission from
/// `is_payable_for_activation` admits the Composite (each leg alone is
/// drawable, and the first leg's Dredge choice pauses the payment), so the
/// refused row raises a Dredge prompt.
#[test]
fn a_fresh_activation_draw_cost_is_judged_as_a_whole() {
    let two_draws = || AbilityCost::Composite {
        costs: vec![draw_cost(1), draw_cost(1)],
    };

    // Control: one draw is within Spirit's limit; its Dredge choice is legal.
    let (mut runner, source, imp) = board(
        Restriction::Spirit,
        Some(draw_ability_costing(draw_cost(1))),
    );
    assert!(activate(&mut runner, source), "a one-draw cost is admitted");
    assert!(
        is_dredge_prompt_for_p0(runner.state()),
        "the cost's draw offers Dredge"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting Dredge is legal");
    assert!(
        runner.state().players[P0.0 as usize].hand.contains(&imp),
        "Dredge returned the Imp"
    );

    // Control: without a limit the two-draw cost is admitted and reaches leg 1.
    let (mut runner, source, _) = board(Restriction::None, Some(draw_ability_costing(two_draws())));
    assert!(activate(&mut runner, source), "a two-draw cost is admitted");
    assert!(is_dredge_prompt_for_p0(runner.state()));

    let (mut runner, source, imp) =
        board(Restriction::Spirit, Some(draw_ability_costing(two_draws())));
    let p0 = &runner.state().players[P0.0 as usize];
    let (hand_before, library_before) = (p0.hand.len(), p0.library.len());
    assert!(
        !activate(&mut runner, source),
        "a cost that includes two draws can't be paid under a one-card limit"
    );
    assert_no_dredge(runner.state(), imp, "Spirit, two-draw cost");
    let p0 = &runner.state().players[P0.0 as usize];
    assert_eq!(p0.hand.len(), hand_before, "nothing was drawn");
    assert_eq!(p0.library.len(), library_before);
    assert!(
        runner.state().stack.is_empty(),
        "the ability was not activated"
    );
}
