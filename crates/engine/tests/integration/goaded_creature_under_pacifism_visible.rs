//! Combat-requirement visibility (B1): a goaded creature under a Pacifism-class
//! restriction ("Enchanted creature can't attack or block.") must surface as
//! `CantAttack`, NOT `MustAttack`, and must NOT be force-attacked.
//!
//! CR 508.1c beats CR 508.1d: a "can't attack" restriction overrides an "attacks
//! if able" requirement. Before B1, `creature_must_attack` returned `true` for a
//! goaded creature even under Pacifism, so the declare-attackers enforcement
//! rejected an empty declaration AND the display would have shown `MustAttack`.
//!
//! Drives the REAL layer → combat pipeline: the constraints come from
//! `attacker_constraints_for_active_player` (the production authority `turns.rs`
//! uses to populate the `DeclareAttackers` waiting payload) and the enforcement
//! comes from `declare_attackers`. The Pacifism restriction is installed as a
//! functioning `CantAttackOrBlock` static (the same static the parser lowers
//! "can't attack or block" to) and resolved through `evaluate_layers`.

use std::sync::Arc;

use engine::game::combat::{
    attacker_constraints_for_active_player, creature_cant_attack, declare_attackers,
    get_valid_attacker_ids, CombatRequirement,
};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, StaticDefinition, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

/// Mark a creature goaded by `goader` (P0 is the active player, so its own
/// creatures are subject to CR 508.1d during its combat).
fn goad(runner: &mut engine::game::scenario::GameRunner, creature: ObjectId, goader: PlayerId) {
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .goaded_by
        .insert(goader);
}

/// Install a functioning intrinsic `CantAttackOrBlock` static on `creature`
/// (the Pacifism restriction: "Enchanted creature can't attack or block."),
/// mirroring the proven pattern in `willie_lumpkin_cant_attack.rs`.
fn pacify(runner: &mut engine::game::scenario::GameRunner, creature: ObjectId) {
    let def = StaticDefinition::new(StaticMode::CantAttackOrBlock)
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::CantAttackOrBlock,
        }]);
    let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
    obj.static_definitions = vec![def.clone()].into();
    obj.base_static_definitions = Arc::new(vec![def]);
}

fn refresh(runner: &mut engine::game::scenario::GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

#[test]
fn goaded_creature_under_pacifism_is_visible_as_cant_attack_not_must_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    // A goaded creature P0 controls (P0 is the active player).
    let pacified = scenario.add_creature(P0, "Goaded Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    goad(&mut runner, pacified, P1);
    pacify(&mut runner, pacified);
    refresh(&mut runner);

    // Reach-guard: the Pacifism static is functioning (else the assertions below
    // would be vacuous — the creature would just be a plain goaded creature).
    assert!(
        creature_cant_attack(runner.state(), pacified),
        "sanity: Pacifism's CantAttackOrBlock static must be functioning"
    );

    // (a) DISPLAY: the pacified goaded creature surfaces as CantAttack.
    // REVERT-FAIL: before B1 the must-attack predicate returned true (goad), so
    // the helper emitted `MustAttack` here instead of `CantAttack`.
    let constraints = attacker_constraints_for_active_player(
        runner.state(),
        &get_valid_attacker_ids(runner.state()),
    );
    assert_eq!(
        constraints.get(&pacified),
        // CR 508.1c: intrinsic SelfRef Pacifism → carrier is the creature itself.
        Some(&CombatRequirement::CantAttack {
            sources: vec![pacified]
        }),
        "a goaded creature under Pacifism is CantAttack, not MustAttack"
    );

    // (b) ENFORCEMENT: declaring no attackers is legal — the pacified creature is
    // not force-attacked. REVERT-FAIL: before B1 this returned Err (goad forced).
    {
        let mut s = runner.state().clone();
        let mut events = Vec::new();
        assert!(
            declare_attackers(&mut s, &[], &mut events).is_ok(),
            "CR 508.1c: a goaded creature that can't attack is not force-attacked"
        );
    }

    // (c) DIFFERENTIAL: an unencumbered goaded creature in the SAME state is
    // MustAttack{players:[]}, proving it is Pacifism specifically that neutralizes
    // the requirement — not a blanket suppression of goad display.
    let unencumbered = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(9999),
        P0,
        "Unencumbered Goaded Bear".to_string(),
        engine::types::zones::Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&unencumbered).unwrap();
        obj.card_types.core_types = vec![engine::types::card_type::CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        // CR 302.6: not summoning sick, so it is a valid attacker (must-attack
        // only surfaces for eligible attackers).
        obj.summoning_sick = false;
    }
    goad(&mut runner, unencumbered, P1);
    refresh(&mut runner);

    let constraints = attacker_constraints_for_active_player(
        runner.state(),
        &get_valid_attacker_ids(runner.state()),
    );
    assert_eq!(
        constraints.get(&unencumbered),
        // CR 701.15b: direct player-goad (`goaded_by`) carries no object source →
        // EMPTY sources. This is the documented player-level goad row.
        Some(&CombatRequirement::MustAttack {
            players: vec![],
            sources: vec![]
        }),
        "an unencumbered goaded creature must surface as MustAttack with no specific-player constraint"
    );
    assert_eq!(
        constraints.get(&pacified),
        // CR 508.1c: intrinsic SelfRef Pacifism → carrier is the creature itself.
        Some(&CombatRequirement::CantAttack {
            sources: vec![pacified]
        }),
        "the pacified creature stays CantAttack even with a sibling must-attacker present"
    );
}
