//! `CostReductionPolicy` — makes a cost-reduction permanent a reason the AI can
//! see to deploy the discount BEFORE the spells it discounts.
//!
//! ## The gap this closes
//!
//! CR 601.2f: Goblin Electromancer, Baral, Foundry Inspector and the Medallion
//! cycle are acceleration that never taps for mana — every later spell costs
//! less for as long as the permanent survives. The engine already applies the
//! discount at cast time (`casting::collect_battlefield_cost_modifiers`), so the AI is never
//! overcharged; what it lacks is any reason to *sequence the reducer first*.
//! `RampTimingPolicy` supplies exactly that signal for permanents that add mana
//! (`Effect::Mana`, land fetch, extra land drops) and structurally cannot see a
//! cost reducer, so a deck whose entire acceleration plan is cost reduction gets
//! no sequencing guidance at all. This policy adds it.
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node. The card-local check — do
//! this candidate's OWN statics carry a board-wide reduction of your spells —
//! runs FIRST and rejects every non-reducer candidate after reading one card's
//! AST. Only a confirmed reducer pays for the hand walk, which is bounded by
//! hand size and touches no battlefield sweep, no `find_legal_targets`, and no
//! affordability query.

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::types::ability::TargetFilter;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::cast_facts::{CastCostMode, CastFacts};
use crate::features::cost_reduction::{
    live_your_spell_discounts, LiveDiscount, COST_REDUCTION_FLOOR,
};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct CostReductionPolicy;

/// Cap on how many future casts one deployment is credited for, so a full grip
/// cannot push a single reducer out of the intended band.
///
/// `pub(crate)` so the bounded-score regression asserts against this constant
/// rather than a copied literal — raising the cap must move the test with it.
pub(crate) const MAX_REWARDED_FUTURE_CASTS: u32 = 4;

/// Cap on the per-application generic discount credited, so a misparsed or
/// unusually large `amount` cannot dominate the candidate's prior.
pub(crate) const MAX_REWARDED_DISCOUNT: u32 = 3;

impl TacticalPolicy for CostReductionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CostReduction
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.cost_reduction.commitment < COST_REDUCTION_FLOOR {
            None
        } else {
            Some(features.cost_reduction.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let Some(facts) = ctx.cast_facts() else {
            // CR 601.2 cast-shaped siblings outside the cast family (foretell,
            // face-down plays, stack copies) do not populate `cast_facts`, so
            // there is no AST to classify.
            return PolicyVerdict::neutral(PolicyReason::new("cost_reduction_na"));
        };

        // Card-local first: does THIS candidate carry a discount engine that
        // would actually be applying right now (condition and dynamic multiplier
        // resolved, not assumed)?
        let discounts = live_your_spell_discounts(
            ctx.state,
            facts.object.id,
            ctx.ai_player,
            facts.object.static_definitions.iter_unchecked(),
        );
        if !discounts.is_empty() {
            // A discount only pays off on the spells it actually reduces, so
            // count the grip through each reducer's own `spell_filter` rather
            // than crediting every card in hand.
            let discount: u32 = discounts.iter().map(|d| d.generic).sum();
            let future_casts = discountable_cards_in_hand(ctx, &discounts, facts.object.id);
            if future_casts == 0 {
                return PolicyVerdict::neutral(PolicyReason::new("cost_reduction_no_future_casts"));
            }
            let rewarded_casts = future_casts.min(MAX_REWARDED_FUTURE_CASTS);
            let rewarded_discount = discount.min(MAX_REWARDED_DISCOUNT);
            // CR 601.2f: each future cast saves `discount` generic mana; the
            // configured weight converts saved mana into card-equivalents.
            let saved_mana = f64::from(rewarded_discount * rewarded_casts);
            return PolicyVerdict::score(
                ctx.config.policy_penalties.cost_reduction_deploy_bonus * saved_mana,
                PolicyReason::new("cost_reduction_deploy_engine")
                    .with_fact("discount", i64::from(discount))
                    .with_fact("future_casts", i64::from(future_casts)),
            );
        }

        // Otherwise: are we casting past an unplayed, cheaper reducer that would
        // actually have discounted THIS spell? Deploying the discount first is
        // strictly better sequencing — the same shape as
        // `RampTimingPolicy::defer_to_ramp`. Both the mana-value gate and the
        // spell-filter match are required, so a narrow reducer never penalizes a
        // spell it cannot reduce.
        if hand_holds_cheaper_reducer(ctx, &facts) {
            return PolicyVerdict::score(
                ctx.config.policy_penalties.cost_reduction_defer_penalty,
                PolicyReason::new("cost_reduction_defer_to_engine")
                    .with_fact("mana_value", i64::from(facts.mana_value)),
            );
        }

        PolicyVerdict::neutral(PolicyReason::new("cost_reduction_na"))
    }
}

/// CR 601.2f: does `filter` (a reducer's `spell_filter`; `None` = unfiltered)
/// admit the object `id` as a spell it discounts?
///
/// Delegates to the engine's live object authority `matches_target_filter`, with
/// the reducer as the filter source so a `ControllerRef::You` scope resolves
/// against the reducer's controller exactly as it does at cast time.
fn filter_admits_object(
    ctx: &PolicyContext<'_>,
    filter: Option<&TargetFilter>,
    source: ObjectId,
    id: ObjectId,
) -> bool {
    match filter {
        None => true,
        Some(filter) => matches_target_filter(
            ctx.state,
            id,
            filter,
            &FilterContext::from_source(ctx.state, source),
        ),
    }
}

/// Cards in the AI's hand that at least one of `discounts` would actually
/// reduce, excluding `exclude` (the candidate itself, which is being spent now).
///
/// Lands are excluded first because CR 305.1 land plays are not spells and are
/// never discounted by a CR 601.2f cost reducer — and that check is far cheaper
/// than a filter evaluation, so it runs before one.
fn discountable_cards_in_hand(
    ctx: &PolicyContext<'_>,
    discounts: &[LiveDiscount],
    exclude: ObjectId,
) -> u32 {
    let Some(player) = ctx.state.players.get(ctx.ai_player.0 as usize) else {
        return 0;
    };
    player
        .hand
        .iter()
        .filter(|id| **id != exclude)
        .filter(|id| {
            ctx.state
                .objects
                .get(id)
                .is_some_and(|obj| !obj.card_types.core_types.contains(&CoreType::Land))
        })
        .filter(|id| {
            discounts.iter().any(|discount| {
                filter_admits_object(ctx, discount.spell_filter.as_ref(), exclude, **id)
            })
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// True when the AI's hand still holds a live cost-reduction permanent that is
/// cheaper than this spell AND would actually discount it — i.e. the engine
/// could have been deployed first, to this very spell's benefit.
fn hand_holds_cheaper_reducer(ctx: &PolicyContext<'_>, facts: &CastFacts<'_>) -> bool {
    // CR 118.9: the comparison below is against `facts.mana_value` — the PRINTED
    // mana value. An alternative or free cast does not pay that cost at all
    // (CR 107.3b even fixes an undefined X at 0 for a free cast), so "a cheaper
    // reducer would have discounted THIS spell" is not a claim this gate can
    // make: it would compare a reducer's cost against a number nobody pays.
    // Deploy-first sequencing advice is withheld rather than guessed.
    if !matches!(facts.cost_mode, CastCostMode::Printed) {
        return false;
    }
    let Some(player) = ctx.state.players.get(ctx.ai_player.0 as usize) else {
        return false;
    };
    player.hand.iter().any(|id| {
        *id != facts.object.id
            && ctx.state.objects.get(id).is_some_and(|obj| {
                obj.effective_mana_value() < facts.mana_value
                    && live_your_spell_discounts(
                        ctx.state,
                        *id,
                        ctx.ai_player,
                        obj.static_definitions.iter_unchecked(),
                    )
                    .iter()
                    .any(|discount| {
                        filter_admits_object(
                            ctx,
                            discount.spell_filter.as_ref(),
                            *id,
                            facts.object.id,
                        )
                    })
            })
    })
}
