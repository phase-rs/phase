//! std-card BATCH 10 — "set base power/toughness to a dynamic count".
//!
//! These tests drive the layer system (`evaluate_layers`) for the base-P/T-set
//! continuous modifications routed by the parser to the existing layer-7b
//! `ContinuousModification::SetPowerDynamic`/`SetToughnessDynamic` variants
//! (CR 613.4b + CR 208.1). No new engine variant is introduced — the batch is a
//! parser extension onto the established dynamic base-P/T-set primitive.
//!
//! Each test is discriminating: the asserted P/T equals the *dynamic count/power*
//! that differs from the printed value, so reverting the parser fix — which leaves
//! the line `Unimplemented`, hence no modification and the printed P/T unchanged —
//! flips the assertion. The permanent-static case (Porcelain Gallery) additionally
//! proves the value re-evaluates when the count changes; the one-shot
//! activated-ability cases (Pupu UFO, Sita Varma) prove the CR 608.2h resolution
//! snapshot (the value is locked in once and does not track later changes).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::remove_from_zone;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Porcelain Gallery: "Creatures you control have base power and toughness each
/// equal to the number of creatures you control."
///
/// CR 613.4b layer 7b set + CR 208.1: every creature you control has base P/T
/// equal to the live creature count, re-evaluated each layer pass.
#[test]
fn porcelain_gallery_creatures_become_count_and_track_changes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The static lives on a noncreature artifact (so it does not count itself).
    scenario
        .add_creature(P0, "Porcelain Gallery", 0, 0)
        .as_artifact()
        .from_oracle_text(
            "Creatures you control have base power and toughness each equal to the number of creatures you control.",
        );

    // Three creatures you control with printed P/T differing from the count, so
    // a passing assertion cannot be a coincidence of the printed value.
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let elf = scenario.add_creature(P0, "Elf", 1, 1).id();
    let wolf = scenario.add_creature(P0, "Wolf", 5, 5).id();
    // An opponent's creature must NOT be affected and must NOT be counted.
    let goblin = scenario.add_creature(P1, "Goblin", 4, 4).id();

    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // Three creatures you control → base P/T 3/3 for each (overwriting printed).
    for id in [&bear, &elf, &wolf] {
        assert_eq!(
            runner.state().objects[id].power,
            Some(3),
            "base power becomes the creature count (3), not the printed value"
        );
        assert_eq!(runner.state().objects[id].toughness, Some(3));
    }

    // Opponent's creature is untouched (not affected, not counted).
    assert_eq!(runner.state().objects[&goblin].power, Some(4));
    assert_eq!(runner.state().objects[&goblin].toughness, Some(4));

    // Perturb the count: remove one of your creatures → count = 2 → base P/T
    // tracks down to 2/2. This is what discriminates the fix: with the line left
    // Unimplemented the printed P/T would be unchanged (2/2, 1/1) and would not
    // track at all.
    remove_from_zone(runner.state_mut(), wolf, Zone::Battlefield, P0);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&bear].power,
        Some(2),
        "base power tracks the count down to 2 after a creature leaves"
    );
    assert_eq!(runner.state().objects[&bear].toughness, Some(2));
    assert_eq!(runner.state().objects[&elf].power, Some(2));
    assert_eq!(runner.state().objects[&elf].toughness, Some(2));
}

/// Pupu UFO: "{3}: Until end of turn, this creature's base power becomes equal to
/// the number of Towns you control."
///
/// CR 613.4b layer 7b + CR 608.2h: the *power* axis only is set, to the Town
/// count snapshotted at resolution (a one-shot continuous effect from a resolving
/// ability locks the count in once — it does NOT track later count changes, in
/// contrast with Porcelain Gallery's permanent static). Toughness is unchanged.
#[test]
fn pupu_ufo_base_power_becomes_town_count_power_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Pupu UFO is a 2/2 flier; its base power will be set, toughness untouched.
    let pupu = scenario.add_creature(P0, "Pupu UFO", 2, 2).id();

    // Three Towns you control (subtype "Town"); a fourth Town is the opponent's
    // and must not count.
    scenario
        .add_creature(P0, "Town A", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Town"]);
    scenario
        .add_creature(P0, "Town B", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Town"]);
    let town_c = scenario
        .add_creature(P0, "Town C", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Town"])
        .id();
    scenario
        .add_creature(P1, "Opponent Town", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Town"]);

    let mut runner = scenario.build();

    // Resolve the "{3}" base-power-set effect (parse just the effect body — the
    // mana cost is irrelevant to the continuous-set semantics under test).
    let def = parse_effect_chain(
        "Until end of turn, this creature's base power becomes equal to the number of Towns you control",
        AbilityKind::Activated,
    );
    let ability = build_resolved_from_def(&def, pupu, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("the base-power-set ability must resolve");

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // Three Towns you control → base power 3 (overwriting the printed 2 — the
    // count differs from the printed value, so the assertion discriminates).
    assert_eq!(
        runner.state().objects[&pupu].power,
        Some(3),
        "base power becomes the Town count"
    );
    // Toughness untouched (power-only axis): still the printed 2.
    assert_eq!(
        runner.state().objects[&pupu].toughness,
        Some(2),
        "toughness must remain the printed value (power-only set)"
    );

    // CR 608.2h: the count is locked in at resolution. Removing a Town afterward
    // does NOT change the set power — it stays 3 (a one-shot continuous effect,
    // unlike Porcelain Gallery's continuously-recomputed static).
    remove_from_zone(runner.state_mut(), town_c, Zone::Battlefield, P0);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&pupu].power,
        Some(3),
        "base power was snapshotted at resolution (CR 608.2h) — stays 3 after a Town leaves"
    );
    assert_eq!(
        runner.state().objects[&pupu].toughness,
        Some(2),
        "toughness still untouched"
    );
}

/// Sita Varma, Masked Racer: "… have the base power and toughness of each other
/// creature you control become equal to Sita Varma's power until end of turn."
///
/// CR 613.4b layer 7b: each OTHER creature you control has both base P/T set to
/// Sita Varma's (the source's) power. Sita Varma itself and opponents' creatures
/// are unaffected.
#[test]
fn sita_varma_other_creatures_base_pt_become_source_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Sita Varma's power is 5 (printed) — the value the other creatures take.
    let sita = scenario
        .add_creature(P0, "Sita Varma, Masked Racer", 5, 5)
        .id();
    let ally_a = scenario.add_creature(P0, "Ally A", 1, 1).id();
    let ally_b = scenario.add_creature(P0, "Ally B", 2, 3).id();
    let enemy = scenario.add_creature(P1, "Enemy", 4, 4).id();

    let mut runner = scenario.build();

    let def = parse_effect_chain(
        "have the base power and toughness of each other creature you control become equal to ~'s power until end of turn",
        AbilityKind::Activated,
    );
    let ability = build_resolved_from_def(&def, sita, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("the base-P/T-set ability must resolve");

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // Each OTHER creature you control: base P/T become 5/5 (Sita Varma's power).
    for id in [&ally_a, &ally_b] {
        assert_eq!(
            runner.state().objects[id].power,
            Some(5),
            "other creature base power becomes Sita Varma's power (5)"
        );
        assert_eq!(
            runner.state().objects[id].toughness,
            Some(5),
            "other creature base toughness becomes Sita Varma's power (5)"
        );
    }

    // Sita Varma itself is NOT affected ("each OTHER creature"): still 5/5.
    assert_eq!(runner.state().objects[&sita].power, Some(5));
    assert_eq!(runner.state().objects[&sita].toughness, Some(5));

    // The opponent's creature is untouched: still 4/4.
    assert_eq!(runner.state().objects[&enemy].power, Some(4));
    assert_eq!(runner.state().objects[&enemy].toughness, Some(4));
}
