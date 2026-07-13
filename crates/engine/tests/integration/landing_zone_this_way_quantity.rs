//! Runtime coverage for "the number of <type> {returned | put into a graveyard}
//! this way" quantities — the bounce/destroy landing-zone sibling of the
//! destroyed/sacrificed tracked-set count (see `issue_2943_kayas_wrath.rs`).
//!
//! Before the fix the downstream damage clause lowered to `Effect::Unimplemented`
//! because `parse_event_context_quantity` had no arm for the return-to-hand /
//! put-into-graveyard "this way" suffixes. The fix routes them through
//! `QuantityRef::FilteredTrackedSetSize { caused_by: None }`.
//!
//! Class: Volcanic Eruption ("put into a graveyard this way", via destroy) and
//! Barrel Down Sokenzan ("returned this way", via bounce).
//!
//! CR 701.8a: Destroy moves battlefield permanents to their owner's graveyard.
//! CR 400.7j: The same effect can still find those objects to count them.
//! CR 608.2c: A downstream sub-ability counts only objects the preceding effect
//!   moved this way, restricted here by the Mountain subtype filter.
//! CR 120.3: Damage dealt equals that filtered count.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, Effect, QuantityExpr, QuantityRef, TargetFilter, TypeFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim damage sentence from Volcanic Eruption; the destroy lead is adapted
/// to "Destroy all Mountains" so the ability needs no target selection (the
/// quantity-bearing sentence — the seam under test — is the card's real text).
const VOLCANIC_ERUPTION_ORACLE: &str = "Destroy all Mountains. ~ deals damage to each creature \
and each player equal to the number of Mountains put into a graveyard this way.";

fn spawn_mountain(state: &mut GameState, card_id: CardId, owner: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        card_id,
        owner,
        "Mountain".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Land];
    obj.card_types.subtypes = vec!["Mountain".to_string()];
    id
}

fn spawn_creature(
    state: &mut GameState,
    card_id: CardId,
    owner: PlayerId,
    toughness: i32,
) -> ObjectId {
    let id = create_object(
        state,
        card_id,
        owner,
        "Bystander".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.power = Some(1);
    obj.toughness = Some(toughness);
    id
}

/// Parse-shape reach-guard: the damage clause must lower to a real `DamageAll`
/// whose amount reads the filtered tracked set — NOT `Effect::Unimplemented`.
/// This is the assertion that flips when the parser fix is reverted.
#[test]
fn volcanic_eruption_lowers_to_destroy_all_plus_filtered_tracked_set_damage() {
    let def = parse_effect_chain(VOLCANIC_ERUPTION_ORACLE, AbilityKind::Spell);
    match def.effect.as_ref() {
        Effect::DestroyAll {
            target: TargetFilter::Typed(tf),
            ..
        } => assert_eq!(
            tf.type_filters,
            vec![TypeFilter::Subtype("Mountain".to_string())]
        ),
        other => panic!("expected DestroyAll{{Mountain}}, got {other:?}"),
    }
    let damage = def
        .sub_ability
        .as_ref()
        .expect("damage must be a sub_ability of DestroyAll");
    match damage.effect.as_ref() {
        Effect::DamageAll {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::FilteredTrackedSetSize { filter, caused_by },
                },
            player_filter: Some(_),
            ..
        } => {
            assert_eq!(*caused_by, None, "landing-zone count is action-agnostic");
            match filter.as_ref() {
                TargetFilter::Typed(tf) => assert_eq!(
                    tf.type_filters,
                    vec![TypeFilter::Subtype("Mountain".to_string())]
                ),
                other => panic!("expected Typed Mountain filter, got {other:?}"),
            }
        }
        other => panic!("expected DamageAll(FilteredTrackedSetSize), got {other:?}"),
    }
}

/// Runtime: destroy 2 Mountains, then deal damage to each creature and each
/// player equal to the number put into a graveyard this way (= 2). Reverting the
/// parser fix makes the damage clause `Unimplemented`, so no damage is dealt and
/// every assertion below flips.
#[test]
fn volcanic_eruption_damages_each_creature_and_player_by_mountains_destroyed() {
    let mut state = GameState::new_two_player(42);
    let m1 = spawn_mountain(&mut state, CardId(1), PlayerId(0));
    let m2 = spawn_mountain(&mut state, CardId(2), PlayerId(0));
    // Bystander with high toughness so it survives to carry the marked damage.
    let bystander = spawn_creature(&mut state, CardId(3), PlayerId(1), 10);
    let p0_life = state.players[0].life;
    let p1_life = state.players[1].life;

    let def = parse_effect_chain(VOLCANIC_ERUPTION_ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, ObjectId(100), PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // Both Mountains were put into a graveyard this way → count is 2.
    assert_eq!(state.objects[&m1].zone, Zone::Graveyard);
    assert_eq!(state.objects[&m2].zone, Zone::Graveyard);
    // CR 120.3: each creature and each player took exactly that count.
    assert_eq!(
        state.objects[&bystander].damage_marked, 2,
        "bystander creature must take damage == Mountains put into a graveyard this way"
    );
    assert_eq!(
        state.players[0].life,
        p0_life - 2,
        "each player takes damage == Mountains put into a graveyard this way"
    );
    assert_eq!(state.players[1].life, p1_life - 2);
}

/// Sibling: a bare "returned this way" (bounce to hand) publishes the same
/// tracked set under a different producer cause, and `caused_by: None` still
/// counts every Mountain returned. This is the Barrel Down Sokenzan producer
/// path (its damage targets a creature; here we assert the runtime tracked-set
/// COUNT via a no-target `DamageEachPlayer` shell so the harness needs no target
/// selection while still driving the real bounce → count resolution).
#[test]
fn returned_this_way_bounce_publishes_countable_tracked_set() {
    const BOUNCE_ORACLE: &str = "Return all Mountains to their owners' hands. ~ deals damage to \
each player equal to twice the number of Mountains returned this way.";
    let mut state = GameState::new_two_player(42);
    let m1 = spawn_mountain(&mut state, CardId(1), PlayerId(0));
    let m2 = spawn_mountain(&mut state, CardId(2), PlayerId(0));
    let m3 = spawn_mountain(&mut state, CardId(3), PlayerId(0));
    let p0_life = state.players[0].life;

    let def = parse_effect_chain(BOUNCE_ORACLE, AbilityKind::Spell);
    // Reach-guard: the damage sibling must be a real effect (not Unimplemented)
    // whose amount doubles the returned-this-way count.
    let damage = def
        .sub_ability
        .as_ref()
        .expect("damage must be a sub_ability of the bounce");
    assert!(
        matches!(
            damage.effect.as_ref(),
            Effect::DamageEachPlayer {
                amount: QuantityExpr::Multiply { .. },
                ..
            }
        ),
        "expected DamageEachPlayer with a doubled tracked-set amount, got {:?}",
        damage.effect
    );

    let ability = build_resolved_from_def(&def, ObjectId(100), PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // 3 Mountains returned to hand → twice the count = 6 damage to each player.
    assert_eq!(state.objects[&m1].zone, Zone::Hand);
    assert_eq!(state.objects[&m2].zone, Zone::Hand);
    assert_eq!(state.objects[&m3].zone, Zone::Hand);
    assert_eq!(
        state.players[0].life,
        p0_life - 6,
        "damage must equal twice the Mountains returned this way (3 * 2 = 6)"
    );
}
