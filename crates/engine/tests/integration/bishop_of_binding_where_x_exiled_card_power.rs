//! CR 107.3c — "where X is the power of the exiled card" must actually pump.
//!
//! Bishop of Binding (RIX), Oracle text (read from the card export, not memory):
//!   "When this creature enters, exile target creature an opponent controls
//!    until this creature leaves the battlefield.
//!    Whenever this creature attacks, target Vampire gets +X/+X until end of
//!    turn, where X is the power of the exiled card."
//!
//! THE BUG this discriminates (harvest task #48):
//! `apply_where_x_expression` used to fall back to
//! `PtValue::Variable("<raw oracle text>")` whenever the where-X expression was
//! not representable — here, `Variable("the power of the exiled card")`. That
//! node is well-typed but completely DEAD: `resolve_variable_pt`
//! (game/effects/pump.rs) dispatches only `X` / `-X` and returns `None` for any
//! other content, so `pt_modifications` pushed NO `ContinuousModification` at
//! all. The Vampire silently got +0/+0 — while the raw text still rendered as a
//! supported dynamic quantity in the coverage report (a fabricated green).
//!
//! The fix binds the clause to `QuantityRef::ExiledCardPower { index: 0 }`
//! (CR 607.2a — the card exiled by this source), which `game/quantity.rs`
//! resolves against `cards_exiled_with_source_this_turn`.
//!
//! This is a RUNTIME test, not an AST-shape test: it drives declare-attackers →
//! the `Attacks` trigger → target selection → stack resolution → the layer
//! system, and reads the EFFECTIVE (post-layer) power/toughness off the pumped
//! Vampire. If the where-X binding is dropped, the pump contributes nothing and
//! the Vampire stays 2/2 — which is exactly what this asserts against.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

/// Bishop of Binding's printed Oracle text.
const BISHOP_OF_BINDING: &str = "When this creature enters, exile target creature an opponent \
controls until this creature leaves the battlefield.\n\
Whenever this creature attacks, target Vampire gets +X/+X until end of turn, where X is the power \
of the exiled card.";

/// CR 107.3c + CR 607.2a: X is DEFINED by the ability's text as the power of the
/// card exiled with Bishop of Binding. With a 4/4 exiled, the targeted Vampire
/// must become an effective 2/2 + 4/4 = 6/6.
#[test]
fn bishop_of_binding_pumps_target_vampire_by_exiled_card_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bishop = scenario
        .add_creature_from_oracle(P0, "Bishop of Binding", 2, 2, BISHOP_OF_BINDING)
        .id();

    // The pump recipient: a Vampire P0 controls (the trigger targets "target
    // Vampire"). Base 2/2 so the +X/+X delta is unambiguous.
    let vampire = {
        let mut builder = scenario.add_creature(P0, "Vampire Ally", 2, 2);
        builder.with_subtypes(vec!["Vampire"]);
        builder.id()
    };

    // The card whose power DEFINES X: a 4/4. Created on P1's battlefield, then
    // moved to exile and linked to Bishop below — the state Bishop's own ETB
    // ("exile target creature an opponent controls until this leaves") would
    // have established. Seeding it directly keeps this test focused on the
    // where-X binding rather than re-testing the exile machinery.
    let exiled = scenario.add_creature(P1, "Exiled Brute", 4, 4).id();

    // Idle defender so the DeclareBlockers prompt appears (it declares no blocks).
    scenario.add_creature(P1, "Idle Bystander", 1, 1);

    let mut runner = scenario.build();

    // CR 607.2a: link the exiled card to its exiling source. `ExiledCardPower`
    // resolves against `cards_exiled_with_source_this_turn`.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != exiled);
        state.exile.push_back(exiled);
        state
            .cards_exiled_with_source_this_turn
            .insert(bishop, vec![exiled]);
    }

    // CR 508.1: declare Bishop as an attacker — this fires the `Attacks` trigger.
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(bishop, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Drive the trigger to resolution. The Vampire is the ONLY legal target for
    // "target Vampire" (Bishop itself is not given the subtype here), so the
    // engine may auto-select it rather than raising a TriggerTargetSelection
    // prompt — handle both paths.
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(vampire)),
                    })
                    .expect("choose the Vampire as the pump target");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("empty DeclareBlockers should succeed");
            }
            WaitingFor::Priority { .. } => {
                runner.advance_until_stack_empty();
                break;
            }
            other => panic!("unexpected prompt while resolving the attack trigger: {other:?}"),
        }
    }

    // THE DISCRIMINATOR. Pre-fix this read (2, 2): the dropped where-X binding
    // left a dead `PtValue::Variable("the power of the exiled card")`, which
    // `resolve_variable_pt` maps to `None`, so no modification was ever pushed.
    let pumped = &runner.state().objects[&vampire];
    assert_eq!(
        (pumped.power, pumped.toughness),
        (Some(6), Some(6)),
        "CR 107.3c: X is DEFINED as the exiled card's power (4), so the targeted \
         Vampire must be 2/2 + 4/4 = 6/6. A 2/2 here means the where-X binding was \
         dropped and the pump silently resolved as a +0/+0 no-op."
    );

    // Bishop itself is not the target — it must be untouched (scope check).
    let attacker = &runner.state().objects[&bishop];
    assert_eq!(
        (attacker.power, attacker.toughness),
        (Some(2), Some(2)),
        "the pump targets the Vampire only — Bishop must not be pumped"
    );
}
