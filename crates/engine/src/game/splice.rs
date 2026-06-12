//! CR 702.47: Splice onto [subtype] — reveal a card from hand and copy its text
//! box onto a spell of the matching subtype you're casting, paying its splice
//! cost as an additional cost.
//!
//! Splice is announced during CR 601.2b (as the spell is cast, before targets
//! and before the total cost is locked), so this module hooks the cast pipeline
//! at the same pre-target seam that Emerge and Casualty use. When the caster
//! reveals a splice card:
//!
//! * its splice cost is folded into the host spell's mana cost (CR 702.47b);
//! * its text-box spell ability is cloned and appended to the host spell's
//!   resolved-ability chain (CR 702.47c) so it resolves as part of that spell;
//! * the card is revealed and **stays in the caster's hand** (CR 702.47a).
//!
//! Because the spliced abilities are merged into `PendingCast::ability` *before*
//! target selection, the existing deferred-target machinery
//! ([`casting_costs::begin_deferred_target_selection`]) collects targets for the
//! spliced effects alongside the host spell's own targets (CR 702.47c–d). Any
//! number of cards may be spliced onto one spell (CR 702.47e) — the offer is
//! re-presented after each acceptance until the caster declines or the hand runs
//! out of eligible cards.

use crate::game::ability_utils::{append_to_sub_chain, build_resolved_from_def};
use crate::game::casting::combined_spell_ability_def;
use crate::game::casting_costs::begin_deferred_target_selection;
use crate::game::engine::EngineError;
use crate::game::game_object::GameObject;
use crate::types::ability::{CastTimingPermission, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastPaymentMode, CastingVariant, DistributionUnit, GameState, PendingCast, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 702.47a: Extract the splice subtype and cost an object grants while in
/// hand, if any.
fn splice_grant(obj: &GameObject) -> Option<(&str, &ManaCost)> {
    obj.keywords.iter().find_map(|k| match k {
        Keyword::Splice { subtype, cost } => Some((subtype.as_str(), cost)),
        _ => None,
    })
}

/// CR 702.47a: Cards in `player`'s hand that may be spliced onto the spell with
/// id `spell_obj_id` — i.e. cards whose "Splice onto [subtype]" subtype matches
/// one of the spell's subtypes. Returns them in hand order so the offer is
/// stable. The spell being cast is never itself a candidate (it is on the stack,
/// not in hand, but the guard makes the intent explicit).
pub(crate) fn eligible_splice_cards(
    state: &GameState,
    player: PlayerId,
    spell_obj_id: ObjectId,
) -> Vec<ObjectId> {
    let Some(spell) = state.objects.get(&spell_obj_id) else {
        return Vec::new();
    };
    let spell_subtypes = spell.card_types.subtypes.clone();
    state.players[player.0 as usize]
        .hand
        .iter()
        .copied()
        .filter(|&id| id != spell_obj_id)
        .filter(|id| {
            state.objects.get(id).is_some_and(|card| {
                splice_grant(card)
                    .is_some_and(|(subtype, _)| spell_subtypes.iter().any(|s| s == subtype))
            })
        })
        .collect()
}

/// CR 601.2b + CR 702.47a: Present the splice offer for a freshly announced
/// Arcane (or other matching-subtype) spell. Builds the in-flight `PendingCast`
/// for the host spell — mirroring the pre-target cost branches in
/// `continue_with_prepared` — and pauses on [`WaitingFor::SpliceOffer`]. Callers
/// must only invoke this when `eligible` is non-empty.
#[allow(clippy::too_many_arguments)]
pub(crate) fn begin_offer(
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    mana_cost: ManaCost,
    base_mana_cost: ManaCost,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    distribute: Option<DistributionUnit>,
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
    player: PlayerId,
    eligible: Vec<ObjectId>,
) -> WaitingFor {
    let mut pending = PendingCast::new(object_id, card_id, ability, mana_cost);
    pending.base_cost = Some(base_mana_cost);
    pending.casting_variant = casting_variant;
    pending.cast_timing_permission = cast_timing_permission;
    pending.distribute = distribute;
    pending.origin_zone = origin_zone;
    pending.payment_mode = payment_mode;
    WaitingFor::SpliceOffer {
        player,
        pending_cast: Box::new(pending),
        eligible,
    }
}

/// CR 702.47b–e: Resolve the caster's response to a splice offer.
///
/// * `Some(card)` — splice `card` onto the host spell: fold its splice cost into
///   the host's total cost (CR 702.47b), clone its text-box spell ability onto
///   the host's resolved-ability chain (CR 702.47c), reveal it (it stays in
///   hand, CR 702.47a), then re-offer the remaining eligible cards (CR 702.47e).
/// * `None` — the caster is done splicing: proceed to target selection for the
///   merged ability and on into cost payment (CR 601.2c onward).
pub(crate) fn resolve_offer(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    mut eligible: Vec<ObjectId>,
    card: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(card) = card else {
        // CR 601.2c: done splicing — collect targets for the merged ability,
        // then pay the (splice-inclusive) cost.
        return begin_deferred_target_selection(state, player, pending, events);
    };

    if !eligible.contains(&card) {
        return Err(EngineError::ActionNotAllowed(
            "Card is not an eligible splice card for this spell".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&card)
        .ok_or_else(|| EngineError::InvalidAction("Splice card object missing".to_string()))?;
    let (_, splice_cost) = splice_grant(obj)
        .ok_or_else(|| EngineError::InvalidAction("Splice card lost its keyword".to_string()))?;
    let def = combined_spell_ability_def(obj).ok_or_else(|| {
        EngineError::InvalidAction("Splice card has no spell ability to copy".to_string())
    })?;
    let card_name = obj.name.clone();

    // CR 702.47b: the splice cost is an additional cost. Spliced cards never
    // carry {X}, so folding the fixed mana into both the working cost and the
    // tax/X base keeps the eventual concrete-cost recompute correct.
    pending.cost = pending.cost.plus(splice_cost);
    if let Some(base) = pending.base_cost.as_mut() {
        *base = base.plus(splice_cost);
    }

    // CR 702.47c: copy the card's text box onto the host spell. The cloned
    // ability is sourced to the host spell object and controlled by the caster
    // so its effects resolve as part of that spell.
    let spliced = build_resolved_from_def(&def, pending.object_id, player);
    append_to_sub_chain(&mut pending.ability, spliced);

    // CR 702.47a: the card is revealed and stays in hand.
    events.push(GameEvent::CardsRevealed {
        player,
        card_ids: vec![card],
        card_names: vec![card_name],
    });

    eligible.retain(|&id| id != card);

    // CR 702.47e: more than one card may be spliced onto the same spell.
    if eligible.is_empty() {
        begin_deferred_target_selection(state, player, pending, events)
    } else {
        Ok(WaitingFor::SpliceOffer {
            player,
            pending_cast: Box::new(pending),
            eligible,
        })
    }
}
