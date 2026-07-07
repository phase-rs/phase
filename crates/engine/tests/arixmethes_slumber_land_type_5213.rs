//! Discriminating regression test for **issue #5213**: Arixmethes, Slumbering
//! Isle must become JUST a land (never a land creature) while it has a slumber
//! counter.
//!
//! Arixmethes' static:
//!
//! > As long as Arixmethes has a slumber counter on it, it's a land.
//! > (It's not a creature.)
//!
//! Arixmethes is normally a "Legendary Creature — Kraken". CR 205.1a: setting an
//! object's card type REPLACES its previous card types — so "it's a land" makes
//! it just a Land and removes the Creature type. Before the fix the line parsed
//! to an additive `AddType { Land }`, so at runtime Arixmethes kept the Creature
//! type and became an (illegal) land creature.
//!
//! After the fix the static carries `SetCardTypes { core_types: [Land] }`, which
//! the layer system (`game/layers.rs`) applies as a replacement: core types
//! become exactly `[Land]`, the Creature type (and its Kraken subtype) drop, and
//! the Legendary supertype is retained.
//!
//! CR 205.1a: setting card type(s) replaces the previous card types; correlated
//!   subtypes drop unless they belong to a card type the object still has.
//! CR 122.1: counters on a permanent (the slumber-counter gate).
//! CR 613.3d (Layer 4): continuous type-changing static ability.

use std::sync::Arc;

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::ContinuousModification;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

const ARIXMETHES_ORACLE: &str = "Arixmethes enters tapped with five slumber counters on it.\n\
As long as Arixmethes has a slumber counter on it, it's a land. (It's not a creature.)\n\
Whenever you cast a spell, you may remove a slumber counter from Arixmethes.\n\
{T}: Add {G}{U}.";

#[test]
fn arixmethes_with_slumber_counter_is_just_a_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Build Arixmethes from his real Oracle text. Parsing of the land static
    // happens here, at build time.
    let arixmethes = scenario
        .add_creature_from_oracle(P0, "Arixmethes, Slumbering Isle", 12, 12, ARIXMETHES_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Reshape the printed baseline into the real card: Legendary Creature —
    // Kraken with 12/12 P/T. `GameScenario` exposes object mutation only after
    // `build()` (mirroring the Gideon Blackblade test). The reshape touches only
    // the type/P-T baseline, not `base_static_definitions` (already parsed).
    {
        let obj = runner.state_mut().objects.get_mut(&arixmethes).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.card_types.supertypes = vec![Supertype::Legendary];
        obj.card_types.subtypes = vec!["Kraken".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(12);
        obj.toughness = Some(12);
        obj.base_power = Some(12);
        obj.base_toughness = Some(12);
    }

    // ----- Reach-guard: the static parsed to a core-type REPLACEMENT, not an
    // additive AddType (the #5213 fix). This is what makes the runtime assertions
    // below discriminating: reverting to `AddType { Land }` fails right here, and
    // also fails the "not a Creature" / "core == [Land]" checks. -----
    let statics: &Arc<Vec<_>> = &runner.state().objects[&arixmethes].base_static_definitions;
    let land_static = statics
        .iter()
        .find(|s| {
            s.modifications
                .contains(&ContinuousModification::SetCardTypes {
                    core_types: vec![CoreType::Land],
                })
        })
        .expect(
            "Arixmethes' land static must carry SetCardTypes([Land]) (replacement), \
             not additive AddType{Land} (#5213)",
        );
    assert!(
        !land_static
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddType { .. })),
        "the land static must not carry additive AddType; got {:?}",
        land_static.modifications
    );

    // ----- With a slumber counter, Arixmethes is JUST a land. -----
    // CR 122.1: add the slumber counter that satisfies the static's HasCounters gate.
    {
        let obj = runner.state_mut().objects.get_mut(&arixmethes).unwrap();
        obj.counters
            .insert(CounterType::Generic("slumber".to_string()), 1);
    }
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&arixmethes];
    // CR 205.1a: core types are replaced by exactly [Land] — Creature is removed.
    assert_eq!(
        obj.card_types.core_types,
        vec![CoreType::Land],
        "with a slumber counter Arixmethes must be JUST a Land, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        !obj.card_types.core_types.contains(&CoreType::Creature),
        "Arixmethes must NOT be a Creature while a land (#5213), got {:?}",
        obj.card_types.core_types
    );
    // CR 205.1a: the Kraken creature-subtype drops (no Creature type to hold it).
    assert!(
        !obj.card_types.subtypes.iter().any(|s| s == "Kraken"),
        "Kraken subtype must drop when the Creature type is removed, got {:?}",
        obj.card_types.subtypes
    );
    // The Legendary supertype is unaffected by a core-type replacement.
    assert!(
        obj.card_types.supertypes.contains(&Supertype::Legendary),
        "Legendary supertype must be retained, got {:?}",
        obj.card_types.supertypes
    );

    // ----- Remove the counter: the static no longer applies, Arixmethes reverts
    // to a Legendary Creature — Kraken (the discriminator vs. an always-on fix). -----
    {
        let obj = runner.state_mut().objects.get_mut(&arixmethes).unwrap();
        obj.counters
            .remove(&CounterType::Generic("slumber".to_string()));
    }
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&arixmethes];
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "without a slumber counter Arixmethes reverts to a Creature, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        !obj.card_types.core_types.contains(&CoreType::Land),
        "without a slumber counter Arixmethes is not a Land, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Kraken"),
        "Kraken subtype restored once the Creature type returns, got {:?}",
        obj.card_types.subtypes
    );
}
