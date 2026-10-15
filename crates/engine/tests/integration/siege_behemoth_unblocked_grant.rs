//! "[As long as <cond>, ]for each <creature class> you control, you may have that
//! creature assign its combat damage as though it weren't blocked." (Siege Behemoth,
//! Zilortha, Apex of Ikoria, Ruxa, Patient Professor).
//!
//! CR 510.1c: a blocked attacker normally assigns its damage among its blockers; the
//! grant replaces that for the affected creatures only (CR 609.4). The Siege Behemoth
//! gate "as long as this creature is attacking" is live (CR 508.1k + CR 611.3a), and
//! "you" is the current controller of the source (CR 109.4 / CR 108.4).
//!
//! All cards are built from their verbatim Oracle text, so the static is parsed, not
//! hand-built. Before the parser fix the Behemoth line was an `Unrecognized` gate on
//! `SelfRef` with no modification, so no creature ever got the flag.

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CombatDamageAssignmentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SIEGE_BEHEMOTH: &str = "Hexproof\nAs long as this creature is attacking, for each creature you control, you may have that creature assign its combat damage as though it weren't blocked.";
const ZILORTHA: &str = "Reach\nFor each non-Human creature you control, you may have that creature assign its combat damage as though it weren't blocked.";
const RUXA: &str = "Whenever Ruxa enters or attacks, return target creature card with no abilities from your graveyard to your hand.\nCreatures you control with no abilities get +1/+1.\nFor each creature you control with no abilities, you may have that creature assign its combat damage as though it weren't blocked.";
const LONE_WOLF_GRANT: &str =
    "You may have this creature assign its combat damage as though it weren't blocked.";

fn flag(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].assigns_damage_as_though_unblocked
}

/// Pass priority from precombat main and declare `attackers` against P1, landing at
/// the declare-blockers prompt.
fn declare_attack(runner: &mut GameRunner, attackers: &[ObjectId]) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .iter()
                .map(|&id| (id, AttackTarget::Player(P1)))
                .collect(),
            bands: vec![],
        })
        .expect("DeclareAttackers");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected DeclareBlockers, got {:?}",
        runner.state().waiting_for
    );
}

/// Drive the combat-damage step, answering every `AssignCombatDamage` prompt:
/// `AsThoughUnblocked` when `choose(attacker)` says so and the mode is offered,
/// otherwise `Normal` with all damage on the first blocker. Returns each prompt as
/// `(attacker, offered modes)` in order.
fn run_damage(
    runner: &mut GameRunner,
    choose: impl Fn(ObjectId) -> bool,
) -> Vec<(ObjectId, Vec<CombatDamageAssignmentMode>)> {
    let mut prompts = Vec::new();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::AssignCombatDamage {
                attacker_id,
                total_damage,
                blockers,
                assignment_modes,
                ..
            } => {
                prompts.push((attacker_id, assignment_modes.clone()));
                let action = if choose(attacker_id)
                    && assignment_modes.contains(&CombatDamageAssignmentMode::AsThoughUnblocked)
                {
                    GameAction::AssignCombatDamage {
                        mode: CombatDamageAssignmentMode::AsThoughUnblocked,
                        assignments: vec![],
                        trample_damage: 0,
                        controller_damage: 0,
                    }
                } else {
                    GameAction::AssignCombatDamage {
                        mode: CombatDamageAssignmentMode::Normal,
                        assignments: vec![(blockers[0].blocker_id, total_damage)],
                        trample_damage: 0,
                        controller_damage: 0,
                    }
                };
                runner.act(action).expect("AssignCombatDamage");
            }
            WaitingFor::Priority { .. } => {
                if matches!(
                    runner.state().phase,
                    Phase::EndCombat | Phase::PostCombatMain | Phase::End
                ) {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    prompts
}

struct Board {
    runner: GameRunner,
    behemoth: ObjectId,
    bear: ObjectId,
    blocker: ObjectId,
}

/// P0: Siege Behemoth 5/5 + Bear 3/3. P1: a 1/4 blocker.
fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut b = scenario.add_creature(P0, "Siege Behemoth", 5, 5);
    b.from_oracle_text_with_keywords(&["hexproof"], SIEGE_BEHEMOTH);
    let behemoth = b.id();
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Wall", 1, 4).id();
    Board {
        runner: scenario.build(),
        behemoth,
        bear,
        blocker,
    }
}

/// T1 + T5: with the Behemoth attacking, the blocked Bear may assign as though
/// unblocked and the chosen mode sends its damage to the player, not the blocker.
#[test]
fn attacking_behemoth_lets_blocked_creature_hit_the_player() {
    let Board {
        mut runner,
        behemoth,
        bear,
        blocker,
    } = board();
    assert!(!flag(&runner, bear), "gate is closed before attacking");

    declare_attack(&mut runner, &[behemoth, bear]);
    // T5 timing: the layer pass after declare-attackers opens the gate before blockers.
    assert!(
        flag(&runner, behemoth),
        "reach guard: Behemoth itself is granted"
    );
    assert!(
        flag(&runner, bear),
        "Bear granted once Behemoth is attacking"
    );

    runner
        .declare_blockers(&[(blocker, bear)])
        .expect("DeclareBlockers");
    let life = runner.life(P1);
    let prompts = run_damage(&mut runner, |_| true);

    assert_eq!(
        prompts.len(),
        1,
        "only the blocked Bear is prompted: {prompts:?}"
    );
    assert_eq!(prompts[0].0, bear);
    assert_eq!(
        prompts[0].1,
        vec![
            CombatDamageAssignmentMode::Normal,
            CombatDamageAssignmentMode::AsThoughUnblocked
        ]
    );
    // Unblocked Behemoth (5) + Bear as though unblocked (3).
    assert_eq!(runner.life(P1), life - 8);
    assert_eq!(runner.state().objects[&blocker].damage_marked, 0);
    assert_eq!(runner.state().objects[&blocker].zone, Zone::Battlefield);
}

/// T2: gate closed (Behemoth stays home) -> the blocked Bear has no option. Paired
/// with the open-gate run in the same body so the negative cannot be an upstream
/// short-circuit.
#[test]
fn non_attacking_behemoth_grants_nothing() {
    // Reach guard: Behemoth attacking offers the mode.
    let Board {
        mut runner,
        behemoth,
        bear,
        blocker,
    } = board();
    declare_attack(&mut runner, &[behemoth, bear]);
    runner.declare_blockers(&[(blocker, bear)]).unwrap();
    let guard = run_damage(&mut runner, |_| false);
    assert!(
        guard.iter().any(|(id, modes)| *id == bear
            && modes.contains(&CombatDamageAssignmentMode::AsThoughUnblocked)),
        "reach guard: {guard:?}"
    );

    let Board {
        mut runner,
        bear,
        blocker,
        ..
    } = board();
    declare_attack(&mut runner, &[bear]);
    assert!(!flag(&runner, bear), "Behemoth not attacking: gate closed");
    runner.declare_blockers(&[(blocker, bear)]).unwrap();
    let life = runner.life(P1);
    let prompts = run_damage(&mut runner, |_| true);
    assert!(
        prompts.is_empty(),
        "single blocker, no grant: auto-assigned, {prompts:?}"
    );
    assert_eq!(runner.life(P1), life, "Bear's damage goes to the blocker");
    assert_eq!(runner.state().objects[&blocker].damage_marked, 3);
}

/// CR 609.4a: a second source of the same permission (a creature with its own
/// "you may have this creature assign ..." static) is independent of the closed gate.
#[test]
fn independent_self_grant_is_unaffected_by_closed_gate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut b = scenario.add_creature(P0, "Siege Behemoth", 5, 5);
    b.from_oracle_text_with_keywords(&["hexproof"], SIEGE_BEHEMOTH);
    let mut w = scenario.add_creature(P0, "Lone Wolf", 3, 3);
    w.from_oracle_text(LONE_WOLF_GRANT);
    let wolf = w.id();
    let plain = scenario.add_creature(P0, "Bear", 3, 3).id();
    scenario.add_creature(P1, "Wall", 1, 4);
    let mut runner = scenario.build();
    declare_attack(&mut runner, &[wolf, plain]);
    assert!(flag(&runner, wolf), "own static still grants it");
    assert!(!flag(&runner, plain), "closed Behemoth gate grants nothing");
}

/// T3: "you control" binds to the Behemoth's controller, live (CR 109.4). The
/// owner-controlled attack is the reach guard; the same Behemoth controlled by P1
/// since before combat (CR 506.4 forbids changing control while it attacks)
/// grants P1's creature, not its owner's.
#[test]
fn behemoth_grant_follows_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut b = scenario.add_creature(P0, "Siege Behemoth", 5, 5);
    b.from_oracle_text_with_keywords(&["hexproof"], SIEGE_BEHEMOTH);
    let behemoth = b.id();
    let mine = scenario.add_creature(P0, "Bear", 3, 3).id();
    let theirs = scenario.add_creature(P1, "Wall", 1, 4).id();
    let mut runner = scenario.build();

    declare_attack(&mut runner, &[behemoth]);
    assert!(
        flag(&runner, mine),
        "reach guard: controller's creature granted"
    );
    assert!(
        !flag(&runner, theirs),
        "opponent's creature is not 'you control'"
    );

    // The same Behemoth (owned by P0) under P1's control since before combat —
    // CR 506.4 removes a permanent from combat when its controller changes, so a
    // control change must precede the attack for the Behemoth to attack for P1.
    // `controlled_by` sets `base_controller`, which every layer pass resets from.
    // P1 is the active player and declares it attacking P0: "you" now binds to P1.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut b = scenario.add_creature(P0, "Siege Behemoth", 5, 5);
    b.from_oracle_text_with_keywords(&["hexproof"], SIEGE_BEHEMOTH);
    b.controlled_by(P1);
    let behemoth = b.id();
    let old_controllers = scenario.add_creature(P0, "Bear", 3, 3).id();
    let new_controllers = scenario.add_creature(P1, "Wall", 1, 4).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(behemoth, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("P1 declares the Behemoth it controls as an attacker");
    let obj = &runner.state().objects[&behemoth];
    assert_eq!((obj.owner, obj.controller), (P0, P1));
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == behemoth)),
        "reach guard: the Behemoth is attacking for P1"
    );
    assert!(
        flag(&runner, new_controllers),
        "the Behemoth's controller P1's creature is granted"
    );
    assert!(
        !flag(&runner, old_controllers),
        "the owner P0's creature is not 'you control'"
    );
}

/// T4: the choice is per creature — one blocked attacker chooses as-though-unblocked,
/// the other chooses Normal, independently.
#[test]
fn each_blocked_creature_chooses_independently() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut b = scenario.add_creature(P0, "Siege Behemoth", 5, 5);
    b.from_oracle_text_with_keywords(&["hexproof"], SIEGE_BEHEMOTH);
    let behemoth = b.id();
    let bear_a = scenario.add_creature(P0, "Bear A", 3, 3).id();
    let bear_b = scenario.add_creature(P0, "Bear B", 3, 3).id();
    let wall_a = scenario.add_creature(P1, "Wall A", 1, 4).id();
    let wall_b = scenario.add_creature(P1, "Wall B", 1, 4).id();
    let mut runner = scenario.build();

    declare_attack(&mut runner, &[behemoth, bear_a, bear_b]);
    runner
        .declare_blockers(&[(wall_a, bear_a), (wall_b, bear_b)])
        .unwrap();
    let life = runner.life(P1);
    let prompts = run_damage(&mut runner, |id| id == bear_a);

    assert_eq!(prompts.len(), 2, "{prompts:?}");
    // Behemoth 5 unblocked + Bear A 3 as though unblocked.
    assert_eq!(runner.life(P1), life - 8);
    assert_eq!(runner.state().objects[&wall_a].damage_marked, 0);
    assert_eq!(runner.state().objects[&wall_b].damage_marked, 3);
}

/// T5: when the Behemoth leaves combat the gate closes at the next layer pass.
#[test]
fn gate_closes_when_behemoth_leaves_combat() {
    let Board {
        mut runner,
        behemoth,
        bear,
        ..
    } = board();
    declare_attack(&mut runner, &[behemoth, bear]);
    assert!(
        flag(&runner, bear),
        "reach guard: gate open while attacking"
    );

    // CR 506.4: removed from combat.
    {
        let state = runner.state_mut();
        state
            .combat
            .as_mut()
            .unwrap()
            .attackers
            .retain(|a| a.object_id != behemoth);
        state.layers_dirty.mark_full();
        evaluate_layers(state);
    }
    assert!(
        !flag(&runner, bear),
        "Behemoth no longer attacking: gate closed"
    );
}

/// T6 (Zilortha, ungated "non-Human" class): only non-Human creatures are granted.
#[test]
fn zilortha_grants_only_non_humans() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut z = scenario.add_creature(P0, "Zilortha, Apex of Ikoria", 6, 6);
    z.from_oracle_text_with_keywords(&["reach"], ZILORTHA);
    let zilortha = z.id();
    let mut bear = scenario.add_creature(P0, "Bear", 3, 3);
    bear.with_subtypes(vec!["Bear"]);
    let bear = bear.id();
    let mut soldier = scenario.add_creature(P0, "Soldier", 2, 2);
    soldier.with_subtypes(vec!["Human", "Soldier"]);
    let soldier = soldier.id();
    let wall_a = scenario.add_creature(P1, "Wall A", 1, 4).id();
    let wall_b = scenario.add_creature(P1, "Wall B", 1, 4).id();
    let mut runner = scenario.build();

    declare_attack(&mut runner, &[bear, soldier]);
    assert!(flag(&runner, bear), "reach guard: non-Human granted");
    assert!(!flag(&runner, soldier), "Human is not granted");
    assert!(
        flag(&runner, zilortha),
        "Zilortha is itself a non-Human creature"
    );
    runner
        .declare_blockers(&[(wall_a, bear), (wall_b, soldier)])
        .unwrap();
    let prompts = run_damage(&mut runner, |_| true);
    assert!(prompts.iter().any(|(id, _)| *id == bear), "{prompts:?}");
    assert!(prompts.iter().all(|(id, _)| *id != soldier), "{prompts:?}");
}

/// T7 (Ruxa, ungated "with no abilities" class): only an ability-less creature is
/// granted; one with an ability is not.
#[test]
fn ruxa_grants_only_creatures_with_no_abilities() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut r = scenario.add_creature(P0, "Ruxa, Patient Professor", 4, 4);
    r.from_oracle_text(RUXA);
    let vanilla = scenario.add_creature(P0, "Vanilla Bear", 2, 2).id();
    let mut flyer = scenario.add_creature(P0, "Flyer", 2, 2);
    flyer.flying();
    let flyer = flyer.id();
    scenario.add_creature(P1, "Wall", 1, 4);
    let mut runner = scenario.build();

    declare_attack(&mut runner, &[vanilla, flyer]);
    assert!(
        flag(&runner, vanilla),
        "reach guard: ability-less creature granted"
    );
    assert!(!flag(&runner, flyer), "creature with flying is not granted");
}
