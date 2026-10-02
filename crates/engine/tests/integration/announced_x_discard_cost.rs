//! A discard cost whose count is X discards the announced number of cards.
//!
//! CR 107.3a: when a cost has an X the text does not define, the controller
//! announces X while casting or activating, and every X in the cost takes that
//! value. CR 601.2h: the total cost, the X discards included, is then paid in
//! full. CR 602.2b applies the same steps to an activated ability.
//!
//! The rows cover each shape of the printed class: X set only by the cost
//! (Sickening Dreams), X shared with the mana cost with an untyped discard
//! (Abandon Hope) and a typed one (Aether Tide), a random discard (Devastating
//! Dreams, CR 701.9b), and an activation cost (Gix, Yawgmoth Praetor).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AdditionalCost, CardSelectionMode, DiscardSelfScope, QuantityExpr,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SICKENING_DREAMS: &str = "As an additional cost to cast this spell, discard X cards.\nSickening Dreams deals X damage to each creature and each player.";
const ABANDON_HOPE: &str = "As an additional cost to cast this spell, discard X cards.\nLook at target opponent's hand and choose X cards from it. That player discards those cards.";
const AETHER_TIDE: &str = "As an additional cost to cast this spell, discard X creature cards.\nReturn X target creatures to their owners' hands.";
const DEVASTATING_DREAMS: &str = "As an additional cost to cast this spell, discard X cards at random.\nEach player sacrifices X lands of their choice. Devastating Dreams deals X damage to each creature.";
const BOND_OF_AGONY: &str =
    "As an additional cost to cast this spell, pay X life.\nEach other player loses X life.";
const GIX: &str = "Whenever a creature deals combat damage to one of your opponents, its controller may pay 1 life. If they do, they draw a card.\n{4}{B}{B}{B}, Discard X cards: Exile the top X cards of target opponent's library. You may play lands and cast spells from among cards exiled this way without paying their mana costs.";

fn pool(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|&kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

/// Starts casting `spell` and returns the largest X the engine offers.
fn offered_x_max(runner: &mut GameRunner, spell: ObjectId) -> u32 {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin the cast");
    match &runner.state().waiting_for {
        WaitingFor::ChooseXValue { max, .. } => *max,
        other => panic!("the cast must announce X, got {other:?}"),
    }
}

fn hand_cards(scenario: &mut GameScenario, names: &[&str]) -> Vec<ObjectId> {
    names
        .iter()
        .map(|name| scenario.add_card_to_hand(P0, name))
        .collect()
}

#[test]
fn zero_announced_x_commits_without_a_discard_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreams = scenario
        .add_spell_to_hand_from_oracle(P0, "Sickening Dreams", false, SICKENING_DREAMS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let kept = scenario.add_card_to_hand(P0, "Kept");
    scenario.with_mana_pool(P0, pool(&[ManaType::Black, ManaType::Colorless]));
    let mut runner = scenario.build();

    assert_eq!(offered_x_max(&mut runner, dreams), 1);
    runner
        .act(GameAction::ChooseX { value: 0 })
        .expect("announce zero X");

    // CR 107.3a + CR 601.2h: announcing zero completes the discard cost.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, dreams);
    assert!(matches!(
        runner.state().stack[0].kind,
        StackEntryKind::Spell { .. }
    ));
    assert_eq!(runner.state().objects[&kept].zone, Zone::Hand);
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
}

#[test]
fn zero_fixed_discard_continues_with_life_and_mana_costs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Composite Cost Spell", false, "You gain 1 life.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 0 },
                    filter: None,
                    selection: CardSelectionMode::Chosen,
                    self_scope: DiscardSelfScope::FromHand,
                },
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                },
            ],
        }))
        .id();
    let kept = scenario.add_card_to_hand(P0, "Kept");
    scenario.with_mana_pool(P0, pool(&[ManaType::Black]));
    let mut runner = scenario.build();
    let mut immediate = GameRunner::from_state(runner.state().clone());
    let life_before = immediate.state().players[P0.0 as usize].life;
    immediate
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: immediate.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast with a zero discard followed by a life cost");

    // CR 601.2h: skipping the zero selection still pays every remaining cost.
    assert!(matches!(
        immediate.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(immediate.state().stack.len(), 1);
    assert_eq!(immediate.state().stack[0].source_id, spell);
    assert!(matches!(
        immediate.state().stack[0].kind,
        StackEntryKind::Spell { .. }
    ));
    assert_eq!(
        immediate.state().players[P0.0 as usize].life,
        life_before - 2
    );
    assert_eq!(
        immediate.state().players[P0.0 as usize].mana_pool.total(),
        0
    );
    assert_eq!(immediate.state().objects[&kept].zone, Zone::Hand);

    let outcome = runner.cast(spell).resolve();

    outcome.assert_life_delta(P0, -1);
    outcome.assert_zone(&[kept], Zone::Hand);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.state().players[P0.0 as usize].mana_pool.total(), 0);
}

#[test]
fn cost_defined_x_discards_the_announced_number_of_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreams = scenario
        .add_spell_to_hand_from_oracle(P0, "Sickening Dreams", false, SICKENING_DREAMS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let hand = hand_cards(&mut scenario, &["Paid One", "Paid Two", "Kept"]);
    scenario.with_mana_pool(P0, pool(&[ManaType::Black, ManaType::Colorless]));
    let mut runner = scenario.build();

    let outcome = runner.cast(dreams).x(2).pay_cost_with(&hand[..2]).resolve();

    assert_eq!(outcome.zone_of(hand[0]), Zone::Graveyard);
    assert_eq!(outcome.zone_of(hand[1]), Zone::Graveyard);
    assert_eq!(
        outcome.zone_of(hand[2]),
        Zone::Hand,
        "only X cards are discarded"
    );
    assert_eq!(outcome.life_delta(P0), -2, "the effect reads the same X");
    assert_eq!(outcome.life_delta(P1), -2);
}

#[test]
fn cost_defined_x_is_capped_by_the_cards_that_can_be_discarded() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreams = scenario
        .add_spell_to_hand_from_oracle(P0, "Sickening Dreams", false, SICKENING_DREAMS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    hand_cards(&mut scenario, &["Paid One", "Paid Two"]);
    scenario.with_mana_pool(P0, pool(&[ManaType::Black, ManaType::Colorless]));
    let mut runner = scenario.build();

    assert_eq!(
        offered_x_max(&mut runner, dreams),
        2,
        "X is capped at the cards the caster can discard (CR 601.2h)"
    );
}

/// CR 107.3a + CR 601.2h: one X is announced for the whole cost, so a spell
/// with {X} in its mana cost caps it by the mana as well as by the discard.
#[test]
fn mana_x_is_capped_by_mana_as_well_as_by_the_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hope = scenario
        .add_spell_to_hand_from_oracle(P0, "Abandon Hope", false, ABANDON_HOPE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    hand_cards(&mut scenario, &["Paid One", "Paid Two"]);
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Black, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = scenario.build();

    assert_eq!(offered_x_max(&mut runner, hope), 1);
}

/// The same cap applies to every additional cost that announces X; "pay X
/// life" is capped by the mana, not only by the life total.
#[test]
fn mana_x_pay_life_is_capped_by_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bond = scenario
        .add_spell_to_hand_from_oracle(P0, "Bond of Agony", false, BOND_OF_AGONY)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Black, ManaType::Colorless]));
    let mut runner = scenario.build();

    assert_eq!(offered_x_max(&mut runner, bond), 1);
}

#[test]
fn mana_x_pay_life_pays_and_resolves_the_announced_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bond = scenario
        .add_spell_to_hand_from_oracle(P0, "Bond of Agony", false, BOND_OF_AGONY)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Black, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(bond).x(2).resolve();

    // CR 107.3a + CR 601.2h: the additional cost and spell use the announced X.
    outcome.assert_life_delta(P0, -2);
    outcome.assert_life_delta(P1, -2);
    outcome.assert_zone(&[bond], Zone::Graveyard);
    assert_eq!(outcome.state().players[P0.0 as usize].mana_pool.total(), 0);
}

#[test]
fn mana_x_untyped_discard_reads_the_announced_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hope = scenario
        .add_spell_to_hand_from_oracle(P0, "Abandon Hope", false, ABANDON_HOPE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let hand = hand_cards(&mut scenario, &["Paid", "Kept"]);
    scenario.add_card_to_hand(P1, "Opponent Card");
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Black, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = scenario.build();

    let commit = runner
        .cast(hope)
        .x(1)
        .target_player(P1)
        .pay_cost_with(&hand[..1])
        .commit();

    assert_eq!(commit.state().objects[&hand[0]].zone, Zone::Graveyard);
    assert_eq!(
        commit.state().objects[&hand[1]].zone,
        Zone::Hand,
        "only X cards are discarded"
    );
}

#[test]
fn mana_x_typed_discard_reads_the_announced_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tide = scenario
        .add_spell_to_hand_from_oracle(P0, "Aether Tide", false, AETHER_TIDE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let paid = scenario.add_creature_to_hand(P0, "Paid Bear", 2, 2).id();
    let kept = scenario.add_creature_to_hand(P0, "Kept Bear", 2, 2).id();
    let bounced = scenario.add_creature(P1, "Bounced Bear", 2, 2).id();
    let stays = scenario.add_creature(P1, "Staying Bear", 2, 2).id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Blue, ManaType::Colorless]));
    let mut runner = scenario.build();

    let outcome = runner
        .cast(tide)
        .x(1)
        .target_objects(&[bounced])
        .pay_cost_with(&[paid])
        .resolve();

    assert_eq!(outcome.zone_of(paid), Zone::Graveyard);
    assert_eq!(
        outcome.zone_of(kept),
        Zone::Hand,
        "only X cards are discarded"
    );
    assert_eq!(outcome.zone_of(bounced), Zone::Hand);
    assert_eq!(outcome.zone_of(stays), Zone::Battlefield);
}

#[test]
fn random_discard_discards_the_announced_number_of_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreams = scenario
        .add_spell_to_hand_from_oracle(P0, "Devastating Dreams", false, DEVASTATING_DREAMS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let hand = hand_cards(&mut scenario, &["First", "Second", "Third"]);
    scenario.with_mana_pool(P0, pool(&[ManaType::Red, ManaType::Red]));
    let mut runner = scenario.build();

    let commit = runner.cast(dreams).x(2).commit();

    let discarded = hand
        .iter()
        .filter(|card| commit.state().objects[card].zone == Zone::Graveyard)
        .count();
    assert_eq!(discarded, 2, "CR 701.9b: X cards are discarded at random");
}

#[test]
fn activation_discard_discards_the_announced_number_of_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gix = scenario
        .add_creature_from_oracle(P0, "Gix, Yawgmoth Praetor", 3, 3, GIX)
        .id();
    let hand = hand_cards(&mut scenario, &["Paid One", "Paid Two", "Kept"]);
    scenario.with_library_top(P1, &["Top One", "Top Two", "Third"]);
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Black,
            ManaType::Black,
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();

    let outcome = runner
        .activate(gix, 0)
        .x(2)
        .target_player(P1)
        .pay_with(&hand[..2])
        .resolve();

    assert_eq!(outcome.zone_of(hand[0]), Zone::Graveyard);
    assert_eq!(outcome.zone_of(hand[1]), Zone::Graveyard);
    assert_eq!(
        outcome.zone_of(hand[2]),
        Zone::Hand,
        "only X cards are discarded"
    );
    assert_eq!(
        outcome.state().players[P1.0 as usize].library.len(),
        1,
        "the effect exiles the same X cards"
    );
}
