//! Issue #3274 — Elder Deep-Fiend cast trigger must carry multi-target metadata
//! for "tap up to four target permanents" so casting does not stall or crash
//! the client during target selection.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, EffectScope, MultiTargetSpec, QuantityExpr, TapStateChange};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const ELDER_DEEP_FIEND_ORACLE: &str = "Flash\n\
Emerge {5}{U}{U} (You may cast this spell by sacrificing a creature and paying the emerge cost reduced by that creature's mana value.)\n\
When you cast this spell, tap up to four target permanents.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn elder_deep_fiend_cast_trigger_parses_up_to_four_multi_target() {
    let parsed = parse_oracle_text(
        ELDER_DEEP_FIEND_ORACLE,
        "Elder Deep-Fiend",
        &["Flash".to_string()],
        &["Creature".to_string()],
        &["Eldrazi".to_string(), "Octopus".to_string()],
    );
    let cast_trigger = parsed
        .triggers
        .iter()
        .find(|t| matches!(&t.mode, TriggerMode::SpellCast))
        .expect("Elder Deep-Fiend must have a When-you-cast trigger");
    let execute = cast_trigger
        .execute
        .as_ref()
        .expect("cast trigger must have execute ability");
    assert_eq!(
        execute.multi_target,
        Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 4 })),
        "tap up to four target permanents must stamp MultiTargetSpec::up_to(4)"
    );
    assert!(
        matches!(
            &*execute.effect,
            Effect::SetTapState {
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
                ..
            }
        ),
        "cast trigger effect must be single-target tap, got {:?}",
        execute.effect
    );
}

#[test]
fn elder_deep_fiend_cast_trigger_surfaces_four_optional_target_slots() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..4 {
        scenario.add_creature(P1, &format!("Defender {i}"), 1, 1);
    }

    let deep_fiend = scenario
        .add_creature_to_hand_from_oracle(P0, "Elder Deep-Fiend", 6, 6, ELDER_DEEP_FIEND_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            floating_mana(6, ManaType::Blue),
            floating_mana(5, ManaType::Colorless),
        ]
        .concat(),
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&deep_fiend].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: deep_fiend,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Elder Deep-Fiend must be castable from hand");

    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            assert_eq!(
                target_slots.len(),
                4,
                "tap up to four target permanents must surface four trigger target slots"
            );
            assert!(
                target_slots.iter().all(|slot| slot.optional),
                "all four slots must be optional for an up-to-four trigger"
            );
        }
        other => panic!(
            "expected TriggerTargetSelection for Elder Deep-Fiend cast trigger, got {other:?}"
        ),
    }
}
