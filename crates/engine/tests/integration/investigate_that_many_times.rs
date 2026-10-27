//! "Investigate that many times" — the repeat count is the triggering event's
//! amount, driven through the real trigger pipeline.
//!
//! CR 701.16a + CR 111.10f: to investigate is to create a Clue token.
//! CR 608.2c + CR 608.2h: "that many" reads the triggering event's amount, and
//! the repeat count is determined once when the effect applies — Clues created
//! by an earlier iteration cannot change it.
//! CR 510.2: Piper Wright's combat damage is dealt in the combat damage step;
//! its trigger reads the damage Piper itself dealt, not the combat total.
//! CR 122.6 + CR 603.2c: Lonis's trigger names a counter kind, so it fires once
//! per +1/+1 placement event and reads that event's counter count.
//!
//! Before the fix, `strip_repeat_count_suffix` split off only the last word
//! before "times" ("many"), the count grammar rejected it, and the clause fell
//! through to the bare `investigate` arm — exactly ONE Clue regardless of the
//! amount. Every "exactly N Clues" assertion below (N > 1) flips to 1 on revert.
//!
//! Oracle text is verbatim from the card database. "Put two +1/+1 counters on
//! target creature." is a fixture driver (one `CounterAdded` event with count 2,
//! as in `l02_bbfu2_batched_counter_triggers`); Unexpected Fangs is the real
//! card.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::KeywordKind;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::run_combat;

const PIPER: &str = "Whenever Piper Wright deals combat damage to a player, investigate that many times. (To investigate, create a Clue token. It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")\n\
Whenever you sacrifice a Clue, put a +1/+1 counter on target creature you control.";

const LONIS: &str = "Evolve (Whenever a creature you control enters, if that creature has greater power or toughness than this creature, put a +1/+1 counter on this creature.)\n\
Whenever one or more +1/+1 counters are put on Lonis, investigate that many times.\n\
Whenever you sacrifice a Clue, put a +1/+1 counter on another target creature you control.";

// Fixture driver: a single +1/+1 placement event of magnitude 2.
const PUT_TWO_PLUS: &str = "Put two +1/+1 counters on target creature.";

// Real card: a +1/+1 AND a lifelink counter in one resolution — two
// distinct-kind `CounterAdded` events.
const UNEXPECTED_FANGS: &str = "Put a +1/+1 counter and a lifelink counter on target creature.";

/// Clue tokens on the battlefield under `player`'s control.
fn clue_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == player && o.name == "Clue")
        .count()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

/// Completion boundary: every trigger resolved and the game is back at priority.
fn assert_settled(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the trigger resolves"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 510.2 + CR 608.2h: an unblocked 3-power Piper Wright deals 3 combat damage
/// to a player and investigates that many (3) times. Revert-failing: the
/// pre-fix parse investigates once (1 Clue).
#[test]
fn piper_wright_investigates_that_many_times_for_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let piper = scenario
        .add_creature_from_oracle(P0, "Piper Wright, Publick Reporter", 3, 2, PIPER)
        .id();
    let mut runner = scenario.build();

    assert_eq!(clue_count(&runner, P0), 0, "precondition: no Clues");
    let life_before = life(&runner, P1);

    run_combat(&mut runner, vec![piper], vec![]);
    runner.advance_until_stack_empty();

    // Reach-guard: the combat damage actually landed.
    assert_eq!(
        life(&runner, P1),
        life_before - 3,
        "reach-guard: Piper dealt 3 combat damage to P1"
    );
    assert_eq!(
        clue_count(&runner, P0),
        3,
        "Piper dealt 3 combat damage → investigate that many (3) times; the \
         pre-fix parse investigates once (1 Clue)"
    );
    assert_settled(&runner);
}

/// CR 510.2 + CR 603.2c: "that many" is the damage Piper itself dealt, not the
/// combat total. Piper (power 2) and a 3-power attacker both connect: Piper's
/// trigger makes exactly 2 Clues (the aggregate reading would make 5; the
/// pre-fix parse makes 1).
#[test]
fn piper_wright_counts_only_its_own_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let piper = scenario
        .add_creature_from_oracle(P0, "Piper Wright, Publick Reporter", 2, 2, PIPER)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 3, 3).id();
    let mut runner = scenario.build();

    let life_before = life(&runner, P1);

    run_combat(&mut runner, vec![piper, bears], vec![]);
    runner.advance_until_stack_empty();

    // Reach-guard: both damage events happened (2 + 3).
    assert_eq!(
        life(&runner, P1),
        life_before - 5,
        "reach-guard: both attackers dealt combat damage to P1"
    );
    assert_eq!(
        clue_count(&runner, P0),
        2,
        "Piper's trigger reads its own 2 damage, not the 5-damage combat total"
    );
    assert_settled(&runner);
}

/// CR 122.6 + CR 608.2h: two +1/+1 counters put on Lonis in one event →
/// investigate that many (2) times. Revert-failing: the pre-fix parse
/// investigates once (1 Clue).
#[test]
fn lonis_investigates_that_many_times_for_counters_placed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lonis = scenario
        .add_creature(P0, "Lonis, Genetics Expert", 1, 2)
        .from_oracle_text_with_keywords(&["Evolve"], LONIS)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Put Two Counters", true, PUT_TWO_PLUS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    assert_eq!(clue_count(&runner, P0), 0, "precondition: no Clues");

    runner.cast(spell).target_object(lonis).resolve();
    runner.advance_until_stack_empty();

    // Reach-guard: both counters landed on Lonis in the placement.
    assert_eq!(
        runner.state().objects[&lonis]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(2),
        "reach-guard: two +1/+1 counters were put on Lonis"
    );
    assert_eq!(
        clue_count(&runner, P0),
        2,
        "two +1/+1 counters put on Lonis → investigate that many (2) times; the \
         pre-fix parse investigates once (1 Clue)"
    );
    assert_settled(&runner);
}

/// CR 122.6 + CR 603.2c: Lonis's trigger names +1/+1 counters, so a mixed-kind
/// placement (one +1/+1 and one lifelink counter, Unexpected Fangs) counts only
/// the +1/+1 counter: exactly 1 Clue. A reading that summed the whole
/// multi-kind batch would make 2. `lonis_investigates_that_many_times_for_counters_placed`
/// is the positive control that the count is read at all.
#[test]
fn lonis_counts_only_plus_one_counters_of_a_mixed_placement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lonis = scenario
        .add_creature(P0, "Lonis, Genetics Expert", 1, 2)
        .from_oracle_text_with_keywords(&["Evolve"], LONIS)
        .id();
    let fangs = scenario
        .add_spell_to_hand_from_oracle(P0, "Unexpected Fangs", true, UNEXPECTED_FANGS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(fangs).target_object(lonis).resolve();
    runner.advance_until_stack_empty();

    // Reach-guards: the placement carried two kinds, one counter each.
    let counters = &runner.state().objects[&lonis].counters;
    assert_eq!(
        counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "reach-guard: one +1/+1 counter landed on Lonis"
    );
    assert_eq!(
        counters
            .get(&CounterType::Keyword(KeywordKind::Lifelink))
            .copied(),
        Some(1),
        "reach-guard: one lifelink counter landed on Lonis (multi-kind placement)"
    );
    assert_eq!(
        clue_count(&runner, P0),
        1,
        "only the one +1/+1 counter counts toward \"that many\""
    );
    assert_settled(&runner);
}
