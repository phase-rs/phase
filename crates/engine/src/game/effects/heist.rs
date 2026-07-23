//! Heist — designed-for-digital (MTG Arena) keyword action.
//!
//! Heist is NOT in the Comprehensive Rules; it operates per the Arena programmed
//! rules. Reminder text: *"Look at three random nonland cards from target
//! opponent's library. Exile one of them face down. You may cast that card for
//! as long as it remains exiled, and you may spend mana as though it were mana
//! of any type to cast that spell."* It is also a crime against the targeted
//! opponent.
//!
//! Two cohesive phases share this module:
//!
//! - [`Heist`](crate::types::ability::Effect::Heist) — the look step. Resolves
//!   the targeted opponent, draws `look_count` (default 3) random nonland cards
//!   from their library with the seeded RNG, and surfaces a
//!   [`WaitingFor::ChooseFromZoneChoice`] over those candidates. A
//!   [`HeistExile`](crate::types::ability::Effect::HeistExile) continuation is
//!   stashed so the existing zone-choice answer handler finalizes the pick. The
//!   candidates never leave the library during the look — only the chosen card
//!   is moved by the finalizer, so the unchosen cards are untouched (CR-faithful
//!   to "exile one of them").
//!
//! - [`HeistExile`](crate::types::ability::Effect::HeistExile) — the finalizer
//!   continuation. The chosen card (carried on `ability.targets` by the answer
//!   handler) is exiled from its owner's library, turned face down (CR 406.3),
//!   linked to the source so the controller may look at it (mirrors Hideaway's
//!   [`ExileLinkKind::HideawayLookable`]), and granted a permanent
//!   [`PlayFromExile`](crate::types::ability::CastingPermission::PlayFromExile)
//!   permission with any-type-or-color mana. The controller may then cast that
//!   card for as long as it remains exiled.

use crate::game::exile_links;
use crate::game::zone_pipeline::{self, ZoneMoveRequest};
use crate::types::ability::{
    CastingPermission, Effect, EffectError, EffectKind, ManaSpendPermission, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{ExileLinkKind, GameState, PendingContinuation, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::statics::CastFrequency;
use crate::types::zones::Zone;

use rand::seq::IndexedRandom;

/// Heist look step — see module docs.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target_filter, look_count) = match &ability.effect {
        Effect::Heist { target, look_count } => (target.clone(), *look_count),
        _ => return Err(EffectError::InvalidParam("Expected Heist".to_string())),
    };

    let controller = ability.controller;
    let source_id = ability.source_id;

    // The targeted opponent is resolved from the ability's player targets (the
    // targeting phase requested it via `Effect::Heist.target`). If the filter
    // is `Any`/unspecified, fall back to the first opponent in APNAP order so
    // a programmatically-constructed ability still resolves.
    let opponent = ability
        .targets
        .iter()
        .find_map(|t| match t {
            TargetRef::Player(p) => Some(*p),
            _ => None,
        })
        .or_else(|| {
            (matches!(target_filter, TargetFilter::Any | TargetFilter::None))
                .then(|| {
                    crate::game::players::opponents(state, controller)
                        .into_iter()
                        .next()
                })
                .flatten()
        })
        .ok_or_else(|| EffectError::MissingParam("Heist target opponent".to_string()))?;

    // Collect the nonland cards in the opponent's library. Lands are skipped per
    // the reminder text ("random nonland cards").
    let nonland: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == opponent)
        .ok_or(EffectError::PlayerNotFound)?
        .library
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| !obj.card_types.core_types.contains(&CoreType::Land))
        })
        .collect();

    if nonland.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Heist,
            source_id,
            subject: None,
        });
        return Ok(());
    }

    // CR 608.2d (override analogue): draw `look_count` distinct random nonland
    // cards using the seeded RNG. Clamp to [1, pool size].
    let look = (look_count.max(1) as usize).min(nonland.len());
    let candidates: Vec<ObjectId> = nonland
        .choose_multiple(&mut state.rng, look)
        .copied()
        .collect();

    // Stash the finalizer continuation. The ChooseFromZoneChoice answer handler
    // injects the single chosen card onto `cont.chain.targets`, and because
    // `HeistExile` carries no `sub_ability`, the unchosen candidates are never
    // forwarded anywhere — they simply stay in the library.
    let finalize = ResolvedAbility::new(Effect::HeistExile, vec![], source_id, controller);
    state.park_ability_continuation(PendingContinuation::new(Box::new(finalize), state));

    state.waiting_for = WaitingFor::ChooseFromZoneChoice {
        player: controller,
        cards: candidates,
        count: 1,
        up_to: false,
        constraint: None,
        source_id,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Heist,
        source_id,
        subject: None,
    });

    Ok(())
}

/// Heist finalizer continuation — see module docs.
pub fn resolve_exile(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::HeistExile = &ability.effect else {
        return Err(EffectError::InvalidParam("Expected HeistExile".to_string()));
    };

    let controller = ability.controller;
    let source_id = ability.source_id;

    // The chosen card is carried on the continuation's targets by the
    // ChooseFromZoneChoice answer handler.
    let chosen: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .collect();

    for obj_id in chosen {
        // Only finalize a card still in a library (the look step did not move
        // it). A card that left the library via a replacement before this step
        // resolves is skipped.
        let in_library = state
            .objects
            .get(&obj_id)
            .is_some_and(|obj| obj.zone == Zone::Library);
        if !in_library {
            continue;
        }

        // CR 406.3 + CR 614.6: exile via the zone-change pipeline so a board-wide
        // redirect is consulted (mirrors `discover.rs`). Park on a needs-choice
        // replacement rather than proceeding past a parked prompt.
        let result = zone_pipeline::move_object(
            state,
            ZoneMoveRequest::effect(obj_id, Zone::Exile, source_id),
            events,
        );
        if let zone_pipeline::ZoneMoveResult::NeedsChoice(player) = result {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }

        // CR 406.3: turn the exiled card face down. CR 702.75a analogue: link it
        // to the source with `HideawayLookable` so `visibility.rs` grants the
        // controller the "may look at this card in exile" permission (the
        // opponent cannot see it).
        let linked = state.objects.get_mut(&obj_id).is_some_and(|obj| {
            if obj.zone == Zone::Exile {
                obj.face_down = true;
                true
            } else {
                false
            }
        });
        if linked {
            exile_links::push_with_kind(state, obj_id, source_id, ExileLinkKind::HideawayLookable);

            // Permanent "cast from exile" permission with any-type-or-color mana
            // (reminder: "for as long as it remains exiled" + "spend mana as
            // though it were mana of any type"). Bound to the heist's
            // controller; `exiled_by_ability_controller` records provenance.
            if let Some(obj) = state.objects.get_mut(&obj_id) {
                obj.casting_permissions
                    .push(CastingPermission::PlayFromExile {
                        duration: crate::types::ability::Duration::Permanent,
                        granted_to: controller,
                        frequency: CastFrequency::Unlimited,
                        source_id: Some(source_id),
                        invalidation: None,
                        exiled_by_ability_controller: Some(controller),
                        mana_spend_permission: Some(ManaSpendPermission::AnyTypeOrColor),
                        card_filter: None,
                        single_use_group: None,
                        single_use: false,
                        cast_cost_raise: None,
                        land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    });
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::HeistExile,
        source_id,
        subject: None,
    });

    Ok(())
}
