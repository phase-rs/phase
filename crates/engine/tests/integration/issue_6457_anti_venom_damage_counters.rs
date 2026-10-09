//! Issue #6457 — Anti-Venom, Horrifying Healer: "If damage would be dealt
//! to Anti-Venom, prevent that damage and put that many +1/+1 counters on
//! him" must prevent damage dealt to him and grow him instead.
//!
//! https://github.com/phase-rs/phase/issues/6457
//!
//! Reported bug: Anti-Venom gains no counters from damage and dies to
//! combat damage. Built from the FULL verbatim Oracle text via
//! `add_creature_from_oracle` so the parsed replacement is exercised
//! exactly as production sees it.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

/// Anti-Venom, Horrifying Healer — full verbatim Oracle text (Scryfall
/// oracle id 3c7bafe9-80cd-48d0-bcae-e7910c9fb83b).
const ANTI_VENOM_ORACLE: &str = "When Anti-Venom enters, if he was cast, return target creature card from your graveyard to the battlefield.\nIf damage would be dealt to Anti-Venom, prevent that damage and put that many +1/+1 counters on him.";

/// Count of +1/+1 counters on an object (CR 122.1), `0` if absent.
fn plus1_counters(runner: &GameRunner, obj: engine::types::ObjectId) -> u32 {
    runner.state().objects[&obj]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
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

/// CR 615.1 + CR 122.1: a blocked Anti-Venom takes 3 combat damage from the
/// blocker — the replacement prevents all of it and puts three +1/+1
/// counters on him instead. He survives at 8/8 with no marked damage.
#[test]
fn anti_venom_prevents_combat_damage_and_grows() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let anti_venom = scenario
        .add_creature_from_oracle(P0, "Anti-Venom, Horrifying Healer", 5, 5, ANTI_VENOM_ORACLE)
        .id();
    // 3/3 blocker: deals 3 to Anti-Venom, takes 5 back and dies.
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 3, 3).id();

    let mut runner = scenario.build();

    assert_eq!(
        plus1_counters(&runner, anti_venom),
        0,
        "Anti-Venom starts with no +1/+1 counters"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(anti_venom, AttackTarget::Player(P1))])
        .expect("P0 attacks with Anti-Venom");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, anti_venom)])
        .expect("P1 blocks Anti-Venom with the 3/3");
    runner.combat_damage();

    // Reach-guard: combat damage happened — the 3/3 blocker took Anti-Venom's
    // 5 and died, so the block (and its 3 back at Anti-Venom) is real.
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Graveyard,
        "reach-guard: the blocker must have taken lethal combat damage from Anti-Venom"
    );

    assert_eq!(
        plus1_counters(&runner, anti_venom),
        3,
        "CR 615.1: the prevented 3 damage must become three +1/+1 counters"
    );
    assert_eq!(
        runner.state().objects[&anti_venom].damage_marked,
        0,
        "CR 615.6: prevented damage is not marked on Anti-Venom"
    );
    assert_eq!(
        runner.state().objects[&anti_venom].zone,
        Zone::Battlefield,
        "Anti-Venom must survive the blocked attack"
    );
}

/// CR 615.1 + CR 510.2: gang-blocked Anti-Venom takes two separate 2-damage
/// instances in one combat-damage batch — each prevented instance must grow
/// him (four +1/+1 counters total), exercising the batch rider path rather
/// than the single-instance path.
#[test]
fn anti_venom_gang_block_prevents_each_instance_and_grows() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let anti_venom = scenario
        .add_creature_from_oracle(P0, "Anti-Venom, Horrifying Healer", 5, 5, ANTI_VENOM_ORACLE)
        .id();
    let first = scenario.add_creature(P1, "First Blocker", 2, 2).id();
    let second = scenario.add_creature(P1, "Second Blocker", 2, 2).id();

    let mut runner = scenario.build();

    // `run_combat` drives the interactive CR 510.1c damage assignment for the
    // two blockers; both 2/2s take lethal from Anti-Venom's 5.
    run_combat(
        &mut runner,
        vec![anti_venom],
        vec![(first, anti_venom), (second, anti_venom)],
    );

    // Reach-guard: both 2/2 blockers took Anti-Venom's 5 and died, so both
    // 2-damage instances back at Anti-Venom are real.
    assert_eq!(
        runner.state().objects[&first].zone,
        Zone::Graveyard,
        "reach-guard: the first blocker must have died to Anti-Venom"
    );
    assert_eq!(
        runner.state().objects[&second].zone,
        Zone::Graveyard,
        "reach-guard: the second blocker must have died to Anti-Venom"
    );

    assert_eq!(
        plus1_counters(&runner, anti_venom),
        4,
        "CR 615.1 + CR 510.2: each prevented 2-damage instance must grow Anti-Venom"
    );
    assert_eq!(
        runner.state().objects[&anti_venom].damage_marked,
        0,
        "CR 615.6: no combat damage may be marked on Anti-Venom"
    );
    assert_eq!(
        runner.state().objects[&anti_venom].zone,
        Zone::Battlefield,
        "Anti-Venom must survive the gang block"
    );
}

/// CR 615.1 + CR 509.1: roles reversed and damage lethal — Anti-Venom
/// blocks a 7/7 attacker. All 7 must be prevented (seven +1/+1 counters);
/// unprevented, the marked 7 would be lethal on his 5 toughness.
/// This is the closest shape to the report's "died from combat damage".
#[test]
fn anti_venom_blocking_prevents_lethal_damage_and_grows() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let anti_venom = scenario
        .add_creature_from_oracle(P0, "Anti-Venom, Horrifying Healer", 5, 5, ANTI_VENOM_ORACLE)
        .id();
    // 7/7 attacker: deals Anti-Venom a would-be-lethal 7, takes 5 back and
    // survives (proving Anti-Venom's own damage was dealt, so the combat
    // damage step fully executed).
    let attacker = scenario.add_creature(P1, "Carnage Tyrant", 7, 7).id();

    let mut runner = scenario.build();

    // Hand the turn to P1 so the 7/7 can attack into Anti-Venom.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 attacks with the 7/7");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(anti_venom, attacker)])
        .expect("P0 blocks with Anti-Venom");
    runner.combat_damage();

    // Reach-guard: the 7/7 survived with Anti-Venom's 5 marked on it, so the
    // combat damage step executed and the 7 back at Anti-Venom is real.
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Battlefield,
        "reach-guard: the 7/7 attacker survives"
    );
    assert_eq!(
        runner.state().objects[&attacker].damage_marked,
        5,
        "reach-guard: Anti-Venom dealt its 5 combat damage to the attacker"
    );

    assert_eq!(
        plus1_counters(&runner, anti_venom),
        7,
        "CR 615.1: the prevented 7 damage must become seven +1/+1 counters"
    );
    assert_eq!(
        runner.state().objects[&anti_venom].damage_marked,
        0,
        "CR 615.6: prevented damage is not marked on Anti-Venom"
    );
    assert_eq!(
        runner.state().objects[&anti_venom].zone,
        Zone::Battlefield,
        "Anti-Venom must survive the would-be-lethal block"
    );
}

/// CR 615.1 + CR 120.4 (noncombat damage uses the same prevention window):
/// a Lightning Bolt at Anti-Venom is prevented and grows him by three.
#[test]
fn anti_venom_prevents_noncombat_damage_and_grows() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let anti_venom = scenario
        .add_creature_from_oracle(P0, "Anti-Venom, Horrifying Healer", 5, 5, ANTI_VENOM_ORACLE)
        .id();
    // P0's own bolt aimed at his Anti-Venom: the damage source's controller
    // is irrelevant to the replacement, and P0 holds priority on his turn.
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();

    runner.cast(bolt).target_objects(&[anti_venom]).resolve();

    assert_eq!(
        plus1_counters(&runner, anti_venom),
        3,
        "CR 615.1: the prevented 3 noncombat damage must become three +1/+1 counters"
    );
    assert_eq!(
        runner.state().objects[&anti_venom].damage_marked,
        0,
        "CR 615.6: prevented damage is not marked on Anti-Venom"
    );
    assert_eq!(
        runner.state().objects[&anti_venom].zone,
        Zone::Battlefield,
        "Anti-Venom must survive the bolt"
    );
}
