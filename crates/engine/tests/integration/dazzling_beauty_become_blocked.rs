//! Cast-pipeline + combat tests for the general `Effect::BecomeBlocked` primitive
//! (Dazzling Beauty class). Every test drives the real pipeline: casts through
//! `GameRunner::cast(..).resolve()` in the declare-blockers window and runs real
//! combat damage / triggers through the scenario runner.
//!
//! Verbatim Oracle text (Dazzling Beauty):
//!   "Cast this spell only during the declare blockers step.\n
//!    Target unblocked attacking creature becomes blocked. (This spell works on
//!    creatures that can't be blocked.)\n
//!    Draw a card at the beginning of the next turn's upkeep."

use engine::game::combat::{unblocked_attackers, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Drive combat from the declare-attackers priority window into the
/// declare-blockers step, then hand priority to `caster` so a defending player
/// can cast during that step (CR 509). Panics if the declare-blockers step is
/// not reached.
fn advance_to_declare_blockers_and_give_priority(runner: &mut GameRunner, caster: PlayerId) {
    for _ in 0..20 {
        let wf = runner.state().waiting_for.clone();
        if runner.state().phase == Phase::DeclareBlockers {
            let state = runner.state_mut();
            state.priority_player = caster;
            state.waiting_for = WaitingFor::Priority { player: caster };
            return;
        }
        let acted = match wf {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            other => panic!("unexpected waiting state before declare-blockers: {other:?}"),
        };
        acted.expect("combat advance action");
    }
    panic!("did not reach the declare-blockers step");
}

const DAZZLING_BEAUTY: &str = "Cast this spell only during the declare blockers step.\nTarget unblocked attacking creature becomes blocked. (This spell works on creatures that can't be blocked.)\nDraw a card at the beginning of the next turn's upkeep.";

/// Just the becomes-blocked line, for tests that only exercise the combat effect.
const BECOMES_BLOCKED_ONLY: &str = "Cast this spell only during the declare blockers step.\nTarget unblocked attacking creature becomes blocked.";

/// Build a scenario with P0 (the default active player) attacking P1 with a
/// single creature, advanced to the declare-blockers step. P1 is the defending
/// caster of Dazzling Beauty. Returns (runner, attacker_id, spell_id).
fn attack_setup(attacker_power: i32, spell_oracle: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Charging Ox", attacker_power, attacker_power)
        .id();
    // P1 (defender) holds Dazzling Beauty; scenario spells are NoCost.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Dazzling Beauty", true, spell_oracle)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    advance_to_declare_blockers_and_give_priority(&mut runner, P1);
    (runner, attacker, spell)
}

#[test]
fn becomes_blocked_deals_no_damage() {
    // CR 510.1c: a creature blocked by an effect (no blockers) assigns no combat
    // damage. Revert-failing assertion: P0 life delta is 0.
    let (mut runner, attacker, spell) = attack_setup(3, BECOMES_BLOCKED_ONLY);
    runner.cast(spell).target_objects(&[attacker]).resolve();
    // The attacker is now blocked with no blockers assigned.
    assert!(!unblocked_attackers(runner.state()).contains(&attacker));
    let outcome = runner.combat_damage();
    outcome.assert_life_delta(P1, 0);
}

#[test]
fn control_without_spell_takes_full_damage() {
    // Reach-guard for `becomes_blocked_deals_no_damage`: the SAME setup WITHOUT
    // casting the spell deals full damage, proving the 0-delta above is caused by
    // the effect, not by an unrelated short-circuit.
    let (mut runner, attacker, _spell) = attack_setup(3, BECOMES_BLOCKED_ONLY);
    assert!(unblocked_attackers(runner.state()).contains(&attacker));
    let outcome = runner.combat_damage();
    outcome.assert_life_delta(P1, -3);
}

#[test]
fn becomes_blocked_trigger_fires_and_block_side_trigger_does_not() {
    // CR 509.3c: a "whenever ~ becomes blocked" trigger on the attacker fires from
    // an effect-block (positive: proves the event fires at all).
    // CR 509.3d: a blocker-side "whenever ~ blocks" trigger must NOT fire from an
    // effect-block (negative, paired so the negative is not vacuous).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 (active) attacks with a creature carrying a "becomes blocked" trigger.
    let attacker = scenario
        .add_creature_from_oracle(
            P0,
            "Watchful Ox",
            2,
            2,
            "Whenever this creature becomes blocked, you gain 3 life.",
        )
        .id();
    // A P1 creature with a "whenever ~ blocks" trigger that must stay silent.
    let _watcher = scenario
        .add_creature_from_oracle(
            P1,
            "Eager Blocker",
            1,
            1,
            "Whenever this creature blocks, you gain 5 life.",
        )
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Dazzling Beauty", true, BECOMES_BLOCKED_ONLY)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    advance_to_declare_blockers_and_give_priority(&mut runner, P1);

    let p0_life_before = runner.life(P0);
    let p1_life_before = runner.life(P1);
    runner.cast(spell).target_objects(&[attacker]).resolve();
    runner.advance_until_stack_empty();

    // CR 509.3c: attacker's "becomes blocked" trigger fired → P0 gained 3 life.
    assert_eq!(
        runner.life(P0) - p0_life_before,
        3,
        "becomes-blocked trigger should fire from an effect-block (CR 509.3c)"
    );
    // CR 509.3d: the blocker-side "whenever ~ blocks" trigger must not fire (no
    // creature was actually declared as a blocker) → P1 did not gain 5 life.
    assert_eq!(
        runner.life(P1) - p1_life_before,
        0,
        "block-side trigger must not fire on an effect-block (CR 509.3d)"
    );
}

#[test]
fn works_on_unblockable() {
    // The effect makes an attacker blocked even though it "can't be blocked" —
    // proving it is NOT routed through place_blocking (which would reject an
    // unblockable). CR 510.1c: still 0 combat damage.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature_from_oracle(P0, "Slippery Ox", 4, 4, "This creature can't be blocked.")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Dazzling Beauty", true, BECOMES_BLOCKED_ONLY)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    advance_to_declare_blockers_and_give_priority(&mut runner, P1);

    runner.cast(spell).target_objects(&[attacker]).resolve();
    assert!(!unblocked_attackers(runner.state()).contains(&attacker));
    let outcome = runner.combat_damage();
    outcome.assert_life_delta(P1, 0);
}

#[test]
fn full_chain_parses_become_blocked_then_delayed_draw() {
    // SHAPE: the full Dazzling Beauty text composes the new BecomeBlocked head
    // effect with the pre-existing delayed-draw sub-ability, and the casting
    // restriction, with NO Effect::Unimplemented anywhere. The delayed draw and
    // casting restriction are pre-existing behavior (out of scope for the
    // BecomeBlocked primitive); this test guards that adding BecomeBlocked did not
    // disturb the chain composition. Runtime behavior of BecomeBlocked itself is
    // covered by the cast-pipeline tests above.
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::Effect;

    let parsed = parse_oracle_text(
        DAZZLING_BEAUTY,
        "Dazzling Beauty",
        &[],
        &["Instant".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1, "one spell ability");
    let ability = &parsed.abilities[0];
    assert!(
        matches!(*ability.effect, Effect::BecomeBlocked { .. }),
        "head effect must be BecomeBlocked, got {:?}",
        ability.effect
    );
    // The delayed draw composes as the sub-ability.
    let sub = ability
        .sub_ability
        .as_ref()
        .expect("delayed-draw sub-ability must compose after BecomeBlocked");
    assert!(
        matches!(*sub.effect, Effect::CreateDelayedTrigger { .. }),
        "sub effect must be the delayed draw, got {:?}",
        sub.effect
    );
    // Reach-guard against a vacuous no-Unimplemented pass: assert the casting
    // restriction parsed too, proving the whole card was consumed.
    assert!(
        !parsed.casting_restrictions.is_empty(),
        "the declare-blockers casting restriction must parse"
    );
    // No Unimplemented anywhere in the parsed abilities.
    let debug = format!("{parsed:?}");
    assert!(
        !debug.contains("Unimplemented"),
        "no Effect::Unimplemented in the parsed Dazzling Beauty chain"
    );
}
