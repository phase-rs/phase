use std::cmp::Ordering;

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::GameObject;
use engine::game::players;
use engine::types::ability::{Effect, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::cast_facts::cast_facts_for_action;
use crate::config::PolicyPenalties;
use crate::eval::{evaluate_creature, opponent_battlefield_creature_threat_value};

use super::context::PolicyContext;

pub(crate) fn is_own_main_phase(ctx: &PolicyContext<'_>) -> bool {
    engine::game::turn_control::turn_decision_maker(ctx.state) == ctx.ai_player
        && ctx.state.stack.is_empty()
        && matches!(
            ctx.state.phase,
            Phase::PreCombatMain | Phase::PostCombatMain
        )
}

pub(crate) fn board_presence_score(object: &GameObject) -> f64 {
    let mut score = 0.0;

    if object.card_types.core_types.contains(&CoreType::Creature) {
        let power = object.power.unwrap_or(0).max(0) as f64;
        let toughness = object.toughness.unwrap_or(0).max(0) as f64;
        score += ((power + toughness) / 8.0).min(0.45);
        score += keyword_pressure(object) * 0.04;
    } else if object
        .card_types
        .core_types
        .contains(&CoreType::Planeswalker)
    {
        score += 0.28 + object.loyalty.unwrap_or(0) as f64 / 20.0;
    } else if object.card_types.core_types.iter().any(|core_type| {
        matches!(
            core_type,
            CoreType::Artifact | CoreType::Battle | CoreType::Enchantment
        )
    }) {
        score += 0.16;
    }

    score.min(0.65)
}

pub(crate) fn best_proactive_cast_score(ctx: &PolicyContext<'_>) -> f64 {
    ctx.decision
        .candidates
        .iter()
        .filter_map(|candidate| cast_facts_for_action(ctx.state, &candidate.action, ctx.ai_player))
        .map(|facts| {
            let mut score = board_presence_score(facts.object);
            if !facts.immediate_etb_triggers.is_empty() || !facts.immediate_replacements.is_empty()
            {
                score += 0.16;
            }
            if facts.has_search_library() {
                score += 0.24;
            }
            if facts.has_draw() {
                score += 0.1;
            }
            if facts.has_direct_removal_text() {
                score += 0.14;
            }
            score
        })
        .fold(0.0, f64::max)
}

pub(crate) fn visible_opponent_creature_value(state: &GameState, ai_player: PlayerId) -> f64 {
    state
        .battlefield
        .iter()
        .filter_map(|object_id| {
            opponent_battlefield_creature_threat_value(state, ai_player, *object_id)
        })
        .fold(0.0, f64::max)
}

/// Max value among untapped opponent creatures that could actually block.
/// Use this instead of `visible_opponent_creature_value` when evaluating whether
/// pre-combat removal "opens combat lanes" — tapped creatures can't block.
pub(crate) fn untapped_opponent_blocker_value(state: &GameState, ai_player: PlayerId) -> f64 {
    state
        .battlefield
        .iter()
        .filter_map(|object_id| {
            let object = state.objects.get(object_id)?;
            (!object.tapped)
                .then(|| opponent_battlefield_creature_threat_value(state, ai_player, *object_id))
                .flatten()
        })
        .fold(0.0, f64::max)
}

/// Max threat value among opponent creatures that match the given target filter.
/// Returns 0.0 if no creatures match (the spell can't hit anything worthwhile).
/// `source_id` is needed for `matches_target_filter` controller-relative checks.
pub(crate) fn targetable_threat_value(
    state: &GameState,
    ai_player: PlayerId,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> f64 {
    let ctx = FilterContext::from_source(state, source_id);
    state
        .battlefield
        .iter()
        .filter_map(|&id| {
            matches_target_filter(state, id, filter, &ctx)
                .then(|| opponent_battlefield_creature_threat_value(state, ai_player, id))
                .flatten()
        })
        .fold(0.0, f64::max)
}

pub(crate) fn battlefield_pressure_delta(state: &GameState, ai_player: PlayerId) -> f64 {
    let mut ours = 0.0;
    let mut theirs = 0.0;

    for object_id in &state.battlefield {
        let Some(object) = state.objects.get(object_id) else {
            continue;
        };
        if !object.card_types.core_types.contains(&CoreType::Creature) {
            continue;
        }
        let value = evaluate_creature(state, *object_id);
        if object.controller == ai_player {
            ours += value;
        } else {
            theirs += value;
        }
    }

    ours - theirs
}

/// Sum of opponent untapped creature power, weighted by evasion.
/// Creatures AI cannot block count at full power; blockable ones at 50%.
pub(crate) fn opponent_lethal_damage(state: &GameState, ai_player: PlayerId) -> i32 {
    let opponents = players::opponents(state, ai_player);

    // Collect AI's untapped creature IDs for blocking checks
    let ai_blocker_ids: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            (obj.controller == ai_player
                && !obj.tapped
                && obj.card_types.core_types.contains(&CoreType::Creature))
            .then_some(id)
        })
        .collect();

    // Hoist block-legality statics once for the O(opponents × blockers) sweep.
    let slices = crate::combat_ai::BlockLegalitySlices::collect(state);

    let mut total = 0i32;
    for &obj_id in &state.battlefield {
        let Some(obj) = state.objects.get(&obj_id) else {
            continue;
        };
        if !opponents.contains(&obj.controller)
            || obj.tapped
            || !obj.card_types.core_types.contains(&CoreType::Creature)
        {
            continue;
        }
        let power = obj.power.unwrap_or(0);
        let can_be_blocked = ai_blocker_ids
            .iter()
            .any(|&bid| slices.can_block_pair(state, bid, obj_id));
        if can_be_blocked {
            // Blockable creatures contribute half power (some will get through)
            total += power / 2;
        } else {
            total += power;
        }
    }
    total
}

/// Whether any of ai_player's untapped creatures can legally block the given creature.
/// Delegates to the precomputed `can_block_pair` for full blocking restriction checks.
pub(crate) fn ai_can_block(
    state: &GameState,
    ai_player: PlayerId,
    attacker_id: ObjectId,
    slices: &crate::combat_ai::BlockLegalitySlices,
) -> bool {
    state.battlefield.iter().any(|&id| {
        state.objects.get(&id).is_some_and(|obj| {
            obj.controller == ai_player
                && !obj.tapped
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && slices.can_block_pair(state, id, attacker_id)
        })
    })
}

/// The most a noncreature, nonland permanent can cost to give up, however
/// expensive it was to cast.
///
/// `config::default_sacrifice_land_penalty` sits strictly above this (4.5 vs
/// 4.0) so the shipped defaults do not merely TIE — at the former value of 4.0
/// they did, and because every consumer ranks with a *stable* sort,
/// `[Swamp, Gilded Lotus]` sacrificed the Swamp purely for being listed first.
/// `search::tests::land_penalty_strictly_exceeds_the_noncreature_cap` pins the
/// defaults.
///
/// **This cap bounds ONE branch of [`sacrifice_cost`], not the function.** Read
/// the name literally: *noncreature, nonland*. Nothing here bounds the
/// function's range, and it is a mistake — one this comment previously invited
/// — to read the 4.0/4.5 pair as the window the whole design lives in. Two
/// branches leave it far behind: a plain creature prices at
/// `eval::evaluate_creature`, and since the dominance rule a creature-land
/// prices at `max(land, body)`, so an animated 12/12 gives up at **30.0**, not
/// 4.5. Both are unbounded above — `creature_combat_value` is
/// `1.5 * power + toughness` plus nine additive keyword bonuses, with no clamp
/// on any path.
///
/// That unboundedness is not a curiosity; it is why `SacrificeValuePolicy`
/// cannot simply clamp. A single vanilla 6/6 prices at exactly
/// `registry::CRITICAL_MAX`, so every larger body saturates to the same verdict
/// unless the policy rescales first. See
/// `sacrifice_value::SACRIFICE_VALUE_RAW_CEILING`, which is derived from the
/// measured distribution of this function's *creature* branches rather than
/// from this cap.
///
/// **The scalar gap is a within-class weight, not the ordering guarantee.** The
/// field is CMA-ES-tuned (`config::ACTIVE_POLICY_PENALTY_FIELDS`), so a trained
/// config can drive it back under this cap, and something else must carry the
/// land-vs-nonland ordering. **Two mechanisms do, and which one owns a given
/// decision depends on the sacrifice state** — see the table on
/// [`sacrifice_cost`]:
///
/// * `EffectZoneChoice { effect_kind: Sacrifice }` — [`SacrificeTier`], a
///   lexicographic tier no trained scalar can invert.
/// * `PayCost { kind: Sacrifice }` and `WardSacrificeChoice` — the bounded
///   `config::PolicyPenalties::sacrifice_needed_land_penalty`, because
///   `SacrificeValuePolicy` reduces a *set* to one banded `f64` and cannot
///   express a tier. Bounded means bounded: it outranks every non-creature
///   alternative, not a large creature, and only within the selection range
///   `sacrifice_value::SACRIFICE_VALUE_RAW_CEILING` keeps unsaturated.
///
/// This constant is what *both* mechanisms are calibrated against, which is why
/// the note lives here.
pub(crate) const NONCREATURE_SACRIFICE_CAP: f64 = 4.0;

/// Give-up order for a battlefield sacrifice selection.
///
/// `Ord` is the contract: **lower tiers are surrendered first**, and the
/// declaration order below IS the specification — the same idiom as
/// `card_value::KeepTier`, applied to the sibling seam.
///
/// The tier exists because the scalar cannot carry this ordering safely.
/// `sacrifice_land_penalty` is a CMA-ES-trained field
/// (`config::ACTIVE_POLICY_PENALTY_FIELDS`), so a trained profile may put it at
/// or below [`NONCREATURE_SACRIFICE_CAP`]; every consumer sorts *stably*, so an
/// equal score is decided by enumeration order and `[Swamp, Gilded Lotus]`
/// would sacrifice the Swamp purely for being listed first. Making the land
/// axis a tier removes that exposure structurally instead of defending one
/// parameter, and leaves 4.5-vs-4.0 as a within-class weight only.
///
/// CR 305.2: a land is the one permanent class whose replacement is
/// rate-limited to one per turn, which is why it — and not, say, an expensive
/// artifact — earns a structural tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SacrificeTier {
    /// Every non-land permanent, and any object that is not on the battlefield
    /// (or has already left it).
    Ordinary,
    /// A land — **including creature-lands** (Dryad Arbor, an animated Treetop
    /// Village). CR 305.9: an object that is both a land and another card type
    /// "can be played only as a land", so its land nature governs deployment
    /// and it occupies the land drop; the once-per-turn CR 305.2 limit on
    /// replacing it therefore applies in full, which is what this tier encodes.
    ///
    /// This is the single land predicate **for the sacrifice seam**, and
    /// [`sacrifice_cost`] reads it too, so the specific
    /// `payment_selection::permanent_value` divergence — a second, private land
    /// predicate that tested `Creature` before `Land` — cannot recur *in that
    /// shape*. It is not a structural impossibility proof: the coupling is a
    /// call, not a type, and nothing stops a future edit from inlining an
    /// equivalent (or worse, a subtly different) predicate here. What guards
    /// the property that actually matters is the test
    /// `land_tier_always_prices_at_or_above_the_land_penalty`; read its
    /// docstring for what it does and does not catch. Note also that this file
    /// deliberately carries a *second*, differently-shaped land predicate
    /// (`is_pure_land`, which excludes creature-lands for CR 302.6 summoning
    /// sickness) — "single predicate" is scoped to this seam, not the file.
    ///
    /// Note the two answer different questions: the **tier** is a
    /// classification (a creature-land is a land, full stop), while the
    /// **scalar** prices a creature-land by dominance — `max(land, creature)`,
    /// because by CR 300.2 ("Some objects have more than one card type ... Such
    /// objects combine the aspects of each of those card types") it is genuinely
    /// both at once. Tiering it as a land
    /// while pricing it by the larger reading is deliberate, not an oversight.
    Land,
}

/// The single land predicate for the sacrifice seam. Used by both
/// [`sacrifice_tier`] and [`sacrifice_cost`] so they cannot disagree.
fn sacrifice_tier_of(obj: &GameObject) -> SacrificeTier {
    if obj.card_types.core_types.contains(&CoreType::Land) {
        SacrificeTier::Land
    } else {
        SacrificeTier::Ordinary
    }
}

pub(crate) fn sacrifice_tier(state: &GameState, obj_id: ObjectId) -> SacrificeTier {
    state
        .objects
        .get(&obj_id)
        .map_or(SacrificeTier::Ordinary, sacrifice_tier_of)
}

/// The sort key for a minimizing sacrifice selection: give-up tier first,
/// [`sacrifice_cost`] as the within-tier tie-break.
pub(crate) fn sacrifice_key(
    state: &GameState,
    obj_id: ObjectId,
    penalties: &PolicyPenalties,
) -> (SacrificeTier, f64) {
    (
        sacrifice_tier(state, obj_id),
        sacrifice_cost(state, obj_id, penalties),
    )
}

/// The single authority for sacrifice-selection order. Tier dominates; the
/// scalar breaks ties within a tier. Mirrors `card_value::cmp_keep`.
pub(crate) fn cmp_sacrifice(a: &(SacrificeTier, f64), b: &(SacrificeTier, f64)) -> Ordering {
    a.0.cmp(&b.0)
        .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
}

/// Value of a permanent for sacrifice-ordering decisions.
/// Higher values mean the permanent is more costly to sacrifice.
///
/// **This is the single battlefield give-up authority.** It prices the
/// `Sacrifice` payment kind, every battlefield arm of `payment_selection`'s
/// exile / return / remove-counter / tap-creatures kinds, `SacrificeValuePolicy`,
/// and `self_cost`'s sacrifice leaves.
///
/// Ordering a *selection* by this scalar alone is not safe — see
/// [`SacrificeTier`] and use [`sacrifice_key`] / [`cmp_sacrifice`].
///
/// **The directive binds sort-based consumers. `SacrificeValuePolicy` is
/// score-based and is guarded differently — by design, not by omission.**
///
/// | state | how the land axis is enforced |
/// |---|---|
/// | `EffectZoneChoice { effect_kind: Sacrifice }` | strict tier — `deterministic_choice` → `pick_lowest_value_sacrifices` → [`cmp_sacrifice`] |
/// | `PayCost { kind: Sacrifice }` | bounded guard — `SacrificeValuePolicy` + `penalties.sacrifice_needed_land_penalty` |
/// | `WardSacrificeChoice` | bounded guard — same |
///
/// The last two cannot use the tier. `SacrificeValuePolicy::score` reduces a
/// *set* to one `f64` for softmax ranking, so a per-object comparator does not
/// substitute into it; and its verdict is clamped to `registry::CRITICAL_MAX`,
/// so the unbounded band a lexicographic tier would need is inexpressible
/// there. The full argument is on that policy's `verdict` docstring. Its guard
/// magnitude strictly exceeds [`NONCREATURE_SACRIFICE_CAP`], which closes the
/// documented CMA-ES exposure (a trained `sacrifice_land_penalty` at or below
/// that cap) against every non-creature alternative **whose selection stays
/// within `sacrifice_value::SACRIFICE_VALUE_RAW_CEILING`** — the guard is an
/// additive term in a banded score, so past that range the band erases it and
/// the ordering collapses again. The qualifier is not a formality: with a bare
/// clamp it bit from four cards upward, which is why that policy rescales into
/// the band instead of clamping. It deliberately does not outrank a large
/// creature.
///
/// So: do not read the "single authority" sentence above as "every consumer
/// sorts by tier". It is a statement about where the *pricing* lives. Ordering
/// discipline is per-consumer, and there are two legitimate shapes of it.
pub(crate) fn sacrifice_cost(
    state: &GameState,
    obj_id: ObjectId,
    penalties: &PolicyPenalties,
) -> f64 {
    let Some(obj) = state.objects.get(&obj_id) else {
        return 0.0;
    };
    if sacrifice_tier_of(obj) == SacrificeTier::Land {
        // CR 305.9 (`docs/MagicCompRules.txt:1711`): "If an object is both a
        // land and another card type, it can be played only as a land." The
        // rule presupposes such objects and settles that the LAND nature
        // governs deployment — so a creature-land really does occupy the land
        // drop (CR 305.1) and the land valuation can never simply be skipped.
        // CR 305.7 (`:1707`): "Setting a land's subtype doesn't add or remove
        // any card types (such as creature)" — land-ness and creature-ness are
        // independent axes, so neither classification erases the other.
        // CR 208.1 (`:1509`): a creature has real power and toughness, so the
        // body is a real thing to lose too.
        //
        // A creature-land (Dryad Arbor; an animated Treetop Village or a
        // Nissa-animated land) is therefore BOTH, simultaneously, by rule.
        // **Dominance, not first-match.** Giving one up loses a mana source AND
        // a body, so the price is at least the larger of the two readings. Any
        // if/else dispatch understates one case whichever order it picks: a
        // Land-first order under-prices an animated 3/3, a Creature-first order
        // under-prices a 1/1 Dryad Arbor. `max` degrades correctly in both
        // directions and is the reason this is not an `else`.
        //
        // UNMEASURED MAGNITUDE — an open obligation, recorded here rather than
        // only in a review artifact, because this is where a maintainer meets
        // it. Adopting dominance raised the price of every animated land at
        // every raw-scalar consumer of this function, not just at the ordering
        // seam: `payment_cost`'s battlefield arms, `crew_or_saddle_score` and
        // `station_activation_score` (`payment_selection.rs`), `blight_value`,
        // `free_outlet_activation`, and `self_cost`'s sacrifice leaves. None of
        // those has a fixture, and the rule shipped without a paired-seed
        // `scripts/ai-gate.sh` calibration. The same run also owes
        // `SacrificeValuePolicy`'s land guard and its band rescale; take the
        // baseline AFTER those, since the guard pushes selections toward the
        // band ceiling and so changes what saturates.
        let land_value = penalties.sacrifice_land_penalty;
        return if obj.card_types.core_types.contains(&CoreType::Creature) {
            land_value.max(evaluate_creature(state, obj_id))
        } else {
            land_value
        };
    }
    // Token creatures: use creature eval if they have meaningful stats,
    // otherwise use flat token cost (Treasures, Maps, Clues, etc.)
    if obj.is_token {
        if obj.card_types.core_types.contains(&CoreType::Creature) {
            return evaluate_creature(state, obj_id).max(penalties.sacrifice_token_cost);
        }
        return penalties.sacrifice_token_cost;
    }
    if obj.card_types.core_types.contains(&CoreType::Creature) {
        return evaluate_creature(state, obj_id);
    }
    // Other permanents: scale by mana value, capped strictly below the land
    // penalty so a land is never merely tied with an expensive artifact.
    //
    // The cap binds THIS branch only. The two creature branches above are
    // unbounded — an animated 12/12 creature-land prices at 30.0 under the
    // dominance rule — so this function's range is not the 0..4.5 window the
    // cap and the land penalty might suggest. Consumers that band or clamp the
    // result must size for the creature branches, not for this one.
    (obj.mana_cost.mana_value() as f64).min(NONCREATURE_SACRIFICE_CAP)
}

/// Count spells in hand with a Counter effect ability.
pub(crate) fn count_counterspells_in_hand(state: &GameState, player: PlayerId) -> usize {
    state.players[player.0 as usize]
        .hand
        .iter()
        .filter(|&&obj_id| {
            state.objects.get(&obj_id).is_some_and(|obj| {
                obj.abilities
                    .iter()
                    .any(|ability| matches!(&*ability.effect, Effect::Counter { .. }))
            })
        })
        .count()
}

/// Heuristic upper bound on the mana the AI could spend on a ward cost *after*
/// paying for the spell it is currently casting. Counts untapped mana sources
/// (lands, non-sick mana dorks, mana rocks) plus any floating mana, then
/// subtracts the spell's own mana value. Colour requirements are approximated by
/// total mana value, matching the engine's auto-tap heuristics used elsewhere in
/// the AI (CR 302.6: a summoning-sick creature can't tap for mana).
pub(crate) fn available_mana_after_spell(ctx: &PolicyContext<'_>) -> u32 {
    let player = &ctx.state.players[ctx.ai_player.0 as usize];
    let mut sources = player.mana_pool.total() as u32;
    for &id in &ctx.state.battlefield {
        let Some(obj) = ctx.state.objects.get(&id) else {
            continue;
        };
        if obj.controller != ctx.ai_player || obj.tapped {
            continue;
        }
        let is_creature = obj.card_types.core_types.contains(&CoreType::Creature);
        // Untapped pure land counts unconditionally (auto-tap tier 0); other mana
        // sources count only if they have a mana ability and — for creatures —
        // aren't summoning-sick (CR 302.6).
        let is_pure_land = obj.card_types.core_types.contains(&CoreType::Land) && !is_creature;
        let is_usable_dork = obj
            .abilities
            .iter()
            .any(engine::game::mana_abilities::is_mana_ability)
            && !(is_creature && engine::game::combat::has_summoning_sickness(obj));
        if is_pure_land || is_usable_dork {
            sources += 1;
        }
    }
    let spell_cost = ctx
        .source_object()
        .map_or(0, |source| source.mana_cost.mana_value());
    sources.saturating_sub(spell_cost)
}

/// CR 702.21a: Whether the AI can pay `ward` after committing to the spell it is
/// casting. Mana / Waterbend costs use the post-spell mana estimate; non-mana
/// costs check the corresponding resource (life, a spare card, sacrificeable
/// permanents). Conservative on the unknown: a cost we can't analyse returns
/// `true` so the AI is never blocked from a cast we can't prove is wasted.
pub(crate) fn can_pay_ward_cost(
    ctx: &PolicyContext<'_>,
    ward: &WardCost,
    warded: &GameObject,
) -> bool {
    match ward {
        WardCost::Mana(cost) | WardCost::Waterbend(cost) => {
            available_mana_after_spell(ctx) >= cost.mana_value()
        }
        // CR 119.4: life may be paid only if the life total is at least the
        // amount. CR 704.5a: a player at 0 life loses, so the AI treats a payment
        // that would drop it to 0 as unaffordable — it leaves at least 1 life.
        WardCost::PayLife(amount) => ctx.state.players[ctx.ai_player.0 as usize].life > *amount,
        WardCost::PayLifeEqualToPower => {
            ctx.state.players[ctx.ai_player.0 as usize].life > warded.power.unwrap_or(0).max(0)
        }
        WardCost::DiscardCard => {
            let source_id = ctx.source_object().map(|source| source.id);
            ctx.state.players[ctx.ai_player.0 as usize]
                .hand
                .iter()
                .any(|&id| Some(id) != source_id)
        }
        WardCost::Sacrifice { count, filter } => {
            let Some(source) = ctx.source_object() else {
                return true;
            };
            let fctx = FilterContext::from_source(ctx.state, source.id);
            let matching = ctx
                .state
                .battlefield
                .iter()
                .filter(|&&id| {
                    ctx.state
                        .objects
                        .get(&id)
                        .is_some_and(|obj| obj.controller == ctx.ai_player)
                        && matches_target_filter(ctx.state, id, filter, &fctx)
                })
                .count();
            matching as u32 >= *count
        }
        // CR 702.21a: every conjoined sub-cost must be payable. Mana contention
        // between multiple mana sub-costs is approximated (each checked against
        // the full post-spell pool) — rare enough not to warrant exact tracking.
        WardCost::Compound(costs) => costs
            .iter()
            .all(|cost| can_pay_ward_cost(ctx, cost, warded)),
    }
}

fn keyword_pressure(object: &GameObject) -> f64 {
    object
        .keywords
        .iter()
        .map(|keyword| match keyword {
            Keyword::Flying
            | Keyword::Trample
            | Keyword::Vigilance
            | Keyword::Menace
            | Keyword::Lifelink
            | Keyword::Deathtouch
            | Keyword::FirstStrike
            | Keyword::DoubleStrike
            | Keyword::Haste => 1.0,
            _ => 0.0,
        })
        .sum::<f64>()
        .min(3.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PolicyPenalties;
    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);

    /// A permanent that is BOTH a land and a creature — the Dryad Arbor /
    /// animated Treetop Village shape. CR 305.7: setting or gaining land types
    /// does not add or remove the creature card type, so both are real.
    fn creature_land(state: &mut GameState, power: i32, toughness: i32) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            AI,
            "Creature Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        id
    }

    /// A creature-land is tiered as a LAND (CR 305.9 — it can be played only as
    /// a land, so it occupies the land drop and the CR 305.2 rate limit governs
    /// replacing it), whatever its body.
    #[test]
    fn a_creature_land_is_tiered_as_a_land() {
        let mut state = GameState::new_two_player(7);
        let arbor = creature_land(&mut state, 1, 1);
        let treetop = creature_land(&mut state, 3, 3);

        assert_eq!(sacrifice_tier(&state, arbor), SacrificeTier::Land);
        assert_eq!(sacrifice_tier(&state, treetop), SacrificeTier::Land);
        assert!(
            SacrificeTier::Ordinary < SacrificeTier::Land,
            "declaration order IS the surrender order"
        );
    }

    /// CR 300.2 + CR 208.1 + CR 305.9: a creature-land is a creature AND a land
    /// simultaneously, so its give-up price is the **dominant** reading, not
    /// whichever branch a first-match dispatch happens to test first.
    ///
    /// This test would fail under EITHER first-match order, which is the point:
    /// - `Land`-first (this function before the fix) returns 4.5 for the 3/3 and
    ///   under-prices the body;
    /// - `Creature`-first (the deleted `payment_selection::permanent_value`)
    ///   returns `evaluate_creature` for the 1/1 and under-prices the land.
    #[test]
    fn a_creature_land_is_priced_by_dominance_not_by_branch_order() {
        let mut state = GameState::new_two_player(7);
        let penalties = PolicyPenalties::default();
        let land_penalty = penalties.sacrifice_land_penalty;

        let arbor = creature_land(&mut state, 1, 1);
        let treetop = creature_land(&mut state, 3, 3);

        let small_body = evaluate_creature(&state, arbor);
        let big_body = evaluate_creature(&state, treetop);

        // Fixture premise: the two cases must straddle the land penalty, or the
        // test cannot distinguish `max` from either first-match order.
        assert!(
            small_body < land_penalty && big_body > land_penalty,
            "fixture premise: need one body under the land penalty ({land_penalty}) \
             and one over it, got {small_body} and {big_body}"
        );

        assert_eq!(
            sacrifice_cost(&state, arbor, &penalties),
            land_penalty,
            "a 1/1 Dryad Arbor is dominated by its land value — a Creature-first \
             order would return {small_body}"
        );
        assert_eq!(
            sacrifice_cost(&state, treetop, &penalties),
            big_body,
            "an animated 3/3 is dominated by its body — a Land-first order would \
             return {land_penalty}"
        );
    }

    /// The dominance rule must not leak into ordinary permanents: a plain land
    /// is still exactly the land penalty, and a plain creature is still exactly
    /// `evaluate_creature`.
    #[test]
    fn dominance_does_not_change_single_type_permanents() {
        let mut state = GameState::new_two_player(7);
        let penalties = PolicyPenalties::default();

        // Bind the id before the `&mut state` borrow: an explicit `&mut state`
        // argument starts the borrow immediately, so reading
        // `state.next_object_id` in the same call is E0503.
        let land_card = CardId(state.next_object_id);
        let plain_land = create_object(
            &mut state,
            land_card,
            AI,
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&plain_land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let bear_card = CardId(state.next_object_id);
        let bear = create_object(
            &mut state,
            bear_card,
            AI,
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        assert_eq!(
            sacrifice_cost(&state, plain_land, &penalties),
            penalties.sacrifice_land_penalty
        );
        assert_eq!(
            sacrifice_cost(&state, bear, &penalties),
            evaluate_creature(&state, bear)
        );
        assert_eq!(sacrifice_tier(&state, bear), SacrificeTier::Ordinary);
    }

    /// **The coupling invariant**, and the only test here that is a property
    /// rather than a point value:
    ///
    /// > `sacrifice_tier(o) == Land`  ⟹  `sacrifice_cost(o) >= sacrifice_land_penalty`
    ///
    /// The three tests above pin point values for point fixtures. Those go red
    /// on a *numeric* change and stay green on a *structural* one, which is the
    /// gap this closes: `cmp_sacrifice` would still order correctly if the
    /// scalar dropped below the land floor (the tier dominates), but every
    /// raw-scalar consumer — `SacrificeValuePolicy`, `payment_cost`'s
    /// battlefield arms, `blight_value`, `free_outlet_activation`, `self_cost`
    /// — silently reverts to land-blind pricing. That is the original defect
    /// class returning through a different door, and nothing else detects it.
    ///
    /// **What this catches**, concretely, not hypothetically:
    /// - a new early return inserted *above* the land check that returns less
    ///   than the floor. Unit 3 ("land-sacrifice protection AND cast-gating for
    ///   sac-cost spells") is in flight and a guard at the top of
    ///   `sacrifice_cost` is its natural shape.
    /// - substituting a land predicate that disagrees with `sacrifice_tier_of`.
    ///   The tempting wrong one already lives 130 lines below in this file:
    ///   `is_pure_land` deliberately *excludes* creature-lands (CR 302.6
    ///   summoning sickness). Reused here it would drop the 1/1 fixture into
    ///   the creature branch at ~2.5, under the 4.5 floor.
    ///
    /// **What it does NOT catch**, stated so the next reader does not over-read
    /// it: inlining `sacrifice_tier_of` back into an *equivalent*
    /// `contains(&CoreType::Land)` test. That re-opens the divergence risk
    /// without changing any value, and no test can see it. The coupling is a
    /// call, not a type.
    #[test]
    fn land_tier_always_prices_at_or_above_the_land_penalty() {
        let mut state = GameState::new_two_player(7);

        let plain_land = {
            let card = CardId(state.next_object_id);
            let id = create_object(&mut state, card, AI, "Swamp".to_string(), Zone::Battlefield);
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            id
        };
        let small_manland = creature_land(&mut state, 1, 1);
        let big_manland = creature_land(&mut state, 6, 6);
        let token_manland = {
            let id = creature_land(&mut state, 2, 2);
            state.objects.get_mut(&id).unwrap().is_token = true;
            id
        };
        let bear = {
            let card = CardId(state.next_object_id);
            let id = create_object(&mut state, card, AI, "Bear".to_string(), Zone::Battlefield);
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            id
        };

        let fixtures = [
            (plain_land, "plain land"),
            (small_manland, "1/1 creature-land (body UNDER the floor)"),
            (big_manland, "6/6 creature-land (body OVER the floor)"),
            (token_manland, "token creature-land"),
            (bear, "plain creature (Ordinary — negative control)"),
        ];

        // Reach guards. Without these the table could go all-degenerate — every
        // fixture Ordinary, or every Land-tier one dominated by its body — and
        // the implication below would hold vacuously.
        let land_tier_count = fixtures
            .iter()
            .filter(|(id, _)| sacrifice_tier(&state, *id) == SacrificeTier::Land)
            .count();
        assert_eq!(
            land_tier_count, 4,
            "reach guard: 4 of the 5 fixtures must be Land-tier, or the \
             implication is vacuous. Got {land_tier_count}."
        );
        assert_eq!(
            sacrifice_tier(&state, bear),
            SacrificeTier::Ordinary,
            "reach guard: the negative control must NOT be Land-tier, or the \
             table does not discriminate tier at all"
        );

        // Two profiles, and the second one proves LESS than it looks like it
        // does — measured, not assumed. The property is one-sided
        // (`cost >= floor`), and the trained profile only *lowers* the floor, so
        // it is strictly weaker than the default profile in the `>=` direction.
        // It cannot fail unless the default profile fails first, and `assert!`
        // aborts, so it never even runs when the default one fires.
        //
        // Confirmed by execution rather than by argument: substituting an
        // `is_pure_land`-shaped predicate in `sacrifice_cost` reddens this test
        // under the default profile; with the default row removed so only the
        // trained profile runs, the same substitution PASSES.
        //
        // What the second profile does earn its place for: it pins that `floor`
        // is read relationally from the passed-in `penalties` rather than
        // hardcoded to 4.5. That is worth keeping, and it is all it proves.
        // `sacrifice_land_penalty` being a live CMA-ES-tuned field is the
        // rationale for `SacrificeTier` existing at all — not for this floor
        // invariant.
        let trained = PolicyPenalties {
            sacrifice_land_penalty: 1.0,
            ..PolicyPenalties::default()
        };
        assert!(
            trained.sacrifice_land_penalty < NONCREATURE_SACRIFICE_CAP,
            "fixture premise: the trained penalty must be under the cap ({NONCREATURE_SACRIFICE_CAP}), \
             or this profile is indistinguishable from the default"
        );

        for (penalties, profile) in [
            (PolicyPenalties::default(), "default"),
            (trained, "trained (land penalty under the cap)"),
        ] {
            let floor = penalties.sacrifice_land_penalty;
            for (id, label) in &fixtures {
                if sacrifice_tier(&state, *id) != SacrificeTier::Land {
                    continue;
                }
                let cost = sacrifice_cost(&state, *id, &penalties);
                assert!(
                    cost >= floor,
                    "COUPLING INVARIANT BROKEN under the {profile} profile: \
                     {label} is Land-tier but prices at {cost}, under the land \
                     floor of {floor}. `cmp_sacrifice` still orders it last \
                     (the tier dominates), so the sort-based consumers look \
                     fine — but every raw-scalar consumer is now land-blind. \
                     Did you add an early return above the land check in \
                     `sacrifice_cost`, or swap `sacrifice_tier_of` for a \
                     predicate that excludes creature-lands?"
                );
            }
        }
    }
}
