//! CR 701.60b + CR 608.2c: Agency Coroner — "{2}{B}, Sacrifice another creature:
//! Draw a card. If the sacrificed creature was suspected, draw two cards instead."
//!
//! Exercises the B3 suspected-through-LKI plumbing end-to-end: the sacrificed
//! creature is gone from the battlefield (and `is_suspected` resets on the
//! zone change), so `CostPaidObjectMatchesFilter { Suspected }` must read the
//! cost-paid LKI snapshot latched at sacrifice time. Reverting either the
//! `FilterProp::Suspected => record.is_suspected` matcher arm or the
//! `is_suspected` field on `LKISnapshot`/`ZoneChangeRecord` flips the suspected
//! arm back to a 1-card draw, failing `assert_eq!(suspected_draw, 2)`.

use engine::game::effects::resolve_effect;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{Effect, EffectScope, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0);

const CORONER: &str = "{2}{B}, Sacrifice another creature: Draw a card. \
If the sacrificed creature was suspected, draw two cards instead.";

/// Suspect a battlefield creature via the real Suspect effect (CR 701.60a).
fn suspect(runner: &mut GameRunner, id: ObjectId) {
    let ability = ResolvedAbility::new(
        Effect::Suspect {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
        },
        vec![TargetRef::Object(id)],
        ObjectId(9_001),
        P0,
    );
    let mut events = Vec::new();
    resolve_effect(runner.state_mut(), &ability, &mut events).expect("suspect resolves");
}

fn hand_len(runner: &GameRunner, p: PlayerId) -> usize {
    runner.state().players[p.0 as usize].hand.len()
}

fn run(suspect_victim: bool) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two library cards so the "draw two" arm has cards to draw.
    scenario.add_card_to_library_top(P0, "Draw A");
    scenario.add_card_to_library_top(P0, "Draw B");
    // {2}{B}: three black mana cover the B pip and the two generic.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );
    let coroner = scenario
        .add_creature_from_oracle(P0, "Agency Coroner", 2, 2, CORONER)
        .id();
    let victim = scenario.add_creature(P0, "Victim", 1, 1).id();
    let mut runner = scenario.build();
    if suspect_victim {
        suspect(&mut runner, victim);
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: coroner,
            ability_index: 0,
        })
        .expect("activate Agency Coroner");

    let mut baseline: Option<usize> = None;
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("finalize mana");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![victim],
                    })
                    .expect("pay sacrifice cost");
            }
            WaitingFor::Priority { .. } => {
                let stack_empty = runner.state().stack.is_empty();
                if !stack_empty && baseline.is_none() {
                    baseline = Some(hand_len(&runner, P0));
                }
                if stack_empty && baseline.is_some() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }

    hand_len(&runner, P0) - baseline.expect("ability committed to the stack")
}

#[test]
fn sacrificing_a_suspected_creature_draws_two() {
    let suspected_draw = run(true);
    let plain_draw = run(false);
    // Negative sibling: a non-suspected sacrifice draws exactly one.
    assert_eq!(plain_draw, 1, "non-suspected sacrifice draws one card");
    // Positive: a suspected sacrifice draws two (the revert-failing assertion).
    assert_eq!(
        suspected_draw, 2,
        "CR 701.60b: sacrificing a suspected creature draws two cards"
    );
}
