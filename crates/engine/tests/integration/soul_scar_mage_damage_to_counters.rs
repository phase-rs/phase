//! Soul-Scar Mage — "If a source you control would deal noncombat damage to a
//! creature an opponent controls, put that many -1/-1 counters on that creature
//! instead."
//!
//! CR 614.1a + CR 614.6: a REPLACEMENT (not a CR 615 prevention). The damage
//! event never happens — no marked damage, no `DamageDealt`, no
//! `DamagePrevented` — and the counters are put on the would-be recipient.
//! Only noncombat damage from sources its controller controls, and only to
//! creatures an opponent controls, is replaced.
//!
//! Prowess is omitted from the fixture Oracle text: its trigger on casting the
//! test's instant is irrelevant to the replacement under test.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const SOUL_SCAR_ORACLE: &str = "If a source you control would deal noncombat damage to a \
creature an opponent controls, put that many -1/-1 counters on that creature instead.";

fn red_mana() -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])]
}

fn give_priority(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// CR 614.6 + CR 704.5q: your burn spell at an opponent's creature puts that
/// many -1/-1 counters on it instead of dealing damage; +1/+1 counters already
/// on it annihilate with the new -1/-1 counters.
#[test]
fn burn_at_opponents_creature_becomes_minus_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    let ogre = scenario
        .add_creature(P1, "Hill Ogre", 4, 4)
        .with_plus_counters(1)
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);
    scenario.with_mana_pool(P0, red_mana());

    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    let outcome = runner.cast(bolt).target_object(ogre).resolve();

    assert_eq!(
        outcome.damage_marked(ogre),
        0,
        "replaced damage is never dealt"
    );
    // 3 -1/-1 counters arrive; one annihilates with the +1/+1 counter.
    outcome.assert_counters(ogre, CounterType::Plus1Plus1, 0);
    outcome.assert_counters(ogre, CounterType::Minus1Minus1, 2);
    assert_eq!(outcome.power_toughness(ogre), (2, 2));
    assert!(
        !outcome
            .events()
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "a replacement is not a prevention: no DamagePrevented"
    );
    assert!(
        !outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::DamageDealt { target, .. }
                if *target == engine::types::ability::TargetRef::Object(ogre)
        )),
        "no damage is dealt to the creature"
    );
}

/// CR 614.1a: the recipient scope is "a creature an opponent controls" — a
/// player is not replaced.
#[test]
fn burn_at_opponent_player_deals_normal_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    let bolt = scenario.add_bolt_to_hand(P0);
    scenario.with_mana_pool(P0, red_mana());

    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    let outcome = runner.cast(bolt).target_player(P1).resolve();

    assert_eq!(outcome.life_delta(P1), -3);
}

/// CR 614.1a: your own creature is not "a creature an opponent controls".
#[test]
fn burn_at_own_creature_deals_normal_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    let golem = scenario.add_creature(P0, "Own Golem", 4, 4).id();
    let bolt = scenario.add_bolt_to_hand(P0);
    scenario.with_mana_pool(P0, red_mana());

    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    let outcome = runner.cast(bolt).target_object(golem).resolve();

    assert_eq!(outcome.damage_marked(golem), 3);
    outcome.assert_counters(golem, CounterType::Minus1Minus1, 0);
}

/// CR 614.1a: only sources Soul-Scar's controller controls are replaced — an
/// opponent's burn spell at your creature deals damage normally.
#[test]
fn opponents_burn_at_your_creature_deals_normal_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    let golem = scenario.add_creature(P0, "Own Golem", 4, 4).id();
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(P1, red_mana());

    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    let outcome = runner.cast(bolt).target_object(golem).resolve();

    assert_eq!(outcome.damage_marked(golem), 3);
    outcome.assert_counters(golem, CounterType::Minus1Minus1, 0);
}

/// CR 120.2a + CR 510.2: combat damage is not noncombat damage — an attacker
/// you control blocked by an opponent's creature deals ordinary damage.
#[test]
fn combat_damage_is_not_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    let attacker = scenario.add_creature(P0, "Attacker", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Blocker", 1, 5).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attacker");
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("advance to blockers");
    }
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("declare blocker");
    let outcome = runner.combat_damage();

    assert_eq!(outcome.damage_marked(blocker), 3);
    outcome.assert_counters(blocker, CounterType::Minus1Minus1, 0);
}

/// CR 614.6 + CR 614.5: a spell that deals damage to several creatures at once
/// produces one damage event per creature, and the replacement applies to each
/// of them — every opponent creature gets its own counters.
#[test]
fn damage_to_each_opponent_creature_is_replaced_per_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    let ogre = scenario.add_creature(P1, "Hill Ogre", 4, 4).id();
    let bear = scenario.add_creature(P1, "Bear", 3, 3).id();
    let sweep = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Opposing Sweep",
            false,
            "Opposing Sweep deals 2 damage to each creature your opponents control.",
        )
        .id();

    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    let outcome = runner.cast(sweep).resolve();

    for creature in [ogre, bear] {
        assert_eq!(outcome.damage_marked(creature), 0);
        outcome.assert_counters(creature, CounterType::Minus1Minus1, 2);
    }
    assert_eq!(outcome.power_toughness(ogre), (2, 2));
    assert_eq!(outcome.power_toughness(bear), (1, 1));
}

/// CR 614.1a + CR 608.2c: "that many" is the replaced damage amount even when
/// the damage is dealt by a resolving triggered ability whose own trigger event
/// carries a different amount (gaining 3 life triggers 1 damage → 1 counter).
#[test]
fn trigger_damage_uses_replaced_amount_not_trigger_event_amount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    scenario.add_creature_from_oracle(
        P0,
        "Lifelinked Pinger",
        1,
        1,
        "Whenever you gain life, Lifelinked Pinger deals 1 damage to each creature your \
         opponents control.",
    );
    let ogre = scenario.add_creature(P1, "Hill Ogre", 4, 4).id();
    let heal = scenario
        .add_spell_to_hand_from_oracle(P0, "Big Heal", true, "You gain 3 life.")
        .id();

    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    runner.cast(heal).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    let ogre_obj = state.objects.get(&ogre).expect("ogre exists");
    assert_eq!(ogre_obj.damage_marked, 0, "replaced damage is never dealt");
    assert_eq!(
        ogre_obj
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0),
        1,
        "one damage replaced → one -1/-1 counter (not the 3 life gained)"
    );
}

/// CR 615.12: "damage can't be prevented" stops prevention effects only. Soul-
/// Scar Mage's substitution is a replacement (CR 614.1a), so it still applies.
#[test]
fn damage_cant_be_prevented_does_not_stop_the_substitution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    scenario.add_enchantment_from_oracle(P1, "No Mercy Field", "Damage can't be prevented.");
    let ogre = scenario.add_creature(P1, "Hill Ogre", 4, 4).id();
    let bolt = scenario.add_bolt_to_hand(P0);
    scenario.with_mana_pool(P0, red_mana());

    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    let outcome = runner.cast(bolt).target_object(ogre).resolve();

    assert_eq!(outcome.damage_marked(ogre), 0);
    outcome.assert_counters(ogre, CounterType::Minus1Minus1, 3);
}

/// CR 616.1: the replacement and a prevention effect both apply to the damage
/// to P1's creature, so P1 (the affected object's controller) chooses the order.
/// Prevention first: the damage is prevented, so there is no damage event left
/// to replace — no counters. Substitution first: the damage event no longer
/// exists for the prevention — the counters are put.
#[test]
fn affected_controller_orders_substitution_and_prevention() {
    for (prevent_first, expected_counters) in [(true, 0u32), (false, 3u32)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
        let ogre = scenario
            .add_creature_from_oracle(
                P1,
                "Shielded Ogre",
                4,
                4,
                "Prevent all damage that would be dealt to Shielded Ogre.",
            )
            .id();
        let bolt = scenario.add_bolt_to_hand(P0);
        scenario.with_mana_pool(P0, red_mana());

        let mut runner = scenario.build();
        give_priority(&mut runner, P0);
        runner.cast(bolt).target_object(ogre).commit();
        for _ in 0..8 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ) {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass priority");
        }
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected a CR 616.1 ordering choice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, P1, "the affected object's controller chooses");
        assert_eq!(candidates.len(), 2);
        let index = candidates
            .iter()
            .position(|c| c.description.to_lowercase().contains("prevent") == prevent_first)
            .expect("both replacements are offered");
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("choose replacement order");
        runner.advance_until_stack_empty();

        let ogre_obj = runner.state().objects.get(&ogre).expect("ogre exists");
        assert_eq!(ogre_obj.damage_marked, 0);
        assert_eq!(
            ogre_obj
                .counters
                .get(&CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            expected_counters,
            "prevent_first = {prevent_first}"
        );
    }
}
