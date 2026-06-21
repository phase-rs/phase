//! Teamwork is its own additional-cost ORIGIN (PR #3916 review blocker #1).
//!
//! Before the fix, Teamwork's optional tap cost was installed into the generic
//! `face.additional_cost` slot and its payment was recorded with
//! `AdditionalCostOrigin::Other`. Consequently "if this spell was cast using
//! teamwork" parsed to `AdditionalCostPaid { origin: None }`, which is satisfied
//! by ANY additional-cost payment — so a different additional cost on the same
//! spell wrongly triggered the Teamwork rider, and Teamwork could not compose
//! with another object additional cost (the single `face.additional_cost` slot
//! held only one).
//!
//! The fix gives Teamwork a dedicated `AdditionalCostOrigin::Teamwork`, queues it
//! through the keyword-cost announcement queue (so it coexists with another
//! additional cost), and parses the rider to `origin: Some(Teamwork)` so it tests
//! the Teamwork payment specifically.
//!
//! This test drives the real cast pipeline for a spell carrying BOTH Casualty
//! (another queued additional cost, whose payment sets the shared
//! `additional_cost_paid` flag) AND Teamwork. Paying Casualty but DECLINING
//! Teamwork must leave "cast using teamwork" FALSE (no life gain). Against the
//! pre-fix `origin: None` behavior the Casualty payment satisfies the rider and
//! the +7 life assertion flips.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityCost, AdditionalCost};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

// Casualty 1 + Teamwork 2. The body's only effect is the Teamwork-gated life
// gain, so any life change directly reads whether "cast using teamwork" was true.
const CASUALTY_AND_TEAMWORK: &str = "Casualty 1 (As you cast this spell, you may sacrifice a creature with power 1 or greater. When you do, copy this spell.)\nTeamwork 2 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 2 or more.)\nIf this spell was cast using teamwork, you gain 7 life.";

/// Returns true if an `AdditionalCost` is the Teamwork tap cost (vs Casualty's
/// sacrifice cost).
fn is_teamwork_cost(cost: &AdditionalCost) -> bool {
    matches!(
        cost,
        AdditionalCost::Optional {
            cost: AbilityCost::TapCreatures { .. },
            ..
        }
    )
}

/// Drive the cast paying Casualty (sacrificing `fodder`) but DECLINING Teamwork.
fn cast_paying_casualty_declining_teamwork(
    runner: &mut GameRunner,
    spell: ObjectId,
    fodder: ObjectId,
) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the spell must be accepted");

    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { cost, .. } => {
                // Decline Teamwork; pay Casualty.
                let pay = !is_teamwork_cost(&cost);
                runner
                    .act(GameAction::DecideOptionalCost { pay })
                    .expect("optional-cost decision must be accepted");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => panic!("Teamwork was declined; no tap-creatures payment should be requested"),
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .expect("paying the Casualty sacrifice must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing {0} mana cost must be accepted");
            }
            _ => return,
        }
    }
    panic!("cast did not settle to priority");
}

fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

#[test]
fn casualty_payment_does_not_satisfy_teamwork_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A creature to feed Casualty's sacrifice (power 1 >= Casualty 1).
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Twin Strike", false, CASUALTY_AND_TEAMWORK);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    cast_paying_casualty_declining_teamwork(&mut runner, spell, fodder);
    resolve_stack(&mut runner);

    assert!(
        !runner.state().battlefield.contains(&fodder),
        "the Casualty sacrifice must have been paid"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "paying Casualty (a NON-teamwork additional cost) must NOT satisfy 'cast using \
         teamwork' — the +7 life rider must not fire"
    );
}

/// Conversely, paying Teamwork (declining Casualty) DOES satisfy the rider. This
/// exercises the queued-Teamwork payment recording its dedicated
/// `AdditionalCostOrigin::Teamwork`, which the `origin: Some(Teamwork)` rider
/// then reads — proving the composing spell still resolves the Teamwork upgrade.
#[test]
fn teamwork_payment_satisfies_teamwork_rider_when_composed_with_casualty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A 2/2 to tap for Teamwork 2 (total power 2 >= 2); also Casualty-eligible,
    // but we decline Casualty here.
    let tapper = scenario.add_creature(P0, "Tapper", 2, 2).id();
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Twin Strike", false, CASUALTY_AND_TEAMWORK);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the spell must be accepted");

    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { cost, .. } => {
                // Pay Teamwork; decline Casualty.
                let pay = is_teamwork_cost(&cost);
                runner
                    .act(GameAction::DecideOptionalCost { pay })
                    .expect("optional-cost decision must be accepted");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![tapper],
                    })
                    .expect("tapping the 2/2 (total power 2 >= 2) must pay Teamwork");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing {0} mana cost must be accepted");
            }
            _ => break,
        }
    }
    resolve_stack(&mut runner);

    assert!(
        runner.state().objects[&tapper].tapped,
        "the Teamwork tap creature must be tapped"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before + 7,
        "paying Teamwork must satisfy 'cast using teamwork' — the +7 life rider fires"
    );
}
