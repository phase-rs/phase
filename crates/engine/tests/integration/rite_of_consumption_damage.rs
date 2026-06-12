//! Integration test for issue #495 — Rite of Consumption deals no damage and
//! gains no life.
//!
//! Oracle:
//!   "As an additional cost to cast this spell, sacrifice a creature.
//!    Rite of Consumption deals damage equal to the sacrificed creature's
//!    power to target player or planeswalker. You gain life equal to the
//!    damage dealt this way."
//!
//! Root cause: the `DealDamage` amount parsed as `Power { scope: Source }`.
//! The source of a resolving Sorcery has no power (it is not a creature), so
//! the spell dealt 0 damage and gained 0 life. The explicit possessive "the
//! sacrificed creature's power" must resolve to the sacrificed creature
//! (CR 608.2k) — `Power { scope: CostPaidObject }`. The bug was the
//! subject-injection rewrite (`rewrite_event_source_power_to_object_power`)
//! unconditionally coercing `CostPaidObject` -> `Source`; the fix narrows that
//! rewrite to the new `ObjectScope::Anaphoric` so explicit possessives are
//! never clobbered.
//!
//! CR 608.2k: an ability's effect referring to a specific untargeted object
//!   previously referred to by that ability's cost still affects it.
//! CR 120.3a: damage dealt to a player by a source without infect reduces
//!   that player's life total.
//! CR 119.3: an effect that causes a player to gain life changes the life
//!   total upward.

use engine::types::ability::{Effect, ObjectScope, QuantityExpr, QuantityRef, TargetRef};
use engine::types::actions::GameAction;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::{PayCostKind, WaitingFor};

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;

const RITE_TEXT: &str = "As an additional cost to cast this spell, sacrifice a creature.\n\
     Rite of Consumption deals damage equal to the sacrificed creature's power to \
     target player or planeswalker. You gain life equal to the damage dealt this way.";

/// CR 608.2k + CR 120.3a + CR 119.3 — the headline end-to-end test. Drives the
/// FULL cast pipeline: a 4/4 creature is sacrificed as Rite's additional cost,
/// the spell resolves, and the opponent must take exactly 4 damage (20 -> 16)
/// while the caster gains exactly 4 life (20 -> 24). With the fix reverted the
/// `DealDamage` amount is `Power { Source }` — a Sorcery has no power, so the
/// opponent takes 0 and the caster gains 0; this test is reverted-fix
/// discriminating.
#[test]
fn rite_of_consumption_deals_sacrificed_power_and_gains_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rite_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Rite of Consumption", false, RITE_TEXT)
        .id();
    let beast_id = scenario.add_creature(P0, "Beast", 4, 4).id();

    // {1}{B} for Rite of Consumption.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let rite_card_id = runner.state().objects[&rite_id].card_id;

    // Parse-shape assertion — the explicit possessive must resolve to the
    // cost-paid object, NOT the source, and must never carry the parse-only
    // `Anaphoric` scope. This is the static contract the runtime depends on.
    {
        let ability = &runner.state().objects[&rite_id].abilities[0];
        assert!(
            matches!(
                ability.effect.as_ref(),
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::CostPaidObject,
                        },
                    },
                    ..
                },
            ),
            "Rite's DealDamage amount must reference Power {{ CostPaidObject }}; got {:?}",
            ability.effect,
        );
    }

    let life_p0_before = runner.state().players[P0.0 as usize].life;
    let life_p1_before = runner.state().players[P1.0 as usize].life;
    assert_eq!(life_p0_before, 20);
    assert_eq!(life_p1_before, 20);

    runner
        .act(GameAction::CastSpell {
            object_id: rite_id,
            card_id: rite_card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Rite of Consumption must succeed");

    // Drive the interactive cast prompts: sacrifice the Beast for the
    // additional cost, then target the opponent. Bounded loop guards a stall.
    let mut sacrificed = false;
    let mut targeted = false;
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices: permanents,
                ..
            } => {
                assert!(
                    permanents.contains(&beast_id),
                    "the Beast must be a legal sacrifice for Rite's additional cost",
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![beast_id],
                    })
                    .expect("sacrificing the Beast for Rite's additional cost must succeed");
                sacrificed = true;
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Player(P1)),
                    "the opponent must be a legal target for Rite",
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("targeting the opponent must succeed");
                targeted = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state during Rite cast: {other:?}"),
        }
    }

    assert!(sacrificed, "the sacrifice-for-cost prompt must have fired");
    assert!(targeted, "the target-selection prompt must have fired");

    // CR 120.3a: the opponent took exactly 4 damage (the Beast's power).
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_p1_before - 4,
        "opponent must lose exactly 4 life — the sacrificed 4/4's power",
    );
    // CR 119.3: the caster gained exactly 4 life (life equal to damage dealt).
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_p0_before + 4,
        "caster must gain exactly 4 life — equal to the damage dealt",
    );
}
