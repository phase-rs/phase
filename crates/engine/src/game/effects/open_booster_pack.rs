//! CR 400.11 + CR 400.11b + CR 701.20: `Effect::OpenBoosterPack` resolver.
//!
//! Opens a sealed Magic booster pack (see `game::boosters` for how a pack is
//! collated), reveals its cards, and offers the ones matching the effect's
//! filter as an outside-the-game choice. The chosen cards are brought into the
//! game at the effect's destination by the shared
//! `WaitingFor::OutsideGameChoice` handler; the rest were never in any zone
//! (CR 400.11: "Outside the game is not a zone") and are neither exiled nor put
//! into a graveyard — they simply stay outside the game.
//!
//! The canonical card is Booster Tutor ("Open a sealed Magic booster pack,
//! reveal the cards, and put one of them into your hand").

use crate::game::boosters;
use crate::game::filter::matches_target_filter_against_face;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, OutsideGameChoiceEntry, OutsideGameChoiceSource, WaitingFor,
};

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::OpenBoosterPack {
        filter,
        count,
        destination,
        reveal,
    } = &ability.effect
    else {
        return Ok(());
    };
    let (inner_count, up_to) = count.peel_up_to();
    let count = resolve_quantity_with_targets(state, inner_count, ability).max(0) as usize;
    let destination = *destination;
    let reveal = *reveal;
    let filter = filter.clone();
    let player = ability.controller;

    // The shelf is stocked at rehydrate for any game whose cards can open a
    // pack (`boosters::game_opens_booster_packs`). An empty shelf means the
    // active source cannot fill a pack — an unavailable cube or a database
    // without a fillable product. CR 609.3 ("do as much as possible"):
    // open nothing rather than fail the whole resolution.
    let pack = {
        let shelf = state.booster_shelf.clone();
        let Some(pack) = boosters::open_pack(&shelf, &mut state.rng) else {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::OpenBoosterPack,
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        };
        pack
    };
    let (origin, cards) = pack;

    // CR 701.20: "reveal the cards" — the WHOLE pack becomes public, not only
    // the card that is taken. The pack's cards are outside the game and have no
    // `ObjectId`, so the reveal carries names only; `card_ids` stays empty (its
    // `serde(default)` shape) and no `revealed_cards` entry is needed, because
    // there is no in-game object whose visibility could be filtered.
    if reveal {
        state.last_revealed_ids.clear();
        events.push(GameEvent::CardsRevealed {
            player,
            card_ids: Vec::new(),
            card_names: cards.iter().map(|card| card.name.clone()).collect(),
        });
    }

    // CR 400.11: only the revealed cards matching the effect's filter may be
    // taken. `pack_slot` indexes the OPENED pack, not the filtered list, so the
    // slot a selection names is stable even when the filter excludes cards.
    //
    // CR 407.3: an ante card "can't be brought into the game from outside the
    // game", so it is never OFFERED either — the pack is drawn from a set's
    // whole card pool, which no deck-construction check has vetted. Filtering
    // here rather than only at materialization keeps the prompt honest: a
    // player is not shown a choice that would then be refused. The reveal above
    // is deliberately left whole, because CR 407.3 restricts what may be taken,
    // not what the pack contains.
    let choices: Vec<OutsideGameChoiceEntry> = cards
        .into_iter()
        .enumerate()
        .filter(|(_, card)| {
            matches_target_filter_against_face(card, &filter)
                && crate::game::ante::admits_face_from_outside_game(state, card)
        })
        .map(|(pack_slot, card)| OutsideGameChoiceEntry {
            name: card.name.clone(),
            source: OutsideGameChoiceSource::BoosterPack {
                pack_slot,
                origin: origin.clone(),
                card: Box::new(card),
            },
            // Each card in a pack is one physical card.
            count: 1,
        })
        .collect();

    // CR 609.3: an empty pack — or one whose cards all fail the filter — takes
    // nothing. Not an error, and no choice is raised.
    if choices.is_empty() || count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::OpenBoosterPack,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    state.waiting_for = WaitingFor::OutsideGameChoice {
        player,
        source_id: ability.source_id,
        count: count.min(choices.len()),
        choices,
        // CR 701.20: the pack was revealed above, as a whole. The choice's own
        // `reveal` flag would additionally reveal the TAKEN card as it enters
        // the game, which would be a second, redundant reveal of a card every
        // player has already seen.
        reveal: false,
        up_to,
        destination,
    };
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::OpenBoosterPack,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}
