//! Regression: GitHub issue #1504 — Baleful Mastery must not require targeting an
//! opponent when casting. "An opponent draws a card" is a resolution-time
//! opponent choice; only "exile target creature or planeswalker" is a cast target.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{TargetSelectionSlot, WaitingFor};
use engine::types::ability::TargetRef;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const BALEFUL_MASTERY_ORACLE: &str = "You may pay {1}{B} rather than pay this spell's mana cost.\n\
If the {1}{B} cost was paid, an opponent draws a card.\n\
Exile target creature or planeswalker.";

fn opponent_only_player_slot(slot: &TargetSelectionSlot) -> bool {
    slot.legal_targets.iter().all(|t| {
        matches!(t, TargetRef::Player(pid) if *pid != P0)
    }) && !slot.legal_targets.is_empty()
}

fn assert_cast_target_slots(slots: &[TargetSelectionSlot]) {
    assert_eq!(
        slots.len(),
        1,
        "Baleful Mastery should have exactly one cast-time target (exile), got {slots:?}"
    );
    assert!(
        !opponent_only_player_slot(&slots[0]),
        "the sole target slot must not be opponent-player-only: {:?}",
        slots[0].legal_targets
    );
    assert!(
        slots[0]
            .legal_targets
            .iter()
            .any(|t| matches!(t, TargetRef::Object(_))),
        "exile target must allow object targets, got {:?}",
        slots[0].legal_targets
    );
}

/// CR 601.2c + CR 115.1: Cast-time targets are only the exile — not an opponent.
#[test]
fn baleful_mastery_cast_targeting_excludes_opponent_player_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bear", 2, 2).id();

    let mut spell_builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Baleful Mastery",
        true,
        BALEFUL_MASTERY_ORACLE,
    );
    spell_builder.with_mana_cost(ManaCost::Cost {
        generic: 3,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    });
    let spell = spell_builder.id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
            ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
            ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
            ManaUnit::new(ManaType::Blue, spell, false, vec![]),
            ManaUnit::new(ManaType::Blue, spell, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
        })
        .expect("cast should start");

    for _ in 0..16 {
        match &runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: false })
                    .expect("optional cost decision should succeed");
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert_cast_target_slots(target_slots);
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(bear)],
                    })
                    .expect("exile target selection should succeed");
            }
            WaitingFor::ManaPayment { .. } | WaitingFor::Priority { .. } => return,
            _ => runner.pass_both_players(),
        }
    }

    panic!(
        "cast pipeline did not finish; last waiting_for = {:?}",
        runner.state().waiting_for
    );
}
