//! S07 Batch 4 — damage-doubler value-modifier replacements (CR 614.1a / CR 120.8).
//!
//! Card 1 — Trance Kuja, Fate Defied:
//!   "Flare Star — If a Wizard you control would deal damage to a permanent or
//!    player, it deals double that damage instead." — an UNCONDITIONAL
//!   damage-increase replacement scoped to a Wizard you control. Parser output
//!   was already correct; this increment only clears a spurious `Condition_If`
//!   swallow warning (asserted in `swallow_check.rs`). These runtime tests guard
//!   that the card genuinely functions (source-filter doubling), so the coverage
//!   flip is not hollow.
//!
//! Card 2 — The Rollercrusher Ride:
//!   "Delirium — If a source you control would deal noncombat damage to a
//!    permanent or player while there are four or more card types among cards in
//!    your graveyard, it deals double that damage instead." + ETB "deals X damage
//!    to each of up to X target creatures." — a GATED damage-increase replacement
//!   (CombatDamageScope::NoncombatOnly, delirium `OnlyIfQuantity` threshold). This
//!   increment captures the `while` delirium gate that was previously dropped;
//!   the (c) and (e) tests below fail if that capture is reverted.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    CombatDamageScope, Comparator, ControllerRef, DamageModification, Effect, QuantityExpr,
    QuantityRef, ReplacementCondition, ReplacementDefinition, TargetFilter,
};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;

use super::rules::run_combat;

const TRANCE_KUJA_LINE: &str = "Flare Star — If a Wizard you control would deal damage to a \
     permanent or player, it deals double that damage instead.";

const ROLLERCRUSHER_ORACLE: &str =
    "Delirium — If a source you control would deal noncombat damage to a permanent or player \
     while there are four or more card types among cards in your graveyard, it deals double \
     that damage instead.\n\
     When The Rollercrusher Ride enters, it deals X damage to each of up to X target creatures.";

fn parse_card(oracle: &str, name: &str, types: &[&str]) -> ParsedAbilities {
    let types: Vec<String> = types.iter().map(|s| (*s).to_string()).collect();
    parse_oracle_text(oracle, name, &[], &types, &[])
}

fn double_damage_replacement(oracle: &str, name: &str, types: &[&str]) -> ReplacementDefinition {
    parse_card(oracle, name, types)
        .replacements
        .into_iter()
        .find(|r| r.damage_modification == Some(DamageModification::Double))
        .expect("expected a Double damage-modification replacement")
}

/// Put four distinct card types (Creature / Instant / Sorcery / Artifact) into
/// P0's graveyard so the delirium threshold (four or more) is met.
fn seed_four_gy_types(scenario: &mut GameScenario) {
    scenario.add_creature_to_graveyard(P0, "Grave Creature", 1, 1);
    scenario
        .add_creature_to_graveyard(P0, "Grave Instant", 1, 1)
        .as_instant();
    scenario
        .add_creature_to_graveyard(P0, "Grave Sorcery", 1, 1)
        .as_sorcery();
    scenario
        .add_creature_to_graveyard(P0, "Grave Artifact", 1, 1)
        .as_artifact();
}

// ── Card 1 — Trance Kuja ────────────────────────────────────────────────

/// Positive: a Wizard you control deals 2 combat damage → doubled to 4 via the
/// `Typed{Wizard, You}` source filter. Revert surface: if the parsed replacement
/// regresses (no Double / no Wizard-You source filter), the damage stays 2.
#[test]
fn trance_kuja_wizard_source_damage_doubles() {
    let repl = double_damage_replacement(
        TRANCE_KUJA_LINE,
        "Trance Kuja, Fate Defied",
        &["Legendary", "Creature", "Avatar", "Wizard"],
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wizard = scenario
        .add_creature(P0, "Wizard Attacker", 2, 2)
        .with_subtypes(vec!["Wizard"])
        .id();
    let blocker = scenario.add_creature(P1, "Stone Wall", 0, 5).id();
    // Kuja holds the replacement but does not itself attack; the doubler must
    // apply to *any* Wizard you control, not only its own source.
    scenario
        .add_creature(P0, "Trance Kuja", 1, 1)
        .with_subtypes(vec!["Avatar", "Wizard"])
        .with_replacement_definition(repl);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![wizard], vec![(blocker, wizard)]);

    let marked = runner.state().objects.get(&blocker).unwrap().damage_marked;
    assert_eq!(
        marked, 4,
        "CR 614.1a: a Wizard you control's 2 damage must double to 4"
    );
}

/// Negative scope: a non-Wizard source you control is NOT doubled — the
/// `Subtype: Wizard` source filter must reject it.
#[test]
fn trance_kuja_non_wizard_source_not_doubled() {
    let repl = double_damage_replacement(
        TRANCE_KUJA_LINE,
        "Trance Kuja, Fate Defied",
        &["Legendary", "Creature", "Avatar", "Wizard"],
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Attacker is a Bear (no Wizard subtype) you control.
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Stone Wall", 0, 5).id();
    scenario
        .add_creature(P0, "Trance Kuja", 1, 1)
        .with_subtypes(vec!["Avatar", "Wizard"])
        .with_replacement_definition(repl);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![bear], vec![(blocker, bear)]);

    let marked = runner.state().objects.get(&blocker).unwrap().damage_marked;
    assert_eq!(
        marked, 2,
        "a non-Wizard source must not be doubled (Wizard source filter)"
    );
}

// ── Card 2 — The Rollercrusher Ride ─────────────────────────────────────

/// Parse gate (e): the delirium `while` antecedent must be captured as an
/// `OnlyIfQuantity{DistinctCardTypes, GE, 4}` condition (guards silent-drop).
/// Revert surface: without the `parse_while_antecedent` generalization + wire-up,
/// `condition` is `None` and this assertion fails.
#[test]
fn rollercrusher_parses_delirium_gated_noncombat_doubler() {
    let repl = double_damage_replacement(
        ROLLERCRUSHER_ORACLE,
        "The Rollercrusher Ride",
        &["Enchantment"],
    );

    assert_eq!(repl.event, ReplacementEvent::DamageDone);
    assert_eq!(repl.combat_scope, Some(CombatDamageScope::NoncombatOnly));
    match &repl.damage_source_filter {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        other => panic!("expected Typed source filter scoped to You, got {other:?}"),
    }
    match &repl.condition {
        Some(ReplacementCondition::OnlyIfQuantity {
            lhs:
                QuantityExpr::Ref {
                    qty: QuantityRef::DistinctCardTypes { .. },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
            ..
        }) => {}
        other => panic!("expected delirium OnlyIfQuantity(DistinctCardTypes GE 4), got {other:?}"),
    }
}

/// Install the parsed Rollercrusher delirium replacement on a battlefield
/// enchantment you control (mirrors the Mjölnir install-on-battlefield pattern;
/// the replacement is a source-agnostic "a source you control" doubler).
fn install_rollercrusher_doubler(scenario: &mut GameScenario) {
    let repl = double_damage_replacement(
        ROLLERCRUSHER_ORACLE,
        "The Rollercrusher Ride",
        &["Enchantment"],
    );
    scenario
        .add_creature(P0, "The Rollercrusher Ride", 0, 0)
        .as_enchantment()
        .with_replacement_definition(repl);
}

/// (a) With four+ card types in the graveyard the delirium gate is TRUE, so a
/// noncombat 3-damage burn from a source you control (Lightning Bolt) doubles to
/// 6 via the installed replacement.
#[test]
fn rollercrusher_noncombat_doubles_when_delirium_met() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_four_gy_types(&mut scenario);
    install_rollercrusher_doubler(&mut scenario);
    let target = scenario.add_creature(P1, "Big Wall", 0, 10).id();
    let bolt = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    runner.cast(bolt).target_object(target).resolve();

    let marked = runner.state().objects.get(&target).unwrap().damage_marked;
    assert_eq!(
        marked, 6,
        "delirium met: 3 noncombat damage from a source you control must double to 6"
    );
}

/// (c) DISCRIMINATING for the delirium capture: with fewer than four card types
/// in the graveyard the gate is FALSE, so the doubler does not apply and the
/// 3-damage burn stays 3. Revert the `while`-gate capture (condition → None) and
/// this doubles to 6, failing the test.
#[test]
fn rollercrusher_noncombat_not_doubled_when_delirium_unmet() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Only one card type in the graveyard → delirium NOT met.
    scenario.add_creature_to_graveyard(P0, "Lone Creature", 1, 1);
    install_rollercrusher_doubler(&mut scenario);
    let target = scenario.add_creature(P1, "Big Wall", 0, 10).id();
    let bolt = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    runner.cast(bolt).target_object(target).resolve();

    let marked = runner.state().objects.get(&target).unwrap().damage_marked;
    assert_eq!(
        marked, 3,
        "delirium unmet: gate false, 3 noncombat damage must stay 3 (not doubled)"
    );
}

/// (b) Combat scope: even with delirium met, the doubler is `NoncombatOnly`, so
/// combat damage from a source you control is NOT doubled. Revert surface: if
/// `combat_scope` were dropped, this would double to 4.
#[test]
fn rollercrusher_combat_damage_not_doubled() {
    let repl = double_damage_replacement(
        ROLLERCRUSHER_ORACLE,
        "The Rollercrusher Ride",
        &["Enchantment"],
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_four_gy_types(&mut scenario);
    let attacker = scenario.add_creature(P0, "Combatant", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Stone Wall", 0, 5).id();
    scenario
        .add_creature(P0, "The Rollercrusher Ride", 0, 0)
        .as_enchantment()
        .with_replacement_definition(repl);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);

    let marked = runner.state().objects.get(&blocker).unwrap().damage_marked;
    assert_eq!(
        marked, 2,
        "NoncombatOnly: combat damage must not double even with delirium met"
    );
}

/// (d) ETB X-damage shape: "When ~ enters, it deals X damage to each of up to X target
/// creatures" parses to a `DealDamage{CostXPaid}` over a `0..=X` multi-target set.
///
/// CR 107.3i — BOTH X sites must bind to the same value. This assertion previously pinned
/// `multi_target.max` as a bare `QuantityRef::Variable{"X"}` while `amount` was already
/// `CostXPaid`: the same X, lowered two different ways. That asymmetry was the bug, not the
/// spec. `multi_target` is consumed at TARGET SELECTION, before the trigger resolves and
/// before `current_trigger_event` is set, so an unbound `Variable("X")` there resolves to
/// **0** — "up to 0 target creatures". The Ride dealt its damage to nobody while rendering as
/// fully supported, and this test pinned that as correct.
///
/// Both slots now bind to `CostXPaid` (t96). If `max` ever reverts to `Variable("X")`, the
/// card silently stops targeting anything again.
#[test]
fn rollercrusher_etb_parses_x_damage_multitarget() {
    let parsed = parse_card(
        ROLLERCRUSHER_ORACLE,
        "The Rollercrusher Ride",
        &["Enchantment"],
    );
    let exec = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_ref())
        .find(|e| matches!(&*e.effect, Effect::DealDamage { .. }))
        .expect("ETB must parse to a DealDamage trigger");

    match &*exec.effect {
        Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            ..
        } => {}
        other => panic!("expected DealDamage with CostXPaid amount, got {other:?}"),
    }
    let multi = exec
        .multi_target
        .as_ref()
        .expect("ETB deals to each of up to X targets → multi_target present");
    assert_eq!(multi.min, QuantityExpr::Fixed { value: 0 });
    assert_eq!(
        multi.max,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid
        }),
        "CR 107.3i: the target-count X must bind to the same paid X as the damage amount. A \
         bare Variable(\"X\") here resolves to 0 at target selection — 'up to 0 target \
         creatures' — so the ETB damages nobody."
    );
}
