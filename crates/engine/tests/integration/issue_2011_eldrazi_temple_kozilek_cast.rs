//! Regression for issue #2011: autopass with only untapped Eldrazi Temple as
//! colorless source and Kozilek's Command in hand.
//!
//! https://github.com/phase-rs/phase/issues/2011

use std::sync::Arc;

use engine::ai_support::{auto_pass_recommended, legal_actions, legal_actions_full};
use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, ManaSpendRestriction,
    ModalChoice, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn main_phase_state() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.build().state().clone()
}

fn add_eldrazi_temple_two_abilities(state: &mut GameState) -> engine::types::identifiers::ObjectId {
    let temple = create_object(
        state,
        CardId(9100),
        P0,
        "Eldrazi Temple".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&temple).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    Arc::make_mut(&mut obj.abilities).extend([
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap),
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 2 },
                },
                restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation {
                    spell_type: "Colorless Eldrazi".to_string(),
                    ability: engine::types::mana::AbilityActivationScope::OfSpellType,
                }],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap),
    ]);
    temple
}

fn add_kozileks_command_hand(
    state: &mut GameState,
    with_eldrazi_subtype: bool,
) -> engine::types::identifiers::ObjectId {
    let command = create_object(
        state,
        CardId(9101),
        P0,
        "Kozilek's Command".to_string(),
        Zone::Hand,
    );
    let obj = state.objects.get_mut(&command).unwrap();
    obj.card_types.core_types.push(CoreType::Kindred);
    obj.card_types.core_types.push(CoreType::Instant);
    if with_eldrazi_subtype {
        obj.card_types.subtypes.push("Eldrazi".to_string());
    }
    obj.color.clear();
    obj.mana_cost = ManaCost::Cost {
        shards: vec![
            ManaCostShard::X,
            ManaCostShard::Colorless,
            ManaCostShard::Colorless,
        ],
        generic: 0,
    };
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 0 },
            target: TargetFilter::Controller,
        },
    ));
    obj.modal = Some(ModalChoice {
        min_choices: 2,
        max_choices: 2,
        mode_count: 4,
        mode_descriptions: vec![
            "Mode 0".to_string(),
            "Mode 1".to_string(),
            "Mode 2".to_string(),
            "Mode 3".to_string(),
        ],
        ..ModalChoice::default()
    });
    command
}

#[test]
fn kozileks_command_castable_with_only_untapped_eldrazi_temple() {
    let mut state = main_phase_state();
    let _temple = add_eldrazi_temple_two_abilities(&mut state);
    let command = add_kozileks_command_hand(&mut state, true);

    assert!(
        can_cast_object_now(&state, P0, command),
        "can_cast_object_now must be true for {{X}}{{C}}{{C}} with only untapped Eldrazi Temple"
    );
    let actions = legal_actions(&state);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == command
        )),
        "legal_actions must include CastSpell (issue #2011 — game must not autopass)"
    );

    let (flat, _costs, by_object) = legal_actions_full(&state);
    assert!(
        by_object.get(&command).is_some_and(|a| {
            a.iter()
                .any(|act| matches!(act, GameAction::CastSpell { .. }))
        }),
        "legal_actions_by_object must expose CastSpell on Kozilek's Command"
    );
    assert!(
        !auto_pass_recommended(&state, &flat),
        "auto_pass_recommended must be false when Kozilek's Command is castable"
    );
}

/// Reporter may still have colored mana for {X}; only Eldrazi Temple supplies {C}.
#[test]
fn kozileks_command_castable_with_temple_plus_generic_mana_lands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..5 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    }
    let mut state = scenario.build().state().clone();
    let _temple = add_eldrazi_temple_two_abilities(&mut state);
    let command = add_kozileks_command_hand(&mut state, true);

    assert!(
        can_cast_object_now(&state, P0, command),
        "can_cast_object_now must be true for {{X}}{{C}}{{C}} with Temple + 5 Mountains"
    );
    assert!(
        legal_actions(&state)
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == command)),
        "legal_actions must include CastSpell when generic can come from Mountains and {{C}}{{C}} from Temple"
    );
}

/// Missing Eldrazi subtype must block spending restricted Temple mana on {{C}}{{C}}.
#[test]
fn kozileks_command_without_eldrazi_subtype_is_not_castable_with_restricted_temple_mana() {
    let mut state = main_phase_state();
    let _temple = add_eldrazi_temple_two_abilities(&mut state);
    let command = add_kozileks_command_hand(&mut state, false);

    assert!(
        !can_cast_object_now(&state, P0, command),
        "missing Eldrazi subtype must block restricted Eldrazi Temple mana for {{C}}{{C}}"
    );
}
