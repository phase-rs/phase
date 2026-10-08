//! Regression for issue #5949 — `ParentTarget` must stay on "each of them"
//! counter lines instead of mis-promoting to `PutCounterAll`.
//!
//! Three coverage-parse-diff cards share the parser fix; two resolver paths:
//!
//! | Card | Route | Oracle fragment |
//! |------|-------|-----------------|
//! | Vrestin, Menoptra Leader | Attack-batch (`parent_target_refs_from_attack_trigger_context`) | attack trigger, no chosen targets |
//! | Heroic Feast | Selected targets (`ability.targets` on `PutCounter { ParentTarget }`) | lifegain trigger, choose up to that many |
//! | Twisted Riddlekeeper | Selected targets (same as Heroic Feast) | cast trigger, tap up to two |
//!
//! The generic counter parser wrongly promoted "on each of them" to mass
//! `PutCounterAll { target: TriggeringSource }`, which matches nothing at
//! resolution. The fix excludes `TargetFilter::ParentTarget` from the shared
//! mass-classification branch in `oracle_effect/mod.rs` (matching imperative
//! authority) and keeps `"each of them"` as `ParentTarget` in
//! `oracle_target.rs` even when a typed trigger subject is in scope.
//!
//! https://github.com/phase-rs/phase/issues/5949

use engine::game::effects::life::apply_life_gain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::AttackTarget;

const ATTACK_TRIGGER: &str =
    "Whenever you attack with one or more Insects, put a +1/+1 counter on each of them.";

const VRESTIN_ORACLE: &str = "Flying\n\
Vrestin enters with X +1/+1 counters on it.\n\
When Vrestin enters, create X 1/1 green and white Alien Insect creature tokens with flying.\n\
Whenever you attack with one or more Insects, put a +1/+1 counter on each of them.";

const HEROIC_FEAST_LIFEGAIN: &str = "Whenever you gain life, choose up to that many target creatures you control. Put a +1/+1 counter on each of them.";

const HEROIC_FEAST_ORACLE: &str = "When this enchantment enters, create a Food token.\n\
Whenever you gain life, choose up to that many target creatures you control. Put a +1/+1 counter on each of them.";

const TWISTED_RIDDLEKEEPER_CAST: &str = "When you cast this spell, tap up to two target permanents. Put a stun counter on each of them.";

fn put_counter_target_in_ability(ability: &AbilityDefinition) -> Option<&TargetFilter> {
    put_counter_target_in_effect(ability.effect.as_ref()).or_else(|| {
        ability
            .sub_ability
            .as_ref()
            .and_then(|sub| put_counter_target_in_ability(sub))
    })
}

fn put_counter_target_in_effect(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::PutCounter { target, .. } => Some(target),
        _ => None,
    }
}

#[test]
fn vrestin_attack_trigger_parses_as_parent_target_put_counter() {
    let parsed = parse_oracle_text(VRESTIN_ORACLE, "Vrestin, Menoptra Leader", &[], &[], &[]);
    let attack_trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::YouAttack)
        .expect("Vrestin should have a YouAttack trigger");
    assert!(
        attack_trigger.batched,
        "'one or more Insects' is a batched attack trigger"
    );
    let execute = attack_trigger.execute.as_ref().expect("execute");
    match execute.effect.as_ref() {
        // CR 508.1 + CR 608.2c: the batch anaphor "each of them" must lower to
        // the singular PutCounter over the ParentTarget batch — NOT a mass
        // PutCounterAll bound to the single-object TriggeringSource anaphor.
        Effect::PutCounter { target, .. } => {
            assert_eq!(*target, TargetFilter::ParentTarget);
        }
        other => panic!("expected PutCounter {{ target: ParentTarget }}, got {other:?}"),
    }
}

#[test]
fn heroic_feast_lifegain_trigger_parses_parent_target_put_counter() {
    let parsed = parse_oracle_text(HEROIC_FEAST_ORACLE, "Heroic Feast", &[], &[], &[]);
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::LifeGained)
        .expect("Heroic Feast should have a LifeGained trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    let target = put_counter_target_in_ability(execute).unwrap_or_else(|| {
        panic!(
            "expected a PutCounter sub-effect, got {:?}",
            execute.effect.as_ref()
        )
    });
    assert_eq!(
        *target,
        TargetFilter::ParentTarget,
        "Heroic Feast's 'each of them' counter must stay ParentTarget, not PutCounterAll"
    );
}

#[test]
fn twisted_riddlekeeper_cast_trigger_parses_parent_target_put_counter() {
    let parsed = parse_oracle_text(
        TWISTED_RIDDLEKEEPER_CAST,
        "Twisted Riddlekeeper",
        &[],
        &[],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::SpellCast)
        .expect("Twisted Riddlekeeper should have a SpellCast trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    let target = put_counter_target_in_ability(execute).unwrap_or_else(|| {
        panic!(
            "expected a PutCounter sub-effect, got {:?}",
            execute.effect.as_ref()
        )
    });
    assert_eq!(
        *target,
        TargetFilter::ParentTarget,
        "Twisted Riddlekeeper's stun 'each of them' must stay ParentTarget"
    );
}

fn resolve_attack_triggers(runner: &mut GameRunner) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..count).collect(),
                    })
                    .expect("order triggers");
            }
            other => panic!("unexpected waiting state during attack triggers: {other:?}"),
        }
    }
    panic!("attack triggers did not resolve");
}

fn plus1_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object exists")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn vrestin_puts_counter_on_each_attacking_insect_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Vrestin (the ability source) — an Insect that carries the attack trigger.
    let vrestin = scenario
        .add_creature(P0, "Vrestin, Menoptra Leader", 2, 2)
        .with_subtypes(vec!["Insect"])
        .from_oracle_text(ATTACK_TRIGGER)
        .id();

    // Two more Insects controlled by the attacking player.
    let insect_a = scenario
        .add_creature(P0, "Alien Insect A", 1, 1)
        .with_subtypes(vec!["Insect"])
        .id();
    let insect_b = scenario
        .add_creature(P0, "Alien Insect B", 1, 1)
        .with_subtypes(vec!["Insect"])
        .id();

    // A non-Insect attacker — must NOT receive a counter (CR 508.1 batch is
    // gated by the trigger's Insect subject filter).
    let bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();

    let attack_pairs: Vec<_> = [vrestin, insect_a, insect_b, bear]
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attack_pairs)
        .expect("declare attackers");

    resolve_attack_triggers(&mut runner);

    assert_eq!(
        plus1_counters(&runner, vrestin),
        1,
        "Vrestin (attacking Insect) must get a +1/+1 counter"
    );
    assert_eq!(
        plus1_counters(&runner, insect_a),
        1,
        "attacking Insect A must get a +1/+1 counter"
    );
    assert_eq!(
        plus1_counters(&runner, insect_b),
        1,
        "attacking Insect B must get a +1/+1 counter"
    );
    assert_eq!(
        plus1_counters(&runner, bear),
        0,
        "the non-Insect attacker must NOT get a +1/+1 counter"
    );
}

/// Pass priority until the lifegain trigger asks for targets, or panic.
fn advance_to_lifegain_trigger_targets(runner: &mut GameRunner) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                return;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..count).collect(),
                    })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while waiting for lifegain trigger targets");
                if runner.state().stack.is_empty() {
                    panic!(
                        "stack emptied before lifegain trigger target selection; waiting_for = {:?}",
                        runner.state().waiting_for
                    );
                }
            }
            other => panic!("unexpected waiting state before lifegain trigger: {other:?}"),
        }
    }
    panic!("lifegain trigger never reached target selection");
}

/// CR 119.3 + CR 603.2 + CR 608.2c: Heroic Feast's selected-target route —
/// gain 2 life, choose two of three creatures, and only the chosen pair receive
/// the ParentTarget counter. The unselected sibling must stay at zero counters.
#[test]
fn heroic_feast_puts_counter_on_each_chosen_creature_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _feast = scenario
        .add_creature(P0, "Heroic Feast", 0, 0)
        .as_enchantment()
        .from_oracle_text(HEROIC_FEAST_LIFEGAIN)
        .id();

    let chosen_a = scenario.add_creature(P0, "Chosen A", 2, 2).id();
    let chosen_b = scenario.add_creature(P0, "Chosen B", 2, 2).id();
    let unchosen = scenario.add_creature(P0, "Unchosen C", 2, 2).id();

    let mut runner = scenario.build();

    let mut events = Vec::new();
    let gained =
        apply_life_gain(runner.state_mut(), P0, 2, &mut events).expect("life gain must resolve");
    assert_eq!(
        gained, 2,
        "precondition: gain exactly 2 life for 'that many'"
    );
    process_triggers(runner.state_mut(), &events);

    advance_to_lifegain_trigger_targets(&mut runner);

    let mut guard = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
    ) {
        guard += 1;
        assert!(guard < 10, "target selection did not terminate");

        let target = match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { selection, .. }
            | WaitingFor::TargetSelection { selection, .. } => match selection.current_slot {
                0 => Some(TargetRef::Object(chosen_a)),
                1 => Some(TargetRef::Object(chosen_b)),
                _ => None,
            },
            _ => None,
        };
        runner
            .act(GameAction::ChooseTarget { target })
            .expect("ChooseTarget for Heroic Feast lifegain trigger");
    }

    runner.advance_until_stack_empty();

    assert_eq!(
        plus1_counters(&runner, chosen_a),
        1,
        "chosen A must receive a +1/+1 counter from ParentTarget resolution"
    );
    assert_eq!(
        plus1_counters(&runner, chosen_b),
        1,
        "chosen B must receive a +1/+1 counter from ParentTarget resolution"
    );
    assert_eq!(
        plus1_counters(&runner, unchosen),
        0,
        "the unchosen creature must NOT receive a counter — ParentTarget is the chosen set"
    );
}
