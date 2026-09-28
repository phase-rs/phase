//! Target-instance identity for player zone counts (PR #9280 review).
//!
//! NOTE ON /card-test's verbatim-Oracle-text rule: these tests use SYNTHETIC
//! cards. A corpus query over all 35,804 cards in `data/card-data.json`
//! finds zero printed cards pairing a separately announced recipient with a
//! separate count-source target (MED1, MED3): every printed Explicit count
//! (Recurring Insight, Jeska's Will, Gerrard Capashen, Borrowed Knowledge,
//! Rousing Refrain) pairs with a Controller primary or no player target at
//! all. It likewise finds zero damage triggers pairing an event-bound
//! recipient with an explicit "target player's/opponent's" zone count (MED2).
//! Verbatim Oracle text cannot cover these branches; the sentences below are
//! genuine Oracle grammar exercising the production possessive parsers, the
//! same way Recurring Insight's sentence exercises the opponent branch.

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::run_combat;

const P2: PlayerId = PlayerId(2);

// Synthetic Oracle sentence: Tibalt word order ("damage equal to <count> to
// <recipient>") with the count and the recipient as SEPARATE instances of
// "target".
const SEPARATE_INSTANCES_ORACLE: &str =
    "Separate Instances deals damage equal to the number of cards in target opponent's hand to target player.";

// Synthetic Sword-shape trigger with an explicit count binding.
const EXPLICIT_TRIGGER_COUNT_ORACLE: &str = "Whenever equipped creature deals combat damage to a player, Test Blade deals damage to that player equal to the number of cards in target player's hand.";

// Synthetic Cut-Your-Losses word order ("mills <count>") with the count and
// the recipient as SEPARATE instances of "target".
const SEPARATE_MILL_INSTANCES_ORACLE: &str =
    "Target player mills cards equal to the number of cards in target opponent's hand.";

/// CR 601.2c + CR 115.3 (MED1): recipient and count source are separate
/// instances of "target", announced separately. Three divergent hands prove
/// the count reads the count-source's choice, not the recipient's:
/// recipient P1 holds 2, count-source P2 holds 5, caster P0 holds 1 —
/// damage must be 5. Reverting the slot gate (suppression) announces once
/// and counts the recipient's 2 instead.
#[test]
fn explicit_count_and_recipient_are_separate_announcements() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Separate Instances", false, SEPARATE_INSTANCES_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_cards_in_hand(P0, &["Caster One"]);
    scenario.with_cards_in_hand(P1, &["Recipient One", "Recipient Two"]);
    scenario.with_cards_in_hand(
        P2,
        &[
            "Count One",
            "Count Two",
            "Count Three",
            "Count Four",
            "Count Five",
        ],
    );
    scenario.with_life(P1, 20);
    let mut runner = scenario.build();

    // Slot order is quantity-first: count source, then recipient.
    let outcome = runner.cast(spell).target_players(&[P2, P1]).resolve();

    // CR 120.3: 20 − 5 (count-source P2's hand) = 15. Counting the
    // recipient's hand instead would leave 18.
    let life = runner.state().players[P1.0 as usize].life;
    assert_eq!(
        life, 15,
        "damage must equal the count-source's hand size (5), P1 life = {life}"
    );
    // CR 115.1 + CR 601.2c: the quantity slot serves the magnitude, not
    // receipt — the count-source takes no damage.
    let source_life = runner.state().players[P2.0 as usize].life;
    assert_eq!(
        source_life, 20,
        "count-source P2 must take no damage, P2 life = {source_life}"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no further prompt after resolution, got {:?}",
        outcome.final_waiting_for()
    );
}

/// CR 115.3: the same player may be chosen once for EACH instance — naming
/// P1 for both slots is legal and counts P1's hand for both halves. Guards
/// against a "distinct targets" over-correction of the two-slot shape.
#[test]
fn same_player_may_fill_both_instances() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Separate Instances", false, SEPARATE_INSTANCES_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_cards_in_hand(P0, &["Caster One"]);
    scenario.with_cards_in_hand(P1, &["Recipient One", "Recipient Two"]);
    scenario.with_cards_in_hand(
        P2,
        &[
            "Count One",
            "Count Two",
            "Count Three",
            "Count Four",
            "Count Five",
        ],
    );
    scenario.with_life(P1, 20);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_players(&[P1, P1]).resolve();
    // 20 − 2 (P1's hand, chosen for both instances) = 18.
    let life = runner.state().players[P1.0 as usize].life;
    assert_eq!(
        life, 18,
        "same-player-both-instances must count P1's hand (2), P1 life = {life}"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no further prompt after resolution, got {:?}",
        outcome.final_waiting_for()
    );
}

/// MED2: an explicit count in a damage trigger keeps its announcement slot
/// and reads the announced player — not the event player. The damaged P1
/// holds 1 card; the announced P0 holds 4; the trigger must deal 4 (plus 1
/// combat damage). Reverting the walker guard rebinds the count to the
/// event player and deals 1 instead.
#[test]
fn explicit_trigger_count_reads_announced_player_not_event_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_cards_in_hand(P1, &["Damaged One"]);
    scenario.with_cards_in_hand(P0, &["C One", "C Two", "C Three", "C Four"]);
    scenario.with_life(P1, 20);

    let equipped_creature = scenario.add_creature(P0, "Blade Bearer", 1, 1).id();
    let blade = scenario
        .add_creature(P0, "Test Blade", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(EXPLICIT_TRIGGER_COUNT_ORACLE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), blade, equipped_creature);
    evaluate_layers(runner.state_mut());

    run_combat(&mut runner, vec![equipped_creature], vec![]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "explicit trigger count must prompt for its announcement, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
        })
        .expect("announcing P0 as the count source must be accepted");
    runner.advance_until_stack_empty();

    // 20 − 1 combat − 4 (announced P0's hand) = 15. An event-bound
    // reading would deal 1 instead of 4 → 18.
    let damaged_life = runner.state().players[P1.0 as usize].life;
    assert_eq!(
        damaged_life, 15,
        "trigger must deal the announced player's hand size (4); got {damaged_life} \
         (18 would mean it counted the damaged player's 1-card hand)"
    );
}

/// CR 601.2c + CR 115.3 + CR 701.17a (MED3): non-damage recipient selection
/// honors the primary target's distinct slot identity. Recipient P1 mills
/// exactly the count-source P2's hand size (5); the count-source's own
/// library is untouched. Reading the first player slot for the recipient
/// would mill P2 instead of P1.
#[test]
fn explicit_mill_count_reads_count_source_but_mills_recipient() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Separate Mills", false, SEPARATE_MILL_INSTANCES_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_cards_in_hand(P0, &["Caster One"]);
    scenario.with_cards_in_hand(P1, &["Recipient One", "Recipient Two"]);
    scenario.with_cards_in_hand(
        P2,
        &[
            "Count One",
            "Count Two",
            "Count Three",
            "Count Four",
            "Count Five",
        ],
    );
    scenario.with_library_top(
        P1,
        &[
            "Mill One",
            "Mill Two",
            "Mill Three",
            "Mill Four",
            "Mill Five",
            "Mill Six",
        ],
    );
    scenario.with_library_top(P2, &["Untouched One", "Untouched Two", "Untouched Three"]);
    let mut runner = scenario.build();
    let p1_library_before = runner.state().players[P1.0 as usize].library.len();
    let p1_graveyard_before = runner.state().players[P1.0 as usize].graveyard.len();
    let p2_library_before = runner.state().players[P2.0 as usize].library.len();
    let p2_graveyard_before = runner.state().players[P2.0 as usize].graveyard.len();

    // Slot order is quantity-first: count source, then recipient.
    let outcome = runner.cast(spell).target_players(&[P2, P1]).resolve();

    // CR 701.17a: P1 mills exactly 5 (count-source P2's hand).
    let p1_library = runner.state().players[P1.0 as usize].library.len();
    let p1_graveyard = runner.state().players[P1.0 as usize].graveyard.len();
    assert_eq!(
        p1_library_before - p1_library,
        5,
        "recipient P1 must mill 5 cards, library went {p1_library_before} -> {p1_library}"
    );
    assert_eq!(
        p1_graveyard - p1_graveyard_before,
        5,
        "milled cards land in P1's graveyard, went {p1_graveyard_before} -> {p1_graveyard}"
    );
    // The count-source is not the recipient: P2's library is untouched.
    let p2_library = runner.state().players[P2.0 as usize].library.len();
    let p2_graveyard = runner.state().players[P2.0 as usize].graveyard.len();
    assert_eq!(
        p2_library, p2_library_before,
        "count-source P2's library must be untouched, went {p2_library_before} -> {p2_library}"
    );
    assert_eq!(
        p2_graveyard, p2_graveyard_before,
        "count-source P2's graveyard must be untouched, went {p2_graveyard_before} -> {p2_graveyard}"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no further prompt after resolution, got {:?}",
        outcome.final_waiting_for()
    );
}
