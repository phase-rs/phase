//! Wakka, Devoted Guardian: "Whenever Wakka deals combat damage to a player,
//! destroy up to one target artifact that player controls and put a +1/+1
//! counter on Wakka. / Blitzball Captain — At the beginning of your end step,
//! if a counter was put on Wakka this turn, put a +1/+1 counter on each other
//! creature you control."
//!
//! CR 122.1 + CR 603.4: the end-step trigger's intervening "if" reads whether
//! any counter, of any kind and from any source, was put on Wakka this turn.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const WAKKA: &str = "Reach, trample\nWhenever Wakka deals combat damage to a player, destroy up to one target artifact that player controls and put a +1/+1 counter on Wakka.\nBlitzball Captain — At the beginning of your end step, if a counter was put on Wakka this turn, put a +1/+1 counter on each other creature you control.";
const WITHER_SPELL: &str = "Put a -1/-1 counter on target creature.";

struct Board {
    runner: GameRunner,
    wakka: ObjectId,
    bears: ObjectId,
    spell: ObjectId,
}

/// P0: Wakka (4/4), Grizzly Bears, and a sorcery putting a -1/-1 counter on a
/// target creature, with {B} to cast it.
fn board() -> Board {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let wakka = s
        .add_creature(P0, "Wakka, Devoted Guardian", 4, 4)
        .from_oracle_text_with_keywords(&["Reach", "Trample"], WAKKA)
        .id();
    let bears = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Wither Probe", false, WITHER_SPELL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    s.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, spell, false, vec![])],
    );
    Board {
        runner: s.build(),
        wakka,
        bears,
        spell,
    }
}

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

/// Advance to P0's end step (declaring no attackers if combat has not been
/// declared yet) and resolve whatever triggers there.
fn end_step(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner.declare_attackers(&[]).expect("no attackers");
        runner.advance_to_end_step();
    }
    assert_eq!(runner.state().phase, Phase::End, "reach guard: end step");
    runner.advance_until_stack_empty();
}

/// CR 603.4: Wakka connects, its own trigger puts a +1/+1 counter on it, and at
/// the end step each OTHER creature you control gets a +1/+1 counter.
#[test]
fn wakka_counter_from_combat_rewards_each_other_creature() {
    let mut b = board();
    b.runner.advance_to_combat();
    b.runner
        .declare_attackers(&[(b.wakka, AttackTarget::Player(P1))])
        .expect("Wakka attacks");
    b.runner.combat_damage();
    b.runner.advance_until_stack_empty();
    assert_eq!(
        counters(&b.runner, b.wakka, CounterType::Plus1Plus1),
        1,
        "reach guard: the combat-damage trigger put a counter on Wakka"
    );
    end_step(&mut b.runner);
    assert_eq!(counters(&b.runner, b.bears, CounterType::Plus1Plus1), 1);
    assert_eq!(
        counters(&b.runner, b.wakka, CounterType::Plus1Plus1),
        1,
        "Wakka is not an \"other\" creature"
    );
}

/// CR 122.1: "a counter" is any kind of counter from any source: a -1/-1
/// counter a sorcery puts on Wakka satisfies it.
#[test]
fn wakka_minus_counter_from_a_spell_satisfies_the_condition() {
    let mut b = board();
    let (spell, wakka) = (b.spell, b.wakka);
    b.runner.cast(spell).target_object(wakka).resolve();
    assert_eq!(
        counters(&b.runner, wakka, CounterType::Minus1Minus1),
        1,
        "reach guard"
    );
    end_step(&mut b.runner);
    assert_eq!(counters(&b.runner, b.bears, CounterType::Plus1Plus1), 1);
}

/// CR 603.4: a counter put on another creature, or no counter at all, does not
/// satisfy "a counter was put on Wakka this turn".
#[test]
fn wakka_without_a_counter_on_it_does_nothing() {
    for counter_on_bears in [false, true] {
        let mut b = board();
        let (spell, bears) = (b.spell, b.bears);
        if counter_on_bears {
            b.runner.cast(spell).target_object(bears).resolve();
            assert_eq!(counters(&b.runner, bears, CounterType::Minus1Minus1), 1);
        }
        end_step(&mut b.runner);
        assert_eq!(
            counters(&b.runner, bears, CounterType::Plus1Plus1),
            0,
            "counter_on_bears={counter_on_bears}"
        );
    }
}
