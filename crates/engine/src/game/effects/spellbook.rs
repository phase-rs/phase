//! Alchemy "draft a card from [this card]'s spellbook" (digital-only, no CR
//! entry). A spellbook is a fixed, card-specific list of card names. Drafting
//! means: reveal the list, the controller chooses one card, and that card is
//! created in a destination zone.
//!
//! This composes two existing building blocks rather than reinventing them:
//!
//! * the interactive choice pauses on [`WaitingFor::SpellbookDraft`] (resumed by
//!   `GameAction::SubmitSpellbookDraft` in `engine.rs`); and
//! * the card creation delegates to [`super::conjure`] — the chosen name is
//!   conjured into the destination, reusing the registry lookup, characteristic
//!   application, summoning-sickness reset and ETB zone-change emission.
//!
//! The spellbook list itself is not in the Oracle text — it is carried on the
//! source object (`GameObject::spellbook`, copied from
//! `CardFace::metadata.spellbook`); the resolver reads it from the source.

use crate::types::ability::{
    ConjureCard, ConjureSource, Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::zones::Zone;

/// Resolve [`Effect::DraftFromSpellbook`]: present the source card's spellbook
/// list for the controller to choose from. With an empty list (no spellbook
/// data) the draft is a no-op.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::DraftFromSpellbook {
        destination,
        tapped,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("DraftFromSpellbook".to_string()));
    };

    // The spellbook list lives on the source object (from its card face). Read
    // it from the source; an empty/missing list resolves as a no-op rather than
    // pausing on an empty choice.
    let spellbook = state
        .objects
        .get(&ability.source_id)
        .map(|obj| obj.spellbook.clone())
        .unwrap_or_default();

    if spellbook.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Digital-only Alchemy spellbook choice: pause for the controller to choose
    // one card from the list.
    state.waiting_for = WaitingFor::SpellbookDraft {
        player: ability.controller,
        source_id: ability.source_id,
        options: spellbook,
        destination: *destination,
        tapped: *tapped,
    };
    Ok(())
}

/// Complete a spellbook draft once the controller has chosen `card`: conjure the
/// chosen card into `destination`. Reuses [`super::conjure::resolve`] for the
/// actual card creation. `card` must be one of the offered `options`.
#[allow(clippy::too_many_arguments)]
pub fn complete_draft(
    state: &mut GameState,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
    options: &[String],
    card: &str,
    destination: Zone,
    tapped: bool,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !options.iter().any(|name| name == card) {
        return Err(EffectError::InvalidParam(format!(
            "{card} is not in the offered spellbook"
        )));
    }

    // Conjure the chosen card — one copy — into the destination, reusing the
    // single card-creation authority.
    let conjure = ResolvedAbility::new(
        Effect::Conjure {
            cards: vec![ConjureCard {
                source: ConjureSource::Named {
                    name: card.to_string(),
                },
                count: QuantityExpr::Fixed { value: 1 },
            }],
            destination,
            tapped,
        },
        Vec::new(),
        source_id,
        controller,
    );
    super::conjure::resolve(state, &conjure, events)
}
