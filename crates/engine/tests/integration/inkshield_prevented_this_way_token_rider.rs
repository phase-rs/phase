//! The prevented-this-way rider class (#8777): a prevention shield's leading
//! "for each 1 damage prevented this way, <effect>" rider must actually fire
//! against the referent the RIDER names — the parent's chosen target when the
//! rider anaphorically refers to it ("that creature"/"it"), or the shield's
//! own untargeted scope when the rider does not (Inkshield's token payoff).
//! Class members exercised here: Inkshield (untargeted, non-anaphoric —
//! H-12), Test of Faith (targeted, `Next(N)` shield — H-1/H-3), Brace for
//! Impact (targeted, `All` shield, combat-batch drain — H-2).
//!
//! Oracle text under test (verified against Scryfall 2026-09-12):
//!   Inkshield: "Prevent all combat damage that would be dealt to you this
//!     turn. For each 1 damage prevented this way, create a 2/1 white and
//!     black Inkling creature token with flying."
//!   Test of Faith: "Prevent the next 3 damage that would be dealt to target
//!     creature this turn. For each 1 damage prevented this way, put a +1/+1
//!     counter on that creature."
//!   Brace for Impact: "Prevent all damage that would be dealt to target
//!     multicolored creature this turn. For each 1 damage prevented this way,
//!     put a +1/+1 counter on that creature."
//!
//! CR 615 (prevention) + CR 615.5 (prevented-this-way follow-up) +
//! CR 510.2 (simultaneous combat damage) + CR 111.1 (token creation) +
//! CR 615.7 (`Next(N)` depletion shields) + CR 122.1a (+1/+1 counters) +
//! CR 704.5g (lethal damage SBA).

use engine::game::combat::AttackTarget;
use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const INKSHIELD_TEXT: &str = "Prevent all combat damage that would be dealt to you this turn. \
     For each 1 damage prevented this way, create a 2/1 white and black Inkling creature token \
     with flying.";

/// Cast Inkshield from P0's hand on P0's own pre-combat main, then flip the
/// active player to P1 so P1's combat runs into the turn-scoped shield.
/// Mirrors `comeuppance.rs`'s `cast_comeuppance_then_p1_turn`.
fn cast_inkshield_then_p1_turn(
    scenario_setup: impl FnOnce(&mut GameScenario),
) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let inkshield = scenario
        .add_spell_to_hand_from_oracle(P0, "Inkshield", true, INKSHIELD_TEXT)
        .id();
    scenario_setup(&mut scenario);

    let mut runner = scenario.build();
    runner.cast(inkshield).resolve();
    runner.state_mut().active_player = P1;
    runner
}

fn run_combat(
    runner: &mut engine::game::scenario::GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
) {
    let mut attacked = false;
    for _ in 0..400 {
        match runner.state().phase {
            Phase::EndCombat | Phase::PostCombatMain => break,
            _ => {}
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. } if !attacked => {
                attacked = true;
                let a = if player == P1 {
                    attacks.to_vec()
                } else {
                    vec![]
                };
                if runner.declare_attackers(&a).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                // The attacker is unblocked in every prompt: this fixture's
                // whole point is that its combat damage reaches P0 and is
                // prevented there.
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

fn inkling_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P0 && o.name.contains("Inkling"))
        .count()
}

/// #8777 — a single unblocked 3/3 attacker: all 3 combat damage is prevented
/// AND three 2/1 flying Inklings are created (CR 615.5 + CR 510.2).
#[test]
fn inkshield_prevents_combat_damage_and_creates_one_inkling_per_damage() {
    let mut attacker_id = None;
    let mut runner = cast_inkshield_then_p1_turn(|sc| {
        attacker_id = Some(sc.add_creature(P1, "Raging Bear", 3, 3).id());
    });
    let attacker = attacker_id.unwrap();
    let p0_life_before = runner.life(P0);

    // Reach-guard: the shield really is installed before combat.
    assert!(
        runner
            .state()
            .pending_damage_replacements
            .iter()
            .any(|r| r.shield_kind.is_shield()),
        "reach guard: Inkshield's prevention shield must be installed before combat"
    );

    runner.advance_to_combat();
    run_combat(&mut runner, &[(attacker, AttackTarget::Player(P0))]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "Inkshield prevents all combat damage dealt to its controller"
    );
    assert_eq!(
        inkling_count(&runner),
        3,
        "CR 615.5: one 2/1 Inkling per 1 damage prevented this way (3 prevented)"
    );
}

const TEST_OF_FAITH_TEXT: &str = "Prevent the next 3 damage that would be dealt to target \
     creature this turn. For each 1 damage prevented this way, put a +1/+1 counter on that \
     creature.";

const BRACE_FOR_IMPACT_TEXT: &str = "Prevent all damage that would be dealt to target \
     multicolored creature this turn. For each 1 damage prevented this way, put a +1/+1 \
     counter on that creature.";

/// Hand-built noncombat damage ability, mirroring
/// `awe_strike_prevention.rs`'s `damage_ability` helper: the spell under test
/// is cast through the real pipeline; this is unrelated scaffolding to
/// deliver damage afterward, not a `resolve_top` shortcut for the spell.
fn noncombat_damage_ability(
    source_id: ObjectId,
    target: TargetRef,
    amount: i32,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source_id,
        P1,
    )
}

fn plus1plus1_counters(runner: &engine::game::scenario::GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn damage_marked(runner: &engine::game::scenario::GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .map(|o| o.damage_marked)
        .unwrap_or(0)
}

/// #8777 (H-1) — Test of Faith: real cast, targeted, `Next(3)` depletion
/// shield (CR 615.7), per-event drain. This is the maintainer-named
/// regression: pre-fix, the `PutCounter { target: ParentTarget }` rider is
/// detached into `runtime_execute` with an empty `targets` vector, so
/// `ParentTarget` resolves to nothing and zero counters are put on. Step T0
/// measures this RED at `BASE_SHA`.
///
/// CR 615.7 (Next(3) depletion shield) + CR 615.5 (one counter per 1 damage
/// prevented) + CR 122.1a (+1/+1 counters) + CR 608.2c (the rider's "that
/// creature" anaphor names the parent's chosen target).
#[test]
fn test_of_faith_puts_one_counter_per_damage_prevented() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let attacker = scenario.add_creature(P1, "Attacker", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test of Faith", true, TEST_OF_FAITH_TEXT)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[bear]).resolve();

    // Reach-guards: the cast resolved cleanly to a priority window, and the
    // shield installed on the chosen creature.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "reach guard: Test of Faith must resolve to a clean priority window, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        !outcome.state().objects[&bear]
            .replacement_definitions
            .as_slice()
            .is_empty(),
        "reach guard: the prevention shield must be installed on the targeted bear"
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &noncombat_damage_ability(attacker, TargetRef::Object(bear), 3),
        &mut events,
    )
    .expect("3 noncombat damage from the attacker resolves");

    assert_eq!(
        damage_marked(&runner, bear),
        0,
        "CR 615.7: the Next(3) shield must prevent all 3 damage"
    );
    assert_eq!(
        plus1plus1_counters(&runner, bear),
        3,
        "CR 615.5: one +1/+1 counter per 1 damage prevented (3 prevented) — the rider must \
         resolve against the parent's chosen target, not an empty referent set"
    );
}

/// #8777 (H-3, negative) — Test of Faith: no damage is ever dealt, so zero
/// counters go on. Mandatory positive pairing: the same test asserts the
/// shield WAS installed and its `runtime_execute` names the bear, so the
/// zero cannot be satisfied by "the spell fizzled" (foot-gun 6).
#[test]
fn test_of_faith_puts_no_counters_when_no_damage_is_prevented() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test of Faith", true, TEST_OF_FAITH_TEXT)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[bear]).resolve();

    // Mandatory positive pairing: the shield was installed AND its
    // runtime_execute names the bear.
    let shield = outcome.state().objects[&bear]
        .replacement_definitions
        .as_slice()
        .iter()
        .find(|r| r.shield_kind.is_shield())
        .expect("reach guard: the prevention shield must be installed on the bear");
    let rider = shield
        .runtime_execute
        .as_ref()
        .expect("reach guard: the shield must carry a runtime_execute rider");
    assert_eq!(
        rider.targets,
        vec![TargetRef::Object(bear)],
        "reach guard: the installed rider must name the bear as its referent"
    );

    // Advance to the end step without ever dealing damage.
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        plus1plus1_counters(&runner, bear),
        0,
        "no damage was prevented, so zero +1/+1 counters must be put on"
    );
}

/// #8777 (H-2) — Brace for Impact: real cast, targeted (`ColorCount {GE,2}`),
/// `All` shield, combat-batch aggregate drain. A multicolored 1/1 blocks a
/// 3/3 attacker: all 3 damage is prevented, 3 +1/+1 counters go on, and the
/// blocker survives SBAs (CR 704.5g).
///
/// CR 510.2 (simultaneous combat damage) + CR 615.5 + CR 704.5g.
#[test]
fn brace_for_impact_puts_one_counter_per_combat_damage_prevented() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mut blocker_builder = scenario.add_creature(P0, "Multicolored Blocker", 1, 1);
    blocker_builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::White],
        generic: 0,
    });
    let blocker = blocker_builder.id();
    let attacker = scenario.add_creature(P1, "Attacker", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Brace for Impact", true, BRACE_FOR_IMPACT_TEXT)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[blocker]).resolve();

    // Reach-guard: the cast did NOT halt on TargetSelection — the
    // ColorCount{GE,2} slot accepted the two-color blocker.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "reach guard: Brace for Impact must resolve past target selection onto a clean \
         priority window, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        !outcome.state().objects[&blocker]
            .replacement_definitions
            .as_slice()
            .is_empty(),
        "reach guard: the prevention shield must be installed on the targeted blocker"
    );

    runner.state_mut().active_player = P1;
    let mut attacked = false;
    let mut blocked = false;
    for _ in 0..200 {
        match runner.state().phase {
            Phase::EndCombat | Phase::PostCombatMain => break,
            _ => {}
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. } if !attacked => {
                attacked = true;
                let a = if player == P1 {
                    vec![(attacker, AttackTarget::Player(P0))]
                } else {
                    vec![]
                };
                if runner.declare_attackers(&a).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { player, .. } if !blocked => {
                blocked = true;
                let b = if player == P0 {
                    vec![(blocker, attacker)]
                } else {
                    vec![]
                };
                if runner.declare_blockers(&b).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();
    // Reach-guard: the loop reached BOTH declaration prompts. This exists for
    // the two assertions below that pass TRIVIALLY when no combat damage step
    // ran — `damage_marked == 0` and the SBA-survival check are both satisfied
    // by a board where the attacker never attacked. (The `== 3` counter
    // assertion needs no such guard: it reads 0 and fails outright on that
    // board, so it is the load-bearing positive.)
    //
    // Scope, stated precisely: this proves the prompts were REACHED, not that
    // `declare_attackers`/`declare_blockers` succeeded — each flag is set
    // before its call, which may error and break. The counter assertion is
    // what covers that residue. Note also that merely observing the attacker
    // still alive with its printed power — which this guard replaced — would
    // establish nothing, being equally true of a run that never reached
    // `DeclareAttackers` at all.
    assert!(
        attacked && blocked,
        "reach guard: combat must have reached both declaration prompts \
         (attacked={attacked}, blocked={blocked})"
    );

    assert_eq!(
        damage_marked(&runner, blocker),
        0,
        "Brace for Impact's All shield must prevent all combat damage dealt to the blocker"
    );
    assert_eq!(
        plus1plus1_counters(&runner, blocker),
        3,
        "CR 615.5: one +1/+1 counter per 1 damage prevented (3 prevented)"
    );
    assert!(
        runner.state().objects.contains_key(&blocker),
        "CR 704.5g: the blocker must survive SBAs (0 damage marked, no lethal check trips)"
    );
}
