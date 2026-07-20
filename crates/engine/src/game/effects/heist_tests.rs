//! Tests for the Heist keyword action (Arena digital-only).
//!
//! `heist.rs` is kept implementation-only (no inline `#[cfg(test)]`) so it
//! classifies as a SOURCE file for scoring; these tests live in this sibling
//! file declared from `effects/mod.rs`.

use crate::game::effects::heist;
use crate::game::zones::create_object;
use crate::types::ability::{
    CastingPermission, Effect, ManaSpendPermission, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Build a Heist ability controlled by `controller` targeting `opponent`.
fn heist_ability(controller: PlayerId, opponent: PlayerId, source: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Heist {
            target: TargetFilter::Typed(Default::default()),
            look_count: 3,
        },
        vec![TargetRef::Player(opponent)],
        source,
        controller,
    )
}

/// Put a named nonland card into `player`'s library and stamp a creature type +
/// mana value on it so it is a heistable target.
fn library_creature(
    state: &mut GameState,
    card_id: CardId,
    player: PlayerId,
    name: &str,
) -> ObjectId {
    let id = create_object(state, card_id, player, name.to_string(), Zone::Library);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.mana_cost = ManaCost::generic(2);
    id
}

#[test]
fn heist_offers_random_nonland_candidates_from_opponent_library() {
    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    let c1 = library_creature(&mut state, CardId(1), opponent, "Bear");
    let c2 = library_creature(&mut state, CardId(2), opponent, "Goblin");
    let c3 = library_creature(&mut state, CardId(3), opponent, "Elf");
    // A land in the opponent's library must never be offered.
    let land = create_object(
        &mut state,
        CardId(4),
        opponent,
        "Forest".to_string(),
        Zone::Library,
    );
    state
        .objects
        .get_mut(&land)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    let ability = heist_ability(controller, opponent, ObjectId(100));
    let mut events = Vec::new();
    heist::resolve(&mut state, &ability, &mut events).unwrap();

    match &state.waiting_for {
        WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            up_to,
            ..
        } => {
            assert_eq!(*player, controller);
            assert_eq!(*count, 1, "exactly one card is exiled");
            assert!(!up_to);
            // The three offered candidates are the nonland creatures; the land
            // is excluded. None of them have left the library yet (the look step
            // does not move them).
            assert_eq!(cards.len(), 3, "look at three random nonland cards");
            for id in &[c1, c2, c3] {
                assert!(cards.contains(id), "nonland candidate {id:?} missing");
            }
            assert!(!cards.contains(&land), "land must never be offered");
            for id in cards {
                assert_eq!(
                    state.objects[id].zone,
                    Zone::Library,
                    "candidates stay in the library during the look step"
                );
            }
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    }

    // A HeistExile continuation is parked to finalize the chosen card.
    assert!(
        state.active_ability_continuation().is_some(),
        "Heist must stash a HeistExile continuation",
    );
    assert!(matches!(
        &state.active_ability_continuation().unwrap().chain.effect,
        Effect::HeistExile
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: crate::types::ability::EffectKind::Heist,
            ..
        }
    )));
}

#[test]
fn heist_clamps_look_count_to_available_nonland_pool() {
    let mut state = GameState::new_two_player(7);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    let only = library_creature(&mut state, CardId(1), opponent, "Lonely Bear");

    let ability = heist_ability(controller, opponent, ObjectId(100));
    let mut events = Vec::new();
    heist::resolve(&mut state, &ability, &mut events).unwrap();

    match &state.waiting_for {
        WaitingFor::ChooseFromZoneChoice { cards, count, .. } => {
            assert_eq!(cards.len(), 1);
            assert!(cards.contains(&only));
            assert_eq!(*count, 1);
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    }
}

#[test]
fn heist_with_no_nonland_cards_is_a_noop() {
    let mut state = GameState::new_two_player(9);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    // Only lands — nothing to heist.
    let land = create_object(
        &mut state,
        CardId(1),
        opponent,
        "Forest".to_string(),
        Zone::Library,
    );
    state
        .objects
        .get_mut(&land)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    let ability = heist_ability(controller, opponent, ObjectId(100));
    let mut events = Vec::new();
    heist::resolve(&mut state, &ability, &mut events).unwrap();

    // No choice raised, no continuation stashed, but the effect still resolves.
    assert!(
        !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
        "no choice when the opponent has no heistable nonland cards",
    );
    assert!(state.active_ability_continuation().is_none());
    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: crate::types::ability::EffectKind::Heist,
            ..
        }
    )));
}

#[test]
fn heist_exile_finalizer_exiles_chosen_face_down_links_and_grants() {
    let mut state = GameState::new_two_player(11);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);
    let source = ObjectId(100);

    let chosen = library_creature(&mut state, CardId(1), opponent, "Stolen Bear");

    // The answer handler injects the chosen card onto the continuation's targets.
    let finalize = ResolvedAbility::new(
        Effect::HeistExile,
        vec![TargetRef::Object(chosen)],
        source,
        controller,
    );

    let mut events = Vec::new();
    heist::resolve_exile(&mut state, &finalize, &mut events).unwrap();

    let obj = &state.objects[&chosen];
    assert_eq!(obj.zone, Zone::Exile, "chosen card is exiled");
    assert!(obj.face_down, "exiled card is face down (CR 406.3)");
    // Linked to the source so the controller may look at it.
    assert!(state
        .exile_links
        .iter()
        .any(|link| { link.exiled_id == chosen && link.source_id == source }));
    // Permanent cast-from-exile permission with any-type-or-color mana.
    assert!(obj.casting_permissions.iter().any(|perm| matches!(
        perm,
        CastingPermission::PlayFromExile {
            mana_spend_permission: Some(ManaSpendPermission::AnyTypeOrColor),
            exiled_by_ability_controller: Some(p),
            ..
        } if *p == controller
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: crate::types::ability::EffectKind::HeistExile,
            ..
        }
    )));
}

#[test]
fn heist_exile_finalizer_skips_a_card_that_left_the_library() {
    let mut state = GameState::new_two_player(13);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    // The "chosen" card has already left the library (moved to hand by a
    // replacement) before the finalizer runs.
    let gone = library_creature(&mut state, CardId(1), opponent, "Gone");
    state.objects.get_mut(&gone).unwrap().zone = Zone::Hand;

    let finalize = ResolvedAbility::new(
        Effect::HeistExile,
        vec![TargetRef::Object(gone)],
        ObjectId(100),
        controller,
    );
    let mut events = Vec::new();
    heist::resolve_exile(&mut state, &finalize, &mut events).unwrap();

    // Not exiled, not granted — guarded by the in-library check.
    assert_eq!(state.objects[&gone].zone, Zone::Hand);
    assert!(state.objects[&gone].casting_permissions.is_empty());
    assert!(state.exile_links.is_empty());
}
