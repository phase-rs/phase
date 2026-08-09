//! Brainspoil — "Destroy target creature that isn't enchanted. It can't be
//! regenerated. Transmute {1}{B}{B} (...)".
//!
//! The target restriction is specifically about an Aura (CR 303.4b), not any
//! attachment: an equipped creature remains a legal target (CR 301.5a). The
//! regeneration rider modifies this Destroy instruction (CR 608.2c), so it
//! bypasses a shield actually created by the card Regenerate (CR 701.19c).

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ShieldKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
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

/// Cast the canonical Regenerate card through the production action path.
fn cast_regenerate(runner: &mut GameRunner, regenerate: ObjectId, target: ObjectId) {
    runner.cast(regenerate).target_object(target).resolve();
}

/// Begins a Brainspoil cast and leaves its target-selection prompt open.
fn start_brainspoil_target_selection(runner: &mut GameRunner, brainspoil: ObjectId) {
    let card_id = runner.state().objects[&brainspoil].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: brainspoil,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Brainspoil must reach its production target-selection prompt");
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
    let equipped_brainspoil = scenario
        .add_spell_to_hand_from_oracle(P0, "Brainspoil", false, BRAINSPOIL_ORACLE)
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, equipment, equipped);
    attach(&mut runner, p0_aura, p0_enchanted);
    attach(&mut runner, p1_aura, p1_enchanted);

    start_brainspoil_target_selection(&mut runner, equipped_brainspoil);
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Brainspoil must use the production target-selection path, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(target_slots.len(), 1, "Brainspoil has one printed target");
    let legal = &target_slots[selection.current_slot].legal_targets;
    assert!(
        legal.contains(&TargetRef::Object(bare)),
        "bare creature must be legal: {legal:?}"
    );
    assert!(
        legal.contains(&TargetRef::Object(equipped)),
        "Equipment is not an Aura, so equipped creature must remain legal: {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Object(p0_enchanted))
            && !legal.contains(&TargetRef::Object(p1_enchanted)),
        "an Aura controlled by either player makes its host illegal: {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Object(unattached_aura)),
        "an unattached Aura is not a creature target and gives no host an attachment"
    );

    for enchanted_creature in [p0_enchanted, p1_enchanted] {
        let rejected = runner.act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(enchanted_creature)],
        });
        assert!(
            rejected.is_err(),
            "Aura-enchanted creature {enchanted_creature:?} must be rejected by the production target-selection action"
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ),
            "a rejected Aura-enchanted target must leave Brainspoil's target-selection prompt open"
        );
    }

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(equipped)],
        })
        .expect("an equipped creature must be accepted by production target selection");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&equipped].zone, Zone::Graveyard);
}

#[test]
fn brainspoil_cant_regenerate_rider_bypasses_a_real_regeneration_shield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brainspoil_victim = scenario.add_creature(P0, "Brainspoil Victim", 2, 2).id();
    let control_victim = scenario.add_creature(P0, "Control Victim", 2, 2).id();
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

    cast_regenerate(&mut runner, regenerate_brainspoil, brainspoil_victim);
    cast_regenerate(&mut runner, regenerate_control, control_victim);
    assert!(
        runner.state().objects[&brainspoil_victim]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|replacement| {
                replacement.shield_kind == ShieldKind::Regeneration && !replacement.is_consumed
            }),
        "precondition: Regenerate must install a live shield on Brainspoil's victim"
    );
    assert!(
        runner.state().objects[&control_victim]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|replacement| {
                replacement.shield_kind == ShieldKind::Regeneration && !replacement.is_consumed
            }),
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
