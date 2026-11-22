//! Ichormoon Gauntlet (ONE):
//!
//! "Planeswalkers you control have "[0]: Proliferate" and "[−12]: Take an
//! extra turn after this one."
//! Whenever you cast a noncreature spell, choose a counter on target permanent.
//! Put an additional counter of that kind on that permanent."
//!
//! Witness for the cast trigger — the same "choose a counter on target ..."
//! clause Dathon, His Heart Open and Clockspinning already exercise. CR 603.3d:
//! the trigger's target permanent is chosen when the trigger is put on the
//! stack (above the spell that cast it). CR 608.2d + CR 122.1 / CR 122.6: at
//! resolution the controller picks one kind among those already on the target
//! and exactly one more counter of that kind is put on it. Both kinds' final
//! counts are asserted, so an implementation that ignores the kind answer — or
//! grows the wrong kind — cannot pass.

use crate::support::shared_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityDefinition, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

/// Reach guard, mirroring the Dathon file: the Gauntlet's cast trigger must
/// parse with no `Unimplemented` residue anywhere in its effect chain.
fn contains_unimplemented(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
}

/// P0 controls Ichormoon Gauntlet and a blank enchantment filler carrying one
/// Stun and one Shield counter (added through the scenario's counter API, as
/// the Dathon file does), and holds a real Lightning Bolt with its printed {R}
/// already in the pool.
fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gauntlet = scenario.add_real_card(P0, "Ichormoon Gauntlet", Zone::Battlefield, db);
    let carrier = scenario
        .add_enchantment_from_oracle(P0, "Test Enchantment", "")
        .id();
    scenario.with_counter(carrier, CounterType::Stun, 1);
    scenario.with_counter(carrier, CounterType::Shield, 1);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    // Lightning Bolt's printed {R}: auto-payment draws it from the pool
    // (spell_cost_is_payable_from_pool -> CastPaymentMode::Auto, no stop).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let runner = scenario.build();
    let triggers = runner.state().objects[&gauntlet].trigger_definitions.as_slice();
    assert!(
        triggers.iter().any(|entry| entry.definition.execute.is_some())
            && !triggers.iter().any(|entry| entry
                .definition
                .execute
                .as_deref()
                .is_some_and(contains_unimplemented)),
        "Ichormoon Gauntlet's cast trigger must parse with an effect and no Unimplemented residue: {triggers:?}"
    );
    (runner, gauntlet, carrier, bolt)
}

/// Pass priority until the stack drains after the counter-kind answer.
fn settle(runner: &mut GameRunner) {
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
}

/// CR 601.2 + CR 603.3d + CR 608.2d + CR 122.6: casting the noncreature
/// Lightning Bolt puts the Gauntlet's trigger on the stack with a
/// target-permanent prompt for P0; resolving it asks P0 for the counter KIND
/// (exactly one option per kind already on the target); only the chosen kind
/// grows by exactly one. The trigger sits above the Bolt, so the kind prompt
/// must open before the Bolt can resolve — a driver that drains the stack
/// without the prompt fails the loop's empty-stack guard.
#[test]
fn noncreature_cast_prompts_kind_and_only_chosen_kind_grows() {
    let (mut runner, gauntlet, carrier, bolt) = setup();
    let p1_life_before = runner.life(P1);

    // CR 601.2: real production cast of Lightning Bolt aimed at P1.
    let card_id = runner
        .state()
        .objects
        .get(&bolt)
        .expect("Lightning Bolt object exists")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![TargetRef::Player(P1)],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Lightning Bolt at P1");

    // CR 603.3d: the trigger is dispatched while the cast completes and parks
    // on its target prompt; P0 (the Gauntlet's controller) must answer it.
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Gauntlet trigger must prompt for its target permanent, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "P0 controls the Gauntlet and chooses its trigger target"
    );
    let slot = &target_slots[selection.current_slot];
    assert!(
        slot.legal_targets.contains(&TargetRef::Object(carrier)),
        "the counter carrier must be a legal target permanent: {:?}",
        slot.legal_targets
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(carrier)),
        })
        .expect("target the counter carrier");

    // The trigger is on top of the Bolt; passing priority resolves it and its
    // counter-kind prompt must open. If the stack drains instead, the
    // implementation skipped the kind choice and this guard fires.
    for _ in 0..10 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                assert!(
                    !runner.state().stack.is_empty(),
                    "the Gauntlet trigger resolved without a counter-kind prompt"
                );
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("expected the counter-kind prompt, got {other:?}"),
        }
    }
    let WaitingFor::NamedChoice { player, options, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Gauntlet trigger must ask for the counter kind, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "P0 chooses the counter kind");
    assert_eq!(
        options.len(),
        2,
        "exactly one option per kind already on the carrier: {options:?}"
    );
    assert!(
        options.iter().any(|o| o.eq_ignore_ascii_case("stun"))
            && options.iter().any(|o| o.eq_ignore_ascii_case("shield")),
        "the offered kinds must be exactly Stun and Shield: {options:?}"
    );
    let choice = options
        .iter()
        .find(|o| o.eq_ignore_ascii_case("shield"))
        .cloned()
        .expect("Shield is an option");
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("answer the counter-kind choice");
    settle(&mut runner);

    // CR 122.6: exactly one more counter of the CHOSEN kind. Both counts are
    // asserted, so choosing (or applying) Stun instead fails here.
    assert_eq!(
        counters(&runner, carrier, CounterType::Shield),
        2,
        "the chosen Shield kind grew by exactly one"
    );
    assert_eq!(
        counters(&runner, carrier, CounterType::Stun),
        1,
        "the unchosen Stun kind is unchanged"
    );
    // The noncreature spell itself then resolved: CR 120.3, Bolt's 3 damage to P1.
    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "Lightning Bolt resolved on P1 after the trigger"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        runner.state().objects[&gauntlet].zone,
        Zone::Battlefield,
        "the Gauntlet itself is untouched"
    );
}
