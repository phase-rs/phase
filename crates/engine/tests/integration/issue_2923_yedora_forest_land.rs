//! Regression for issue #2923 — Yedora, Grave Gardener returns dying creatures
//! to the battlefield face down AS A FOREST LAND, not as their creature self.
//!
//! Oracle: "Whenever another nontoken creature you control dies, you may return
//! it to the battlefield face down under its owner's control. It's a Forest
//! land. (It has no other types or abilities.)"
//!
//! CR 708.2a: a permanent put onto the battlefield face down has only the
//! characteristics the effect specifies — here Forest land, no creature type,
//! no power/toughness, no abilities.
//! CR 305.6: a Land with a basic land type (Forest) has the intrinsic ability
//! "{T}: Add {G}", even with an empty text box.
//!
//! https://github.com/phase-rs/phase/issues/2923

use engine::game::mana_sources::activatable_land_mana_options;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const YEDORA_ORACLE: &str = "Whenever another nontoken creature you control dies, you may return it to the battlefield face down under its owner's control. It's a Forest land. (It has no other types or abilities.)";

#[test]
fn yedora_returns_dying_creature_as_face_down_forest_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Yedora, Grave Gardener", 4, 4)
        .from_oracle_text(YEDORA_ORACLE);

    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    runner
        .act(GameAction::Debug(DebugAction::Sacrifice {
            object_id: bear,
        }))
        .expect("sacrificing the bear should succeed");

    runner.advance_until_stack_empty();

    // Yedora's return is a "you may" optional — accept it.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accepting Yedora's return must succeed");
        runner.advance_until_stack_empty();
    }

    let obj = &runner.state().objects[&bear];

    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "the dying creature must return to the battlefield"
    );
    assert!(obj.face_down, "the returned permanent must be face down");

    // CR 708.2a + CR 205.1a: it is a Forest land — Land core type, Forest
    // subtype, and NOT a creature.
    assert!(
        obj.card_types.core_types.contains(&CoreType::Land),
        "returned permanent must be a Land, got {:?}",
        obj.card_types
    );
    assert!(
        !obj.card_types.core_types.contains(&CoreType::Creature),
        "returned permanent must NOT be a creature, got {:?}",
        obj.card_types
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Forest"),
        "returned permanent must have the Forest subtype, got {:?}",
        obj.card_types
    );

    // CR 708.2a: "It has no other types or abilities." — no power/toughness and
    // no creature card abilities ride the face-down land.
    assert!(
        obj.power.is_none() && obj.toughness.is_none(),
        "a Forest land has no power/toughness, got {:?}/{:?}",
        obj.power,
        obj.toughness
    );
    // CR 305.6: a Forest land's ONLY ability is the intrinsic "{T}: Add {G}"
    // mana ability synthesized from the basic land type. No other (creature)
    // ability survives the face-down transform.
    assert!(
        obj.abilities.iter().all(|a| matches!(
            a.effect.as_ref(),
            engine::types::ability::Effect::Mana { .. }
        )),
        "face-down Forest land must have no non-mana abilities, got {:?}",
        obj.abilities
    );

    // CR 305.6: the Forest land taps for {G} via its intrinsic basic-land mana
    // ability, even with an empty text box.
    let options = activatable_land_mana_options(runner.state(), bear, P0);
    assert!(
        options.iter().any(|o| o.mana_type == ManaType::Green),
        "face-down Forest land must tap for {{G}}, got {options:?}"
    );
    assert!(
        options.iter().all(|o| o.mana_type == ManaType::Green),
        "face-down Forest land must produce only {{G}}, got {options:?}"
    );
}
