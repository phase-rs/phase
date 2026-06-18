//! Ertai's Trickery — "Counter target spell if it was kicked."
//!
//! Parser regression: the trailing intervening-if must lower to
//! `AdditionalCostPaid`, not remain as swallowed `Condition_If` text.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AdditionalCost, AdditionalCostRepeatability, Effect,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ERTAI_TRICKERY_ORACLE: &str = "Counter target spell if it was kicked.";

#[test]
fn ertai_trickery_parses_counter_with_kicked_condition() {
    let parsed = parse_oracle_text(
        ERTAI_TRICKERY_ORACLE,
        "Ertai's Trickery",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("Ertai's Trickery must parse a spell ability");
    assert!(matches!(ability.effect.as_ref(), Effect::Counter { .. }));
    assert!(matches!(
        ability.condition.as_ref(),
        Some(AbilityCondition::AdditionalCostPaid { .. })
    ));
}

/// Drive Ertai's Trickery through the real cast->resolve pipeline against a
/// kicked / un-kicked target spell, returning whether the target was countered.
///
/// The "kicked" state is produced authentically: P0 casts a creature spell with
/// a kicker via the real `CastSpell` -> `DecideOptionalCost` path, so the target
/// object's `kickers_paid` is populated by the engine (verified populated:
/// `[First]` when paid, `[]` when skipped). P1 then counters it with Ertai's
/// Trickery.
fn counter_with_ertai(pay_kicker: bool) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 (active player) controls a kickable creature spell: {1}, kicker {2}.
    let target = scenario
        .add_creature_to_hand(P0, "Kickable Brute", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .with_additional_cost(AdditionalCost::Kicker {
            costs: vec![AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                },
            }],
            repeatability: AdditionalCostRepeatability::Once,
        })
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    // P1 holds Ertai's Trickery ({U}).
    let mut ertai =
        scenario.add_spell_to_hand_from_oracle(P1, "Ertai's Trickery", true, ERTAI_TRICKERY_ORACLE);
    ertai.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue],
    });
    let ertai_id = ertai.id();
    scenario.add_basic_land(P1, ManaColor::Blue);

    let mut runner = scenario.build();

    // P0 casts the target spell, paying or skipping the kicker.
    let target_card = runner.state().objects[&target].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: target,
            card_id: target_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the target spell");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalCost { pay: pay_kicker })
            .expect("P0 decides the kicker");
    }

    // P0 passes priority; P1 responds with Ertai's Trickery (auto-targets the
    // only spell on the stack).
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    let ertai_card = runner.state().objects[&ertai_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ertai_id,
            card_id: ertai_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Ertai's Trickery");

    // Resolve the whole stack.
    while !runner.state().stack.is_empty() {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state while resolving: {other:?}"),
        }
    }

    runner.state().objects.get(&target).map(|o| o.zone) == Some(Zone::Graveyard)
}

/// CR 702.33d + CR 608.2c + CR 115.1: Ertai's Trickery counters the target *only*
/// when the target spell was kicked.
///
/// The parser lowers "if it was kicked" to `AbilityCondition::AdditionalCostPaid`
/// and `retarget_counter_additional_cost_to_target` rewrites its `subject` to
/// `ObjectScope::Target` because the effect is `Effect::Counter`. At resolution,
/// `evaluate_condition` then reads the *target spell's* `kickers_paid` (CR 115.1)
/// rather than Ertai's own empty context, so the counter fires iff the target was
/// kicked. Discriminating: if the `subject` axis is reverted to `Source`, both
/// branches read Ertai's empty context, the condition is always false, and the
/// `assert!(counter_with_ertai(true), ...)` below fails (kicked target resolves
/// uncountered to the battlefield).
#[test]
fn ertai_trickery_counters_only_kicked_target() {
    assert!(
        counter_with_ertai(true),
        "kicked target must be countered by Ertai's Trickery"
    );
    assert!(
        !counter_with_ertai(false),
        "un-kicked target must resolve — the counter's condition fails"
    );
}
