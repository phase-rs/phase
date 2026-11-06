//! Seven of Nine, Unassimilated — "differently named lands" thresholds on both
//! a resolution-time "unless" gate and an activation restriction.
//!
//! Oracle text (verbatim, MTGJSON):
//!   "At the beginning of your first main phase, draw a card. Then discard a
//!    card unless you control five or more differently named lands.
//!    {6}{U}: Seven of Nine's base power and toughness become 7/9. Activate
//!    only if you control seven or more differently named lands."
//!
//! Both clauses read the same distinct-name count, spelled with the prefix
//! adjective "differently named <type>" instead of Field of the Dead's suffix
//! "<type> with different names". `parse_control_count_ge_distinct_quality`
//! accepts both spellings and lowers them to
//! `ObjectCountDistinct { qualities: [Name] } >= N`.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 201.2b: objects have different names only if no two share a name, so
//!     two Islands contribute one name to the count.
//!   - CR 608.2c: the "unless" rider gates the discard as written at resolution.
//!   - CR 602.5: a player can't begin to activate a prohibited ability.
//!   - CR 613.4b: "base power and toughness become 7/9" applies in layer 7b,
//!     beneath counters (layer 7c).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SEVEN_OF_NINE: &str = "Seven of Nine, Unassimilated";
const SEVEN_OF_NINE_ORACLE: &str = "At the beginning of your first main phase, draw a card. \
Then discard a card unless you control five or more differently named lands.\n\
{6}{U}: Seven of Nine's base power and toughness become 7/9. Activate only if you control \
seven or more differently named lands.";

fn add_lands(scenario: &mut GameScenario, names: &[&str]) {
    for name in names {
        scenario.add_land_from_oracle(P0, name, "");
    }
}

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

/// Drive P0 from upkeep into the first main phase so the trigger fires from
/// the real phase transition, then let it resolve up to its first prompt.
fn run_main_phase_trigger(land_names: &[&str]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    let seven = scenario
        .add_creature_from_oracle(P0, SEVEN_OF_NINE, 0, 4, SEVEN_OF_NINE_ORACLE)
        .id();
    add_lands(&mut scenario, land_names);
    scenario.with_library_top(P0, &["Opt", "Consider", "Brainstorm", "Ponder"]);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.phase = Phase::Upkeep;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.auto_advance_to_main_phase();
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    runner.advance_until_stack_empty();
    (runner, seven)
}

/// Four distinct names across five lands (two Islands count once, CR 201.2b):
/// the threshold is NOT met, so the discard happens.
#[test]
fn four_differently_named_lands_with_a_duplicate_still_discards() {
    let (mut runner, _) =
        run_main_phase_trigger(&["Island", "Island", "Forest", "Swamp", "Mountain"]);
    let WaitingFor::DiscardChoice {
        player,
        cards,
        count,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "four distinct names must leave the discard live, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(count, 1);
    // Step draw + trigger draw put two cards in hand before the discard.
    assert_eq!(hand_len(&runner), 2);
    let discarded = cards[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![discarded],
        })
        .expect("discard the prompted card");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&discarded].zone, Zone::Graveyard);
    assert_eq!(hand_len(&runner), 1, "draw a card, then discard a card");
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Five distinct names: the "unless" gate holds, so the trigger only draws.
#[test]
fn five_differently_named_lands_skips_the_discard() {
    let (runner, _) =
        run_main_phase_trigger(&["Island", "Island", "Forest", "Swamp", "Mountain", "Plains"]);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "five distinct names must skip the discard, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(hand_len(&runner), 2, "step draw + trigger draw, no discard");
    assert_eq!(
        runner.state().players[P0.0 as usize].graveyard.len(),
        0,
        "nothing was discarded"
    );
}

fn activation_runner(land_names: &[&str]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let seven = scenario
        .add_creature_from_oracle(P0, SEVEN_OF_NINE, 0, 4, SEVEN_OF_NINE_ORACLE)
        .id();
    add_lands(&mut scenario, land_names);
    let mut mana = vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])];
    mana.extend((0..6).map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])));
    scenario.with_mana_pool(P0, mana);
    scenario.with_counter(seven, CounterType::Plus1Plus1, 1);
    (scenario.build(), seven)
}

/// Six distinct names (seven lands, one duplicate): CR 602.5 refuses the
/// activation, and nothing about Seven of Nine changes.
#[test]
fn activation_refused_with_six_differently_named_lands() {
    let (mut runner, seven) = activation_runner(&[
        "Island", "Island", "Forest", "Swamp", "Mountain", "Plains", "Wastes",
    ]);
    let pt_before = {
        let obj = &runner.state().objects[&seven];
        (obj.power, obj.toughness)
    };
    let result = runner.act(GameAction::ActivateAbility {
        source_id: seven,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    let err = result.expect_err("six distinct names must refuse the activation");
    assert!(
        format!("{err:?}").contains("Activation restriction not satisfied"),
        "refused by the activation restriction, got {err:?}"
    );
    let obj = &runner.state().objects[&seven];
    assert_eq!((obj.power, obj.toughness), pt_before);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        7,
        "a refused activation pays nothing"
    );
}

/// Seven distinct names: the activation resolves and sets base P/T to 7/9 in
/// layer 7b; the +1/+1 counter (layer 7c) still applies on top → 8/10.
#[test]
fn activation_allowed_with_seven_differently_named_lands_sets_base_7_9() {
    let (mut runner, seven) = activation_runner(&[
        "Island", "Island", "Forest", "Swamp", "Mountain", "Plains", "Wastes", "Desert",
    ]);
    let outcome = runner.activate(seven, 0).resolve();
    outcome.assert_power_toughness(seven, 8, 10);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
}
