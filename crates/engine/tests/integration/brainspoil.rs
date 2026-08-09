//! Brainspoil — "Destroy target creature that isn't enchanted. It can't be
//! regenerated. Transmute {1}{B}{B} (...)".
//!
//! The target restriction is specifically about an Aura (CR 303.4b), not any
//! attachment: an equipped creature remains a legal target (CR 301.5a). The
//! regeneration rider modifies this Destroy instruction (CR 608.2c), so it
//! bypasses a shield actually created by the card Regenerate (CR 701.19c).

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, ShieldKind, TargetRef};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BRAINSPOIL_ORACLE: &str = "Destroy target creature that isn't enchanted. It can't be regenerated.\n\
Transmute {1}{B}{B} ({1}{B}{B}, Discard this card: Search your library for a card with the same mana value as this card, reveal it, put it into your hand, then shuffle. Transmute only as a sorcery.)";
const REGENERATE_ORACLE: &str = "Regenerate target creature.";
const PLAIN_DESTROY_ORACLE: &str = "Destroy target creature.";

/// Wire both sides of an attachment exactly as the engine's attach actions do.
/// CR 303.4b + CR 301.5a: an Aura or Equipment records its host, and the host
/// records the attachment.
fn attach(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&attachment).unwrap().attached_to = Some(AttachTarget::Object(host));
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(attachment);
}

fn brainspoil_destroy_definition(
    runner: &GameRunner,
    brainspoil: ObjectId,
) -> &engine::types::ability::AbilityDefinition {
    runner.state().objects[&brainspoil]
        .abilities
        .iter()
        .find(|definition| matches!(definition.effect.as_ref(), Effect::Destroy { .. }))
        .expect("the exact Brainspoil Oracle text must produce its Destroy ability")
}

#[test]
fn brainspoil_target_slot_excludes_enchanted_creatures_not_equipped_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bare = scenario.add_creature(P1, "Bare Bear", 2, 2).id();
    let equipped = scenario.add_creature(P1, "Equipped Bear", 2, 2).id();
    let p0_enchanted = scenario.add_creature(P1, "P0 Enchanted Bear", 2, 2).id();
    let p1_enchanted = scenario.add_creature(P1, "P1 Enchanted Bear", 2, 2).id();
    let equipment = scenario
        .add_creature(P0, "Test Equipment", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let p0_aura = scenario
        .add_creature(P0, "P0 Test Aura", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .id();
    let p1_aura = scenario
        .add_creature(P1, "P1 Test Aura", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .id();
    let unattached_aura = scenario
        .add_creature(P0, "Unattached Test Aura", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .id();
    let brainspoil = scenario
        .add_spell_to_hand_from_oracle(P0, "Brainspoil", false, BRAINSPOIL_ORACLE)
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, equipment, equipped);
    attach(&mut runner, p0_aura, p0_enchanted);
    attach(&mut runner, p1_aura, p1_enchanted);

    let resolved = build_resolved_from_def(
        brainspoil_destroy_definition(&runner, brainspoil),
        brainspoil,
        P0,
    );
    let slots = build_target_slots(runner.state(), &resolved).expect("Brainspoil target slot");
    assert_eq!(slots.len(), 1, "Brainspoil has one printed target");
    let legal = &slots[0].legal_targets;
    let object = TargetRef::Object;
    assert!(
        legal.contains(&object(bare)),
        "bare creature must be legal: {legal:?}"
    );
    assert!(
        legal.contains(&object(equipped)),
        "Equipment is not an Aura, so equipped creature must remain legal: {legal:?}"
    );
    assert!(
        !legal.contains(&object(p0_enchanted)) && !legal.contains(&object(p1_enchanted)),
        "an Aura controlled by either player makes its host illegal: {legal:?}"
    );
    assert!(
        !legal.contains(&object(unattached_aura)),
        "an unattached Aura is not a creature target and gives no host an attachment"
    );
}

#[test]
fn brainspoil_cant_regenerate_rider_bypasses_a_real_regeneration_shield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brainspoil_victim = scenario.add_creature(P1, "Brainspoil Victim", 2, 2).id();
    let control_victim = scenario.add_creature(P1, "Control Victim", 2, 2).id();
    let regenerate_brainspoil = scenario
        .add_spell_to_hand_from_oracle(P0, "Regenerate", false, REGENERATE_ORACLE)
        .id();
    let regenerate_control = scenario
        .add_spell_to_hand_from_oracle(P0, "Regenerate", false, REGENERATE_ORACLE)
        .id();
    let brainspoil = scenario
        .add_spell_to_hand_from_oracle(P0, "Brainspoil", false, BRAINSPOIL_ORACLE)
        .id();
    let plain_destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Plain Destroy", false, PLAIN_DESTROY_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner
        .cast(regenerate_brainspoil)
        .target_object(brainspoil_victim)
        .resolve();
    runner
        .cast(regenerate_control)
        .target_object(control_victim)
        .resolve();
    assert!(
        runner.state().objects[&brainspoil_victim]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|replacement| replacement.shield_kind == ShieldKind::Regeneration),
        "precondition: Regenerate must install a live shield on Brainspoil's victim"
    );
    assert!(
        runner.state().objects[&control_victim]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|replacement| replacement.shield_kind == ShieldKind::Regeneration),
        "precondition: Regenerate must install a live shield on the control victim"
    );

    let brainspoil_outcome = runner
        .cast(brainspoil)
        .target_object(brainspoil_victim)
        .resolve();
    brainspoil_outcome.assert_zone(&[brainspoil_victim], Zone::Graveyard);

    let control_outcome = runner
        .cast(plain_destroy)
        .target_object(control_victim)
        .resolve();
    control_outcome.assert_zone(&[control_victim], Zone::Battlefield);
    assert!(
        control_outcome.state().objects[&control_victim]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|replacement| {
                replacement.shield_kind == ShieldKind::Regeneration && replacement.is_consumed
            }),
        "plain Destroy must consume the functional regeneration shield"
    );
}
