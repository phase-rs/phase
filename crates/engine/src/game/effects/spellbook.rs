//! Alchemy spellbooks (digital-only, no CR entry). A spellbook is a fixed,
//! card-specific list of card names. [`Effect::DraftFromSpellbook`] takes one
//! card from it and creates that card in a destination zone, in one of three
//! wordings ([`SpellbookSelection`]):
//!
//! * "draft a card from X's spellbook" — the game offers three distinct entries
//!   chosen at random and the controller chooses one of them;
//! * "conjure a card of your choice from X's spellbook" — the controller
//!   chooses from the whole spellbook;
//! * "conjure a random card from X's spellbook" — one entry is picked at
//!   random, with no prompt.
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

use crate::types::game_state::RandomDraw;
use rand::seq::IndexedRandom; // rand 0.9: `choose_multiple` on `[T]` lives here.
use rand::Rng;

use crate::types::ability::{
    ConjureCard, ConjureSource, Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility,
    SpellbookSelection,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::zones::Zone;

/// Digital-only Alchemy (no CR entry): a draft shows three cards chosen at
/// random from the spellbook. Each draft samples independently.
const SPELLBOOK_DRAFT_OFFER_SIZE: usize = 3;

/// Resolve [`Effect::DraftFromSpellbook`]: pick from the source card's
/// spellbook list per its [`SpellbookSelection`] — raise the controller's choice
/// over a random three-card offer (`Draft`) or the whole book (`Chosen`), or
/// conjure a random entry immediately (`Random`). With an empty list (no
/// spellbook data) the draft is a no-op.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::DraftFromSpellbook {
        destination,
        tapped,
        selection,
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
        // A draft that delivers nothing publishes no created referent, so a
        // following "it" (the exile rider's `TargetFilter::LastCreated`) cannot
        // reach an object an earlier instruction created. The producer
        // convention of `conjure.rs` / `token.rs`: the ledger is ASSIGNED, never
        // left stale. Reached by a token copy of a spellbook card, which carries
        // no spellbook.
        state.last_created_token_ids = Vec::new();
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    let options = match selection {
        // "conjure a random card from ~'s spellbook": the engine picks one
        // uniformly at random and conjures it immediately, instead of pausing
        // for the controller to choose. It shares the card-creation path
        // (`complete_draft`) with the interactive selections.
        SpellbookSelection::Random => {
            let index = state
                .rng
                .draw(RandomDraw::Outcome)
                .random_range(0..spellbook.len());
            let card = spellbook[index].clone();
            return complete_draft(
                state,
                ability.controller,
                ability.source_id,
                &spellbook,
                &card,
                *destination,
                *tapped,
                events,
            );
        }
        // "conjure a card of your choice from ~'s spellbook": the whole book.
        SpellbookSelection::Chosen => spellbook,
        // "draft a card from ~'s spellbook": a random three-card offer.
        SpellbookSelection::Draft => draft_offer(state, &spellbook),
    };

    // Digital-only Alchemy spellbook choice: pause for the controller to choose
    // one card from the offer.
    state.waiting_for = WaitingFor::SpellbookDraft {
        player: ability.controller,
        source_id: ability.source_id,
        options,
        destination: *destination,
        tapped: *tapped,
    };
    Ok(())
}

/// Digital-only Alchemy (no CR entry): the cards a draft shows — up to
/// [`SPELLBOOK_DRAFT_OFFER_SIZE`] entries sampled at random without
/// replacement. MTGJSON spellbook entries are distinct names (`jq '.data |
/// to_entries[] | .value[0] as $f | select($f.relatedCards.spellbook) |
/// [.key, ($f.relatedCards.spellbook | length), ($f.relatedCards.spellbook |
/// unique | length)]'` over `AtomicCards.json` shows equal counts), so the
/// offer holds distinct cards.
fn draft_offer(state: &mut GameState, spellbook: &[String]) -> Vec<String> {
    spellbook
        .choose_multiple(
            state.rng.draw(RandomDraw::Outcome),
            SPELLBOOK_DRAFT_OFFER_SIZE.min(spellbook.len()),
        )
        .cloned()
        .collect()
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
            library_position: None,
            library_players: None,
        },
        Vec::new(),
        source_id,
        controller,
    );
    super::conjure::resolve(state, &conjure, events)
}
