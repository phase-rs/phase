//! CR 115.6: a spell whose LATER instruction prints "up to one target" may be
//! cast with that target chosen or declined, and its earlier instruction still
//! happens either way (CR 608.2c: instructions are followed in written order).
//!
//! Driven through `GameScenario`/`GameRunner` casting with the verbatim Oracle
//! text, so each assertion is a resolution delta.

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GOLD_RUSH_ORACLE: &str = "Create a Treasure token. Until end of turn, up to one target creature gets +2/+2 for each Treasure you control.";

const NEW_HERO_ORACLE: &str = "Target player draws X cards. Create a token that's a copy of up to one target creature with mana value X or less.";

const TREASURE_ORACLE: &str = "{T}, Sacrifice this artifact: Add one mana of any color.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn assert_resolved_to_priority(outcome: &CastOutcome) {
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(outcome.state().stack.is_empty(), "the spell fully resolved");
}

fn treasures_controlled(outcome: &CastOutcome, player: PlayerId) -> usize {
    outcome
        .state()
        .objects
        .values()
        .filter(|object| {
            object.zone == Zone::Battlefield
                && object.controller == player
                && object.card_types.subtypes.iter().any(|s| s == "Treasure")
        })
        .count()
}

fn power_toughness(outcome: &CastOutcome, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = &outcome.state().objects[&id];
    (object.power, object.toughness)
}

// ---------------------------------------------------------------------------
// Gold Rush
// ---------------------------------------------------------------------------

/// A 2/2 on P0's side, one pre-existing Treasure (so the new token makes two),
/// and Gold Rush in hand with mana to cast it.
fn gold_rush_cast(choose_target: bool) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario
        .add_artifact_from_oracle(P0, "Treasure", TREASURE_ORACLE)
        .with_subtypes(vec!["Treasure"]);
    let gold_rush = scenario
        .add_spell_to_hand_from_oracle(P0, "Gold Rush", true, GOLD_RUSH_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let mut pool = floating_mana(1, ManaType::Green);
    pool.extend(floating_mana(1, ManaType::Colorless));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    let cast = runner.cast(gold_rush);
    let cast = if choose_target {
        cast.target_object(bear)
    } else {
        cast
    };
    (cast.resolve(), bear)
}

#[test]
fn gold_rush_with_a_chosen_creature_creates_a_treasure_and_pumps_it() {
    let (outcome, bear) = gold_rush_cast(true);
    assert_resolved_to_priority(&outcome);
    assert_eq!(
        treasures_controlled(&outcome, P0),
        2,
        "one Treasure created"
    );
    // CR 608.2h: the count is taken after the Treasure is created: 2 Treasures.
    assert_eq!(power_toughness(&outcome, bear), (Some(6), Some(6)));
}

#[test]
fn gold_rush_with_the_target_declined_still_creates_a_treasure() {
    let (outcome, bear) = gold_rush_cast(false);
    assert_resolved_to_priority(&outcome);
    assert_eq!(
        treasures_controlled(&outcome, P0),
        2,
        "one Treasure created"
    );
    assert_eq!(
        power_toughness(&outcome, bear),
        (Some(2), Some(2)),
        "the declined pump affects no creature"
    );
}

// ---------------------------------------------------------------------------
// Here Comes a New Hero!
// ---------------------------------------------------------------------------

/// P1 (the target player) has a library; P0 controls a mana value 2 creature;
/// Here Comes a New Hero! is cast with X = 2.
fn new_hero_cast(choose_creature: bool) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Lib 1", "Lib 2", "Lib 3", "Lib 4"]);
    let hero = scenario
        .add_creature(P0, "Hero", 2, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Here Comes a New Hero!", false, NEW_HERO_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
        })
        .id();
    let mut pool = floating_mana(1, ManaType::Blue);
    pool.extend(floating_mana(4, ManaType::Colorless));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    let cast = runner.cast(spell).x(2).target_player(P1);
    let cast = if choose_creature {
        cast.target_object(hero)
    } else {
        cast
    };
    (cast.resolve(), hero)
}

fn hero_tokens(outcome: &CastOutcome) -> usize {
    outcome
        .state()
        .objects
        .values()
        .filter(|object| {
            object.zone == Zone::Battlefield && object.is_token && object.name == "Hero"
        })
        .count()
}

#[test]
fn new_hero_with_a_chosen_creature_draws_and_copies_it() {
    let (outcome, _) = new_hero_cast(true);
    assert_resolved_to_priority(&outcome);
    outcome.assert_hand_drawn(P1, 2);
    outcome.assert_hand_drawn(P0, 0);
    assert_eq!(
        hero_tokens(&outcome),
        1,
        "a token copy of the chosen creature"
    );
}

#[test]
fn new_hero_with_the_creature_declined_still_draws() {
    let (outcome, hero) = new_hero_cast(false);
    assert_resolved_to_priority(&outcome);
    outcome.assert_hand_drawn(P1, 2);
    outcome.assert_hand_drawn(P0, 0);
    assert_eq!(
        hero_tokens(&outcome),
        0,
        "no copy without a chosen creature"
    );
    outcome.assert_zone(&[hero], Zone::Battlefield);
}
