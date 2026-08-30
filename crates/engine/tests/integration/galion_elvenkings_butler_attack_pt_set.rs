//! Galion, Elvenking's Butler (HOB) — runtime + parser-shape proof.
//!
//! Oracle: "Whenever Galion attacks, choose up to one other target creature
//! you control. Its base power and toughness become equal to Galion's power
//! and toughness until end of turn."
//!
//! CR 508.1 + CR 508.2: attack triggers fire when attackers are declared.
//! CR 115.6 + CR 601.2c: "choose up to one ... target" permits declining —
//!   zero targets may be chosen.
//! "another target creature you control" excludes Galion itself (the source)
//!   from the legal target set via the `Another` filter property; CR 115.10a
//!   only distinguishes targets from non-targets and does not itself govern
//!   this exclusion.
//! CR 208.4a + CR 613.4b + CR 608.2c: "Its base power and toughness become
//!   equal to [source]'s power and toughness" is a characteristic-setting
//!   effect (Layer 7b) that reads the source's power/toughness dynamically —
//!   the bare possessive pronoun "Its" resolves to the target chosen by the
//!   first sentence (CR 608.2c: the controller follows the ability's
//!   instructions in the order written and applies the rules of English),
//!   and "Galion's" resolves to the source.
//!
//! Before the fix, the second sentence's subject grammar only recognized a
//! NAMED possessor ("~'s base power ...", "Sita Varma's base power ..."), not
//! the bare possessive pronoun "Its base power ...", and the copula-frame
//! value grammar only recognized a single quantity after "equal to", not a
//! paired "<X>'s power and toughness" referent. Both gaps left the clause
//! unparsed (`Effect::Unimplemented`), so the P/T-set never fired. These
//! tests drive the fix through the real attack -> trigger -> target ->
//! resolution pipeline (not just the parsed AST).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ContinuousModification, Effect, FilterProp, ObjectScope, QuantityExpr, QuantityRef,
    TargetFilter, TargetRef, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::AttackTarget;

const GALION_ORACLE: &str = "Whenever Galion attacks, choose up to one other target creature you control. Its base power and toughness become equal to Galion's power and toughness until end of turn.";

/// Effective (post-layer) power/toughness of an object.
fn power_toughness(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

/// Drive from a just-declared attack through `OrderTriggers` up to the attack
/// trigger's `TriggerTargetSelection` prompt, WITHOUT answering it yet — the
/// self-exclusion test needs to inspect the offered set first.
fn drive_to_attack_trigger_target(runner: &mut GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering Galion's attack trigger should succeed");
            }
            WaitingFor::TriggerTargetSelection { .. } => return,
            other => panic!("unexpected waiting_for before the target prompt: {other:?}"),
        }
    }
    panic!("expected Galion's attack trigger to request a target");
}

/// -----------------------------------------------------------------------
/// Parser SHAPE test — the assembled AST, zero `Effect::Unimplemented`.
/// -----------------------------------------------------------------------
#[test]
fn galion_attack_trigger_parses_choose_and_pt_set_shape() {
    let parsed = parse_oracle_text(
        GALION_ORACLE,
        "Galion, Elvenking's Butler",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Elf".to_string(), "Advisor".to_string()],
    );

    let attack = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("Galion must have an attacks trigger");

    let execute = attack.execute.as_ref().expect("attack trigger execute");

    // First clause: "choose up to one other target creature you control" ->
    // TargetOnly with multi_target { min: 0, max: 1 } and an Another+Typed
    // filter restricted to creatures the controller controls.
    let Effect::TargetOnly { target } = execute.effect.as_ref() else {
        panic!("expected TargetOnly, got {:?}", execute.effect);
    };
    let TargetFilter::Typed(typed) = target else {
        panic!("expected Typed filter, got {target:?}");
    };
    assert!(
        typed.type_filters.contains(&TypeFilter::Creature),
        "must filter to creatures, got {:?}",
        typed.type_filters
    );
    assert!(
        typed.properties.contains(&FilterProp::Another),
        "'other' must exclude Galion itself via the Another filter property, got {:?}",
        typed.properties
    );
    let spec = execute
        .multi_target
        .as_ref()
        .expect("'choose up to one' must carry a multi_target spec");
    assert!(
        spec.min.clone() == QuantityExpr::Fixed { value: 0 },
        "'up to one' must allow zero targets (min == 0), got {:?}",
        spec.min
    );
    assert_eq!(
        spec.max,
        Some(QuantityExpr::Fixed { value: 1 }),
        "'up to one' caps at exactly one target"
    );

    // Second clause: "Its base power and toughness become equal to Galion's
    // power and toughness" -> GenericEffect with SetPowerDynamic /
    // SetToughnessDynamic reading the SOURCE's power/toughness, applied to
    // ParentTarget (the creature chosen by the first clause), for the
    // duration of the turn.
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("the P/T-set clause must chain after the choose-target clause");
    assert_eq!(
        sub.duration,
        Some(engine::types::ability::Duration::UntilEndOfTurn),
        "the trailing 'until end of turn' duration must be preserved"
    );
    let Effect::GenericEffect {
        static_abilities, ..
    } = sub.effect.as_ref()
    else {
        panic!(
            "expected GenericEffect (zero Unimplemented), got {:?}",
            sub.effect
        );
    };
    assert!(
        !static_abilities.is_empty(),
        "must carry at least one static ability"
    );
    let def = &static_abilities[0];
    assert_eq!(
        def.affected,
        Some(TargetFilter::ParentTarget),
        "the P/T set must apply to the creature chosen by the first clause \
         (CR 608.2c 'Its' anaphor), got {:?}",
        def.affected
    );
    assert!(
        def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::SetPowerDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source
                    }
                }
            }
        )),
        "expected SetPowerDynamic(Power{{scope: Source}}) reading Galion's own \
         power, got {:?}",
        def.modifications
    );
    assert!(
        def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::SetToughnessDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Toughness {
                        scope: ObjectScope::Source
                    }
                }
            }
        )),
        "expected SetToughnessDynamic(Toughness{{scope: Source}}) reading \
         Galion's own toughness, got {:?}",
        def.modifications
    );
}

/// -----------------------------------------------------------------------
/// Runtime — choosing the other creature sets its P/T to Galion's until EOT.
/// -----------------------------------------------------------------------
#[test]
fn galion_attack_choosing_other_creature_sets_its_pt_to_galions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Galion is printed 4/4.
    let galion = scenario
        .add_creature_from_oracle(P0, "Galion, Elvenking's Butler", 4, 4, GALION_ORACLE)
        .id();
    let buddy = scenario.add_creature(P0, "Buddy", 2, 2).id();

    let mut runner = scenario.build();

    assert_eq!(power_toughness(&mut runner, galion), (4, 4), "printed P/T");
    assert_eq!(power_toughness(&mut runner, buddy), (2, 2), "printed P/T");

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(galion, AttackTarget::Player(P1))])
        .expect("declare Galion as attacker");

    drive_to_attack_trigger_target(&mut runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(buddy)),
        })
        .expect("choosing Buddy for the up-to-one target should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&mut runner, buddy),
        (4, 4),
        "Buddy's base P/T must become equal to Galion's (4/4) after the trigger resolves"
    );
    assert_eq!(
        power_toughness(&mut runner, galion),
        (4, 4),
        "Galion's own P/T must be unaffected by its own attack trigger"
    );
}

/// -----------------------------------------------------------------------
/// Runtime — declining the "up to one" target leaves the other creature
/// unaffected.
/// -----------------------------------------------------------------------
#[test]
fn galion_attack_declining_target_leaves_other_creature_unaffected() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let galion = scenario
        .add_creature_from_oracle(P0, "Galion, Elvenking's Butler", 4, 4, GALION_ORACLE)
        .id();
    let buddy = scenario.add_creature(P0, "Buddy", 2, 2).id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(galion, AttackTarget::Player(P1))])
        .expect("declare Galion as attacker");

    drive_to_attack_trigger_target(&mut runner);
    runner
        .act(GameAction::ChooseTarget { target: None })
        .expect("declining the up-to-one target should succeed (CR 115.6)");
    runner.advance_until_stack_empty();

    assert_eq!(
        power_toughness(&mut runner, buddy),
        (2, 2),
        "declining the optional target must leave Buddy's P/T untouched"
    );
    assert_eq!(
        power_toughness(&mut runner, galion),
        (4, 4),
        "Galion's own P/T is unaffected either way"
    );
}

/// -----------------------------------------------------------------------
/// Runtime — "another target creature you control" excludes Galion itself
/// from the legal target set via the Another filter property.
/// -----------------------------------------------------------------------
#[test]
fn galion_attack_trigger_cannot_target_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let galion = scenario
        .add_creature_from_oracle(P0, "Galion, Elvenking's Butler", 4, 4, GALION_ORACLE)
        .id();
    let buddy = scenario.add_creature(P0, "Buddy", 2, 2).id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(galion, AttackTarget::Player(P1))])
        .expect("declare Galion as attacker");

    drive_to_attack_trigger_target(&mut runner);
    let WaitingFor::TriggerTargetSelection { selection, .. } = runner.state().waiting_for.clone()
    else {
        panic!("expected TriggerTargetSelection");
    };
    assert!(
        !selection
            .current_legal_targets
            .contains(&TargetRef::Object(galion)),
        "Galion must not be offered as its own 'other' target, got {:?}",
        selection.current_legal_targets
    );
    assert!(
        selection
            .current_legal_targets
            .contains(&TargetRef::Object(buddy)),
        "Buddy is a legal 'other target creature you control', got {:?}",
        selection.current_legal_targets
    );

    // Explicitly attempting to select Galion itself must be rejected.
    let rejected = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(galion)),
    });
    assert!(
        rejected.is_err(),
        "selecting Galion itself for its own 'other target creature' must be illegal"
    );
}
