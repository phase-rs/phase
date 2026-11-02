//! Canker Abomination — "As this creature enters, choose an opponent. This
//! creature enters with a -1/-1 counter on it for each creature that player
//! controls." (Creature — Treefolk Horror, {2}{B/G}{B/G}, 6/6, EVE/UMA)
//!
//! CR 614.12a: the as-enters opponent choice is made before the permanent
//! enters, so the counter count already binds to the chosen opponent's
//! board when `ObjectCount{Creature, controller: ScopedPlayer}` resolves.
//!
//! Regression guard for the `scoped_player` binding in
//! `extract_etb_counters` (replacement.rs): without reading
//! `event.enter_with_choices` for `ChosenAttribute::Player`, the count falls
//! back to the controller's own creature count instead of the chosen
//! opponent's.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CANKER_ABOMINATION: &str = "As this creature enters, choose an opponent. \
                                  This creature enters with a -1/-1 counter on it \
                                  for each creature that player controls.";

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

/// Stage enough black and green mana to pay {2}{B/G}{B/G} via the auto-payer.
fn bg_pool() -> Vec<ManaUnit> {
    mana(ManaType::Black, 4)
        .into_iter()
        .chain(mana(ManaType::Green, 4))
        .collect()
}

/// Cast Canker Abomination for P0, pre-answering the as-enters opponent choice
/// with `opponent` (a `PlayerId`), and return the resolution outcome plus the
/// Abomination's object id.
fn cast_canker_abomination(opponent: PlayerId, p1_creatures: usize) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    // P1 (the opponent) controls `p1_creatures` creatures.
    for i in 0..p1_creatures {
        scenario.add_creature(P1, &format!("Opponent Creature {i}"), 3, 3);
    }

    let canker = scenario
        .add_creature_to_hand_from_oracle(P0, "Canker Abomination", 6, 6, CANKER_ABOMINATION)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::BlackGreen, ManaCostShard::BlackGreen],
        })
        .id();
    scenario.with_mana_pool(P0, bg_pool());

    let mut runner = scenario.build();

    // Pre-answer the as-enters ChoiceType::Opponent prompt so `resolve()` drives
    // straight through entry to completion.
    let outcome = runner
        .cast(canker)
        .choose_option(&opponent.0.to_string())
        .resolve();

    assert_eq!(
        outcome.zone_of(canker),
        Zone::Battlefield,
        "Canker Abomination must resolve onto the battlefield"
    );
    (outcome, canker)
}

/// Cast Canker Abomination for P0 in a two-player game, assert that entry
/// pauses on the as-enters `ChoiceType::Opponent` prompt offering only P1
/// (CR 614.12a: the choice is made before the permanent enters), answer it
/// with P1, and let entry complete.
fn cast_and_answer_opponent_prompt(runner: &mut GameRunner, canker: ObjectId) {
    runner
        .act(GameAction::CastSpell {
            object_id: canker,
            card_id: runner.state().objects[&canker].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Canker Abomination");
    runner.advance_until_stack_empty();

    // The as-enters replacement must pause on the fronted opponent choice.
    let WaitingFor::NamedChoice {
        player: choice_player,
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "as-enters must pause on ChoiceType::Opponent, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(choice_player, P0, "prompt actor must be controller P0");
    assert!(
        matches!(
            choice_type,
            engine::types::ability::ChoiceType::Opponent { .. }
        ),
        "the fronted choice must be Choose(Opponent), got {:?}",
        choice_type
    );
    assert_eq!(
        options,
        vec![P1.0.to_string()],
        "only P1 is a legal opponent"
    );
    assert_eq!(
        runner.state().objects[&canker].zone,
        Zone::Stack,
        "CR 614.12a: permanent must still be on the stack while choice is pending"
    );

    // Answer the choice and let entry complete.
    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choose opponent P1");
    runner.advance_until_stack_empty();
}

/// Positive: P1 controls 2 creatures; P0 chooses P1 → Abomination enters with
/// 2 -1/-1 counters.
#[test]
fn chosen_opponent_creatures_determine_minus_one_counters() {
    let (outcome, canker) = cast_canker_abomination(P1, 2);
    outcome.assert_counters(canker, CounterType::Minus1Minus1, 2);
    outcome.assert_power_toughness(canker, 4, 4);
}

/// Negative polarity: the chosen opponent P1 controls no creatures while the
/// controller P0 controls two → Abomination enters with no counters. The
/// controller's creatures make this discriminate the chosen-player binding: a
/// count that fell back to the controller's board would put two counters on it.
/// The opponent prompt is asserted and answered explicitly, so the test cannot
/// pass without reaching the as-enters choice.
#[test]
fn no_opponent_creatures_no_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..2 {
        scenario.add_creature(P0, &format!("Controller Creature {i}"), 3, 3);
    }

    let canker = scenario
        .add_creature_to_hand_from_oracle(P0, "Canker Abomination", 6, 6, CANKER_ABOMINATION)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::BlackGreen, ManaCostShard::BlackGreen],
        })
        .id();
    scenario.with_mana_pool(P0, bg_pool());

    let mut runner = scenario.build();
    cast_and_answer_opponent_prompt(&mut runner, canker);

    let abomination = &runner.state().objects[&canker];
    assert_eq!(
        abomination.zone,
        Zone::Battlefield,
        "Canker Abomination must resolve onto the battlefield"
    );
    assert_eq!(
        abomination
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0),
        0,
        "the chosen opponent controls no creatures, so no -1/-1 counters"
    );
    assert_eq!(
        (abomination.power, abomination.toughness),
        (Some(6), Some(6))
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "entry must complete back to Priority"
    );
}

/// The choice genuinely binds to the chosen opponent: with P1 controlling 2
/// creatures and another hypothetical P3 controlling 5, choosing P1 yields 2
/// counters (not 5). Uses a 3-player game so a non-first opponent is selectable.
#[test]
fn chooses_chosen_opponent_not_first_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    const P2: PlayerId = PlayerId(2);
    scenario.with_life(P2, 20);

    // P1 controls 2 creatures; P2 controls 5. Choosing P2 must yield 5 counters.
    for i in 0..2 {
        scenario.add_creature(P1, &format!("P1 Creature {i}"), 3, 3);
    }
    for i in 0..5 {
        scenario.add_creature(P2, &format!("P2 Creature {i}"), 3, 3);
    }

    let canker = scenario
        .add_creature_to_hand_from_oracle(P0, "Canker Abomination", 6, 6, CANKER_ABOMINATION)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::BlackGreen, ManaCostShard::BlackGreen],
        })
        .id();
    scenario.with_mana_pool(P0, bg_pool());

    let mut runner = scenario.build();
    let outcome = runner
        .cast(canker)
        .choose_option(&P2.0.to_string())
        .resolve();

    assert_eq!(
        outcome.zone_of(canker),
        Zone::Battlefield,
        "Canker Abomination must resolve onto the battlefield"
    );
    outcome.assert_counters(canker, CounterType::Minus1Minus1, 5);
    outcome.assert_power_toughness(canker, 1, 1);
}

/// The as-enters opponent choice is surfaced as a genuine `ChoiceType::Opponent`
/// prompt before entry completes (CR 614.12a). This guards against a silent
/// auto-first-opponent regression that would make the counter count depend on
/// the wrong player's board.
#[test]
fn surfaces_opponent_choice_before_entry_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    scenario.add_creature(P1, "Opponent Creature", 3, 3);

    let canker = scenario
        .add_creature_to_hand_from_oracle(P0, "Canker Abomination", 6, 6, CANKER_ABOMINATION)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::BlackGreen, ManaCostShard::BlackGreen],
        })
        .id();
    scenario.with_mana_pool(P0, bg_pool());

    let mut runner = scenario.build();
    cast_and_answer_opponent_prompt(&mut runner, canker);

    let abomination = &runner.state().objects[&canker];
    assert_eq!(
        abomination
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0),
        1,
        "after answering the choice, Canker Abomination enters with 1 -1/-1 counter"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ),
        "entry must complete back to Priority"
    );
}
