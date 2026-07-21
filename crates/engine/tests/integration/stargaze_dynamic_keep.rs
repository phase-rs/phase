//! Regression coverage for the dynamic keep-count Dig class: a "look at twice X
//! cards ... put X cards from among them into your hand and the rest into your
//! graveyard" spell whose *keep* count is announced X (Stargaze), not a literal.
//!
//! CARD TEXT (Stargaze, {X}{B}{B} sorcery): "Look at twice X cards from the top
//! of your library. Put X cards from among them into your hand and the rest into
//! your graveyard. You lose X life."
//!
//! The capability is composed of shipped building blocks:
//!   * `Effect::Dig.count` is a `QuantityExpr` (CR 701.20e look) — here
//!     `Multiply { factor: 2, X }` (CR 107.3 "twice X").
//!   * `Effect::Dig.keep_count_expr` (this change) carries the dynamic keep X so
//!     the resolver reads it at resolution (CR 608.2c) instead of collapsing to 1.
//!   * The trailing "You lose X life" chains as `Effect::LoseLife { X }` (CR 119.3).

use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{AbilityKind, Effect, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const STARGAZE_ORACLE: &str = "Look at twice X cards from the top of your library. \
Put X cards from among them into your hand and the rest into your graveyard. You lose X life.";

/// Stargaze's `{X}{B}{B}` mana cost.
fn stargaze_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Black, ManaCostShard::Black],
        generic: 0,
    }
}

fn var_x() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::Variable {
            name: "X".to_string(),
        },
    }
}

/// Add `count` units of `ty` mana to P0's pool (deterministic payment).
fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Put a plain (non-land) card into P0's library. Returns its id.
fn add_library_card(runner: &mut GameRunner, name: &str) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        name.to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    id
}

fn push_chain_effects(ability: &engine::types::ability::AbilityDefinition, out: &mut Vec<Effect>) {
    out.push((*ability.effect).clone());
    let mut node = ability.sub_ability.as_deref();
    while let Some(def) = node {
        out.push((*def.effect).clone());
        node = def.sub_ability.as_deref();
    }
}

fn all_spell_effects(obj: &GameObject) -> Vec<Effect> {
    let mut out = Vec::new();
    for ability in obj
        .abilities
        .iter()
        .filter(|a| a.kind == AbilityKind::Spell)
    {
        push_chain_effects(ability, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// Test 1 (SHAPE): the parsed AST carries the dynamic look (2X) AND dynamic keep (X).
// Labelled SHAPE — asserts parser structure via semantic fields only. The runtime
// behavior is proven by Test 2.
// ---------------------------------------------------------------------------

#[test]
fn stargaze_parses_dynamic_look_and_keep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Stargaze", false, STARGAZE_ORACLE);
    builder.with_mana_cost(stargaze_cost());
    let spell_id = builder.id();
    let runner = scenario.build();

    let effects = all_spell_effects(&runner.state().objects[&spell_id]);

    // CR 701.20e: the Dig's look count is "twice X" (Multiply factor 2) and the
    // keep count is the dynamic X carried in keep_count_expr — NOT a fixed keep.
    let dig = effects
        .iter()
        .find(|e| matches!(e, Effect::Dig { .. }))
        .expect("Stargaze must parse a Dig");
    match dig {
        Effect::Dig {
            count,
            keep_count,
            keep_count_expr,
            destination,
            rest_destination,
            up_to,
            reveal,
            ..
        } => {
            assert_eq!(
                *count,
                QuantityExpr::Multiply {
                    factor: 2,
                    inner: Box::new(var_x()),
                },
                "look count must be twice X"
            );
            assert_eq!(
                *keep_count_expr,
                Some(var_x()),
                "keep count must be the dynamic X"
            );
            assert_eq!(*keep_count, None, "dynamic keep must leave keep_count None");
            assert_eq!(*destination, Some(Zone::Hand), "kept cards go to hand");
            assert_eq!(
                *rest_destination,
                Some(Zone::Graveyard),
                "the rest go to the graveyard"
            );
            assert!(!*up_to, "'put X cards' is exact, not up-to");
            assert!(!*reveal, "private look, not a reveal");
        }
        other => panic!("expected Dig, got {other:?}"),
    }

    // CR 119.3: the trailing "You lose X life" chains as LoseLife { X }.
    let lose = effects
        .iter()
        .find_map(|e| match e {
            Effect::LoseLife { amount, .. } => Some(amount.clone()),
            _ => None,
        })
        .expect("Stargaze must chain a LoseLife");
    assert_eq!(lose, var_x(), "life loss amount must be X");
}

// ---------------------------------------------------------------------------
// Test 2 (RUNTIME): drive the full cast + DigChoice; measure zone/life deltas.
// ---------------------------------------------------------------------------

#[test]
fn stargaze_x_two_keeps_two_of_four() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Stargaze", false, STARGAZE_ORACLE);
    builder.with_mana_cost(stargaze_cost());
    let spell_id = builder.id();

    let mut runner = scenario.build();

    // Top 4 of the library (X=2 → look at twice X = 4).
    let c0 = add_library_card(&mut runner, "Look0");
    let c1 = add_library_card(&mut runner, "Look1");
    let c2 = add_library_card(&mut runner, "Look2");
    let c3 = add_library_card(&mut runner, "Look3");

    // X=2 → total cost {2}{B}{B} = 4 black sources cover the {2} generic + {B}{B}.
    add_mana(&mut runner, ManaType::Black, 4);

    let lib_before = runner.state().players[0].library.len();
    let life_before = runner.state().players[0].life;

    // CR 701.20e: announce X=2; pool auto-pays, driver halts at the DigChoice.
    let outcome = runner.cast(spell_id).x(2).resolve();

    // Discriminator: twice X = 4 cards were looked at, and keep_count == X = 2.
    match outcome.final_waiting_for() {
        WaitingFor::DigChoice {
            cards, keep_count, ..
        } => {
            assert_eq!(cards.len(), 4, "look at twice X = 4 cards, not X");
            assert_eq!(*keep_count, 2, "keep exactly X = 2");
        }
        other => panic!("expected DigChoice, got {other:?}"),
    }

    // Keep 2 of the 4 (the player chooses which dug cards go to hand).
    runner
        .act(GameAction::SelectCards {
            cards: vec![c0, c1],
        })
        .expect("keeping 2 of 4 must be accepted");
    runner.advance_until_stack_empty();

    let st = runner.state();
    // Measure the dig distribution directly on the four dug cards (the aggregate
    // hand/graveyard totals are confounded by the Stargaze spell itself leaving
    // hand and going to the graveyard on resolution).
    let dug = [c0, c1, c2, c3];
    let to_hand = dug
        .iter()
        .filter(|o| st.objects[o].zone == Zone::Hand)
        .count();
    let to_gy = dug
        .iter()
        .filter(|o| st.objects[o].zone == Zone::Graveyard)
        .count();
    assert_eq!(to_hand, 2, "kept 2 cards to hand");
    assert_eq!(to_gy, 2, "the other 2 go to the graveyard");
    assert_eq!(
        st.players[0].library.len(),
        lib_before - 4,
        "twice X = 4 cards left the library"
    );
    assert_eq!(st.players[0].life, life_before - 2, "lose X = 2 life");
    // The two chosen cards are the kept pair; the two unchosen are milled.
    assert_eq!(st.objects[&c0].zone, Zone::Hand);
    assert_eq!(st.objects[&c1].zone, Zone::Hand);
    assert_eq!(st.objects[&c2].zone, Zone::Graveyard);
    assert_eq!(st.objects[&c3].zone, Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// Test 3 (RUNTIME, degenerate): X=0 looks at nothing, keeps nothing, no prompt.
// ---------------------------------------------------------------------------

#[test]
fn stargaze_x_zero_no_dig_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Stargaze", false, STARGAZE_ORACLE);
    builder.with_mana_cost(stargaze_cost());
    let spell_id = builder.id();

    let mut runner = scenario.build();
    add_library_card(&mut runner, "Untouched");

    // X=0 → cost {0}{B}{B} = 2 black.
    add_mana(&mut runner, ManaType::Black, 2);

    let life_before = runner.state().players[0].life;
    let outcome = runner.cast(spell_id).x(0).resolve();

    // No DigChoice surfaces (twice 0 = 0 cards looked at); resolution completes.
    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::DigChoice { .. }),
        "X=0 must not surface a DigChoice, got {:?}",
        outcome.final_waiting_for()
    );
    // Lose 0 life — no change.
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "X=0 loses no life"
    );
}

// ---------------------------------------------------------------------------
// Test 4 (SHAPE, HOSTILE): a FIXED keep count must still lower to the fixed path
// (keep_count: Some, keep_count_expr: None). The dynamic path must not swallow
// literals.
// ---------------------------------------------------------------------------

#[test]
fn fixed_keep_dig_does_not_use_dynamic_path() {
    const FIXED_ORACLE: &str =
        "Look at the top four cards of your library. Put two of them into your hand \
and the rest into your graveyard.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let builder = scenario.add_spell_to_hand_from_oracle(P0, "FixedDig", false, FIXED_ORACLE);
    let spell_id = builder.id();
    let runner = scenario.build();

    let effects = all_spell_effects(&runner.state().objects[&spell_id]);
    let dig = effects
        .iter()
        .find(|e| matches!(e, Effect::Dig { .. }))
        .expect("fixed dig must parse a Dig");
    match dig {
        Effect::Dig {
            keep_count,
            keep_count_expr,
            ..
        } => {
            assert_eq!(*keep_count, Some(2), "fixed keep stays on the u32 path");
            assert_eq!(
                *keep_count_expr, None,
                "a literal keep must NOT route through keep_count_expr"
            );
        }
        other => panic!("expected Dig, got {other:?}"),
    }
}
