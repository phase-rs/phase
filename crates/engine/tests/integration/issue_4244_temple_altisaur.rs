//! Issue #4244: Temple Altisaur must prevent all but 1 damage dealt to other
//! Dinosaurs you control, and must not affect damage to other permanents or players.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    Effect, FilterProp, PreventionAmount, QuantityExpr, ResolvedAbility, ShieldKind, TargetFilter,
    TargetRef, TypeFilter,
};
use engine::types::events::GameEvent;

const TEMPLE_ALTISAUR: &str =
    "If a source would deal damage to another Dinosaur you control, prevent all but 1 of that damage.";

fn damage_ability(
    source_id: engine::types::identifiers::ObjectId,
    target: TargetRef,
    amount: i32,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
        },
        vec![target],
        source_id,
        P1,
    )
}

#[test]
fn temple_altisaur_prevents_all_but_one_to_other_dinosaurs_you_control() {
    let mut scenario = GameScenario::new();
    let temple = scenario
        .add_creature_from_oracle(P0, "Temple Altisaur", 3, 4, TEMPLE_ALTISAUR)
        .id();
    let ally_dino = scenario
        .add_creature(P0, "Ally Dinosaur", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 5, 5).id();
    let mut runner = scenario.build();

    let repl = &runner.state().objects[&temple].replacement_definitions[0];
    assert!(
        matches!(
            repl.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::AllBut(1)
            }
        ),
        "Temple Altisaur must install an AllBut(1) prevention shield, got {:?}",
        repl.shield_kind
    );
    let valid_card = repl.valid_card.as_ref().expect("recipient filter required");
    match valid_card {
        TargetFilter::Typed(tf) => {
            assert!(tf
                .type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dinosaur")));
            assert!(tf.properties.contains(&FilterProp::Another));
        }
        other => panic!("expected typed dinosaur recipient filter, got {other:?}"),
    }

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(ally_dino), 5),
        &mut events,
    )
    .expect("damage to allied dinosaur resolves");

    assert_eq!(
        runner.state().objects[&ally_dino].damage_marked,
        1,
        "5 damage to another dinosaur must be reduced to 1"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::DamagePrevented { amount: 4, .. })),
        "must prevent 4 of the 5 damage"
    );

    let p0_life_before = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0), 5),
        &mut events,
    )
    .expect("damage to player resolves");
    assert_eq!(
        runner.life(P0),
        p0_life_before - 5,
        "Temple Altisaur must not reduce damage dealt to players"
    );

    // CR 615.1a: the "another Dinosaur" restriction excludes Temple Altisaur
    // itself, so damage dealt to the Temple is taken in full.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(temple), 5),
        &mut events,
    )
    .expect("damage to Temple Altisaur itself resolves");
    assert_eq!(
        runner.state().objects[&temple].damage_marked,
        5,
        "Temple Altisaur must not prevent damage dealt to itself (\"another\")"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::DamagePrevented { .. })),
        "no damage should be prevented when the Temple itself is the recipient"
    );
}
