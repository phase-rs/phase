//! CR 601.2b + CR 120.3 + CR 608.2c: Cinder Strike — "As an additional cost to
//! cast this spell, you may blight 1. Cinder Strike deals 2 damage to target
//! creature. It deals 4 damage to that creature instead if this spell's
//! additional cost was paid."
//!
//! Exercises the inverted additional-cost "instead" wiring: the line-level
//! `strip_instead_clause` defers the intra-chain "instead if <additional cost>"
//! to the chain, where `strip_additional_cost_conditional` folds it to the
//! dedicated `AdditionalCostPaidInstead` gating the 4-damage else_ability.
//! Reverting either edit drops the condition, so the spell would deal 2 damage
//! even when blight was paid — failing `assert_eq!(paid, 4)`.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const CINDER: &str = "As an additional cost to cast this spell, you may blight 1. \
(You may put a -1/-1 counter on a creature you control.)\n\
Cinder Strike deals 2 damage to target creature. It deals 4 damage to that \
creature instead if this spell's additional cost was paid.";

fn damage(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

fn run(pay_blight: bool) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Blight fodder for P0 (the optional -1/-1 counter recipient).
    let fodder = scenario.add_creature(P0, "Fodder", 3, 3).id();
    // Target survives 4 damage so `damage_marked` stays observable.
    let target = scenario.add_creature(P1, "Target", 2, 9).id();
    let cinder = scenario
        .add_spell_to_hand_from_oracle(P0, "Cinder Strike", false, CINDER)
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&cinder].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: cinder,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Cinder Strike");

    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("finalize mana");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: pay_blight })
                    .expect("decide blight");
            }
            WaitingFor::BlightChoice { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .expect("pay blight");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("resolve");
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }
    damage(&runner, target)
}

#[test]
fn blight_paid_deals_four_declined_deals_two() {
    let declined = run(false);
    let paid = run(true);
    assert_eq!(declined, 2, "declined blight → base 2 damage");
    assert_eq!(
        paid, 4,
        "CR 601.2b: paying the blight additional cost → 4 damage instead"
    );
}
