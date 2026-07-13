//! Merieke Ri Berit — "When Merieke Ri Berit leaves the battlefield or becomes
//! untapped, destroy that creature. It can't be regenerated."
//!
//! The "It can't be regenerated" rider must bind to the `Destroy` nested inside
//! the leaves-battlefield/becomes-untapped delayed trigger (CR 608.2c), so the
//! stolen creature's destruction BYPASSES any regeneration shield (CR 701.19c).
//! These tests drive the full activation pipeline and assert only on
//! zone/state deltas — never on the AST `cant_regenerate` flag.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, DelayedTriggerCondition, Effect, EffectScope,
    ReplacementDefinition, TapStateChange, TargetFilter, TypedFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const MERIEKE_ORACLE: &str = "Merieke Ri Berit doesn't untap during your untap step.\n\
{T}: Gain control of target creature for as long as you control Merieke Ri Berit. When Merieke \
Ri Berit leaves the battlefield or becomes untapped, destroy that creature. It can't be regenerated.";

/// Build a scenario with a shielded victim (P1) and Merieke (P0). The victim
/// carries a live regeneration-shield replacement definition so that a Destroy
/// with `cant_regenerate: false` would be saved by the shield — the exact
/// behavior these tests discriminate against.
fn scenario_with_merieke_and_shielded_victim() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 701.19a: a 2/2 creature carrying a one-shot regeneration shield.
    let victim = scenario
        .add_creature(P1, "Shielded Bear", 2, 2)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Destroy)
                .valid_card(TargetFilter::SelfRef)
                .description("Regenerate".to_string())
                .regeneration_shield(),
        )
        .id();
    let merieke = scenario
        .add_creature_from_oracle(P0, "Merieke Ri Berit", 1, 1, MERIEKE_ORACLE)
        .id();
    (scenario, merieke, victim)
}

/// Add a P0 source with a `{T}`-cost activated ability carrying `effect`,
/// targeting a single creature. Used for the untapper and the assassin.
fn add_targeted_ability_source(
    scenario: &mut GameScenario,
    name: &str,
    effect: Effect,
) -> ObjectId {
    scenario
        .add_creature(P0, name, 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, effect).cost(AbilityCost::Tap),
        )
        .id()
}

fn untap_effect() -> Effect {
    Effect::SetTapState {
        target: TargetFilter::Typed(TypedFilter::creature()),
        scope: EffectScope::Single,
        state: TapStateChange::Untap,
    }
}

fn destroy_effect() -> Effect {
    Effect::Destroy {
        target: TargetFilter::Typed(TypedFilter::creature()),
        cant_regenerate: false,
    }
}

/// Assert Merieke's `{T}` activation registered exactly one delayed trigger and
/// that it is the `WhenNextEvent { or_trigger: Some }` disjunction (leaves the
/// battlefield OR becomes untapped). This is the reach-guard proving the
/// downstream zone assertions exercise the real delayed-destroy path.
fn assert_single_disjunctive_delayed_trigger(runner: &engine::game::scenario::GameRunner) {
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Merieke's {{T}} must register exactly one delayed destroy trigger"
    );
    match &runner.state().delayed_triggers[0].condition {
        DelayedTriggerCondition::WhenNextEvent { or_trigger, .. } => assert!(
            or_trigger.is_some(),
            "the delayed trigger must carry the leaves-battlefield/becomes-untapped disjunction"
        ),
        other => panic!("expected WhenNextEvent delayed trigger, got {other:?}"),
    }
}

/// PRIMARY discriminating test — becomes-untapped branch. Merieke gains control
/// of the shielded victim, then is untapped through the real activation
/// pipeline. The "It can't be regenerated" rider (now bound to the delayed
/// Destroy) makes the destruction bypass the still-functional regeneration
/// shield, so the victim ends in the graveyard. Reverting the parser fix leaves
/// `cant_regenerate: false`, the shield applies, and this assertion flips (see
/// the `regeneration_shield_saves_victim_from_plain_destroy` control).
#[test]
fn merieke_destroy_bypasses_regeneration_shield_on_untap_branch() {
    let (mut scenario, merieke, victim) = scenario_with_merieke_and_shielded_victim();
    let untapper = add_targeted_ability_source(&mut scenario, "Untapper", untap_effect());
    let mut runner = scenario.build();

    // Activate Merieke's {T}: gain control of the victim and register the
    // delayed destroy. The {T} cost taps Merieke.
    runner.activate(merieke, 0).target_object(victim).resolve();
    assert_single_disjunctive_delayed_trigger(&runner);

    // Untap Merieke through the real pipeline → becomes-untapped branch fires
    // the un-regenerable destroy on the stolen creature.
    let outcome = runner
        .activate(untapper, 0)
        .target_object(merieke)
        .resolve();

    // CR 701.19c: the regeneration shield is not applied — the victim dies.
    outcome.assert_zone(&[victim], Zone::Graveyard);
}

/// or_trigger proof — leaves-battlefield branch. Same setup, but Merieke is
/// destroyed instead of untapped. The OTHER branch of the (unmodified)
/// disjunctive delayed trigger fires the now-un-regenerable destroy, so the
/// stolen creature still ends in the graveyard despite its functional shield.
#[test]
fn merieke_destroy_fires_on_leaves_battlefield_branch() {
    let (mut scenario, merieke, victim) = scenario_with_merieke_and_shielded_victim();
    let assassin = add_targeted_ability_source(&mut scenario, "Assassin", destroy_effect());
    let mut runner = scenario.build();

    runner.activate(merieke, 0).target_object(victim).resolve();
    assert_single_disjunctive_delayed_trigger(&runner);

    // Remove Merieke from the battlefield → leaves-battlefield branch fires the
    // un-regenerable destroy on the stolen creature.
    let outcome = runner
        .activate(assassin, 0)
        .target_object(merieke)
        .resolve();

    outcome.assert_zone(&[merieke], Zone::Graveyard);
    // CR 701.19c: the stolen creature is destroyed and NOT saved by its shield.
    outcome.assert_zone(&[victim], Zone::Graveyard);
}

/// CONTROL for the discriminating gate. A plain `Destroy` with
/// `cant_regenerate: false` against the SAME shielded victim is saved by the
/// regeneration shield: the creature survives on the battlefield, tapped, with
/// marked damage cleared (CR 701.19a/b). This proves the shield is genuinely
/// functional, so the graveyard results above are caused specifically by
/// Merieke's `cant_regenerate` rider — not by an absent or broken shield.
#[test]
fn regeneration_shield_saves_victim_from_plain_destroy() {
    let (mut scenario, _merieke, victim) = scenario_with_merieke_and_shielded_victim();
    let assassin = add_targeted_ability_source(&mut scenario, "Assassin", destroy_effect());
    let mut runner = scenario.build();

    let outcome = runner.activate(assassin, 0).target_object(victim).resolve();

    // CR 701.19a/b: the shield regenerates the creature — it survives, tapped.
    outcome.assert_zone(&[victim], Zone::Battlefield);
    assert!(
        outcome.state().objects[&victim].tapped,
        "CR 701.19b: a regenerated creature is tapped"
    );
}
