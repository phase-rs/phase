//! Corrupted Resolve — "Counter target spell if its controller is poisoned."
//!
//! Drives the real cast->resolve pipeline: the counter fires iff the *target
//! spell's controller* is poisoned (CR 122.1f: one or more poison counters),
//! read via the target-relative `QuantityRef::TargetControllerCounter`.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCondition, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CORRUPTED_RESOLVE_ORACLE: &str = "Counter target spell if its controller is poisoned.";

#[test]
fn corrupted_resolve_parses_counter_with_poisoned_condition() {
    let parsed = parse_oracle_text(
        CORRUPTED_RESOLVE_ORACLE,
        "Corrupted Resolve",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("Corrupted Resolve must parse a spell ability");
    assert!(matches!(ability.effect.as_ref(), Effect::Counter { .. }));
    assert!(matches!(
        ability.condition.as_ref(),
        Some(AbilityCondition::QuantityCheck { .. })
    ));
}

/// Drive Corrupted Resolve through the real cast->resolve pipeline against a
/// target spell whose controller is / isn't poisoned; return whether the target
/// was countered (moved to the graveyard).
///
/// P0 (active) casts a plain creature spell; P1 responds with Corrupted Resolve
/// (auto-targets the only spell on the stack). "its controller" anaphors to the
/// *target spell's* controller — P0 — so we poison P0, never the caster P1. A
/// caster-scoped misread would leave both branches identical (P1 has 0 poison).
fn counter_with_corrupted_resolve(target_controller_poisoned: bool) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 (active player) controls a plain {1} creature spell to be countered.
    let target = scenario
        .add_creature_to_hand(P0, "Doomed Brute", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);

    // P1 holds Corrupted Resolve ({U}{B}).
    let mut cr = scenario.add_spell_to_hand_from_oracle(
        P1,
        "Corrupted Resolve",
        true,
        CORRUPTED_RESOLVE_ORACLE,
    );
    cr.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
    });
    let cr_id = cr.id();
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Black);

    let mut runner = scenario.build();

    // "its controller" == the target spell's controller (P0). Poison P0 (or not);
    // P1 (the caster) is never poisoned, so the read must be target-relative.
    if target_controller_poisoned {
        runner.state_mut().players[0].poison_counters = 1;
    }

    // P0 casts the target spell.
    let target_card = runner.state().objects[&target].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: target,
            card_id: target_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the target spell");

    // P0 passes priority; P1 responds with Corrupted Resolve.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    let cr_card = runner.state().objects[&cr_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: cr_id,
            card_id: cr_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Corrupted Resolve");

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

/// CR 122.1f + CR 109.4 + CR 115.1 + CR 608.2c: Corrupted Resolve counters the
/// target *only* when the target spell's controller is poisoned. Discriminating:
/// if the condition read the caster's (P1's) poison instead of the target
/// controller's (P0's), both branches would behave identically and one of these
/// asserts would fail.
#[test]
fn corrupted_resolve_counters_only_when_controller_poisoned() {
    assert!(
        counter_with_corrupted_resolve(true),
        "poisoned controller: target must be countered"
    );
    assert!(
        !counter_with_corrupted_resolve(false),
        "unpoisoned controller: target must resolve — the counter's condition fails"
    );
}
