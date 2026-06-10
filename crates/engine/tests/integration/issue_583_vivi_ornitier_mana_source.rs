//! Issue #583 — Vivi Ornitier's {0} combination mana ability must count toward
//! castability when it is the only remaining mana source.

use engine::game::casting::can_cast_object_now;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Effect, ManaProduction,
    ObjectScope, QuantityExpr, QuantityRef,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use std::sync::Arc;

const P0: PlayerId = PlayerId(0);

fn setup_priority(state: &mut GameState) {
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: P0 };
}

fn add_vivi(state: &mut GameState, power: i32) -> ObjectId {
    let id = create_object(
        state,
        CardId(5830),
        P0,
        "Vivi Ornitier".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(2);
    obj.summoning_sick = false;
    Arc::make_mut(&mut obj.abilities).push(
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::Source,
                        },
                    },
                    color_options: vec![ManaColor::Blue, ManaColor::Red],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![],
                generic: 0,
            },
        })
        .activation_restrictions(vec![
            ActivationRestriction::DuringYourTurn,
            ActivationRestriction::OnlyOnceEachTurn,
        ]),
    );
    id
}

fn add_hand_spell(state: &mut GameState, card_id: CardId, cost: ManaCost) -> ObjectId {
    let id = create_object(state, card_id, P0, "Test Spell".to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Instant);
    obj.mana_cost = cost;
    id
}

#[test]
fn vivi_combination_mana_makes_colored_spell_castable_without_other_sources() {
    let mut state = GameState::new_two_player(583);
    setup_priority(&mut state);
    let vivi = add_vivi(&mut state, 2);
    let spell = add_hand_spell(
        &mut state,
        CardId(5831),
        ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
            generic: 0,
        },
    );

    assert!(
        can_cast_object_now(&state, P0, spell),
        "Vivi's zero-cost combination mana must make {{U}}{{R}} castable when power is 2"
    );

    let actions = engine::ai_support::legal_actions(&state);
    assert!(
        !engine::ai_support::auto_pass_recommended(&state, &actions),
        "auto_pass must not skip priority when a spell is castable via Vivi mana"
    );

    let _ = vivi;
}

#[test]
fn vivi_power_two_does_not_cover_colored_shards_plus_generic() {
    let mut state = GameState::new_two_player(583);
    setup_priority(&mut state);
    add_vivi(&mut state, 2);
    let spell = add_hand_spell(
        &mut state,
        CardId(5833),
        ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
            generic: 1,
        },
    );

    assert!(
        !can_cast_object_now(&state, P0, spell),
        "one Vivi activation produces two mana total and cannot also pay {{1}}"
    );
}

#[test]
fn vivi_power_three_surplus_still_covers_two_shards() {
    let mut state = GameState::new_two_player(583);
    setup_priority(&mut state);
    add_vivi(&mut state, 3);
    let spell = add_hand_spell(
        &mut state,
        CardId(5834),
        ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
            generic: 0,
        },
    );

    assert!(
        can_cast_object_now(&state, P0, spell),
        "Vivi power 3 over-produces for {{U}}{{R}} — surplus mana must not make the spell uncastable"
    );
}

#[test]
fn vivi_single_blue_shard_castable_at_power_one() {
    let mut state = GameState::new_two_player(583);
    setup_priority(&mut state);
    add_vivi(&mut state, 1);
    let spell = add_hand_spell(
        &mut state,
        CardId(5832),
        ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        },
    );

    assert!(
        can_cast_object_now(&state, P0, spell),
        "Vivi power 1 must cover a single {{U}} shard"
    );
}
