//! Issue #6500 — Loreseeker's Stone activation cost ignores hand size.
//!
//! Oracle: `{3}, {T}: Draw three cards. This ability costs {1} more to activate
//! for each card in your hand.`
//!
//! Discord report: activated for `{3}` with 5 cards in hand (should be `{8}`).
//! Root cause: the trailing "costs {1} more …" clause was an
//! `Effect::Unimplemented` gap — `try_parse_cost_reduction` only recognized
//! "less", and runtime `apply_cost_reduction` only reduced. Fix parameterizes
//! self `CostReduction` with existing `CostModifyMode::Raise` (CR 601.2f) and
//! applies the increase at cost-determination time (CR 602.2b).
//!
//! DISCRIMINATING: `can_activate_ability_now` (production affordability gate)
//! accepts `{3}` with an empty hand but rejects `{3}` with five cards in hand.
//! A revert leaves `{3}` payable at every hand size.

use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, CostReduction};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::CostModifyMode;

const LORESEEKERS_STONE: &str = "{3}, {T}: Draw three cards. This ability costs {1} more \
to activate for each card in your hand.";

const DRAW_ABILITY_INDEX: usize = 0;

fn find_cost_reduction(def: &AbilityDefinition) -> Option<&CostReduction> {
    let mut cur = Some(def);
    while let Some(d) = cur {
        if let Some(cr) = d.cost_reduction.as_ref() {
            return Some(cr);
        }
        cur = d.sub_ability.as_deref();
    }
    None
}

fn add_mana(
    state: &mut engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
    n: u32,
) {
    for _ in 0..n {
        state.players[player.0 as usize]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ));
    }
}

fn loreseekers_stone_on_battlefield(
    scenario: &mut GameScenario,
) -> engine::types::identifiers::ObjectId {
    scenario
        .add_creature(P0, "Loreseeker's Stone", 0, 0)
        .as_artifact()
        .from_oracle_text(LORESEEKERS_STONE)
        .id()
}

#[test]
fn loreseekers_stone_parses_raise_for_each_card_in_hand() {
    let reduction = {
        let parsed = parse_oracle_text(LORESEEKERS_STONE, "Loreseeker's Stone", &[], &[], &[]);
        parsed
            .abilities
            .iter()
            .find_map(find_cost_reduction)
            .cloned()
            .expect("Loreseeker's Stone must capture a self cost modification")
    };
    assert_eq!(reduction.mode, CostModifyMode::Raise);
    assert_eq!(reduction.amount_per, 1);
    assert_eq!(reduction.condition, None);

    let parsed = parse_oracle_text(LORESEEKERS_STONE, "Loreseeker's Stone", &[], &[], &[]);
    let ability = parsed
        .abilities
        .iter()
        .find(|a| find_cost_reduction(a).is_some())
        .expect("activation ability");
    let mut cur = Some(ability);
    while let Some(d) = cur {
        assert!(
            d.effect.unimplemented_description().is_none(),
            "cost-increase clause must not survive as Unimplemented: {:?}",
            d.effect
        );
        cur = d.sub_ability.as_deref();
    }
}

#[test]
fn loreseekers_stone_activation_affordability_scales_with_hand_size() {
    let mut empty_hand = GameScenario::new();
    empty_hand.at_phase(Phase::PreCombatMain);
    let stone_empty = loreseekers_stone_on_battlefield(&mut empty_hand);
    for i in 0..5 {
        empty_hand.add_card_to_hand(P1, &format!("Opponent Hand Filler {i}"));
    }
    let mut runner_empty = empty_hand.build();
    add_mana(runner_empty.state_mut(), P0, 3);
    assert!(
        can_activate_ability_now(runner_empty.state(), P0, stone_empty, DRAW_ABILITY_INDEX),
        "five cards in the opponent's hand must not raise the controller's {{3}} activation"
    );

    let mut five_in_hand = GameScenario::new();
    five_in_hand.at_phase(Phase::PreCombatMain);
    let stone_five = loreseekers_stone_on_battlefield(&mut five_in_hand);
    for i in 0..5 {
        five_in_hand.add_card_to_hand(P0, &format!("Hand Filler {i}"));
    }
    let mut runner_five = five_in_hand.build();
    add_mana(runner_five.state_mut(), P0, 3);
    assert!(
        !can_activate_ability_now(runner_five.state(), P0, stone_five, DRAW_ABILITY_INDEX),
        "five cards in hand: {{3}} generic must NOT afford {{8}} total (Discord report)"
    );

    add_mana(runner_five.state_mut(), P0, 5);
    assert!(
        can_activate_ability_now(runner_five.state(), P0, stone_five, DRAW_ABILITY_INDEX),
        "five cards in hand: {{8}} generic must afford the activation"
    );
}
