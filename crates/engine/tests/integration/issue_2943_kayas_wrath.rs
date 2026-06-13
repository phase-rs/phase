//! Integration test for issue #2943 — Kaya's Wrath lifegain from destroyed creatures.
//!
//! Oracle:
//!   "Destroy all creatures. You gain life equal to the number of creatures
//!    you controlled that were destroyed this way."
//!
//! Before the fix the lifegain clause lowered to `Effect::Unimplemented` because
//! `parse_quantity_ref` let `parse_type_phrase` consume "creatures you controlled"
//! and leave an unresolved "that were destroyed this way" tail. The fix routes
//! "the number of … destroyed/sacrificed this way" through
//! `FilteredTrackedSetSize` before the generic object-count fall-through.
//!
//! CR 701.8a: Destroy moves battlefield permanents to the graveyard.
//! CR 608.2c + CR 400.7: Downstream sub-abilities count only objects actually
//!   moved by the preceding destroy, optionally filtered by controller/type.
//! CR 119.3: Gain life equal to that filtered count.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ControllerRef, Effect, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter,
    TypeFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const KAYAS_WRATH_ORACLE: &str = "Destroy all creatures. You gain life equal to the number of \
creatures you controlled that were destroyed this way.";

fn kayas_wrath_ability(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(KAYAS_WRATH_ORACLE, AbilityKind::Spell);
    build_resolved_from_def(&def, source_id, controller)
}

fn spawn_creature(state: &mut GameState, card_id: CardId, owner: PlayerId, name: &str) -> ObjectId {
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    id
}

/// Parse-shape contract: DestroyAll(creatures) chained to GainLife whose amount
/// reads the filtered tracked set of controller-owned creatures destroyed.
#[test]
fn kayas_wrath_lowers_to_destroy_all_plus_filtered_tracked_set_gain_life() {
    let def = parse_effect_chain(KAYAS_WRATH_ORACLE, AbilityKind::Spell);
    match def.effect.as_ref() {
        Effect::DestroyAll {
            target: TargetFilter::Typed(tf),
            ..
        } => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        }
        other => panic!("expected DestroyAll{{creatures}}, got {other:?}"),
    }

    let gain_life = def
        .sub_ability
        .as_ref()
        .expect("lifegain must be a sub_ability of DestroyAll");
    match gain_life.effect.as_ref() {
        Effect::GainLife {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::FilteredTrackedSetSize { filter, .. },
                },
            player: TargetFilter::Controller,
        } => match filter.as_ref() {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert!(
                    tf.controller
                        .as_ref()
                        .is_some_and(|c| matches!(c, ControllerRef::You)),
                    "lifegain count must be restricted to creatures you controlled"
                );
            }
            other => panic!("expected Typed creature-you filter, got {other:?}"),
        },
        other => panic!("expected FilteredTrackedSetSize GainLife sub_ability, got {other:?}"),
    }
}

/// CR 608.2c: Only the controller's destroyed creatures contribute to lifegain,
/// even when the wrath destroys the whole battlefield.
#[test]
fn kayas_wrath_gains_life_only_for_controller_creatures_destroyed() {
    let mut state = GameState::new_two_player(42);
    for i in 0..3 {
        spawn_creature(&mut state, CardId(i + 1), PlayerId(0), "Yours");
    }
    for i in 0..2 {
        spawn_creature(&mut state, CardId(i + 10), PlayerId(1), "Theirs");
    }
    let starting_life = state.players[0].life;

    let ability = kayas_wrath_ability(PlayerId(0), ObjectId(100));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        state.players[0].life,
        starting_life + 3,
        "controller must gain life equal to their own destroyed creatures only"
    );
    assert_eq!(
        state.battlefield.len(),
        0,
        "all creatures must leave the battlefield"
    );
}

/// CR 702.12b + CR 608.2c: Indestructible creatures are excluded from both
/// destruction and the filtered tracked-set count.
#[test]
fn kayas_wrath_excludes_indestructible_from_life_gained() {
    let mut state = GameState::new_two_player(42);
    let god = spawn_creature(&mut state, CardId(1), PlayerId(0), "Indestructible");
    state
        .objects
        .get_mut(&god)
        .unwrap()
        .keywords
        .push(engine::types::keywords::Keyword::Indestructible);
    spawn_creature(&mut state, CardId(2), PlayerId(0), "Mortal");
    spawn_creature(&mut state, CardId(3), PlayerId(1), "Opponent");
    let starting_life = state.players[0].life;

    let ability = kayas_wrath_ability(PlayerId(0), ObjectId(100));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        state.players[0].life,
        starting_life + 1,
        "only the mortal creature you controlled should count toward lifegain"
    );
    assert!(
        state.battlefield.contains(&god),
        "indestructible creature must survive"
    );
}

/// CR 701.8c: Regenerated creatures are not destroyed and must not count.
#[test]
fn kayas_wrath_excludes_regenerated_creature_from_life_gained() {
    use engine::types::ability::ReplacementDefinition;
    use engine::types::replacements::ReplacementEvent;

    let mut state = GameState::new_two_player(42);
    let shielded = spawn_creature(&mut state, CardId(1), PlayerId(0), "Shielded");
    let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
        .valid_card(TargetFilter::SelfRef)
        .description("Regenerate".to_string())
        .regeneration_shield();
    state
        .objects
        .get_mut(&shielded)
        .unwrap()
        .replacement_definitions
        .push(shield);
    spawn_creature(&mut state, CardId(2), PlayerId(0), "Unshielded");
    let starting_life = state.players[0].life;

    let ability = kayas_wrath_ability(PlayerId(0), ObjectId(100));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        state.players[0].life,
        starting_life + 1,
        "regenerated creature must not increment the destroyed-this-way count"
    );
    assert!(
        state.battlefield.contains(&shielded),
        "regenerated creature must remain on the battlefield"
    );
}

/// Zero controlled creatures destroyed → zero life gained, but the chain still
/// records an empty tracked set so stale prior sets are not reused.
#[test]
fn kayas_wrath_gains_zero_when_controller_had_no_creatures() {
    let mut state = GameState::new_two_player(42);
    spawn_creature(&mut state, CardId(1), PlayerId(1), "Opponent only");
    let starting_life = state.players[0].life;

    let ability = kayas_wrath_ability(PlayerId(0), ObjectId(100));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(state.players[0].life, starting_life);
    assert_eq!(state.battlefield.len(), 0);
}
