//! MSH Wave 3 — Mjölnir, Hammer of Thor: "Double all damage equipped creature
//! would deal." (CR 614.1a static damage-modification replacement; CR 301.5 /
//! CR 702.6 Equipment host link via `TargetFilter::AttachedTo`.)
//!
//! Two layers are guarded here:
//!   1. The runtime CLASSIFICATION GUARD (plan §85-98): a hand-built
//!      `DamageDone + Double + AttachedTo` replacement on an Equipment must
//!      double the equipped creature's damage with NO runtime change. This
//!      confirms `damage_source_filter = AttachedTo` resolves through the
//!      `matches_target_filter` source-filter path (replacement.rs:3996), not
//!      just the `evaluate_replacement_condition` path the prior proof test
//!      (:8642) exercised. If this fails, Mjölnir is NOT parser-only.
//!   2. The parser extension: the actual parsed Mjölnir line produces that same
//!      replacement and doubles damage end-to-end; negatives confirm
//!      `AttachedTo` scoping (unattached / non-equipped creatures unaffected).

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{DamageModification, ReplacementDefinition, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;

use super::rules::run_combat;

const MJOLNIR_DOUBLE_LINE: &str = "Double all damage equipped creature would deal.";

fn parse_card(oracle_text: &str, card_name: &str, types: &[&str]) -> ParsedAbilities {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle_text, card_name, &[], &types, &[])
}

/// Attach `equipment` to `host` in the live state (mirrors how the equip action
/// wires `attached_to` + `attachments`; CR 301.5).
fn attach(runner: &mut GameRunner, equipment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&equipment).unwrap().attached_to = Some(AttachTarget::Object(host));
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(equipment);
}

fn double_equipped_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .damage_modification(DamageModification::Double)
        .damage_source_filter(TargetFilter::AttachedTo)
}

/// CLASSIFICATION GUARD: with a hand-built `Double + AttachedTo` replacement on
/// an Equipment attached to the attacker, the equipped creature's 2 combat
/// damage doubles to 4 with NO runtime change. Revert-fail surface: if
/// `AttachedTo`-as-source-filter were unwired, the replacement would never
/// match and `damage_marked` would be 2.
#[test]
fn attached_to_source_filter_doubles_equipped_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Equipped Bear", 2, 2).id();
    // 0/5 wall survives the doubled 4 so we can read marked damage post-combat.
    let blocker = scenario.add_creature(P1, "Stone Wall", 0, 5).id();
    let equipment = scenario
        .add_creature(P0, "Mjolnir", 0, 0)
        .as_artifact()
        .with_replacement_definition(double_equipped_replacement())
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, equipment, attacker);

    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);

    let marked = runner.state().objects.get(&blocker).unwrap().damage_marked;
    assert_eq!(
        marked, 4,
        "CR 614.1a: equipped creature's 2 damage must double to 4 via AttachedTo source filter"
    );
}

/// Negative scope: the SAME replacement on an UNATTACHED equipment must not
/// double — `AttachedTo` has no host, so the source filter never matches.
#[test]
fn unattached_equipment_does_not_double_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Lone Bear", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Stone Wall", 0, 5).id();
    // Equipment present but never attached.
    scenario
        .add_creature(P0, "Mjolnir", 0, 0)
        .as_artifact()
        .with_replacement_definition(double_equipped_replacement());

    let mut runner = scenario.build();

    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);

    let marked = runner.state().objects.get(&blocker).unwrap().damage_marked;
    assert_eq!(
        marked, 2,
        "unattached equipment has no AttachedTo host: damage must stay 2"
    );
}

/// Parser-driven runtime: the REAL parsed Mjölnir line produces the doubling
/// replacement, which doubles the equipped creature's damage. Revert-fail: if
/// the parser arm regresses, `parse_card` yields no Double/AttachedTo
/// replacement, the installed list is empty, and damage stays 2.
#[test]
fn mjolnir_parsed_line_doubles_equipped_damage() {
    let parsed = parse_card(
        MJOLNIR_DOUBLE_LINE,
        "Mjolnir, Hammer of Thor",
        &["Artifact"],
    );
    let repl = parsed
        .replacements
        .iter()
        .find(|r| r.damage_modification == Some(DamageModification::Double))
        .expect("Mjolnir line must parse to a Double damage replacement")
        .clone();
    assert_eq!(
        repl.damage_source_filter,
        Some(TargetFilter::AttachedTo),
        "parsed Mjolnir replacement must scope to the equipped (attached) creature"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Equipped Bear", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Stone Wall", 0, 5).id();
    let equipment = scenario
        .add_creature(P0, "Mjolnir", 0, 0)
        .as_artifact()
        .with_replacement_definition(repl)
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, equipment, attacker);

    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);

    let marked = runner.state().objects.get(&blocker).unwrap().damage_marked;
    assert_eq!(
        marked, 4,
        "parsed Mjolnir line must double equipped damage to 4"
    );
}

/// Negative scope: a non-equipped creature controlled by the same player is NOT
/// doubled — `AttachedTo` matches only the host, not arbitrary controlled
/// creatures.
#[test]
fn mjolnir_non_equipped_creature_not_doubled() {
    let parsed = parse_card(
        MJOLNIR_DOUBLE_LINE,
        "Mjolnir, Hammer of Thor",
        &["Artifact"],
    );
    let repl = parsed
        .replacements
        .iter()
        .find(|r| r.damage_modification == Some(DamageModification::Double))
        .expect("Mjolnir line must parse to a Double damage replacement")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let equipped = scenario.add_creature(P0, "Equipped Bear", 2, 2).id();
    let unequipped = scenario.add_creature(P0, "Plain Bear", 2, 2).id();
    let blocker_a = scenario.add_creature(P1, "Wall A", 0, 5).id();
    let blocker_b = scenario.add_creature(P1, "Wall B", 0, 5).id();
    let equipment = scenario
        .add_creature(P0, "Mjolnir", 0, 0)
        .as_artifact()
        .with_replacement_definition(repl)
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, equipment, equipped);

    run_combat(
        &mut runner,
        vec![equipped, unequipped],
        vec![(blocker_a, equipped), (blocker_b, unequipped)],
    );

    let equipped_target = runner
        .state()
        .objects
        .get(&blocker_a)
        .unwrap()
        .damage_marked;
    let unequipped_target = runner
        .state()
        .objects
        .get(&blocker_b)
        .unwrap()
        .damage_marked;
    assert_eq!(
        equipped_target, 4,
        "equipped creature's damage doubles to 4"
    );
    assert_eq!(
        unequipped_target, 2,
        "non-equipped creature's damage is NOT doubled (AttachedTo host scoping)"
    );
}

/// Parser unit: the static line parses to exactly the expected typed shape.
#[test]
fn mjolnir_parses_double_attached_to_replacement() {
    let parsed = parse_card(
        MJOLNIR_DOUBLE_LINE,
        "Mjolnir, Hammer of Thor",
        &["Artifact"],
    );
    let repl = parsed
        .replacements
        .iter()
        .find(|r| r.damage_modification == Some(DamageModification::Double))
        .expect("expected a Double damage-modification replacement");
    assert_eq!(repl.event, ReplacementEvent::DamageDone);
    assert_eq!(repl.damage_source_filter, Some(TargetFilter::AttachedTo));
    assert!(
        repl.combat_scope.is_none(),
        "Mjolnir doubles ALL damage, not combat-only"
    );
}
