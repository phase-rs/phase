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

// ---------------------------------------------------------------------------
// CR 608.2c: a resolving spell's "counter that spell" names the spell's earlier
// announced target (the `ParentTarget` anaphor), not a trigger event.
// ---------------------------------------------------------------------------

const STUBBORN_DENIAL_ORACLE: &str = "Counter target noncreature spell unless its controller pays \
     {1}.\nFerocious — If you control a creature with power 4 or greater, counter that spell instead.";

/// Drives the real cast->resolve pipeline: P0 casts Mox Pearl, P1 answers with
/// Stubborn Denial while controlling a 4/4 (Ferocious). The "instead" body's
/// `Counter { target: ParentTarget }` must counter the announced spell.
#[test]
fn stubborn_denial_ferocious_counters_the_announced_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Mox Pearl ({0} Artifact, "{T}: Add {W}.") is a printed zero-mana-cost
    // noncreature PERMANENT spell: a resolved artifact enters the battlefield,
    // while a countered one goes to the graveyard (CR 608.3a vs CR 701.6a), so the
    // final zone discriminates "countered" from "resolved".
    let mut target_spell =
        scenario.add_artifact_to_hand_from_oracle(P0, "Mox Pearl", "{T}: Add {W}.");
    target_spell.with_mana_cost(ManaCost::zero());
    let target = target_spell.id();

    // P1's Ferocious enabler and the answer.
    scenario.add_creature(P1, "Big Beast", 4, 4);
    let mut denial =
        scenario.add_spell_to_hand_from_oracle(P1, "Stubborn Denial", true, STUBBORN_DENIAL_ORACLE);
    denial.with_mana_cost(ManaCost::zero());
    let denial_id = denial.id();

    let mut runner = scenario.build();

    let target_card = runner.state().objects[&target].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: target,
            card_id: target_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the target artifact spell");
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");

    let denial_card = runner.state().objects[&denial_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: denial_id,
            card_id: denial_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Stubborn Denial");

    while !runner.state().stack.is_empty() {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state while resolving: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().objects.get(&target).map(|o| o.zone),
        Some(Zone::Graveyard),
        "Ferocious Stubborn Denial must counter the spell it targeted (a resolved artifact would be on the battlefield)"
    );
}
