//! Digital-only Alchemy (no CR entry): `Effect::Intensify` — increase the
//! intensity of one or more cards.
//!
//! Intensity is a per-card value (`GameObject::intensity`) that follows a card
//! through every zone. "Intensify by N" therefore applies across ALL zones, not
//! just the battlefield, and its scope is one of:
//!
//! * [`IntensityScope::Source`] — "this creature/artifact/… intensifies";
//! * [`IntensityScope::OwnedSameName`] — "cards you own named [this card]
//!   intensify" (every copy you own, anywhere);
//! * [`IntensityScope::OwnedSubtype`] — "all [subtype] cards you own intensify".
//!
//! Because `GameState::objects` is the single store of every object in every
//! zone, an owner-scoped scan over it covers hand, library, graveyard, stack,
//! exile, and battlefield uniformly.

use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, IntensityScope, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// Resolve `Effect::Intensify`: add `amount` to the intensity of every card in
/// `scope`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (scope, amount) = match &ability.effect {
        Effect::Intensify { scope, amount } => (scope.clone(), amount.clone()),
        _ => return Err(EffectError::MissingParam("Intensify".to_string())),
    };

    let by = resolve_quantity_with_targets(state, &amount, ability).max(0) as u32;
    let mut changed = false;
    if by > 0 {
        for id in objects_in_scope(state, ability.source_id, ability.controller, &scope) {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.intensity = obj.intensity.saturating_add(by);
                // Emit per affected card so triggers/animation see exactly which
                // cards intensified (CR-less Alchemy event).
                events.push(GameEvent::ObjectIntensified {
                    object_id: id,
                    amount: by,
                });
                changed = true;
            }
        }
    }

    // No-op contract: a zero-amount intensify (or empty scope) publishes no
    // resolution event, so event-counting/trigger-index consumers aren't
    // perturbed by a draft that changed nothing.
    if changed {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
    }
    Ok(())
}

/// The objects an Intensify effect applies to, across every zone.
fn objects_in_scope(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    scope: &IntensityScope,
) -> Vec<ObjectId> {
    match scope {
        IntensityScope::Source => vec![source_id],
        IntensityScope::OwnedSameName => {
            let Some(name) = state.objects.get(&source_id).map(|o| o.name.clone()) else {
                return Vec::new();
            };
            owned_matching(state, controller, |o| o.name == name)
        }
        IntensityScope::OwnedSubtype { subtype } => owned_matching(state, controller, |o| {
            o.card_types.subtypes.iter().any(|s| s == subtype)
        }),
    }
}

/// Every object `controller` owns (in any zone) that satisfies `pred`.
fn owned_matching(
    state: &GameState,
    controller: PlayerId,
    pred: impl Fn(&crate::game::game_object::GameObject) -> bool,
) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(_, obj)| obj.owner == controller && pred(obj))
        .map(|(id, _)| *id)
        .collect()
}
