//! Regression for issue #2361: spells with "unless [cost]" must offer the
//! alternative payment before applying the primary effect.
//!
//! https://github.com/phase-rs/phase/issues/2361

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const WRENCH_MIND_ORACLE: &str =
    "Target player discards two cards unless they discard an artifact card.";

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn hand_len(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

#[test]
fn wrench_mind_parsed_ability_carries_unless_pay() {
    let mut scenario = GameScenario::new();
    let wrench = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrench Mind", false, WRENCH_MIND_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let runner = scenario.build();
    let ability = &runner.state().objects[&wrench].abilities[0];
    let unless_pay = ability
        .unless_pay
        .as_ref()
        .expect("Wrench Mind spell ability must carry unless_pay (#2361)");
    assert_eq!(unless_pay.payer, TargetFilter::Player);
    assert!(matches!(unless_pay.cost, AbilityCost::Discard { .. }));
}

#[test]
fn wrench_mind_cast_resolve_stops_at_unless_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..7 {
        scenario.add_spell_to_hand(P1, &format!("P1 Card {i}"), true);
    }
    let wrench = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrench Mind", false, WRENCH_MIND_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, &[ManaType::Black, ManaType::Black]);

    runner.cast(wrench).target_player(P1).resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "SpellCast driver must stop at UnlessPayment for manual choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(hand_len(&runner, P1), 7);
}

#[test]
fn wrench_mind_resolution_surfaces_unless_payment_before_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1 holds seven cards so the primary effect would matter.
    for i in 0..7 {
        scenario.add_spell_to_hand(P1, &format!("P1 Card {i}"), true);
    }

    let wrench = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrench Mind", false, WRENCH_MIND_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, &[ManaType::Black, ManaType::Black]);

    runner
        .act(engine::types::actions::GameAction::CastSpell {
            object_id: wrench,
            card_id: runner.state().objects[&wrench].card_id,
            targets: vec![],
        })
        .expect("cast Wrench Mind");

    // Drive through targeting.
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose P1 as target");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected pre-resolution prompt: {other:?}"),
        }
    }

    // Resolve the spell from the stack.
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "Wrench Mind must offer unless-cost payment before discarding two cards, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        hand_len(&runner, P1),
        7,
        "no cards should be discarded before the unless choice"
    );
}
