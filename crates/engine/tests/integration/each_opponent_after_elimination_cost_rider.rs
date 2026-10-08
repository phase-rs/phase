//! CR 102.2 + CR 102.3 + CR 800.4a: "each opponent" in an activation's cost
//! rider means each opponent still in the game. A player who concedes leaves
//! the game, so a later activation compares only the remaining opponents, both
//! through the target-free condition evaluator and through the one that prices
//! a rider with the committed targets.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    CostReduction, ObjectScope, ParsedCondition, PlayerScope, QuantityExpr, QuantityRef, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::CostModifyMode;

const P2: PlayerId = PlayerId(2);

fn pool(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].mana_pool.total()
}

/// `lhs` < every opponent's life total: "{3}: Tap target creature. This
/// ability costs {2} less to activate if <lhs> is less than each opponent's
/// life total." P0 holds three cards, the target has power 3, P1 is at 20 life
/// and P2 at 2. Life, not a hand, because a player who leaves the game takes
/// the cards they own with them (CR 800.4a), and "less than" so that P2 blocks
/// the discount however its departed life is read: only the opponent filter
/// can drop P2 from the comparison.
fn board(lhs: QuantityRef) -> (GameRunner, ObjectId, ObjectId) {
    let mut s = GameScenario::new_n_player(3, 11);
    s.at_phase(Phase::PreCombatMain);
    for i in 0..3 {
        s.add_card_to_hand(P0, &format!("Card {i}"));
    }
    s.with_life(P1, 20);
    s.with_life(P2, 2);
    let target = s.add_creature(P1, "Target", 3, 3).id();
    let src = s
        .add_artifact_from_oracle(P0, "Tapper", "{3}: Tap target creature.")
        .id();
    s.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaColor::Blue.into(), ObjectId(0), false, Vec::new()))
            .collect(),
    );
    let mut r = s.build();
    let comparator = serde_json::from_str("\"LT\"").expect("the LT comparator");
    let rider = CostReduction {
        mode: CostModifyMode::Reduce,
        amount_per: 2,
        count: QuantityExpr::Fixed { value: 1 },
        condition: Some(ParsedCondition::QuantityVsEachOpponent {
            lhs,
            comparator,
            rhs: QuantityRef::LifeTotal {
                player: PlayerScope::ScopedPlayer,
            },
        }),
    };
    let obj = r.state_mut().objects.get_mut(&src).unwrap();
    std::sync::Arc::make_mut(&mut obj.base_abilities)[0].cost_reduction = Some(rider.clone());
    std::sync::Arc::make_mut(&mut obj.abilities)[0].cost_reduction = Some(rider);
    (r, src, target)
}

fn paid(lhs: QuantityRef, concede_p2: bool) -> usize {
    let (mut r, src, target) = board(lhs);
    if concede_p2 {
        r.act(GameAction::Concede { player_id: P2 })
            .expect("P2 concedes");
        assert!(
            r.state().players[P2.0 as usize].is_eliminated,
            "reach guard: P2 left the game"
        );
    }
    let before = pool(&r);
    r.act(GameAction::ActivateAbility {
        source_id: src,
        ability_index: 0,
    })
    .expect("the activation starts");
    if matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
        r.act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("the target settles");
    }
    for _ in 0..4 {
        if matches!(r.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            r.act(GameAction::PassPriority).expect("pay");
        }
    }
    assert_eq!(
        r.state().stack.len(),
        1,
        "reach guard: the ability was placed"
    );
    before - pool(&r)
}

#[test]
fn a_target_free_each_opponent_rider_ignores_a_player_who_conceded() {
    let your_hand = || QuantityRef::HandSize {
        player: PlayerScope::Controller,
    };
    assert_eq!(paid(your_hand(), false), 3, "P2's 2 life: no discount");
    assert_eq!(paid(your_hand(), true), 1, "only P1's 20 life is compared");
}

#[test]
fn a_target_reading_each_opponent_rider_ignores_a_player_who_conceded() {
    let target_power = || QuantityRef::Power {
        scope: ObjectScope::Target,
    };
    assert_eq!(paid(target_power(), false), 3, "P2's 2 life: no discount");
    assert_eq!(
        paid(target_power(), true),
        1,
        "only P1's 20 life is compared"
    );
}
