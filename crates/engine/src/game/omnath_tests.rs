//! Runtime tests for Omnath, Locus of All's precombat-main trigger.
//!
//! These drive the real production seams:
//! - the parser (`parse_oracle_text`) for the trigger AST (GAP-1 eligibility
//!   condition + GAP-2 dynamic-color mana production);
//! - the production trigger-resolution function (`resolve_ability_chain` at
//!   depth 0, exactly as the engine resolves a stack trigger), which exercises
//!   the GAP-1 sub-condition binding (last-revealed → parent targets, STEP 4);
//! - the real action handler (`apply`) for the optional-reveal decision and the
//!   `ChooseManaColor` mana-combination prompt (GAP-2 runtime resolver, STEP 6).
//!
//! CR 608.2c / CR 608.2d / CR 400.7j / CR 106.1 / CR 106.5 / CR 202.2c.

#![cfg(test)]

use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::resolve_ability_chain;
use crate::game::scenario::{GameRunner, GameScenario};
use crate::parser::oracle::{parse_oracle_text, ParsedAbilities};
use crate::types::actions::GameAction;
use crate::types::game_state::{GameState, ManaChoice, ManaChoicePrompt, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;

const OMNATH_ORACLE: &str = "If you would lose unspent mana, that mana becomes black instead.\n\
    At the beginning of your first main phase, look at the top card of your library. \
    You may reveal that card if it has three or more colored mana symbols in its mana cost. \
    If you do, add three mana in any combination of its colors and put it into your hand. \
    If you don't reveal it, put it into your hand.";

const P0: PlayerId = PlayerId(0);

fn parse_omnath() -> ParsedAbilities {
    parse_oracle_text(
        OMNATH_ORACLE,
        "Omnath, Locus of All",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &[],
    )
}

/// Omnath on P0's battlefield with a single top-of-library card carrying the
/// given mana-cost shards and colors. The precombat-main trigger is resolved
/// through the production resolver at depth 0 (no hand-built `WaitingFor`).
fn omnath_runtime(shards: Vec<ManaCostShard>, colors: Vec<ManaColor>) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    let omnath = scenario
        .add_creature_from_oracle(P0, "Omnath, Locus of All", 4, 4, OMNATH_ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Top Card");
    {
        let obj = scenario.state.objects.get_mut(&top).unwrap();
        obj.mana_cost = ManaCost::Cost { shards, generic: 1 };
        obj.color = colors;
    }
    let mut runner = scenario.build();

    let parsed = parse_omnath();
    let phase = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase)
        .expect("Omnath has a precombat-main Phase trigger");
    let execute = phase
        .execute
        .as_ref()
        .expect("Phase trigger has an execute");
    let resolved = build_resolved_from_def(execute, omnath, P0);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("trigger resolution");
    (runner, top)
}

fn top_in_hand(state: &GameState, top: ObjectId) -> bool {
    state
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.hand.contains(&top))
        .unwrap_or(false)
}

fn pool_total(state: &GameState) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0)
}

fn pool_color(state: &GameState, color: ManaType) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.mana_pool.count_color(color))
        .unwrap_or(0)
}

fn is_optional_reveal_pause(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. })
}

fn mana_prompt(state: &GameState) -> Option<ManaChoicePrompt> {
    match &state.waiting_for {
        WaitingFor::ChooseManaColor { choice, .. } => Some(choice.clone()),
        _ => None,
    }
}

// ── GAP-1: ineligible top card never parks at an illegal optional reveal ──────

/// CR 608.2d: Kitchen-Finks-shaped top card ({1}{G/W}{G/W} → 2 colored mana
/// symbols) fails the "three or more colored mana symbols" eligibility check, so
/// the optional reveal is NEVER offered, no mana is produced, and the card goes
/// to hand. DISCRIMINATING: reverting the GAP-1 parser condition (STEP 3) drops
/// the `QuantityCheck` gate, so the optional reveal would be offered for the
/// ineligible card — `is_optional_reveal_pause` would be true and the engine
/// would park (the reported bug).
#[test]
fn ineligible_two_symbol_top_card_no_reveal_no_mana() {
    let (runner, top) = omnath_runtime(
        vec![ManaCostShard::GreenWhite, ManaCostShard::GreenWhite],
        vec![ManaColor::Green, ManaColor::White],
    );
    assert!(
        !is_optional_reveal_pause(runner.state()),
        "ineligible (2-symbol) top card must not offer the optional reveal: {:?}",
        runner.state().waiting_for
    );
    assert!(
        mana_prompt(runner.state()).is_none(),
        "no mana prompt for an ineligible card"
    );
    assert_eq!(pool_total(runner.state()), 0, "no mana produced");
    assert!(
        top_in_hand(runner.state(), top),
        "the card is put into hand"
    );
}

/// CR 608.2d: a zero-colored-symbol top card (Plains-shaped) is likewise
/// ineligible — same no-reveal / no-mana / to-hand outcome.
#[test]
fn ineligible_zero_symbol_top_card_no_reveal_no_mana() {
    let (runner, top) = omnath_runtime(vec![], vec![]);
    assert!(
        !is_optional_reveal_pause(runner.state()),
        "ineligible (0-symbol) top card must not offer the optional reveal: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(pool_total(runner.state()), 0);
    assert!(top_in_hand(runner.state(), top));
}

// ── GAP-2: eligible multicolor card offers exactly its colors ────────────────

/// CR 106.1 + CR 202.2c + CR 400.7j: an eligible 3-distinct-color top card
/// ({B}{U}{G}) offers the optional reveal; accepting surfaces a
/// `ManaChoicePrompt::AnyCombination` whose option set is EXACTLY the revealed
/// card's colors {B,U,G} — not all five, not Omnath's colors. Choosing produces
/// three mana of the chosen colors and the card is put into hand.
///
/// DISCRIMINATING for the STEP 4 deep binding: the add-mana sub reads its colors
/// from the revealed object via `ObjectScope::Target`. Reverting the "carry the
/// injected targets into the performed-true sub-resolution" half of STEP 4 leaves
/// the sub with no target, so `object_colors_for_scope` returns empty → CR 106.5
/// produces no mana (or no prompt), failing the option-set / pool assertions.
#[test]
fn eligible_multicolor_offers_exact_card_colors_and_produces_mana() {
    let (mut runner, top) = omnath_runtime(
        vec![
            ManaCostShard::Black,
            ManaCostShard::Blue,
            ManaCostShard::Green,
        ],
        vec![ManaColor::Black, ManaColor::Blue, ManaColor::Green],
    );
    assert!(
        is_optional_reveal_pause(runner.state()),
        "eligible top card offers the optional reveal, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the reveal");

    let prompt = mana_prompt(runner.state()).unwrap_or_else(|| {
        panic!(
            "expected a mana prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    let ManaChoicePrompt::AnyCombination { count, options } = prompt else {
        panic!("expected AnyCombination prompt, got {prompt:?}");
    };
    assert_eq!(count, 3, "three mana to distribute");
    assert_eq!(options.len(), 3, "exactly three color options: {options:?}");
    for c in [ManaType::Black, ManaType::Blue, ManaType::Green] {
        assert!(
            options.contains(&c),
            "option set must include {c:?}: {options:?}"
        );
    }
    for c in [ManaType::White, ManaType::Red, ManaType::Colorless] {
        assert!(
            !options.contains(&c),
            "option set must NOT include {c:?} (only the card's colors): {options:?}"
        );
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(vec![ManaType::Black, ManaType::Blue, ManaType::Green]),
            count: 1,
        })
        .expect("choose the three colors");

    assert_eq!(pool_total(runner.state()), 3, "three mana produced");
    assert_eq!(pool_color(runner.state(), ManaType::Black), 1);
    assert_eq!(pool_color(runner.state(), ManaType::Blue), 1);
    assert_eq!(pool_color(runner.state(), ManaType::Green), 1);
    assert!(
        top_in_hand(runner.state(), top),
        "the card is put into hand"
    );
}

/// CR 106.1 + CR 106.5: a monocolored eligible top card ({B}{B}{B}) needs no
/// color prompt — accepting produces three black mana directly and the card goes
/// to hand.
#[test]
fn eligible_monocolor_produces_three_black_without_prompt() {
    let (mut runner, top) = omnath_runtime(
        vec![
            ManaCostShard::Black,
            ManaCostShard::Black,
            ManaCostShard::Black,
        ],
        vec![ManaColor::Black],
    );
    assert!(is_optional_reveal_pause(runner.state()));

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the reveal");

    assert!(
        mana_prompt(runner.state()).is_none(),
        "a single-color object needs no prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(pool_total(runner.state()), 3);
    assert_eq!(pool_color(runner.state(), ManaType::Black), 3);
    assert!(top_in_hand(runner.state(), top));
}

/// CR 608.2c: declining the optional reveal on an eligible card produces NO mana.
/// The reveal is offered (eligible) and then declined; the "If you do, add three
/// mana …" rider must not fire. DISCRIMINATING for the GAP-2 gate: producing mana
/// here would mean the add-mana sub ran without the optional reveal being
/// performed.
#[test]
fn eligible_declined_reveal_produces_no_mana() {
    let (mut runner, _top) = omnath_runtime(
        vec![
            ManaCostShard::Black,
            ManaCostShard::Blue,
            ManaCostShard::Green,
        ],
        vec![ManaColor::Black, ManaColor::Blue, ManaColor::Green],
    );
    assert!(
        is_optional_reveal_pause(runner.state()),
        "eligible card offers the optional reveal"
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the reveal");

    assert_eq!(pool_total(runner.state()), 0, "no mana when declined");
    assert!(
        mana_prompt(runner.state()).is_none(),
        "no mana prompt on decline"
    );
    // CR 608.2c: the decline branch must resolve to completion, not leave the
    // chain parked at the optional reveal (the original stuck-state regression).
    assert!(
        !is_optional_reveal_pause(runner.state()),
        "decline must not leave the chain parked at the optional reveal"
    );
}

/// CR 608.2c: declining the optional reveal still puts the card into hand ("If
/// you don't reveal it, put it into your hand"). The `Not(OptionalEffectPerformed)`
/// decline clause is nested as a GRANDCHILD of the IfYouDo head (the "If you do"
/// body has two instructions — add mana AND put into hand — so the decline clause
/// is chained after the in-hand move). `nested_optional_decline_clause` walks the
/// head's sub-chain to find it, and the decline resolves ONLY that clause — not
/// the accept-only add-mana instruction. DISCRIMINATING: reverting the resolver
/// change leaves the card on top of the library (decline branch never reached).
#[test]
fn eligible_declined_reveal_puts_card_to_hand() {
    let (mut runner, top) = omnath_runtime(
        vec![
            ManaCostShard::Black,
            ManaCostShard::Blue,
            ManaCostShard::Green,
        ],
        vec![ManaColor::Black, ManaColor::Blue, ManaColor::Green],
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the reveal");
    assert!(
        top_in_hand(runner.state(), top),
        "the card still goes to hand"
    );
}
