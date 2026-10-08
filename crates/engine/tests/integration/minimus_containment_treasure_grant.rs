//! Realizability for issue #6000: Minimus Containment makes the enchanted
//! permanent a Treasure artifact that HAS "{T}, Sacrifice this artifact: Add one
//! mana of any color" and loses its own abilities. Before the parser fix the
//! quoted granted ability and the "loses all other abilities" clause were both
//! dropped (only the type + Treasure subtype survived), so the enchanted
//! permanent kept its abilities and could not make mana.
//!
//! This drives the production Layer engine (`evaluate_layers`) — the same path
//! `granted_ability_self_binding.rs` uses — to prove the `type_change` static's
//! `GrantAbility` + `RemoveAllAbilities` actually apply to the host.
//!
//! https://github.com/phase-rs/phase/issues/6000

use std::sync::Arc;

use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect, StaticDefinition};
use engine::types::card_type::CoreType;
use engine::types::phase::Phase;

const MINIMUS: &str = "Enchanted permanent is a Treasure artifact with \"{T}, Sacrifice this artifact: Add one mana of any color,\" and it loses all other abilities.";

fn minimus_static() -> StaticDefinition {
    let parsed = parse_oracle_text(
        MINIMUS,
        "Minimus Containment",
        &[],
        &["Enchantment".to_string(), "Aura".to_string()],
        &["Aura".to_string()],
    );
    parsed
        .statics
        .into_iter()
        .find(|s| {
            s.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::GrantAbility { .. }))
        })
        .expect("Minimus must emit a GrantAbility type-change static")
}

#[test]
fn minimus_containment_makes_host_a_treasure_with_granted_mana_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Host: a creature whose ONLY ability is a non-mana "{T}: Draw a card" — so a
    // surviving Draw effect proves the wipe failed, and the granted mana ability
    // proves the grant landed.
    let host = scenario
        .add_creature_from_oracle(P0, "Studious Bear", 2, 2, "{T}: Draw a card.")
        .id();
    let aura = scenario.add_creature(P0, "Minimus Containment", 0, 0).id();

    let grant = minimus_static();
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        let aura_obj = st.objects.get_mut(&aura).unwrap();
        aura_obj.card_types.core_types = vec![CoreType::Enchantment];
        aura_obj.card_types.subtypes = vec!["Aura".to_string()];
        aura_obj.base_card_types = aura_obj.card_types.clone();
        aura_obj.power = None;
        aura_obj.toughness = None;
        aura_obj.base_power = None;
        aura_obj.base_toughness = None;
        aura_obj.attached_to = Some(AttachTarget::Object(host));
        aura_obj.static_definitions.push(grant.clone());
        Arc::make_mut(&mut aura_obj.base_static_definitions).push(grant);
        st.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());

    let host_obj = &runner.state().objects[&host];

    // Type change (CR 205.1a, Layer 4): becomes an Artifact, is no longer a Creature.
    assert!(
        host_obj.card_types.core_types.contains(&CoreType::Artifact),
        "host must become an Artifact, got {:?}",
        host_obj.card_types.core_types
    );
    assert!(
        !host_obj.card_types.core_types.contains(&CoreType::Creature),
        "host must lose its Creature type (SetCardTypes replaces), got {:?}",
        host_obj.card_types.core_types
    );
    assert!(
        host_obj.card_types.subtypes.iter().any(|s| s == "Treasure"),
        "host must gain the Treasure subtype, got {:?}",
        host_obj.card_types.subtypes
    );

    // GrantAbility (Layer 6): the host carries the granted mana ability…
    assert!(
        host_obj
            .abilities
            .iter()
            .any(|a| matches!(*a.effect, Effect::Mana { .. })),
        "host must carry the granted Treasure mana ability after the wipe, got {:?}",
        host_obj.abilities
    );
    // …and RemoveAllAbilities wiped its original non-mana "{T}: Draw a card".
    assert!(
        !host_obj
            .abilities
            .iter()
            .any(|a| matches!(*a.effect, Effect::Draw { .. })),
        "host must lose its own \"{{T}}: Draw a card\" ability, got {:?}",
        host_obj.abilities
    );
}
