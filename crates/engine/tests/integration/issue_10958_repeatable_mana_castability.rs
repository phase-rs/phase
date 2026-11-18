//! Issue #10958: Repeatable mana abilities without {T} (pay life, sacrifice
//! another) should count as multiple activations for castability.
//!
//! https://github.com/phase-rs/phase/issues/10958
//!
//! Cards like Treasonous Ogre ("Pay 3 life: Add {R}"), Ashnod's Altar
//! ("Sacrifice a creature: Add {C}{C}"), Phyrexian Altar, etc. can be
//! activated multiple times in a single payment step. The castability check
//! must account for this by multiplying the per-activation mana output by the
//! maximum number of activations possible given the current game state.

use engine::game::casting::can_cast_object_now;
use engine::game::zones::create_object;
use engine::game::layers;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, ManaContribution,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use std::sync::Arc;

const P0_ID: PlayerId = PlayerId(0);

fn setup_main_phase(state: &mut GameState) {
    state.phase = Phase::PreCombatMain;
    state.active_player = P0_ID;
    state.priority_player = P0_ID;
    state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: P0_ID };
    // Flush layers so basic land intrinsic mana abilities are added.
    layers::flush_layers(state);
}

fn add_basic_land(state: &mut GameState, owner: PlayerId, color: engine::types::mana::ManaColor) -> ObjectId {
    let id = create_object(state, CardId(0), owner, "Basic Land".to_string(), engine::types::zones::Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    match color {
        engine::types::mana::ManaColor::White => obj.card_types.subtypes.push("Plains".to_string()),
        engine::types::mana::ManaColor::Blue => obj.card_types.subtypes.push("Island".to_string()),
        engine::types::mana::ManaColor::Black => obj.card_types.subtypes.push("Swamp".to_string()),
        engine::types::mana::ManaColor::Red => obj.card_types.subtypes.push("Mountain".to_string()),
        engine::types::mana::ManaColor::Green => obj.card_types.subtypes.push("Forest".to_string()),
    }
    obj.tapped = false;
    id
}

fn add_treasonous_ogre(state: &mut GameState) -> ObjectId {
    let id = create_object(state, CardId(1), P0_ID, "Treasonous Ogre".to_string(), engine::types::zones::Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(2);
    obj.toughness = Some(3);
    obj.summoning_sick = false;
    // "Pay 3 life: Add {R}."
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![engine::types::mana::ManaColor::Red],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    ).cost(AbilityCost::PayLife {
        amount: engine::types::ability::QuantityExpr::Fixed { value: 3 },
    }));
    id
}

fn add_ashnod_altar(state: &mut GameState) -> ObjectId {
    let id = create_object(state, CardId(2), P0_ID, "Ashnod's Altar".to_string(), engine::types::zones::Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    // "Sacrifice a creature: Add {C}{C}."
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    ).cost(AbilityCost::Sacrifice(engine::types::ability::SacrificeCost::count(
        engine::types::ability::TargetFilter::Typed(engine::types::ability::TypedFilter::new(engine::types::ability::TypeFilter::Creature)),
        1,
    ))));
    id
}

fn add_creature(state: &mut GameState, owner: PlayerId, name: &str, power: i32, toughness: i32) -> ObjectId {
    let id = create_object(state, CardId(0), owner, name.to_string(), engine::types::zones::Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.summoning_sick = false;
    id
}

fn add_spell_to_hand(state: &mut GameState, name: &str, cost: ManaCost) -> ObjectId {
    let id = create_object(state, CardId(0), P0_ID, name.to_string(), engine::types::zones::Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Instant);
    obj.mana_cost = cost;
    id
}

/// Divination {2}{U} with Island + one Treasonous Ogre at 20 life.
/// Should be castable (two Ogre activations = 6 life, leaving 14).
#[test]
fn divination_with_island_and_one_treasonous_ogre_is_castable() {
    let mut state = GameState::new_two_player(20);
    
    // Island for {U}
    add_basic_land(&mut state, P0_ID, engine::types::mana::ManaColor::Blue);
    // Treasonous Ogre at 20 life can activate 6 times (19 life / 3 = 6)
    add_treasonous_ogre(&mut state);
    // Divination {2}{U}
    let divination = add_spell_to_hand(&mut state, "Divination", ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 2,
    });

    setup_main_phase(&mut state);

    assert!(
        can_cast_object_now(&state, P0_ID, divination),
        "Divination should be castable with Island + one Treasonous Ogre at 20 life (two activations pay {{2}})"
    );
}

/// Ball Lightning {R}{R}{R} with one Treasonous Ogre at 20 life.
/// Should be castable (three Ogre activations = 9 life, leaving 11).
#[test]
fn ball_lightning_with_one_treasonous_ogre_is_castable() {
    let mut state = GameState::new_two_player(20);
    
    // Treasonous Ogre at 20 life can activate 6 times
    add_treasonous_ogre(&mut state);
    // Ball Lightning {R}{R}{R}
    let ball_lightning = add_spell_to_hand(&mut state, "Ball Lightning", ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red, ManaCostShard::Red],
        generic: 0,
    });

    setup_main_phase(&mut state);

    assert!(
        can_cast_object_now(&state, P0_ID, ball_lightning),
        "Ball Lightning should be castable with one Treasonous Ogre at 20 life (three activations pay {{R}}{{R}}{{R}})"
    );
}

/// Juggernaut {4} with one Ashnod's Altar + two Walking Corpses.
/// Should be castable (two Altar activations = {C}{C}{C}{C}).
#[test]
fn juggernaut_with_ashnod_altar_and_two_creatures_is_castable() {
    let mut state = GameState::new_two_player(20);
    
    // Ashnod's Altar
    add_ashnod_altar(&mut state);
    // Two creatures to sacrifice
    add_creature(&mut state, P0_ID, "Walking Corpse 1", 2, 2);
    add_creature(&mut state, P0_ID, "Walking Corpse 2", 2, 2);
    // Juggernaut {4}
    let juggernaut = add_spell_to_hand(&mut state, "Juggernaut", ManaCost::Cost {
        shards: vec![],
        generic: 4,
    });

    setup_main_phase(&mut state);

    assert!(
        can_cast_object_now(&state, P0_ID, juggernaut),
        "Juggernaut should be castable with Ashnod's Altar + two creatures (two activations pay {{4}})"
    );
}

/// Control: Divination with Island + two Treasonous Ogres at 5 life each.
/// Should be castable (two Ogres can each activate once, 2 red = {2} generic).
#[test]
fn divination_with_two_treasonous_ogres_at_low_life_is_castable() {
    let mut state = GameState::new_two_player(5);
    
    // Island for {U}
    add_basic_land(&mut state, P0_ID, engine::types::mana::ManaColor::Blue);
    // Two Treasonous Ogres at 5 life each can only activate once each (4 life / 3 = 1)
    add_treasonous_ogre(&mut state);
    add_treasonous_ogre(&mut state);
    // Divination {2}{U}
    let divination = add_spell_to_hand(&mut state, "Divination", ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 2,
    });

    setup_main_phase(&mut state);

    assert!(
        can_cast_object_now(&state, P0_ID, divination),
        "Divination should be castable with two Treasonous Ogres at 5 life each (each activates once, 2 red = {{2}})"
    );
}

/// Divination with only Island - should NOT be castable (need {2}{U}, only have {U})
#[test]
fn divination_with_island_only_is_not_castable() {
    let mut state = GameState::new_two_player(20);
    
    // Island for {U}
    add_basic_land(&mut state, P0_ID, engine::types::mana::ManaColor::Blue);
    // Divination {2}{U}
    let divination = add_spell_to_hand(&mut state, "Divination", ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 2,
    });

    setup_main_phase(&mut state);

    assert!(
        !can_cast_object_now(&state, P0_ID, divination),
        "Divination should NOT be castable with only Island (need {{2}}{{U}}, only have {{U}})"
    );
}