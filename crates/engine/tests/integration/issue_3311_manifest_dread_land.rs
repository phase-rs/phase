//! Issue #3311 — Manifest dread must allow manifesting a noncreature land
//! (Undercity Sewers) face down as a 2/2 creature.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectScope, ReplacementDefinition, TapStateChange,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const MANIFEST_DREAD_ORACLE: &str = "Manifest dread.";

fn enter_tap_state_battlefield_replacement(
    description: &str,
    state: TapStateChange,
) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Battlefield)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state,
            },
        ))
        .description(description.to_string())
}

fn undercity_sewers_land_types() -> CardType {
    CardType {
        supertypes: vec![],
        core_types: vec![CoreType::Land],
        subtypes: vec!["Island".to_string(), "Swamp".to_string()],
    }
}

#[test]
fn manifest_dread_can_manifest_land_face_down_on_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let other = scenario.add_card_to_library_top(P0, "Other Card");
    let sewers = scenario.add_card_to_library_top(P0, "Undercity Sewers");
    let spell = scenario
        .add_spell_to_hand(P0, "Dread Test", false)
        .from_oracle_text(MANIFEST_DREAD_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&sewers).unwrap();
        obj.card_types = undercity_sewers_land_types();
        obj.base_card_types = obj.card_types.clone();
    }

    runner.cast(spell).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManifestDreadChoice { .. }
    ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![sewers],
        })
        .expect("choose Undercity Sewers to manifest");
    runner.advance_until_stack_empty();

    let obj = runner.state().objects.get(&sewers).expect("sewers object");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.face_down);
    assert_eq!(obj.power, Some(2));
    assert_eq!(obj.toughness, Some(2));
    assert_eq!(
        runner.state().objects[&other].zone,
        Zone::Graveyard,
        "other looked-at card must be graved"
    );
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "manifested card must present as a creature while face down"
    );
}

#[test]
fn undercity_sewers_manifest_dread_finishes_after_material_tap_state_collision() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let other = scenario.add_card_to_library_top(P0, "Other Card");
    let sewers = scenario.add_card_to_library_top(P0, "Undercity Sewers");
    scenario
        .add_creature(P1, "Kismet", 0, 0)
        .as_enchantment()
        .with_replacement_definition(enter_tap_state_battlefield_replacement(
            "Creatures enter the battlefield tapped.",
            TapStateChange::Tap,
        ));
    scenario
        .add_creature(P1, "Spelunking", 0, 0)
        .as_enchantment()
        .with_replacement_definition(enter_tap_state_battlefield_replacement(
            "Permanents enter the battlefield untapped.",
            TapStateChange::Untap,
        ));
    let spell = scenario
        .add_spell_to_hand(P0, "Dread Test", false)
        .from_oracle_text(MANIFEST_DREAD_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&sewers).unwrap();
        obj.card_types = undercity_sewers_land_types();
        obj.base_card_types = obj.card_types.clone();
    }

    runner.cast(spell).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![sewers],
        })
        .expect("choose Undercity Sewers to manifest");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "manifesting a land must still finish after material tap-state collision, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&other].zone,
        Zone::Library,
        "other looked-at card must not be graved while the land's manifest entry is paused"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer enter-tapped ordering");
    runner.advance_until_stack_empty();

    let obj = runner.state().objects.get(&sewers).expect("sewers object");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.face_down);
    assert_eq!(runner.state().objects[&other].zone, Zone::Graveyard);
}
