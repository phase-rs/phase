//! Discriminating integration coverage for the **"defending player can't cast
//! spells this turn"** combat-attack-trigger class (Xantid Swarm).
//!
//! Oracle text under test:
//!   "Flying\nWhenever this creature attacks, defending player can't cast spells
//!    this turn."
//!
//! ROOT CAUSE under test: the can't-cast parser (`try_parse_cant_cast_spells_effect`)
//! recognized your-opponents / players / target-player / that-player scopes but
//! NOT "defending player", and `RestrictionPlayerScope` had no `DefendingPlayer`
//! variant — so the effect lowered to `Unimplemented`. The fix adds the variant,
//! parses "defending player", and resolves it to the attack's defending player
//! (`combat::defending_player_for_attacker`) when the restriction is created
//! (CR 508.5a — the defending player is fixed once attackers are declared).
//!
//! These tests drive the REAL combat pipeline (`advance_to_combat` +
//! `declare_attackers`) so the attack trigger fires from a genuine
//! declare-attackers event.

use engine::game::casting::can_cast_object_now;
use engine::game::combat::AttackTarget;
use engine::game::effects::remove_from_combat::remove_object_from_combat;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{GameRestriction, ProhibitedActivity, RestrictionPlayerScope};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const XANTID: &str =
    "Flying\nWhenever this creature attacks, defending player can't cast spells this turn.";

/// Build a 2-player board: Xantid Swarm (P0's attacker) plus a free instant in
/// each player's hand so the only difference in castability is the restriction.
/// Returns (runner, xantid, p0_instant, p1_instant).
fn board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let xantid = scenario
        .add_creature_from_oracle(P0, "Xantid Swarm", 1, 1, XANTID)
        .id();

    // {0} instants — always affordable, so `can_cast_object_now` differs only by
    // the prohibition, not by available mana.
    let p0_instant = scenario
        .add_spell_to_hand_from_oracle(P0, "P0 Bolt", true, "P0 Bolt deals 1 damage to any target.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let p1_instant = scenario
        .add_spell_to_hand_from_oracle(P1, "P1 Bolt", true, "P1 Bolt deals 1 damage to any target.")
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let runner = scenario.build();
    (runner, xantid, p0_instant, p1_instant)
}

/// Advance to combat, declare Xantid attacking P1 (firing the trigger), and
/// drive priority so the trigger resolves and the restriction is added.
fn attack_and_resolve(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declaring Xantid as attacker must succeed");

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// CR 508.5a + CR 101.2: after Xantid attacks P1, the restriction resolves to the
/// DEFENDING player (P1) — not the controller, not all players — and prohibits
/// P1 from casting spells.
#[test]
fn xantid_attack_prohibits_defending_player_casting() {
    let (mut runner, xantid, p0_instant, p1_instant) = board();
    attack_and_resolve(&mut runner, xantid);

    // PRIMARY (discrimination): the stored restriction must be a CastSpells
    // prohibition resolved to SpecificPlayer(P1) — the defending player. A broken
    // resolution would leave it unresolved (DefendingPlayer) or bind the wrong
    // player, and on pre-fix HEAD no restriction is created at all (Unimplemented).
    let resolved_to_p1 = runner.state().restrictions.iter().any(|r| {
        matches!(
            r,
            GameRestriction::ProhibitActivity {
                affected_players: RestrictionPlayerScope::SpecificPlayer(p),
                activity: ProhibitedActivity::CastSpells { spell_filter: None },
                ..
            } if *p == P1
        )
    });
    assert!(
        resolved_to_p1,
        "CR 508.5a: the can't-cast restriction must resolve to the defending player P1, got {:?}",
        runner.state().restrictions
    );

    // BEHAVIORAL: the defending player P1 can't cast; the attacker's controller
    // P0 (not the defending player) is unaffected. Both spells are {0} instants,
    // so the only difference is the prohibition.
    assert!(
        !can_cast_object_now(runner.state(), P1, p1_instant),
        "the defending player P1 must be prohibited from casting spells"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, p0_instant),
        "the attacking player P0 (not the defending player) must NOT be prohibited"
    );
}

/// CR 508.5: an ability of an attacking creature still refers to the player
/// that creature was attacking if the creature is no longer attacking when the
/// ability resolves.
#[test]
fn xantid_restriction_uses_attack_event_after_source_leaves_combat() {
    let (mut runner, xantid, _p0_instant, p1_instant) = board();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(xantid, AttackTarget::Player(P1))])
        .expect("declaring Xantid as attacker must succeed");

    remove_object_from_combat(runner.state_mut(), xantid);
    assert!(
        !runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == xantid)),
        "test setup must remove Xantid from live combat before trigger resolution"
    );

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }

    assert!(
        !can_cast_object_now(runner.state(), P1, p1_instant),
        "CR 508.5: P1 remains the defending player even after Xantid leaves combat"
    );
}
