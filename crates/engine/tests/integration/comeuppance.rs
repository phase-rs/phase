//! Comeuppance (MKC #58) — full-pipeline prevention + reflection regression.
//!
//! Oracle text under test:
//!   "Prevent all damage that would be dealt to you and planeswalkers you control
//!    this turn by sources you don't control. If damage from a creature source is
//!    prevented this way, Comeuppance deals that much damage to that creature. If
//!    damage from a noncreature source is prevented this way, Comeuppance deals
//!    that much damage to the source's controller."
//!
//! Ruling 2014-11-07: the reflection is NOT redirection — Comeuppance (the spell)
//! is the new damage source and the reflected damage is never combat damage.
//!
//! CR 615 (prevention) + CR 615.5 (prevented-this-way follow-up) + CR 614.1a
//! (damage recipient/source filters) + CR 609.7b/615.9 (source controller axis) +
//! CR 120.1 (an object that deals damage is the source of that damage).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const COMEUPPANCE_TEXT: &str =
    "Prevent all damage that would be dealt to you and planeswalkers you control \
     this turn by sources you don't control. If damage from a creature source is \
     prevented this way, Comeuppance deals that much damage to that creature. If \
     damage from a noncreature source is prevented this way, Comeuppance deals \
     that much damage to the source's controller.";

/// Cast Comeuppance from P0's hand on P0's own pre-combat main and resolve it,
/// installing the prevention shield through the real cast pipeline. The shield is
/// turn-scoped, so flipping the active player to P1 afterward keeps it live for an
/// opponent's combat.
fn cast_comeuppance_then_p1_turn(
    scenario_setup: impl FnOnce(&mut GameScenario),
) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let comeuppance = scenario
        .add_spell_to_hand_from_oracle(P0, "Comeuppance", true, COMEUPPANCE_TEXT)
        .id();
    scenario_setup(&mut scenario);

    let mut runner = scenario.build();
    runner.cast(comeuppance).resolve();
    // Flip to an opponent's turn so P1 can attack P0 into the shield.
    runner.state_mut().active_player = P1;
    runner
}

fn run_combat(
    runner: &mut engine::game::scenario::GameRunner,
    attacker_player: PlayerId,
    attacks: &[(ObjectId, AttackTarget)],
    blockers: &[(ObjectId, ObjectId)],
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
                let a = if player == attacker_player {
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
            WaitingFor::DeclareBlockers { .. } if !blocked => {
                blocked = true;
                if runner.declare_blockers(blockers).is_err() {
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

/// #1 — An opponent's creature deals combat damage to you: prevented, and
/// Comeuppance reflects that much back to the creature (which is NOT combat
/// damage — a `DealDamage` resolution, never a `CombatDamage` event).
///
/// Revert-failing assertions: dropping the `untargeted_damage_filter` lowering
/// (Gap A) leaves P0 unprotected → the life assertion fails; dropping the
/// per-source reflection (Gap C / the per-event routing) leaves the attacker on
/// the battlefield → the graveyard assertion fails.
#[test]
fn opponent_creature_combat_damage_prevented_and_reflected() {
    let mut attacker_id = None;
    let mut runner = cast_comeuppance_then_p1_turn(|sc| {
        // 3/3 attacker: 3 reflected = lethal to itself, and its combat damage
        // to P0 must be fully prevented.
        attacker_id = Some(sc.add_creature(P1, "Raging Bear", 3, 3).id());
    });
    let attacker = attacker_id.unwrap();
    let p0_life_before = runner.life(P0);

    runner.advance_to_combat();
    run_combat(
        &mut runner,
        P1,
        &[(attacker, AttackTarget::Player(P0))],
        &[],
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "P0 takes NO combat damage — Comeuppance prevents damage dealt to you by \
         sources you don't control"
    );
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Graveyard,
        "the attacking creature is destroyed by Comeuppance's reflected 3 damage \
         (CR 615.5 reflection to the creature source)"
    );
    // Reach-guard: the reflected damage is non-combat — Comeuppance is a spell in
    // the graveyard, so a `combat_damage` record for the attacker taking 3 would
    // be impossible; the kill came from the `DealDamage` rider.
    assert!(
        !runner
            .state()
            .players
            .iter()
            .any(|p| p.id == P0 && p.life != p0_life_before),
        "P0's life is untouched, proving the attacker's combat damage never landed"
    );
}

/// #1b — TWO opponent creatures with DISTINCT powers attack you in the SAME
/// declare-attackers step: both combat-damage events are prevented, and each
/// reflection deals exactly that attacker's OWN power back to that same attacker.
/// This exercises the per-event batch routing through PRODUCTION combat with N≥2
/// simultaneous attackers (Finding M2) — the single-attacker test #1 cannot catch
/// a fold-into-aggregate or sibling-discard bug in the combat batch path.
///
/// Toughnesses are distinct (5 and 6) and above the reflected amount so both
/// attackers survive and their `damage_marked` is individually observable.
///
/// Revert-failing: folding both prevented events into one aggregate would mark
/// each attacker 5 (2+3) instead of its own power; a sibling-discard bug would
/// mark only one. The distinct 2-vs-3 split is exactly what those bugs break.
#[test]
fn two_simultaneous_attackers_each_reflected_own_power() {
    let mut small_id = None;
    let mut big_id = None;
    let mut runner = cast_comeuppance_then_p1_turn(|sc| {
        // Distinct powers (2, 3) and distinct toughnesses (5, 6) so each attacker
        // survives its own sub-lethal reflection and stays individually inspectable.
        small_id = Some(sc.add_creature(P1, "Small Attacker", 2, 5).id());
        big_id = Some(sc.add_creature(P1, "Big Attacker", 3, 6).id());
    });
    let small = small_id.unwrap();
    let big = big_id.unwrap();
    let p0_life_before = runner.life(P0);

    runner.advance_to_combat();
    run_combat(
        &mut runner,
        P1,
        &[
            (small, AttackTarget::Player(P0)),
            (big, AttackTarget::Player(P0)),
        ],
        &[],
    );
    runner.advance_until_stack_empty();

    // Positive reach-guard: both creatures actually attacked (declaring an attacker
    // taps it — CR 508.1f), so the prevented events genuinely occurred in combat and
    // the life/no-change negative below cannot pass vacuously.
    assert!(
        runner.state().objects[&small].tapped && runner.state().objects[&big].tapped,
        "both creatures were declared as attackers (tapped), so both combat-damage \
         events actually happened this step"
    );

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "P0 takes NO combat damage — both simultaneous events are prevented by the \
         shield"
    );
    assert_eq!(
        runner.state().objects[&small].damage_marked,
        2,
        "the 2-power attacker is reflected exactly its OWN 2 damage (creature source \
         → that creature), not an aggregate of both attackers"
    );
    assert_eq!(
        runner.state().objects[&big].damage_marked,
        3,
        "the 3-power attacker is reflected exactly its OWN 3 damage — per-event \
         source attribution survives the N≥2 combat batch"
    );
}

/// #3 — Damage to a planeswalker you control is prevented and reflected. Guards
/// the `permanent_type: Some(Planeswalker)` leg of the compound recipient (Gap
/// A(a)). Revert-failing: without the planeswalker leg the shield only protects
/// the player, so the planeswalker takes 3 and the attacker survives.
#[test]
fn opponent_creature_damage_to_planeswalker_prevented_and_reflected() {
    let mut attacker_id = None;
    let mut runner = cast_comeuppance_then_p1_turn(|sc| {
        attacker_id = Some(sc.add_creature(P1, "Raging Bear", 3, 3).id());
    });
    let attacker = attacker_id.unwrap();

    // A P0-controlled planeswalker with 4 loyalty.
    let pw = create_object(
        runner.state_mut(),
        CardId(300),
        P0,
        "Test Walker".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&pw).unwrap();
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.counters
            .insert(engine::types::counter::CounterType::Loyalty, 4);
    }
    let pw_loyalty_before = loyalty(&runner, pw);

    runner.advance_to_combat();
    run_combat(
        &mut runner,
        P1,
        &[(attacker, AttackTarget::Planeswalker(pw))],
        &[],
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        loyalty(&runner, pw),
        pw_loyalty_before,
        "the planeswalker loses NO loyalty — Comeuppance protects planeswalkers you \
         control (CR 614.1a permanent_type leg)"
    );
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Graveyard,
        "the attacker is destroyed by the reflected 3 damage even when the \
         prevented damage was aimed at a planeswalker"
    );
}

/// #5 — An opponent's creature damaging YOUR creature is NOT prevented (guards
/// against an over-broad recipient — Gap A review finding #5). The shield covers
/// "you and planeswalkers you control", not creatures you control.
///
/// Revert-failing: parameterizing the recipient with `None` instead of
/// `Some(Planeswalker)` would wrongly protect the creature; the marked-damage
/// assertion catches it.
#[test]
fn opponent_damage_to_your_creature_not_prevented() {
    let mut attacker_id = None;
    let mut blocker_id = None;
    let mut runner = cast_comeuppance_then_p1_turn(|sc| {
        // P1's 3/3 attacker; P0's 0/4 blocker (survives to be inspected).
        attacker_id = Some(sc.add_creature(P1, "Raging Bear", 3, 3).id());
        blocker_id = Some(sc.add_creature(P0, "Wall", 0, 4).id());
    });
    let attacker = attacker_id.unwrap();
    let blocker = blocker_id.unwrap();

    runner.advance_to_combat();
    run_combat(
        &mut runner,
        P1,
        &[(attacker, AttackTarget::Player(P0))],
        &[(blocker, attacker)],
    );
    runner.advance_until_stack_empty();

    // The blocker (P0's creature) took the attacker's 3 combat damage — the shield
    // protects only P0 and P0's planeswalkers, not P0's creatures.
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        3,
        "combat damage to YOUR CREATURE is not prevented (the recipient is \
         restricted to you and planeswalkers you control)"
    );
    // And because that combat damage was dealt (not prevented), no reflection
    // fired against the attacker.
    assert_eq!(
        runner.state().objects[&attacker].damage_marked,
        0,
        "no reflection fires for creature-recipient damage that was never prevented"
    );
}

/// #4 — Your OWN source's damage to you is NOT prevented (source controller axis,
/// Gap B). A P0-controlled source dealing damage to P0 must land, because the
/// shield only prevents damage from sources you don't control.
///
/// Driven through the real damage pipeline by resolving a P0-controlled
/// `DealDamage` from a P0 creature to P0. Revert-failing: dropping the
/// `ControllerRef::Opponent` source filter (Gap B) makes the shield prevent
/// everything, and P0 would lose no life.
#[test]
fn your_own_source_damage_to_you_not_prevented() {
    let mut own_source = None;
    let mut runner = cast_comeuppance_then_p1_turn(|sc| {
        own_source = Some(sc.add_creature(P0, "Your Zapper", 1, 1).id());
    });
    let source = own_source.unwrap();
    let p0_life_before = runner.life(P0);

    // Resolve a P0-controlled DealDamage from P0's own creature to P0.
    let ability = ResolvedAbility::new(
        Effect::DealDamage {
            amount: engine::types::ability::QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::SpecificPlayer { id: P0 },
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Player(P0)],
        source,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .unwrap();

    assert_eq!(
        runner.life(P0),
        p0_life_before - 2,
        "P0's own source's damage is NOT prevented — the shield only prevents \
         damage from sources you don't control (CR 609.7b source-controller axis)"
    );
}

/// #2 — An opponent's NONCREATURE source damaging you is prevented, and reflected
/// to the source's controller (Gap C noncreature rider → PostReplacementSource-
/// Controller). Driven through the real damage pipeline by resolving a
/// P1-controlled noncreature (artifact) source's `DealDamage` to P0.
///
/// Revert-failing: dropping the noncreature reflection rider leaves P1 at full
/// life; dropping the source filter would prevent nothing and P0 would take 2.
#[test]
fn opponent_noncreature_source_damage_prevented_and_reflected_to_controller() {
    let mut runner = cast_comeuppance_then_p1_turn(|_| {});

    // A P1-controlled artifact source on the battlefield (noncreature).
    let artifact = create_object(
        runner.state_mut(),
        CardId(400),
        P1,
        "Cursed Totem".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&artifact)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Artifact];

    let p0_life_before = runner.life(P0);
    let p1_life_before = runner.life(P1);

    // P1's artifact deals 2 damage to P0.
    let ability = ResolvedAbility::new(
        Effect::DealDamage {
            amount: engine::types::ability::QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::SpecificPlayer { id: P0 },
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Player(P0)],
        artifact,
        P1,
    );
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .unwrap();

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "P0 takes no damage — the noncreature source's damage is prevented"
    );
    assert_eq!(
        runner.life(P1),
        p1_life_before - 2,
        "the prevented 2 damage is reflected to the noncreature source's \
         controller (P1) — CR 615.5 reflection to PostReplacementSourceController"
    );
}

/// #6 — Two separate prevention events in one turn each fire the rider against
/// their own per-event amount and source. Guards the per-event routing (the batch
/// path would fold both into one aggregate with no per-source attribution).
#[test]
fn two_separate_prevention_events_reflect_per_event() {
    // Two distinct P1 creature sources (5 toughness so the reflections are
    // sub-lethal and observable as marked damage).
    let mut bear_id = None;
    let mut wolf_id = None;
    let mut runner = cast_comeuppance_then_p1_turn(|sc| {
        bear_id = Some(sc.add_creature(P1, "Bear A", 2, 5).id());
        wolf_id = Some(sc.add_creature(P1, "Wolf B", 2, 5).id());
    });
    let bear = bear_id.unwrap();
    let wolf = wolf_id.unwrap();

    let deal = |runner: &mut engine::game::scenario::GameRunner, src: ObjectId, amount: i32| {
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: engine::types::ability::QuantityExpr::Fixed { value: amount },
                target: TargetFilter::SpecificPlayer { id: P0 },
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(P0)],
            src,
            P1,
        );
        let mut events = Vec::new();
        engine::game::effects::resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
            .unwrap();
    };

    // Event 1: bear deals 2 → prevented, reflected 2 to bear.
    deal(&mut runner, bear, 2);
    // Event 2: wolf deals 3 → prevented, reflected 3 to wolf.
    deal(&mut runner, wolf, 3);

    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        2,
        "the first event reflects exactly its own 2 damage to its own source"
    );
    assert_eq!(
        runner.state().objects[&wolf].damage_marked,
        3,
        "the second event reflects exactly its own 3 damage to its own source \
         (per-event amount + source, not an aggregate)"
    );
}

fn loyalty(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id]
        .counters
        .get(&engine::types::counter::CounterType::Loyalty)
        .copied()
        .unwrap_or(0) as i32
}
