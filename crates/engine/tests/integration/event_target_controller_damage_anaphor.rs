//! GameRunner integration regression for the "that creature's controller"
//! anaphor on ACTIVE-voice damage triggers.
//!
//! CR 120.1 is the rule this file exists to enforce: "an object that deals
//! damage is the source of that damage" — the DEALER — while the recipient is
//! the object that *receives* it. `GameEvent::DamageDealt` carries the two roles
//! in different fields (`source_id` vs `target`), and before the fix under test
//! every one of these cards read the DEALER through
//! `extract_source_from_event`. "That creature's controller" therefore resolved
//! to the attacking creature's own controller, so each card in the class
//! punished its own controller instead of its victim's.
//!
//! Every test here is deliberately **two-sided**: it asserts both that the
//! correct player was affected AND that the wrong player was not. A one-sided
//! assertion ("P1 lost life") passes on the pre-fix engine whenever the board is
//! symmetric, which is exactly the shape these regressions take.
//!
//! Coverage:
//!  1. **Flayed Nim** — `Effect::LoseLife` with a dynamic amount, exercising
//!     `TargetFilter::EventTargetController`.
//!  2. **Bellowing Fiend** — a two-clause chain that damages BOTH the
//!     recipient's controller and "you", so the pre-fix engine doubles up on one
//!     player. Inherently two-sided.
//!  3. **Maarika, Brutal Gladiator** — exercises the other new variant,
//!     `ControllerRef::EventTargetController`, through a CR 120.10 excess-damage
//!     intervening-if and a `Sacrifice` population scope.
//!
//! CR 120.1 + CR 120.10 + CR 109.4 + CR 510.1 + CR 608.2c + CR 608.2h.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FLAYED_NIM_TEXT: &str = "Whenever this creature deals combat damage to a creature, that \
                               creature's controller loses that much life.";

const BELLOWING_FIEND_TEXT: &str = "Whenever this creature deals damage to a creature, this \
                                    creature deals 3 damage to that creature's controller and 3 \
                                    damage to you.";

const MAARIKA_TEXT: &str = "Whenever Maarika deals damage to a creature, if that creature was \
                            dealt excess damage this turn, that creature's controller sacrifices \
                            a noncreature, nonland permanent.";

/// Drive from the current state through the end of combat, answering combat
/// prompts: `attacker` attacks `defend_player`, `blocker` blocks it, and every
/// other priority window is auto-passed. Mirrors the driver in
/// `weeping_angel_combat_prevention.rs`.
fn run_combat(
    runner: &mut engine::game::scenario::GameRunner,
    attacker_player: engine::types::player::PlayerId,
    attacker: engine::types::identifiers::ObjectId,
    defend_player: engine::types::player::PlayerId,
    blocker: Option<engine::types::identifiers::ObjectId>,
) {
    let mut attacked = false;
    let mut blocked = false;

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
                let attacks = if player == attacker_player {
                    vec![(attacker, AttackTarget::Player(defend_player))]
                } else {
                    vec![]
                };
                if runner.declare_attackers(&attacks).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } if !blocked => {
                blocked = true;
                let blocks = if let Some(blk) = blocker {
                    vec![(blk, attacker)]
                } else {
                    vec![]
                };
                if runner.declare_blockers(&blocks).is_err() {
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
}

/// CR 120.1 + CR 109.4: Flayed Nim's "that creature's controller loses that much
/// life" must charge the BLOCKER's controller (P1), not Flayed Nim's own
/// controller (P0).
///
/// Discriminating: before the fix, the anaphor lowered to
/// `TargetFilter::ParentTargetController`, which — with no parent target on an
/// untargeted trigger — fell through to `extract_source_from_event`, i.e. the
/// damage DEALER. P0 lost the life instead of P1. The paired `P0` assertion is
/// what makes this regression non-vacuous: asserting only "P1 lost 3" would also
/// have held on the broken engine if both players happened to lose life.
#[test]
fn flayed_nim_combat_damage_drains_the_blockers_controller_not_its_own() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    // P0's Flayed Nim: 3 power, 4 toughness so it survives the 1/1 block and the
    // trigger resolves with the creature still on the battlefield.
    let nim = scenario
        .add_creature_from_oracle(P0, "Flayed Nim", 3, 4, FLAYED_NIM_TEXT)
        .id();
    let blocker = scenario.add_creature(P1, "Blocker", 1, 1).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    run_combat(&mut runner, P0, nim, P1, Some(blocker));
    runner.advance_until_stack_empty();

    let p0_life = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .life;
    let p1_life = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;

    // CR 120.1: the damage RECIPIENT is the blocker, so its controller (P1)
    // loses life equal to the 3 damage dealt.
    assert_eq!(
        p1_life, 17,
        "P1 controls the damaged blocker, so P1 must lose 3 life (CR 120.1 + CR 109.4); \
         got {p1_life}"
    );
    // Two-sided: the DEALER's controller must be untouched. Blocked combat deals
    // no damage to the defending player and the blocker deals creature damage
    // only, so P0's life total cannot legitimately move.
    assert_eq!(
        p0_life, 20,
        "P0 controls Flayed Nim (the damage SOURCE, CR 120.1) and must NOT lose life; \
         losing life here is the pre-fix dealer-controller misbinding. got {p0_life}"
    );
}

/// CR 120.1 + CR 109.4: Bellowing Fiend deals 3 to the damaged creature's
/// controller AND 3 to its own controller. The two halves must land on DIFFERENT
/// players.
///
/// Discriminating by construction: before the fix both clauses resolved to P0,
/// so P0 took 6 and P1 took 0. The split assertion cannot pass on the broken
/// engine under any board symmetry.
#[test]
fn bellowing_fiend_splits_damage_between_victims_controller_and_its_own() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    // 2/5: survives the block, and its 2 combat damage is sub-lethal to the 1/3
    // blocker so the recipient is still on the battlefield when the trigger
    // resolves (the LKI path is covered by the Maarika case below).
    let fiend = scenario
        .add_creature_from_oracle(P0, "Bellowing Fiend", 2, 5, BELLOWING_FIEND_TEXT)
        .id();
    let blocker = scenario.add_creature(P1, "Blocker", 1, 3).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    run_combat(&mut runner, P0, fiend, P1, Some(blocker));
    runner.advance_until_stack_empty();

    let p0_life = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .life;
    let p1_life = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;

    // CR 120.1: "that creature's controller" is the blocker's controller, P1.
    assert_eq!(
        p1_life, 17,
        "P1 controls the damaged creature and must take the 3 damage from \
         \"that creature's controller\" (CR 120.1); got {p1_life}"
    );
    // CR 109.5: "and 3 damage to you" is the ability's controller, P0 — exactly
    // once. 14 here would mean both clauses hit P0 (the pre-fix behaviour).
    assert_eq!(
        p0_life, 17,
        "P0 must take exactly the 3 damage from the \"and 3 damage to you\" clause \
         (CR 109.5). 14 means both clauses resolved onto the dealer's controller, \
         which is the bug under test. got {p0_life}"
    );
}

/// CR 120.10 + CR 109.4 + CR 608.2h: Maarika's excess-damage trigger makes the
/// DAMAGED creature's controller sacrifice a noncreature, nonland permanent.
///
/// This is the `ControllerRef::EventTargetController` path — the anaphor scopes a
/// `Sacrifice` population rather than naming a player outright, so it travels
/// through `TypedFilter.controller` and `sacrifice::controller_scope` instead of
/// the effect's target slot.
///
/// Also covers the CR 608.2h LKI fallback that makes this variant work at all:
/// Maarika's 7 power is lethal to the 1/1 blocker, so CR 704.5g has already put
/// the recipient into a graveyard by the time the trigger resolves, at which
/// point CR 109.4 says it no longer has a controller. Without the last-known
/// information lookup the anaphor would resolve to nobody and the sacrifice
/// would silently not happen.
#[test]
fn maarika_excess_damage_makes_the_victims_controller_sacrifice() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // 7/4 — the printed P/T. 7 damage to a 1/1 is 6 excess (CR 120.10).
    let maarika = scenario
        .add_creature_from_oracle(P0, "Maarika, Brutal Gladiator", 7, 4, MAARIKA_TEXT)
        .id();
    let blocker = scenario.add_creature(P1, "Chump Blocker", 1, 1).id();

    // Both players control exactly one noncreature, nonland permanent, so the
    // test is two-sided: only the victim's controller may lose theirs.
    let p0_artifact = scenario.add_artifact_from_oracle(P0, "P0 Trinket", "").id();
    let p1_artifact = scenario.add_artifact_from_oracle(P1, "P1 Trinket", "").id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    run_combat(&mut runner, P0, maarika, P1, Some(blocker));
    runner.advance_until_stack_empty();

    // CR 120.10 + CR 109.4: P1 controlled the creature dealt excess damage, so
    // P1 sacrifices. CR 608.2h: resolved from the LKI snapshot, because the
    // blocker is already in the graveyard (CR 704.5g).
    assert_eq!(
        runner.state().objects[&p1_artifact].zone,
        Zone::Graveyard,
        "P1 controlled the creature dealt excess damage, so P1's noncreature, \
         nonland permanent must be sacrificed (CR 120.10 + CR 109.4)"
    );
    // Two-sided: Maarika's own controller keeps their permanent.
    assert_eq!(
        runner.state().objects[&p0_artifact].zone,
        Zone::Battlefield,
        "P0 controls Maarika (the damage SOURCE, CR 120.1) and must NOT sacrifice; \
         a sacrifice here is the pre-fix dealer-controller misbinding"
    );
}
