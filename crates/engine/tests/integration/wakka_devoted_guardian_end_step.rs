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
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WAKKA: &str = "Reach, trample\nWhenever Wakka deals combat damage to a player, destroy up to one target artifact that player controls and put a +1/+1 counter on Wakka.\nBlitzball Captain — At the beginning of your end step, if a counter was put on Wakka this turn, put a +1/+1 counter on each other creature you control.";
/// Scar ({B/R} instant).
const SCAR: &str = "Put a -1/-1 counter on target creature.";
/// Cloudshift ({W} instant).
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

struct Board {
    runner: GameRunner,
    wakka: ObjectId,
    bears: ObjectId,
    scar: ObjectId,
    cloudshift: ObjectId,
}

/// P0: Wakka (4/4), Grizzly Bears, Scar and Cloudshift in hand, and a Swamp and
/// a Plains to cast them.
fn board() -> Board {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let wakka = s
        .add_creature(P0, "Wakka, Devoted Guardian", 4, 4)
        .from_oracle_text_with_keywords(&["Reach", "Trample"], WAKKA)
        .id();
    let bears = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let scar = s
        .add_spell_to_hand_from_oracle(P0, "Scar", true, SCAR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::BlackRed],
            generic: 0,
        })
        .id();
    s.add_basic_land(P0, ManaColor::Black);
    let cloudshift = s
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    s.add_basic_land(P0, ManaColor::White);
    Board {
        runner: s.build(),
        wakka,
        bears,
        scar,
        cloudshift,
    }
}

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

/// Cast Scar on `target` and check the -1/-1 counter landed.
fn scar(b: &mut Board, target: ObjectId) {
    let scar = b.scar;
    b.runner.cast(scar).target_object(target).resolve();
    assert_eq!(
        counters(&b.runner, target, CounterType::Minus1Minus1),
        1,
        "reach guard: Scar's counter"
    );
}

/// Advance to P0's end step (declaring no attackers if combat has not been
/// declared yet) and return how many Blitzball Captain triggers are on the stack.
fn reach_end_step(runner: &mut GameRunner) -> usize {
    runner.advance_to_end_step();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner.declare_attackers(&[]).expect("no attackers");
        runner.advance_to_end_step();
    }
    assert_eq!(runner.state().phase, Phase::End, "reach guard: end step");
    runner
        .stack_names()
        .iter()
        .filter(|name| *name == "Wakka, Devoted Guardian")
        .count()
}

/// Resolve what is on the stack and check the step settled.
fn settle(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the end step settled, got {}",
        runner.waiting_for_kind()
    );
}

/// Reach P0's end step, resolve its triggers, and return how many Blitzball
/// Captain triggers went on the stack.
fn end_step(runner: &mut GameRunner) -> usize {
    let triggered = reach_end_step(runner);
    settle(runner);
    triggered
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
    assert_eq!(end_step(&mut b.runner), 1, "Blitzball Captain triggered");
    assert_eq!(counters(&b.runner, b.bears, CounterType::Plus1Plus1), 1);
    assert_eq!(
        counters(&b.runner, b.wakka, CounterType::Plus1Plus1),
        1,
        "Wakka is not an \"other\" creature"
    );
}

/// CR 122.1 + CR 603.4: "a counter" is any kind of counter from any source. A
/// -1/-1 counter Scar puts on Wakka satisfies it; one on the Bears, or none at
/// all, does not. Whether Blitzball Captain triggers is asserted directly, since a
/// wrong +1/+1 counter on the Bears would cancel with Scar's -1/-1 counter
/// (CR 704.5q). The Wakka case is this test's positive control.
#[test]
fn wakka_condition_reads_any_counter_on_wakka_only() {
    for (case, expected) in [("none", 0), ("bears", 0), ("wakka", 1)] {
        let mut b = board();
        let (wakka, bears) = (b.wakka, b.bears);
        match case {
            "bears" => scar(&mut b, bears),
            "wakka" => scar(&mut b, wakka),
            _ => {}
        }
        assert_eq!(
            end_step(&mut b.runner),
            expected as usize,
            "{case}: triggered"
        );
        assert_eq!(
            counters(&b.runner, bears, CounterType::Plus1Plus1),
            expected,
            "{case}"
        );
    }
}

/// CR 400.7: a counter put on Wakka before it is blinked was put on a different
/// object. The Wakka that returns had no counter put on it this turn, so its
/// end-step trigger does nothing.
#[test]
fn wakka_blinked_after_its_counter_does_nothing() {
    let mut b = board();
    let (wakka, cloudshift) = (b.wakka, b.cloudshift);
    scar(&mut b, wakka);
    let before = b.runner.state().objects[&wakka].incarnation;
    b.runner.cast(cloudshift).target_object(wakka).resolve();
    let returned = &b.runner.state().objects[&wakka];
    assert_ne!(
        returned.incarnation, before,
        "reach guard: Wakka returned as a new object"
    );
    assert_eq!(
        counters(&b.runner, wakka, CounterType::Minus1Minus1),
        0,
        "reach guard: the new Wakka has no counter"
    );
    assert_eq!(
        end_step(&mut b.runner),
        0,
        "Blitzball Captain does not trigger"
    );
    assert_eq!(counters(&b.runner, b.bears, CounterType::Plus1Plus1), 0);
}

/// CR 113.7a + CR 603.4: Wakka is blinked in response to its end-step trigger.
/// The ability's source is still the Wakka a counter was put on, so the
/// condition holds as the trigger resolves and the Bears get a counter.
#[test]
fn wakka_blinked_in_response_to_its_trigger_still_rewards() {
    let mut b = board();
    let (wakka, bears, cloudshift) = (b.wakka, b.bears, b.cloudshift);
    scar(&mut b, wakka);
    assert_eq!(
        reach_end_step(&mut b.runner),
        1,
        "Blitzball Captain triggered"
    );
    let before = b.runner.state().objects[&wakka].incarnation;
    b.runner.cast(cloudshift).target_object(wakka).resolve();
    let returned = &b.runner.state().objects[&wakka];
    assert!(
        returned.incarnation != before && returned.zone == Zone::Battlefield,
        "reach guard: Cloudshift returned Wakka as a new object"
    );
    settle(&mut b.runner);
    assert_eq!(counters(&b.runner, bears, CounterType::Plus1Plus1), 1);
    // CR 400.7: the returned Wakka is a new object, not the ability's source,
    // so it is one of the "other" creatures.
    assert_eq!(counters(&b.runner, wakka, CounterType::Plus1Plus1), 1);
}

/// Beast, Erudite Aerialist: "As long as you've put one or more +1/+1 counters on
/// Beast this turn, he has flying." The same self-referential counter history,
/// read by a static: only a +1/+1 counter, and only one Beast's controller put
/// on it, counts (CR 122.1). Battlegrowth ({G} instant): "Put a +1/+1 counter on
/// target creature."
#[test]
fn beast_flies_only_after_you_put_a_plus_one_counter_on_it() {
    const BEAST: &str = "As long as you've put one or more +1/+1 counters on Beast this turn, he has flying.\nWhenever Beast deals combat damage to a player, draw a card.";
    const BATTLEGROWTH: &str = "Put a +1/+1 counter on target creature.";
    for (case, caster, scar, flying) in [
        ("you, +1/+1", P0, false, true),
        ("opponent, +1/+1", P1, false, false),
        ("you, -1/-1", P0, true, false),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let beast = s
            .add_creature(P0, "Beast, Erudite Aerialist", 3, 3)
            .from_oracle_text(BEAST)
            .id();
        let (name, text, shard, color, kind) = if scar {
            (
                "Scar",
                SCAR,
                ManaCostShard::BlackRed,
                ManaColor::Black,
                CounterType::Minus1Minus1,
            )
        } else {
            (
                "Battlegrowth",
                BATTLEGROWTH,
                ManaCostShard::Green,
                ManaColor::Green,
                CounterType::Plus1Plus1,
            )
        };
        let spell = s
            .add_spell_to_hand_from_oracle(caster, name, true, text)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![shard],
                generic: 0,
            })
            .id();
        s.add_basic_land(caster, color);
        let mut r = s.build();
        if caster == P1 {
            r.act(GameAction::PassPriority).expect("P0 passes");
            assert!(
                matches!(r.state().waiting_for, WaitingFor::Priority { player: P1 }),
                "reach guard: P1 has priority"
            );
        }
        r.cast(spell).target_object(beast).resolve();
        assert_eq!(
            counters(&r, beast, kind),
            1,
            "reach guard ({case}): the counter"
        );
        assert!(
            r.state().stack.is_empty()
                && matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
            "{case}: settled"
        );
        assert_eq!(
            r.state().objects[&beast].has_keyword(&Keyword::Flying),
            flying,
            "{case}"
        );
    }
}
