//! Ironscale Hydra (Command Zone #51): "If a creature would deal combat damage
//! to this creature, prevent that damage and put a +1/+1 counter on this
//! creature."
//!
//! The Hydra is built from its verbatim Oracle text via
//! `add_creature_from_oracle`, so the parsed replacement (a self-scoped,
//! creature-sourced, combat-only prevention shield with a self +1/+1 counter
//! rider) is exercised exactly as production sees it.
//!
//! Ruling (2020-01-24): "You put only one +1/+1 counter on Ironscale Hydra per
//! creature whose combat damage is prevented, no matter how much damage would
//! have been dealt." Combat damage is proposed as one event per (source,
//! recipient) assignment, so the rider runs once per prevented event (CR 615.5).
//!
//! Out of scope: the CR 615.12 ruling ("damage that can't be prevented … you'll
//! simultaneously put a +1/+1 counter on it") — tracked as Command Zone #86.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use super::rules::run_combat;

/// Ironscale Hydra — verbatim Oracle text (MTGJSON AtomicCards).
const IRONSCALE: &str = "If a creature would deal combat damage to this creature, prevent that damage and put a +1/+1 counter on this creature.";

/// Count of +1/+1 counters on an object (CR 122.1), `0` if absent.
fn plus1_counters(runner: &GameRunner, obj: ObjectId) -> u32 {
    runner.state().objects[&obj]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life)
        .expect("player exists")
}

/// Advance to the declare-blockers prompt, passing any priority window opened
/// after attackers are declared (CR 508.2).
fn advance_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority before blockers must succeed");
            }
            ref other => panic!("expected DeclareBlockers or Priority, got {other:?}"),
        }
    }
    panic!("never reached the DeclareBlockers prompt");
}

/// Completion boundary: combat damage was dealt and the game settled back into
/// a clean priority window (no pending prompt, nothing on the stack).
fn assert_combat_settled(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "combat must settle into a priority window, got {:?}",
        state.waiting_for
    );
    assert!(
        state.stack.is_empty(),
        "nothing may remain on the stack after combat damage"
    );
    assert!(
        matches!(
            state.phase,
            Phase::CombatDamage | Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat damage must have been dealt, phase is {:?}",
        state.phase
    );
}

/// CR 615.1a + CR 615.5 + CR 510.2: blocked by one 3/3, the Hydra's combat
/// damage from the blocker is prevented and exactly ONE +1/+1 counter is put on
/// it (ruling: one counter per creature, not one per point of damage). The
/// Hydra's own combat damage to the blocker is not prevented.
#[test]
fn ironscale_single_blocker_prevents_and_adds_one_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hydra = scenario
        .add_creature_from_oracle(P0, "Ironscale Hydra", 5, 5, IRONSCALE)
        .id();
    let blocker = scenario.add_creature(P1, "Hill Giant", 3, 3).id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(hydra, AttackTarget::Player(P1))])
        .expect("P0 attacks with the Hydra");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, hydra)])
        .expect("P1 blocks the Hydra with the 3/3");
    runner.combat_damage();
    assert_combat_settled(&runner);

    // Reach guard + negative: the Hydra's 5 damage to the blocker landed (the
    // shield is scoped to damage dealt TO the Hydra), so combat damage happened.
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Graveyard,
        "the Hydra's own combat damage to the blocker must not be prevented"
    );

    assert_eq!(
        runner.state().objects[&hydra].damage_marked,
        0,
        "CR 615.1a: the blocker's 3 combat damage to the Hydra must be prevented"
    );
    assert_eq!(
        plus1_counters(&runner, hydra),
        1,
        "ruling: exactly one +1/+1 counter per creature whose damage was prevented"
    );
    assert_eq!(runner.state().objects[&hydra].zone, Zone::Battlefield);
}

/// CR 615.5 + CR 510.2: double-blocked by two 2/2s, the Hydra receives two
/// separate prevented combat-damage events and gets one counter for EACH
/// source creature (2), not one aggregate counter (1) and not one per point
/// (4). A different P0 creature dealt combat damage by a third blocker is not
/// protected (the shield is self-scoped).
#[test]
fn ironscale_gang_block_adds_one_counter_per_source_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hydra = scenario
        .add_creature_from_oracle(P0, "Ironscale Hydra", 5, 5, IRONSCALE)
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let first = scenario.add_creature(P1, "First Blocker", 2, 2).id();
    let second = scenario.add_creature(P1, "Second Blocker", 2, 2).id();
    let ally_blocker = scenario.add_creature(P1, "Hill Giant", 3, 3).id();

    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![hydra, ally],
        vec![(first, hydra), (second, hydra), (ally_blocker, ally)],
    );
    assert_combat_settled(&runner);

    // Reach guard: both 2/2 blockers took lethal from the Hydra, so both
    // 2-damage events back at the Hydra are real.
    assert_eq!(
        runner.state().objects[&first].zone,
        Zone::Graveyard,
        "reach guard: the first blocker must have died to the Hydra"
    );
    assert_eq!(
        runner.state().objects[&second].zone,
        Zone::Graveyard,
        "reach guard: the second blocker must have died to the Hydra"
    );

    assert_eq!(
        plus1_counters(&runner, hydra),
        2,
        "CR 615.5 + ruling: one +1/+1 counter per source creature whose damage was prevented"
    );
    assert_eq!(
        runner.state().objects[&hydra].damage_marked,
        0,
        "CR 615.1a: both blockers' combat damage to the Hydra must be prevented"
    );

    // Negative (recipient scope): the 3/3's combat damage to P0's other
    // creature is not prevented by the Hydra's self-scoped shield.
    assert_eq!(
        runner.state().objects[&ally].zone,
        Zone::Graveyard,
        "the Hydra's shield must not protect another creature"
    );
}

/// CR 702.19b + CR 510.1a: a 7/7 trampler blocked by the Hydra assigns lethal
/// damage (5) to the Hydra without regard to the prevention shield, and the
/// excess 2 tramples over to the defending player — that player-recipient
/// event is not prevented. The Hydra gets one counter.
#[test]
fn ironscale_blocking_trampler_takes_lethal_assignment_and_excess_hits_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let trampler = scenario.add_creature(P0, "Trampler", 7, 7).trample().id();
    let hydra = scenario
        .add_creature_from_oracle(P1, "Ironscale Hydra", 5, 5, IRONSCALE)
        .id();

    let mut runner = scenario.build();
    let life_before = life_of(&runner, P1);

    run_combat(&mut runner, vec![trampler], vec![(hydra, trampler)]);
    assert_combat_settled(&runner);

    assert_eq!(
        life_of(&runner, P1) - life_before,
        -2,
        "CR 702.19b: lethal (5) is assigned to the Hydra; only the excess 2 tramples over \
         and the player-recipient damage is not prevented"
    );
    assert_eq!(
        runner.state().objects[&trampler].damage_marked,
        5,
        "the Hydra's own combat damage to the trampler must land"
    );
    assert_eq!(
        runner.state().objects[&hydra].damage_marked,
        0,
        "CR 615.1a: the trampler's assigned combat damage to the Hydra must be prevented"
    );
    assert_eq!(
        plus1_counters(&runner, hydra),
        1,
        "ruling: one +1/+1 counter for the one creature whose damage was prevented"
    );
}

/// CR 615.1a: the shield prevents only damage dealt TO the Hydra — its own
/// unblocked combat damage to a player is dealt in full and adds no counter.
#[test]
fn ironscale_unblocked_damage_to_player_is_not_prevented() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hydra = scenario
        .add_creature_from_oracle(P0, "Ironscale Hydra", 5, 5, IRONSCALE)
        .id();

    let mut runner = scenario.build();

    // Structural reach guard: the shield is installed, self-scoped.
    assert!(
        runner.state().objects[&hydra]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|def| def.event == ReplacementEvent::DamageDone
                && def.valid_card == Some(TargetFilter::SelfRef)),
        "the Hydra must carry its self-scoped DamageDone prevention shield"
    );

    let life_before = life_of(&runner, P1);
    run_combat(&mut runner, vec![hydra], vec![]);
    assert_combat_settled(&runner);

    assert_eq!(
        life_of(&runner, P1) - life_before,
        -5,
        "the Hydra's combat damage to the defending player must not be prevented"
    );
    assert_eq!(
        plus1_counters(&runner, hydra),
        0,
        "no damage was prevented, so no counter is put on the Hydra"
    );
}

/// CR 615.1a: noncombat damage is still dealt — from a creature source (the
/// "combat damage" scope refuses it) and from a noncreature source (the "a
/// creature" source filter and the combat scope both refuse it). The same game
/// then proceeds to combat, where the shield is live: the blocker's 2 combat
/// damage (lethal on top of the 3 already marked, were it dealt) is prevented
/// and one counter is added.
#[test]
fn ironscale_noncombat_damage_is_dealt_then_combat_damage_is_prevented() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );

    let hydra = scenario
        .add_creature_from_oracle(P0, "Ironscale Hydra", 5, 5, IRONSCALE)
        .id();
    // P0's own sources: the shield is controller-agnostic ("a creature"), and
    // P0 holds priority on its own turn.
    let pinger = scenario
        .add_creature_from_oracle(
            P0,
            "Pinger",
            1,
            1,
            "{T}: This creature deals 1 damage to any target.",
        )
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Shock Probe",
            true,
            "This spell deals 2 damage to any target.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // (a) Noncombat damage from a creature source.
    let ping_index = runner.state().objects[&pinger]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("the pinger exposes its activated ability");
    let outcome = runner
        .activate(pinger, ping_index)
        .target_objects(&[hydra])
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the ping must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        runner.state().objects[&hydra].damage_marked,
        1,
        "noncombat damage from a creature is not combat damage and must be dealt"
    );
    assert_eq!(plus1_counters(&runner, hydra), 0);

    // (b) Noncombat damage from a noncreature source.
    let outcome = runner.cast(shock).target_objects(&[hydra]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the spell must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        runner.state().objects[&hydra].damage_marked,
        3,
        "noncombat damage from a noncreature spell must be dealt"
    );
    assert_eq!(plus1_counters(&runner, hydra), 0);

    // (c) Same game: combat damage from a creature is prevented — the shield
    // is live, so the negatives above are not vacuous.
    run_combat(&mut runner, vec![hydra], vec![(blocker, hydra)]);
    assert_combat_settled(&runner);

    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Graveyard,
        "reach guard: the blocker took the Hydra's combat damage"
    );
    assert_eq!(
        runner.state().objects[&hydra].damage_marked,
        3,
        "CR 615.1a: the blocker's combat damage must be prevented (no new damage marked)"
    );
    assert_eq!(plus1_counters(&runner, hydra), 1);
    assert_eq!(
        runner.state().objects[&hydra].zone,
        Zone::Battlefield,
        "with 3 damage marked, an unprevented 2 would have been lethal"
    );
}

/// CR 615.5 provenance: two Hydras fight each other. Each is the other's "a
/// creature" source; each shield prevents only the damage dealt to its own host
/// and puts its counter on its own host — a cross-bound rider would put two
/// counters on one Hydra and none on the other.
#[test]
fn ironscale_two_hydras_each_rider_binds_its_own_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario
        .add_creature_from_oracle(P0, "Ironscale Hydra", 5, 5, IRONSCALE)
        .id();
    let defender = scenario
        .add_creature_from_oracle(P1, "Ironscale Hydra", 5, 5, IRONSCALE)
        .id();

    let mut runner = scenario.build();

    run_combat(&mut runner, vec![attacker], vec![(defender, attacker)]);
    assert_combat_settled(&runner);

    for (label, hydra) in [("attacking", attacker), ("blocking", defender)] {
        assert_eq!(
            runner.state().objects[&hydra].damage_marked,
            0,
            "the {label} Hydra's shield must prevent the other Hydra's combat damage"
        );
        assert_eq!(
            plus1_counters(&runner, hydra),
            1,
            "the {label} Hydra's rider must put exactly one counter on its own host"
        );
        assert_eq!(runner.state().objects[&hydra].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&hydra].power,
            Some(6),
            "the {label} Hydra is 6/6 with its counter"
        );
    }
}
