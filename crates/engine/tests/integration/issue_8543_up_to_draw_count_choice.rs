//! CR 608.2d: the magnitude of an "up to N" draw is a resolution-time choice
//! made by the drawing player — every count in `0..=N` must be reachable.
//!
//! Reported against Arcane Denial, whose real Oracle text (verified against
//! `data/mtgjson/AtomicCards.json`) reads:
//!
//! > Counter target spell. Its controller may draw up to two cards at the
//! > beginning of the next turn's upkeep.
//! > You draw a card at the beginning of the next turn's upkeep.
//!
//! The parse was already correct — the count lowered to
//! `UpTo { max: Fixed { value: 2 } }`. The defect was entirely below the parser:
//! `draw::resolve` read the count through `resolve_quantity_with_targets`, the
//! TRANSPARENT resolver, and `game/quantity.rs` folds `UpTo { max }` to `max`.
//! So no choice was ever opened and the draw silently resolved at the upper
//! bound: Arcane Denial always drew exactly two, with both 0 and 1 unreachable.
//!
//! These tests are deliberately pitched at the BUILDING BLOCK — a bare
//! `Effect::Draw { count: UpTo { .. } }` resolving through the production cast
//! pipeline — rather than replaying one card, because the bug belongs to every
//! "up to N" draw. A parser-level shape test cannot see this defect at all: the
//! AST was already right.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

/// Deep enough that the count choice, not the library, bounds every draw in the
/// matrix below. The boundary tests set their own shallower libraries.
const DEEP_LIBRARY: &[&str] = &[
    "Lib A", "Lib B", "Lib C", "Lib D", "Lib E", "Lib F", "Lib G", "Lib H",
];

fn hand_size(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// A free instant whose only instruction is `draw up to {max} cards`.
fn up_to_draw_spell(scenario: &mut GameScenario, max: i32) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, "Up To Draw Witness", true)
        .with_ability(Effect::Draw {
            count: QuantityExpr::up_to(QuantityExpr::Fixed { value: max }),
            target: TargetFilter::Controller,
        })
        .id()
}

/// A free instant whose only instruction is a MANDATORY `draw {count} cards` —
/// the negative control for the `up_to` guard.
fn fixed_draw_spell(scenario: &mut GameScenario, count: i32) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, "Fixed Draw Witness", true)
        .with_ability(Effect::Draw {
            count: QuantityExpr::Fixed { value: count },
            target: TargetFilter::Controller,
        })
        .id()
}

fn scenario_with_library(library: &[&str]) -> GameScenario {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, library);
    // P1 never draws here, but an empty library would let an unrelated
    // state-based loss end the game mid-test.
    scenario.with_library_top(P1, DEEP_LIBRARY);
    scenario
}

/// Pass priority until the count choice opens (or the engine stops elsewhere).
fn advance_to_choice(runner: &mut GameRunner) {
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseOneOfBranch { .. } => return,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Read the open count menu, assert it offers exactly `0..=max`, and return the
/// index whose label announces `chosen`.
///
/// Resolving the index BY LABEL rather than assuming `index == chosen` is what
/// makes the assertion non-vacuous: it pins the menu's count-to-branch mapping
/// instead of trusting positional coincidence. That the two then agree is
/// asserted separately.
fn count_branch_index(runner: &GameRunner, max: u32, chosen: u32) -> usize {
    let WaitingFor::ChooseOneOfBranch {
        player,
        branch_descriptions,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "CR 608.2d: an \"up to {max}\" draw must open a count choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        *player, P0,
        "CR 608.2d: the DRAWING player announces the count"
    );
    assert_eq!(
        branch_descriptions.len(),
        (max + 1) as usize,
        "an \"up to {max}\" draw must offer every count 0..={max}, got {branch_descriptions:?}"
    );
    let wanted = match chosen {
        0 => "Draw no cards".to_string(),
        1 => "Draw 1 card".to_string(),
        n => format!("Draw {n} cards"),
    };
    let index = branch_descriptions
        .iter()
        .position(|label| label == &wanted)
        .unwrap_or_else(|| panic!("no branch labelled {wanted:?} in {branch_descriptions:?}"));
    assert_eq!(
        index, chosen as usize,
        "the count menu must be ordered 0..={max}"
    );
    index
}

/// Cast an "up to `max`" draw, select `chosen`, and report `(before, after)`
/// hand sizes measured across the choice only.
///
/// The baseline is taken AT THE PROMPT, after the spell itself has left hand for
/// the stack, so the delta is exactly the cards drawn.
fn draw_up_to(library: &[&str], max: i32, chosen: u32) -> (GameRunner, usize, usize) {
    let mut scenario = scenario_with_library(library);
    let spell = up_to_draw_spell(&mut scenario, max);
    let mut runner = scenario.build();

    runner.cast(spell).commit();
    advance_to_choice(&mut runner);

    let index = count_branch_index(&runner, max as u32, chosen);
    let before = hand_size(&runner, P0);
    runner
        .act(GameAction::ChooseBranch { index })
        .expect("selecting a draw count must succeed");
    let after = hand_size(&runner, P0);
    (runner, before, after)
}

/// THE REGRESSION. Every count in `0..=N` is selectable and draws exactly that
/// many cards, for several N. Before the fix no prompt opened at all and the
/// draw always resolved at N.
#[test]
fn every_count_from_zero_to_the_maximum_is_selectable_and_draws_exactly_that_many() {
    for max in 1..=3i32 {
        for chosen in 0..=(max as u32) {
            let (_runner, before, after) = draw_up_to(DEEP_LIBRARY, max, chosen);
            assert_eq!(
                after - before,
                chosen as usize,
                "selecting {chosen} on an \"up to {max}\" draw must draw exactly {chosen} card(s)"
            );
        }
    }
}

/// CR 608.2d: choosing zero RESOLVES the draw — it is not the same as the
/// ability failing to resolve. The resolution completes and hands priority
/// back with an empty stack.
#[test]
fn choosing_zero_resolves_the_draw_rather_than_leaving_it_pending() {
    let (mut runner, before, after) = draw_up_to(DEEP_LIBRARY, 2, 0);

    assert_eq!(after, before, "selecting 0 must draw no cards");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "the zero-count branch must resolve the ability off the stack, not strand it"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "the count choice must be consumed by the selection"
    );
}

/// A MANDATORY `Draw { count: Fixed }` of the same magnitude must NOT prompt.
/// The negative control for the `up_to` guard: without it, a passing matrix
/// above could just mean every draw now prompts.
#[test]
fn a_mandatory_fixed_draw_opens_no_count_choice() {
    let mut scenario = scenario_with_library(DEEP_LIBRARY);
    let spell = fixed_draw_spell(&mut scenario, 2);
    let mut runner = scenario.build();

    let before = hand_size(&runner, P0);
    runner.cast(spell).resolve();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "a fixed-count draw is not a CR 608.2d choice and must not prompt"
    );
    // The spell left hand for the stack, so a 2-card draw nets +1.
    assert_eq!(
        hand_size(&runner, P0),
        before + 1,
        "a mandatory \"draw two cards\" must draw exactly two"
    );
}

/// The boundary where the chosen count equals the library exactly: all of it is
/// drawn and the player is still in the game (CR 704.5b loses only on an
/// ATTEMPTED draw from an empty library, which has not happened yet).
#[test]
fn choosing_the_whole_library_draws_all_of_it() {
    let (runner, before, after) = draw_up_to(&["Only A", "Only B"], 2, 2);

    assert_eq!(after - before, 2, "both remaining cards must be drawn");
    assert!(
        runner.state().players[0].library.is_empty(),
        "the library must be exhausted exactly"
    );
    assert!(
        !runner.state().players[0].is_eliminated,
        "emptying a library is not itself a loss (CR 704.5b needs an attempted draw)"
    );
}

/// CR 121.3: "If there are no cards in a player's library and an effect offers
/// that player the choice to draw a card, that player can choose to do so."
/// CR 608.2d carries the same exemption from its own "can't choose an illegal or
/// impossible option" restriction.
///
/// So the menu must NOT be clamped to library size: a player may legally
/// announce more cards than they can draw. This is the one case where narrowing
/// the offer would look like a helpful safety check and be a rules violation.
///
/// Announcing 3 against a one-card library is therefore legal, is carried out as
/// far as it can go, and then loses the game — the assertions below pin all
/// three, because a menu that offered 3 but silently truncated the instruction
/// to 1 would satisfy the first assertion alone.
#[test]
fn the_count_menu_is_not_clamped_to_library_size() {
    let mut scenario = scenario_with_library(&["Only One"]);
    let spell = up_to_draw_spell(&mut scenario, 3);
    let mut runner = scenario.build();

    runner.cast(spell).commit();
    advance_to_choice(&mut runner);

    // The decisive assertion: four options (0,1,2,3) against a one-card library.
    let index = count_branch_index(&runner, 3, 3);
    runner
        .act(GameAction::ChooseBranch { index })
        .expect("announcing more cards than the library holds is a legal CR 608.2d choice");

    let player = &runner.state().players[0];
    // CR 121.2: the instruction performs three INDIVIDUAL draws. The first
    // delivers the only card; the other two are attempted against an empty
    // library and deliver nothing.
    assert!(
        player.library.is_empty(),
        "the one available card must actually be drawn, not skipped"
    );
    // CR 704.5b: "If a player attempted to draw a card from a library with no
    // cards in it since the last time state-based actions were checked, that
    // player loses the game." This is what makes the over-announcement real
    // rather than silently truncated to the library size.
    assert!(
        player.drew_from_empty_library,
        "the counts beyond the library must be ATTEMPTED, not clamped away"
    );
    assert!(
        player.is_eliminated,
        "CR 704.5b: attempting to draw from an empty library loses the game"
    );
    // The hand is not asserted here: a player who has left the game no longer
    // has one, so hand size cannot witness the delivered card at this point.
}
