//! Fling — damage equal to the sacrificed creature's power, to any target.
//!
//! Verbatim Oracle:
//!   As an additional cost to cast this spell, sacrifice a creature.
//!   Fling deals damage equal to the sacrificed creature's power to any target.
//!
//! These tests drive the REAL parser (`add_spell_to_hand_from_oracle`) and the
//! REAL cast pipeline (scenario runner). The composition under test is the
//! required sacrifice additional cost plus `DealDamage { Power { CostPaidObject } }`
//! (CR 608.2k: the sacrificed creature is the object the cost referred to).
//! Sibling coverage: Rite of Consumption pins the same quantity to a player
//! target with a life-gain rider; these rows pin "any target" (player and
//! creature) with no rider.
//!
//! Revert map: if the damage path stops reaching the cost-paid quantity arm,
//! the amount parses as `unparsed_quantity` (or resolves against the wrong
//! scope) and both rows fail — the player takes the wrong damage and the
//! opposing creature survives.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, ObjectScope, QuantityExpr, QuantityRef, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PayCostKind;

const FLING_ORACLE: &str = "As an additional cost to cast this spell, sacrifice a creature.\n\
     Fling deals damage equal to the sacrificed creature's power to any target.";

/// Stage Fling in hand ({1}{R} paid from the pool), a P0 creature with the
/// given power/toughness to sacrifice, and optionally a P1 blocker. Returns
/// the runner, the spell, the victim, and the blocker (if staged).
fn setup(
    power: i32,
    toughness: i32,
    blocker: Option<(i32, i32)>,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let fling = scenario
        .add_spell_to_hand_from_oracle(P0, "Fling", true, FLING_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    let victim = scenario
        .add_creature(P0, "Fling Fodder", power, toughness)
        .id();
    let blocker = blocker
        .map(|(power, toughness)| scenario.add_creature(P1, "Blocker", power, toughness).id());

    (scenario.build(), fling, victim, blocker)
}

/// Assert the parsed shape the runtime depends on, then cast Fling sacrificing
/// `victim` and targeting `target`. Drives the PayCost, TargetSelection, and
/// priority prompts to full resolution.
fn cast_fling_at(runner: &mut GameRunner, fling: ObjectId, victim: ObjectId, target: TargetRef) {
    let ability = &runner.state().objects[&fling].abilities[0];
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
        "Fling's DealDamage amount must reference Power {{ CostPaidObject }}; got {:?}",
        ability.effect,
    );

    let fling_card_id = runner.state().objects[&fling].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: fling,
            card_id: fling_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Fling must succeed");

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
                    permanents.contains(&victim),
                    "the victim must be a legal sacrifice for Fling's additional cost"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![victim],
                    })
                    .expect("sacrificing the victim must succeed");
                sacrificed = true;
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0].legal_targets.contains(&target),
                    "the chosen target must be legal for Fling's any-target"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![target.clone()],
                    })
                    .expect("targeting must succeed");
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
            other => panic!("unexpected waiting state during Fling cast: {other:?}"),
        }
    }
    assert!(sacrificed, "the sacrifice-for-cost prompt must have fired");
    assert!(targeted, "the target-selection prompt must have fired");
    assert!(
        runner.state().stack.is_empty(),
        "Fling must be fully resolved"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "the additional-cost victim must be sacrificed"
    );
}

/// CR 120.3a: a sacrificed 4/4 flung at the opponent deals exactly 4 (20 -> 16).
#[test]
fn fling_deals_sacrificed_power_to_player() {
    let (mut runner, fling, victim, _) = setup(4, 4, None);
    let life_before = runner.state().players[P1.0 as usize].life;
    assert_eq!(life_before, 20);

    cast_fling_at(&mut runner, fling, victim, TargetRef::Player(P1));

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 4,
        "opponent must lose exactly 4 life — the sacrificed 4/4's power",
    );
}

/// CR 120.3e + CR 704.5g: a sacrificed 3/3 flung at a 2/2 destroys it by lethal
/// damage (0 power would leave it alive, so survival discriminates the amount).
#[test]
fn fling_deals_sacrificed_power_to_creature() {
    let (mut runner, fling, victim, blocker) = setup(3, 3, Some((2, 2)));
    let blocker = blocker.expect("blocker staged");
    cast_fling_at(&mut runner, fling, victim, TargetRef::Object(blocker));

    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Graveyard,
        "the 2/2 must die to 3 flung damage"
    );
}
