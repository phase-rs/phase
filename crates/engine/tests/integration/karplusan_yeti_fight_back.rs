//! Regression for the fight-back-clause cross-context anaphora bug (CR 608.2c).
//!
//! Karplusan Yeti — Oracle:
//!   `{T}: This creature deals damage equal to its power to target creature.
//!    That creature deals damage equal to its power to this creature.`
//!
//! Clause 2 parses to `DealDamage { amount: Power{Anaphoric}, target: SelfRef }`:
//! "to this creature" is the source object's self-reference (CR 201.5), so the
//! recipient is the Yeti itself, and the `Anaphoric` amount is the power of the
//! creature clause 1 damaged ("that creature", CR 608.2c).
//!
//! The fix is TWO-part:
//!   1. Resolver: `damaged_object_context_from_events` is the weakest
//!      `parent_referent_context_from_events` referent — a single object-targeted
//!      `DamageDealt` event from the parent instruction introduces the "that
//!      creature" antecedent, seeding the `Anaphoric` amount (without it, the
//!      amount resolved to 0 and the Yeti took 0 damage).
//!   2. Parser: `bind_anaphoric_damage_subject_keep_recipient` preserves a
//!      `TargetFilter::SelfRef` recipient verbatim (CR 201.5), so the blanket
//!      anaphoric parent-rewrite no longer clobbers clause 2's recipient to
//!      `ParentTarget` (which would re-aim the fight-back at clause 1's chosen
//!      target instead of the source).
//!
//! This test drives the real `apply` pipeline (activate `{T}`, select target,
//! resolve) and asserts both damage prongs. It FAILS on revert of either half
//! (the Yeti takes 0 without the resolver half; the fought creature is hit
//! twice and the Yeti takes 0 without the parser half).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const YETI_ORACLE: &str = "{T}: This creature deals damage equal to its power to target creature. That creature deals damage equal to its power to this creature.";

/// CR 608.2c: the fight-back clause's "that creature" binds to the creature the
/// earlier clause damaged. Karplusan Yeti (3/3) fights a 5/5: the 5/5 takes the
/// Yeti's power (3), then the Yeti takes the 5/5's power (5) — not 0, not 3.
#[test]
fn karplusan_yeti_fight_back_deals_fought_creatures_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yeti = scenario
        .add_creature_from_oracle(P0, "Karplusan Yeti", 3, 3, YETI_ORACLE)
        .id();
    // A 5/5 under the opponent: power (5) differs from the Yeti's power (3), so a
    // spurious self-power referent would read 3, not 5 — distinguishing the fix.
    let opponent = scenario.add_creature(P1, "Hulking Brute", 5, 5).id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: yeti,
            ability_index: 0,
        })
        .expect("activating Karplusan Yeti's {T} ability must succeed");

    // Target the opponent's 5/5.
    let target = match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots[0]
            .legal_targets
            .iter()
            .find(|t| matches!(t, TargetRef::Object(id) if *id == opponent))
            .cloned()
            .expect("the opponent's 5/5 must be a legal target"),
        other => panic!("expected target selection for the fight, got {other:?}"),
    };
    runner
        .act(GameAction::SelectTargets {
            targets: vec![target],
        })
        .expect("choosing the fight target must succeed");

    // Drain the stack, collecting the damage events across resolution so the
    // exact amounts are observable even though 5 damage is lethal to the 3/3.
    let mut damage: Vec<(TargetRef, u32)> = Vec::new();
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority to resolve the fight must succeed");
        for event in &result.events {
            if let GameEvent::DamageDealt { target, amount, .. } = event {
                damage.push((target.clone(), *amount));
            }
        }
    }

    let damage_to = |id: ObjectId| -> u32 {
        damage
            .iter()
            .filter(|(t, _)| matches!(t, TargetRef::Object(o) if *o == id))
            .map(|(_, amt)| *amt)
            .sum()
    };

    // Clause 1: the Yeti deals its power (3) to the fought creature.
    assert_eq!(
        damage_to(opponent),
        3,
        "the fought 5/5 must take the Yeti's power (3); damage = {damage:?}",
    );
    // Clause 2 (the fix): the fought creature deals ITS power (5) back to the
    // Yeti via the CR 608.2c "that creature" referent — not 0 (no referent) and
    // not 3 (a wrong self-power referent).
    assert_eq!(
        damage_to(yeti),
        5,
        "the Yeti must take the fought creature's power (5) via the fight-back \
         referent, not 0 (the pre-fix bug); damage = {damage:?}",
    );
}
