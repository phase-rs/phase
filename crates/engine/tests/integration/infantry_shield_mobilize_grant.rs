//! Issue #5266: Infantry Shield — "Equipped creature has menace and mobilize X,
//! where X is its power." The granted mobilize count must be the equipped
//! creature's power (dynamic), and the mobilize attack trigger must be installed
//! on the equipped creature — not a Fixed-1 mobilize with no effect.
//!
//! This drives the RUNTIME grant pipeline: the static's `AddDynamicKeyword`
//! resolves "its power" against the equipped creature at layer-evaluation time
//! (`with_value`) and installs the mobilize attack trigger via `triggers_for`.
//!
//! CR 702.181a (Mobilize N) + CR 301.5f (Equipment attaches to a creature).

use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, QuantityExpr};
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::triggers::TriggerMode;

const INFANTRY_SHIELD: &str =
    "Equipped creature has menace and mobilize X, where X is its power.\nEquip {2}";

#[test]
fn infantry_shield_grants_dynamic_mobilize_equal_to_power() {
    let mut scenario = GameScenario::new();
    // A 3-power creature: the equipped creature. Mobilize must resolve to 3.
    let bear = scenario.add_creature(P0, "War Bear", 3, 3).id();
    // Infantry Shield installs its static from Oracle. Toughness 1 so it survives
    // build-time SBAs before we convert it into an Equipment.
    let shield = scenario
        .add_creature_from_oracle(P0, "Infantry Shield", 0, 1, INFANTRY_SHIELD)
        .id();
    let mut runner = scenario.build();

    // Convert the shield into an Equipment attached to the 3-power creature so the
    // static's `EquippedBy` affected filter (CR 301.5f) resolves to `bear`.
    {
        let obj = runner.state_mut().objects.get_mut(&shield).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Equipment".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.attached_to = Some(AttachTarget::Object(bear));
    }
    evaluate_layers(runner.state_mut());

    let host = &runner.state().objects[&bear];

    // CR 702.181a: the equipped creature gains Mobilize with a count equal to its
    // power (3), resolved by the continuous layer pass — NOT the buggy Fixed 1.
    let mobilize_count = host.keywords.iter().find_map(|k| match k {
        Keyword::Mobilize(qty) => Some(qty.clone()),
        _ => None,
    });
    assert!(
        matches!(mobilize_count, Some(QuantityExpr::Fixed { value: 3 })),
        "equipped creature must have Mobilize 3 (= its power), got {mobilize_count:?}"
    );

    // Menace is still granted alongside the dynamic mobilize.
    assert!(
        host.keywords.contains(&Keyword::Menace),
        "menace must be granted too, got {:?}",
        host.keywords
    );

    // CR 702.181a: the mobilize attack trigger (create Warrior tokens) must be
    // installed on the equipped creature, or the grant is inert ("doesn't
    // mobilize").
    assert!(
        host.trigger_definitions.iter_unchecked().any(|t| {
            matches!(t.definition.mode, TriggerMode::Attacks)
                && matches!(
                    t.definition.execute.as_deref().map(|a| &*a.effect),
                    Some(Effect::Token { name, .. }) if name == "Warrior"
                )
        }),
        "the mobilize attack trigger must be installed on the equipped creature"
    );
}
