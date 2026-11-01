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

const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your \
control, it creates twice that many of those tokens instead.\nIf an effect would put one or \
more counters on a permanent you control, it puts twice that many of those counters on that \
permanent instead.";
const VIZIER: &str = "If one or more -1/-1 counters would be put on a creature you control, \
that many -1/-1 counters minus one are put on it instead.";

/// Answer every CR 616.1 prompt P1 is asked, taking the candidate from each
/// source of `order` in turn, until the stack is empty. Returns how many
/// prompts were answered.
fn answer_p1_orderings(
    runner: &mut engine::game::scenario::GameRunner,
    order: &[ObjectId],
) -> usize {
    let mut answered = 0usize;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                assert_eq!(player, P1, "the affected creature's controller orders");
                let wanted = order[answered];
                let index = candidates
                    .iter()
                    .position(|c| c.source_id == wanted)
                    .unwrap_or_else(|| panic!("no candidate from {wanted:?} in {candidates:?}"));
                answered += 1;
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("choose replacement order");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    answered
}

fn minus_counters(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Minus1Minus1).copied())
        .unwrap_or(0)
}

/// CR 616.1 + CR 616.1g: the counter event the substitution puts meets two
/// counter replacements of the creature's controller (Doubling Season and
/// Vizier of Remedies), so P1 orders them — once per creature of a sweep.
/// Doubling Season first: 2 × 2 − 1 = 3. Vizier first: (2 − 1) × 2 = 2. The
/// prompt for the first creature must not orphan the second creature's
/// damage event: both are replaced, each with its own chosen order.
#[test]
fn counter_order_inside_the_substitution_is_asked_per_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    let first = scenario.add_creature(P1, "First Wall", 0, 10).id();
    let second = scenario.add_creature(P1, "Second Wall", 0, 10).id();
    let season = scenario
        .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
        .id();
    let vizier = scenario
        .add_creature_from_oracle(P1, "Vizier of Remedies", 2, 1, VIZIER)
        .id();
    let sweep = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Wall Sweep",
            false,
            "Wall Sweep deals 2 damage to each creature your opponents control with power 0.",
        )
        .id();

    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    runner.cast(sweep).commit();

    // Doubling Season first for the first creature's prompt, Vizier first for
    // the second's.
    let answered = answer_p1_orderings(&mut runner, &[season, vizier]);
    assert_eq!(answered, 2, "one ordering prompt per creature, each once");

    let mut got = [
        minus_counters(&runner, first),
        minus_counters(&runner, second),
    ];
    got.sort_unstable();
    assert_eq!(got, [2, 3], "one creature got (2-1)×2, the other 2×2-1");
    for creature in [first, second] {
        assert_eq!(
            runner.state().objects[&creature].damage_marked,
            0,
            "replaced damage is never dealt"
        );
    }
}

/// CR 616.1 + CR 616.1g: after P1 orders Soul-Scar Mage's substitution ahead
/// of a prevention shield, the substitute's counter event meets Doubling
/// Season and Vizier of Remedies and P1 orders those too. The nested prompt is
/// surfaced, not skipped: Doubling Season first → 3 × 2 − 1 = 5; Vizier first →
/// (3 − 1) × 2 = 4.
#[test]
fn counter_order_after_choosing_the_substitution_is_still_asked() {
    for (season_first, expected) in [(true, 5u32), (false, 4u32)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mage = scenario
            .add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE)
            .id();
        let ogre = scenario
            .add_creature_from_oracle(
                P1,
                "Shielded Ogre",
                4,
                10,
                "Prevent all damage that would be dealt to Shielded Ogre.",
            )
            .id();
        let season = scenario
            .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
            .id();
        let vizier = scenario
            .add_creature_from_oracle(P1, "Vizier of Remedies", 2, 1, VIZIER)
            .id();
        let bolt = scenario.add_bolt_to_hand(P0);
        scenario.with_mana_pool(P0, red_mana());

        let mut runner = scenario.build();
        give_priority(&mut runner, P0);
        runner.cast(bolt).target_object(ogre).commit();
        let inner = if season_first { season } else { vizier };
        let answered = answer_p1_orderings(&mut runner, &[mage, inner]);
        assert_eq!(
            answered, 2,
            "substitution-vs-shield, then the counter order"
        );
        assert_eq!(
            minus_counters(&runner, ogre),
            expected,
            "season_first={season_first}"
        );
        assert_eq!(runner.state().objects[&ogre].damage_marked, 0);
    }
}

/// P1's 0/20 wall guarded by Doubling Season and Vizier of Remedies, and P0's
/// Soul-Scar Mage plus two Birds of power 3 and 2.
fn multi_source_board() -> (GameScenario, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    let bird3 = scenario
        .add_creature(P0, "Big Bird", 3, 3)
        .with_subtypes(vec!["Bird"])
        .id();
    let bird2 = scenario
        .add_creature(P0, "Small Bird", 2, 2)
        .with_subtypes(vec!["Bird"])
        .id();
    let wall = scenario.add_creature(P1, "Great Wall", 0, 20).id();
    let season = scenario
        .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
        .id();
    scenario.add_creature_from_oracle(P1, "Vizier of Remedies", 2, 1, VIZIER);
    (scenario, bird3, bird2, wall, season)
}

/// CR 616.1g + CR 120.1: several sources each deal damage equal to their own
/// power to one creature (`DamageSource::EachTarget`, Allies at Last). Each
/// source's damage is substituted, and each substitute's counter event asks
/// P1 for the Doubling Season / Vizier order. The first prompt must not orphan
/// the second source: Doubling Season first both times → (2·3 − 1) + (2·2 − 1).
#[test]
fn targeted_sources_each_ask_their_counter_order() {
    let (mut scenario, bird3, bird2, wall, season) = multi_source_board();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Allies at Last",
            false,
            "Up to two target creatures you control each deal damage equal to their power to \
             target creature an opponent controls.",
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    runner
        .cast(spell)
        .target_objects(&[bird3, bird2, wall])
        .commit();

    let answered = answer_p1_orderings(&mut runner, &[season, season]);
    assert_eq!(answered, 2, "one ordering prompt per source, each once");
    assert_eq!(minus_counters(&runner, wall), 5 + 3);
    assert_eq!(runner.state().objects[&wall].damage_marked, 0);
}

/// CR 616.1g + CR 120.1: the filter-source form ("each Bird you control deals
/// damage equal to its power", `Effect::EachSourceDealsDamage`) — same
/// expectation as the targeted form above.
#[test]
fn filtered_sources_each_ask_their_counter_order() {
    let (mut scenario, _bird3, _bird2, wall, season) = multi_source_board();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Bird Volley",
            false,
            "Each Bird you control deals damage equal to its power to target creature an \
             opponent controls.",
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    runner.cast(spell).target_object(wall).commit();

    let answered = answer_p1_orderings(&mut runner, &[season, season]);
    assert_eq!(answered, 2, "one ordering prompt per source, each once");
    assert_eq!(minus_counters(&runner, wall), 5 + 3);
    assert_eq!(runner.state().objects[&wall].damage_marked, 0);
}

/// CR 608.2c + CR 616.1g: pausing the sweep behind the first creature's
/// counter prompt keeps the rest of the sweep AND runs the instruction after it
/// ("Draw a card.") exactly once — the pause stashes only the remaining
/// recipients, because `resolve_ability_chain` parks the parent's own tail.
#[test]
fn sweep_tail_runs_once_after_the_counter_prompts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_ORACLE);
    let first = scenario.add_creature(P1, "First Wall", 0, 10).id();
    let second = scenario.add_creature(P1, "Second Wall", 0, 10).id();
    let season = scenario
        .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
        .id();
    scenario.add_creature_from_oracle(P1, "Vizier of Remedies", 2, 1, VIZIER);
    scenario.with_library_top(P0, &["L1", "L2", "L3"]);
    let sweep = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Wall Sweep",
            false,
            "Wall Sweep deals 2 damage to each creature your opponents control with power 0. \
             Draw a card.",
        )
        .id();

    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    let hand_before = runner.state().players[0].hand.len();
    runner.cast(sweep).commit();
    let answered = answer_p1_orderings(&mut runner, &[season, season]);

    assert_eq!(answered, 2, "one ordering prompt per creature");
    assert_eq!(minus_counters(&runner, first), 3);
    assert_eq!(minus_counters(&runner, second), 3);
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before - 1 + 1,
        "the spell left the hand and the trailing draw ran once"
    );
}
