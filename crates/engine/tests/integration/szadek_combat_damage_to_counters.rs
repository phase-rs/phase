//! Szadek, Lord of Secrets — "If Szadek would deal combat damage to a player,
//! instead put that many +1/+1 counters on Szadek and that player mills that
//! many cards."
//!
//! CR 614.1a + CR 614.6: a REPLACEMENT (not a CR 615 prevention). The combat
//! damage to the player never happens — no life loss, no `DamageDealt`, no
//! `DamagePrevented` — and the substitute events (counters on Szadek, mill for
//! the damaged player) happen instead. Combat damage to a creature and
//! noncombat damage are dealt normally. With trample, only the portion assigned
//! to the player is replaced (CR 510.1c + CR 702.19b).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CombatDamageAssignmentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SZADEK_NAME: &str = "Szadek, Lord of Secrets";
const SZADEK_ORACLE: &str = "If Szadek would deal combat damage to a player, instead put \
that many +1/+1 counters on Szadek and that player mills that many cards.";

/// Szadek 5/5 attacks P1; `blocker` (if any) blocks it. Returns the runner at
/// the combat-damage step boundary, ready for `combat_damage()`.
fn attack_with_szadek(runner: &mut GameRunner, szadek: ObjectId, blocker: Option<ObjectId>) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(szadek, AttackTarget::Player(P1))])
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
    // With no potential blocker the engine skips the declare-blockers prompt.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        let blocks: Vec<(ObjectId, ObjectId)> = blocker.map(|b| (b, szadek)).into_iter().collect();
        runner.declare_blockers(&blocks).expect("declare blockers");
    } else {
        assert!(blocker.is_none(), "expected a declare-blockers prompt");
    }
}

fn library_count(runner_state: &engine::types::game_state::GameState) -> usize {
    runner_state
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .library
        .len()
}

/// CR 614.6: unblocked, Szadek's 5 combat damage to the defending player is
/// replaced — no life loss, Szadek gets five +1/+1 counters and the damaged
/// player mills five cards.
#[test]
fn unblocked_combat_damage_becomes_counters_and_mill() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, None);
    let outcome = runner.combat_damage();

    assert_eq!(outcome.life_delta(P1), 0, "replaced damage is never dealt");
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 5);
    assert_eq!(outcome.power_toughness(szadek), (10, 10));
    outcome.assert_zone_count(P1, Zone::Graveyard, 5);
    assert_eq!(
        library_count(outcome.state()),
        2,
        "the damaged player mills five"
    );
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
            GameEvent::DamageDealt { target, .. } if *target == TargetRef::Player(P1)
        )),
        "no damage is dealt to the player"
    );
}

/// CR 614.1a: the recipient scope is "a player" — combat damage to a blocking
/// creature is dealt normally.
#[test]
fn blocked_combat_damage_to_creature_is_dealt_normally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    let wall = scenario.add_creature(P1, "Wall", 0, 7).id();
    scenario.with_library_top(P1, &["C1", "C2", "C3"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, Some(wall));
    let outcome = runner.combat_damage();

    assert_eq!(outcome.damage_marked(wall), 5);
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 0);
    assert_eq!(outcome.life_delta(P1), 0);
    outcome.assert_zone_count(P1, Zone::Graveyard, 0);
    assert_eq!(library_count(outcome.state()), 3);
}

/// CR 702.19b + CR 614.1a: a trampling Szadek blocked by a 0/2 assigns 2 to
/// the blocker (dealt normally) and 3 to the player — only the player portion
/// is replaced (3 counters, mill 3).
#[test]
fn trample_replaces_only_the_player_portion() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .trample()
        .id();
    let wall = scenario.add_creature(P1, "Wall", 0, 2).id();
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, Some(wall));
    // CR 510.1c: the trampler's controller assigns lethal (2) to the 0/2 wall
    // and the remaining 3 to the player.
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::AssignCombatDamage { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("advance to damage assignment");
    }
    runner
        .act(GameAction::AssignCombatDamage {
            mode: CombatDamageAssignmentMode::Normal,
            assignments: vec![(wall, 2)],
            trample_damage: 3,
            controller_damage: 0,
        })
        .expect("assign trample damage");
    let outcome = runner.combat_damage();

    assert_eq!(
        outcome.state().players[1].life,
        20,
        "the player portion is never dealt"
    );
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 3);
    assert_eq!(library_count(outcome.state()), 2);
    // The 0/2 wall took lethal damage and died; with the 3 milled cards, P1's
    // graveyard holds four cards.
    outcome.assert_zone_count(P1, Zone::Graveyard, 4);
}

/// CR 614.1a + CR 616.1: "that many" names the replaced damage amount for BOTH
/// substitute events. A counter modifier (Hardened Scales) changes how many
/// counters Szadek gets (5 + 1) but not how many cards the player mills (5).
#[test]
fn counter_modifier_does_not_change_mill_amount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    scenario.add_creature_from_oracle(
        P0,
        "Hardened Scales Bearer",
        0,
        1,
        "If one or more +1/+1 counters would be put on a creature you control, that many \
         plus one +1/+1 counters are put on it instead.",
    );
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, None);
    let outcome = runner.combat_damage();

    assert_eq!(outcome.life_delta(P1), 0);
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 6);
    assert_eq!(library_count(outcome.state()), 2, "mill that many = 5");
    outcome.assert_zone_count(P1, Zone::Graveyard, 5);
}

/// CR 614.1a + CR 608.2c: the mill reads the replaced damage amount, not how
/// many counters were actually put. When no counter can be put on Szadek, the
/// damaged player still mills five.
#[test]
fn mill_amount_holds_when_no_counter_can_be_put() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    scenario.add_enchantment_from_oracle(
        P1,
        "Counter Ban",
        "Counters can't be put on artifacts, creatures, enchantments, or lands.",
    );
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, None);
    let outcome = runner.combat_damage();

    assert_eq!(outcome.life_delta(P1), 0, "replaced damage is never dealt");
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 0);
    assert_eq!(library_count(outcome.state()), 2, "mill that many = 5");
    outcome.assert_zone_count(P1, Zone::Graveyard, 5);
}

/// CR 510.2 + CR 614.5: two Szadeks' combat damage events in the same batch are
/// each replaced — each Szadek gets its own counters and the player mills for
/// both events.
#[test]
fn two_szadeks_in_one_damage_batch_are_each_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    let second = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 3, 3, SZADEK_ORACLE)
        .id();
    let library: Vec<String> = (1..=10).map(|i| format!("C{i}")).collect();
    let library: Vec<&str> = library.iter().map(String::as_str).collect();
    scenario.with_library_top(P1, &library);

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");
    let outcome = runner.combat_damage();

    assert_eq!(outcome.life_delta(P1), 0);
    outcome.assert_counters(first, CounterType::Plus1Plus1, 5);
    outcome.assert_counters(second, CounterType::Plus1Plus1, 3);
    assert_eq!(library_count(outcome.state()), 2, "mill 5 + mill 3");
    outcome.assert_zone_count(P1, Zone::Graveyard, 8);
}

/// CR 616.1 + CR 510.2: Szadek's replacement and a prevention effect both apply
/// to the combat damage to P1, so their order is material (the substitution is
/// classified order-material, like the found-card replacements). The combat-
/// damage batch cannot pause for that choice; its existing policy
/// (`replace_combat_damage_batch`'s `NeedsChoice` arm) skips that source's
/// damage. The result is the prevention-first ordering: no damage, no
/// counters, no mill — never the damage being dealt, and never both the
/// substitution and the prevention.
#[test]
fn prevention_on_defending_player_leaves_no_damage_counters_or_mill() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    scenario.add_enchantment_from_oracle(
        P1,
        "Combat Ward",
        "Prevent all combat damage that would be dealt to you.",
    );
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, None);
    let outcome = runner.combat_damage();

    assert_eq!(outcome.state().players[1].life, 20);
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 0);
    assert_eq!(library_count(outcome.state()), 7);
    outcome.assert_zone_count(P1, Zone::Graveyard, 0);
}
