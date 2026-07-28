use engine::game::filter::{matches_target_filter, FilterContext};
use engine::types::ability::{Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CostResume, GameState, PayCostKind, WaitingFor};
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{
    rescale_into_critical_band, DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy,
};
use super::strategy_helpers::{sacrifice_cost, sacrifice_tier, SacrificeTier, SINGLE_CARD_VALUE};

pub struct SacrificeValuePolicy;

/// Max expected magnitude of [`SacrificeValuePolicy::score`] — the point past
/// which [`rescale_into_critical_band`] starts to saturate; below it, ordering
/// is preserved exactly.
///
/// **Why this policy needs a ceiling at all.** `score` is a *sum over the
/// selection*, so its range scales with cardinality while a single verdict is
/// clamped at [`CRITICAL_MAX`](super::registry::CRITICAL_MAX) = 15.0. Routing
/// the raw sum through `PolicyVerdict::score` alone therefore **saturates**:
/// every magnitude past `STRONG_MAX` collapses onto one value and the top of
/// the distribution flattens. Two distinct sacrifices then score identically
/// and the stable sort — enumeration order — picks between them.
///
/// **Derived from the measured card pool, not from a formula.** Population:
/// the 19,318 printed creatures in `client/public/card-data.json` with fixed
/// power and toughness. `eval::creature_combat_value` is `1.5 * power +
/// toughness`, so that distribution *is* this policy's range for the dominant
/// case — a one-card creature sacrifice, which is what 58 of the pool's
/// additional-cost sacrifice spells and effectively every `WardSacrificeChoice`
/// ask for:
///
/// | percentile | `1.5p + t` |
/// |---|---|
/// | p50   |  6.0 |
/// | p90   | 12.5 |
/// | p95   | **15.0 — exactly `CRITICAL_MAX`** |
/// | p99   | 20.0 |
/// | p99.9 | 30.0 |
///
/// The p95 row is the defect in one line: under a bare clamp the top **5.15%**
/// of printed bodies (995 of 19,318) are indistinguishable from each other at a
/// *single-card* selection. A vanilla 6/6 is `6*1.5 + 6 = 15.0` and already
/// sits on the ceiling. This constant is p99.9, so ordering is exact for all
/// but the top 0.1% (28 cards).
///
/// **The price, stated so a maintainer can act on it rather than rediscover
/// it.** Rescaling is not free: every magnitude above `STRONG_MAX` is
/// compressed *relative to sibling policies* in `PolicyRegistry`'s sum. At this
/// ceiling a raw 15.0 reaches the registry as 9.0, and a raw 10.0 as 7.0. A
/// ceiling of 21.0 would give 11.25 and 8.12 — but it collapses the
/// two-creature costs (Bankrupt in Blood, Phyrexian Tribute) as soon as the two
/// bodies sum past 21, which two p90 creatures already do. Lower it if the AI
/// is measured under-valuing expensive sacrifices; raise it if multi-card
/// sacrifices are measured being chosen arbitrarily. Either way it owes a
/// paired-seed `scripts/ai-gate.sh` run — see `strategy_helpers::sacrifice_cost`.
///
/// **What still saturates, named rather than left to be discovered.** Gaea's
/// Balance (*"sacrifice five lands"*) reaches 45.0 at this policy's own
/// `PayCost { Sacrifice }` state. Its selection is all-land (`type_filters:
/// ["Land"], count: 5`, verified in `data/card-data.json`), so the per-land
/// guard is a constant offset across every candidate set and cannot invert;
/// what is lost is `-total_cost` ordering *among* lands, which matters only
/// when a creature-land is in the pool.
/// `rescale_into_critical_band`'s own docstring places exactly this in "the
/// extreme tail, where ordering no longer matters".
///
/// Tectonic Split (*"sacrifice half the lands you control"*) was previously
/// named here as a second, *unbounded* all-land case. **Both halves of that
/// were wrong** and it is recorded rather than quietly dropped, because the
/// mistake is the kind a re-derivation would repeat: it parses to
/// `count: 1` with an **empty** `type_filters` (verified in
/// `data/card-data.json`), so it is bounded at one card and its pool is
/// *mixed* — every permanent matches. The guard is therefore live and useful
/// there, not lost. (The parse is a pre-existing parser defect — the Oracle
/// text is a dynamic count over lands — and is out of scope here.)
///
/// Pinned by `sacrifice_value_ceiling_pins_the_compression_it_costs` (the
/// figures above) and `verdict_preserves_the_land_guard_where_a_bare_clamp_collapses_it`
/// (the behaviour at the measured collapse point).
const SACRIFICE_VALUE_RAW_CEILING: f64 = 30.0;

impl SacrificeValuePolicy {
    fn optional_single_card_draw_sacrifices_only_source(ctx: &PolicyContext<'_>) -> Option<f64> {
        let GameAction::DecideOptionalEffect { accept: true } = &ctx.candidate.action else {
            return None;
        };
        let WaitingFor::OptionalEffectChoice { source_id, .. } = &ctx.decision.waiting_for else {
            return None;
        };
        let ability = ctx.state.active_optional_effect_frame()?.ability.as_ref();
        if ability.source_id != *source_id || ability.controller != ctx.ai_player {
            return None;
        }

        let Effect::Sacrifice {
            target,
            count: QuantityExpr::Fixed { value: 1 },
            ..
        } = &ability.effect
        else {
            return None;
        };
        // Explicit SelfRef sacrifices are designed to consume their source. This
        // guard is for filtered outlets whose source only happens to be the last
        // matching permanent, such as a Zombie engine with no other Zombies.
        if matches!(target, TargetFilter::SelfRef)
            || ability.else_ability.is_some()
            || ability.repeat_for.is_some()
            || ability.player_scope.is_some()
            || !single_card_draw_is_only_payoff(ability.sub_ability.as_deref())
        {
            return None;
        }

        let filter_ctx = FilterContext::from_ability(ability);
        let mut eligible = ctx.state.battlefield.iter().copied().filter(|id| {
            ctx.state.objects.get(id).is_some_and(|object| {
                object.controller == ability.controller
                    && !object.is_emblem
                    && matches_target_filter(ctx.state, *id, target, &filter_ctx)
            })
        });
        if eligible.next()? != *source_id || eligible.next().is_some() {
            return None;
        }

        let cost = sacrifice_cost(ctx.state, *source_id, ctx.penalties());
        (cost > SINGLE_CARD_VALUE).then_some(cost)
    }

    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        // Guard: only score SelectCards during sacrifice decisions
        let GameAction::SelectCards { cards } = &ctx.candidate.action else {
            return 0.0;
        };
        if !matches!(
            ctx.decision.waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                resume: CostResume::Spell { .. } | CostResume::SpellCost { .. },
                ..
            } | WaitingFor::WardSacrificeChoice { .. }
                | WaitingFor::EffectZoneChoice {
                    effect_kind: engine::types::ability::EffectKind::Sacrifice,
                    ..
                }
        ) {
            return 0.0;
        }

        // Score inversely to value: cheap sacrifices produce less negative scores
        let total_cost: f64 = cards
            .iter()
            .map(|&obj_id| sacrifice_cost(ctx.state, obj_id, ctx.penalties()))
            .sum();
        // CR 305.2: a land is the one permanent class whose replacement is
        // rate-limited to one per turn, so giving one up is categorically worse
        // than the scalar alone conveys. Sort-based consumers get this from
        // `SacrificeTier`; this policy cannot (see the docstring on `verdict`),
        // so it applies the bounded equivalent, once per land in the selection.
        // Per-land rather than per-selection so that a two-land give-up ranks
        // below a one-land give-up.
        let land_count = cards
            .iter()
            .filter(|&&obj_id| sacrifice_tier(ctx.state, obj_id) == SacrificeTier::Land)
            .count();
        -total_cost + land_count as f64 * ctx.penalties().sacrifice_needed_land_penalty
    }
}

fn single_card_draw_is_only_payoff(
    ability: Option<&engine::types::ability::ResolvedAbility>,
) -> bool {
    let Some(ability) = ability else {
        return false;
    };
    matches!(
        &ability.effect,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    ) && ability.sub_ability.is_none()
        && ability.else_ability.is_none()
        && ability.repeat_for.is_none()
        && ability.player_scope.is_none()
}

impl TacticalPolicy for SacrificeValuePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SacrificeValue
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Sacrifice resource valuation is intrinsic to the permanent being given
        // up — a 6/6 costs the same to sacrifice on turn 2 as on turn 9 — so it
        // must not scale with game phase. Mirrors the sibling
        // PaymentSelectionPolicy, which handles the same SelectCards / PayCost
        // decision with a constant 1.0 activation. A turn-phase multiplier (>1.0)
        // here could push a legitimate critical-band score past the registry's
        // CRITICAL_MAX ceiling (see issue #4282).
        // activation-constant: phase-independent sacrifice resource valuation.
        Some(1.0)
    }

    /// **Why this policy uses `sacrifice_needed_land_penalty` and not
    /// [`sacrifice_key`](super::strategy_helpers::sacrifice_key) /
    /// [`cmp_sacrifice`](super::strategy_helpers::cmp_sacrifice).**
    ///
    /// `sacrifice_cost`'s docstring directs *selection ordering* through the
    /// tier comparator. This policy cannot follow that directive, for two
    /// independent structural reasons — recorded here because the obvious
    /// "just route it through `sacrifice_key`" edit does not compile into
    /// anything meaningful, and the next reader deserves to know why before
    /// trying:
    ///
    /// 1. **Shape.** `sacrifice_key`/`cmp_sacrifice` are a per-object sort key
    ///    and comparator. [`Self::score`] does not compare two objects — it takes
    ///    a *set* (`SelectCards { cards }`), reduces it to one `f64`, and the
    ///    registry ranks candidate sets by softmax. A comparator does not
    ///    substitute into a set reduction.
    /// 2. **Range.** A tier is lexicographic, so encoding one additively needs a
    ///    band exceeding every achievable scalar. `sacrifice_cost` is unbounded
    ///    above (`evaluate_creature` is `power * 1.5 + toughness + bonuses`,
    ///    unclamped), while this verdict is clamped to
    ///    [`CRITICAL_MAX`](super::registry::CRITICAL_MAX) — deliberately, and
    ///    pinned by `large_sacrifice_stays_within_critical_band` (issue #4282).
    ///    `PolicyRegistry::verdicts` additionally tripwires out-of-band literals.
    ///    So a dominating band is not merely discouraged here, it is
    ///    inexpressible.
    ///
    /// The bounded penalty is the codebase's established answer to this exact
    /// shape — compare `cycling_needed_land_penalty` ("occupies the finite
    /// strong band") and `payment_selection_needed_land_penalty`, both finite
    /// land guards inside set-summing policy scores.
    ///
    /// **What it does and does not guarantee.** Its magnitude strictly exceeds
    /// `NONCREATURE_SACRIFICE_CAP`, so a land is never given up ahead of any
    /// *non-creature* permanent — **for selections whose raw magnitude stays
    /// within `SACRIFICE_VALUE_RAW_CEILING`**. At the shipped defaults, that
    /// means every all-non-creature selection containing no owned commander up
    /// to **seven** cards. With one owned commander whose `sacrifice_cost` is
    /// `C`, the bound is instead `ceil((34 - C) / 4) - 1`: **five** cards for
    /// the canonical `{3}{W}` Vehicle commander cast once (`C = 10.0`), and
    /// **three** for a high-tax ten-drop (`C = 20.0`). There is deliberately no
    /// single commander-present cardinality. Both bounds are measured by
    /// `sacrifice_value_ceiling_pins_the_compression_it_costs`. That qualifier is load-bearing
    /// and is why this returns a *rescaled* score below rather than the raw
    /// one: the guard is an additive term, so a bare `PolicyVerdict::score`
    /// clamp erases it first, and it did so from four cards upward. Past the
    /// ceiling the rescale saturates and the guard is erased again — bounded
    /// guards buy a bounded range, and anyone re-tuning this must re-derive the
    /// ceiling with it.
    ///
    /// Within that range the exposure is closed: `sacrifice_land_penalty` is
    /// CMA-ES-trained and may legally fall to or below the cap, and the guard
    /// outranks it regardless. It does **not** outrank a large creature, and
    /// that is intentional: sacrificing a 6/6 to save a Swamp is bad play, and
    /// a true tier would force it. Sort-based consumers keep the strict tier
    /// because they can express it; this one gets the bounded approximation
    /// because it cannot.
    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // VeryEasy and Easy do not search optional-effect continuations, so they
        // need a hard guard against accepting this immediately losing exchange.
        // Search-enabled difficulties must remain free to discover death-trigger,
        // recursion, and other continuation value that outweighs the source.
        let protected_source_cost = if ctx.config.search.enabled {
            None
        } else {
            Self::optional_single_card_draw_sacrifices_only_source(ctx)
        };
        if let Some(cost) = protected_source_cost {
            return PolicyVerdict::reject(
                PolicyReason::new("optional_sacrifice_only_source_for_single_card")
                    .with_fact("cost_milli", (cost * 1000.0) as i64),
            );
        }

        // `self.score()` is an unbounded sum of per-card sacrifice costs, and
        // `PolicyVerdict::score` clamps magnitude into the declared bands
        // (|delta| <= CRITICAL_MAX). Routing the raw sum straight through that
        // clamp SATURATES, and the land guard is an additive term, so
        // saturation erases the guard before anything else: a four-card
        // land-free set (-16.0) and the same set with a land (-21.0) both clamp
        // to -15.0 and the stable sort decides. Rescale into the band first so
        // ordering survives, then band-dispatch — the same construction
        // `copy_value` uses, and the failure mode
        // `rescale_into_critical_band_preserves_order_where_saturation_collapses`
        // is named after. With activation pinned to 1.0 above, the scaled delta
        // still never exceeds the critical ceiling.
        PolicyVerdict::score(
            rescale_into_critical_band(self.score(ctx), SACRIFICE_VALUE_RAW_CEILING),
            PolicyReason::new("sacrifice_value_score"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::strategy_helpers::NONCREATURE_SACRIFICE_CAP;
    use super::*;

    /// The largest all-non-creature selection cardinality containing no owned
    /// commander at which the land guard must still order correctly under
    /// [`SACRIFICE_VALUE_RAW_CEILING`].
    ///
    /// Not chosen — *measured*, then checked against reachability. The worst-case
    /// magnitude of such a selection is
    /// `NONCREATURE_SACRIFICE_CAP * (N - 1) + sacrifice_land_penalty +
    /// |sacrifice_needed_land_penalty|` = `4N + 5` at the shipped defaults, so the
    /// ceiling above orders the guard up to `N = 7`. This constant intentionally
    /// has no commander arm: a commander prices at its live `sacrifice_cost`, so
    /// the commander-present bound is a function of that price rather than one
    /// universal integer.
    ///
    /// **The bound is set by where BOTH sides saturate, not by `4N + 5 <=
    /// ceiling`.** Do not re-derive it the short way: `4 * 7 + 5 = 33` already
    /// exceeds the 30.0 ceiling, so the naive rule yields `N = 6` and would
    /// "correct" a constant that is right. At `N = 7` only the *land-bearing*
    /// side is clamped (`-28 -> -14.2` against `-33 -> -15.0`), and one-sided
    /// saturation still orders. Collapse begins at `N = 8`, where the land-free
    /// side `4N = 32` also passes the ceiling and both sides land on `-15.0`.
    ///
    /// **Reachability, queried against `data/card-data.json` (35,516 entries)
    /// rather than asserted.** Fixed multi-card all-non-creature sacrifice costs
    /// above five do exist, and two exceed this constant: Bolas's Citadel at 10,
    /// Glass-Cast Heart at 13, Shilgengar at 6. The guard still cannot invert on
    /// any of them, but **for a different reason than pool composition** — their
    /// pools are land-free *by filter*, so `land_count` is identically zero and
    /// the guard term is a constant offset:
    ///
    /// * Bolas's Citadel — `type_filters: ["Permanent", {"Non": "Land"}]`. A
    ///   land cannot enter the pool by construction. This one is airtight.
    /// * Glass-Cast Heart and Shilgengar — `type_filters: [{"Subtype": "Blood"}]`
    ///   with the `Token` property. Blood is a token type, so the printed-card
    ///   corpus is silent on it: a `Land`/`Blood` overlap query over
    ///   `card-data.json` returns zero only because the corpus holds **no**
    ///   `Blood` entries at all, which is a vacuous zero, not a measurement.
    ///   These two rest on the token definition, not on a card-pool query.
    ///
    /// Whoever adds a fourth card should re-run the filter check rather than
    /// re-read this list, and should note which of the two kinds of evidence
    /// above it rests on. The multi-card cases that would otherwise stress the
    /// guard — Wildfire, Destructive Force, Nicol Bolas — are `Effect::Sacrifice`,
    /// which `search::deterministic_choice` resolves through the strict
    /// `cmp_sacrifice` tier before this policy is ever consulted.
    const GUARDED_SELECTION_CARDINALITY: f64 = 7.0;

    use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TypedFilter};
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{GameState, PendingCast};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    fn dummy_pending() -> Box<PendingCast> {
        Box::new(PendingCast::new(
            ObjectId(100),
            CardId(100),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 0 },
                    target: engine::types::ability::TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(100),
                PlayerId(0),
            ),
            ManaCost::zero(),
        ))
    }

    fn creature_body(state: &mut GameState, name: &str, power: i32, toughness: i32) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(power);
        object.toughness = Some(toughness);
        id
    }

    fn owned_commander_body(
        state: &mut GameState,
        name: &str,
        power: i32,
        toughness: i32,
        mana_value: u32,
        command_zone_casts: u32,
    ) -> ObjectId {
        let id = creature_body(state, name, power, toughness);
        let object = state.objects.get_mut(&id).unwrap();
        object.is_commander = true;
        object.mana_cost = ManaCost::generic(mana_value);
        object.base_mana_cost = ManaCost::generic(mana_value);
        state.format_config.command_zone = true;
        if command_zone_casts > 0 {
            state.commander_cast_count.insert(id, command_zone_casts);
        }
        id
    }

    fn sacrifice_policy_score_and_verdict(
        state: &GameState,
        choices: &[ObjectId],
        cards: Vec<ObjectId>,
    ) -> (f64, PolicyVerdict) {
        let config = AiConfig::default();
        let selection_count = cards.len();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::PayCost {
                player: PlayerId(0),
                kind: PayCostKind::Sacrifice,
                choices: choices.to_vec(),
                count: selection_count,
                min_count: selection_count,
                resume: CostResume::Spell {
                    spell: dummy_pending(),
                },
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectCards { cards },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let context = crate::context::AiContext::empty(&config.weights);
        let policy_context = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let raw = SacrificeValuePolicy.score(&policy_context);
        (raw, SacrificeValuePolicy.verdict(&policy_context))
    }

    /// At temperature 1.0, two equally priced 4/4 bodies gave the commander a
    /// 0.500 softmax rate before this change. The commander now scores -9.40
    /// against the bear's -7.00, giving it a 0.083 rate.
    #[test]
    fn commander_premium_reduces_the_sacrifice_policy_softmax_rate() {
        let mut state = GameState::new_two_player(42);
        let commander = owned_commander_body(&mut state, "Commander", 4, 4, 4, 1);
        let bear = creature_body(&mut state, "Bear", 4, 4);

        let (bear_raw, bear_verdict) =
            sacrifice_policy_score_and_verdict(&state, &[commander, bear], vec![bear]);
        let (commander_raw, commander_verdict) =
            sacrifice_policy_score_and_verdict(&state, &[commander, bear], vec![commander]);
        let PolicyVerdict::Score {
            delta: bear_delta, ..
        } = bear_verdict
        else {
            panic!("sacrifice value policy must score the bear selection");
        };
        let PolicyVerdict::Score {
            delta: commander_delta,
            ..
        } = commander_verdict
        else {
            panic!("sacrifice value policy must score the commander selection");
        };

        assert_eq!(
            bear_raw, -10.0,
            "reach guard: the bear reaches the policy score"
        );
        assert_eq!(
            commander_raw, -16.0,
            "reach guard: the premium reaches the policy score"
        );
        assert!(
            bear_raw.abs() < SACRIFICE_VALUE_RAW_CEILING
                && commander_raw.abs() < SACRIFICE_VALUE_RAW_CEILING,
            "the rate is meaningful only before saturation: bear={bear_raw}, commander={commander_raw}"
        );
        assert_eq!(bear_delta, -7.0);
        assert_eq!(commander_delta, -9.4);

        let pre_change_commander_rate = 1.0 / (1.0 + (bear_delta - bear_delta).exp());
        let commander_rate =
            (commander_delta - bear_delta).exp() / (1.0 + (commander_delta - bear_delta).exp());
        assert_eq!(pre_change_commander_rate, 0.5);
        assert!(
            (commander_rate - 0.083_172_696_493_922_38).abs() < 1e-12,
            "the changed softmax rate must remain the documented 0.083, got {commander_rate}"
        );
        assert!(
            commander_rate < 0.5 * pre_change_commander_rate,
            "the commander rate must fall materially: post={commander_rate} pre={pre_change_commander_rate}"
        );
    }

    /// Two 6/6 bodies cost 15.0 each, so the unchanged pair lands exactly at
    /// the 30.0 raw ceiling. Adding the command-zone premium only moves the
    /// pair farther into the same saturated result.
    #[test]
    fn two_six_sixes_keep_the_sacrifice_verdict_at_the_raw_ceiling() {
        use super::super::registry::CRITICAL_MAX;

        let mut state = GameState::new_two_player(42);
        let commander = owned_commander_body(&mut state, "Commander", 6, 6, 4, 1);
        let bear = creature_body(&mut state, "Bear", 6, 6);
        let second_bear = creature_body(&mut state, "Second Bear", 6, 6);
        let penalties = crate::config::PolicyPenalties::default();

        assert_eq!(sacrifice_cost(&state, bear, &penalties), 15.0);
        assert_eq!(sacrifice_cost(&state, second_bear, &penalties), 15.0);
        assert_eq!(
            sacrifice_cost(&state, commander, &penalties),
            21.0,
            "reach guard: the commander premium is live before the ceiling pin"
        );

        let (commander_raw, commander_verdict) = sacrifice_policy_score_and_verdict(
            &state,
            &[commander, bear, second_bear],
            vec![commander, bear],
        );
        let (control_raw, control_verdict) = sacrifice_policy_score_and_verdict(
            &state,
            &[commander, bear, second_bear],
            vec![bear, second_bear],
        );
        let PolicyVerdict::Score {
            delta: commander_delta,
            reason: commander_reason,
        } = commander_verdict
        else {
            panic!("sacrifice value policy must score the premium-bearing selection");
        };
        let PolicyVerdict::Score {
            delta: control_delta,
            reason: control_reason,
        } = control_verdict
        else {
            panic!("sacrifice value policy must score the pre-change control");
        };

        assert_eq!(control_raw, -SACRIFICE_VALUE_RAW_CEILING);
        assert_eq!(commander_raw, -36.0);
        assert_eq!(control_delta, -CRITICAL_MAX);
        assert_eq!(commander_delta, -CRITICAL_MAX);
        assert_eq!(
            commander_delta, control_delta,
            "the pre-change control and premium-bearing selection must have the same saturated delta"
        );
        assert_eq!(
            commander_reason.kind, control_reason.kind,
            "the saturated verdict reason must remain identical"
        );
        assert_eq!(
            commander_reason.facts, control_reason.facts,
            "the saturated verdict facts must remain identical"
        );
    }

    /// Before the premium, the owned commander and ordinary 4/4 both cost 10.0
    /// and had identical `(Ordinary, 10.0)` keys, so P(commander) was 0.5. Now
    /// the bear scores -7.0 and the commander -9.4: the bear selection wins and
    /// the commander is spared.
    ///
    /// The premium is finite, not absolute protection: a 12/12 body costs 30.0
    /// and is the kind of irreplaceable body that can still outrank a recastable
    /// commander. It sits on this policy's ceiling, so it is documented here
    /// rather than used in a live assertion.
    #[test]
    fn comparable_body_sacrifice_verdict_gives_up_the_bear_and_spares_the_commander() {
        let mut state = GameState::new_two_player(42);
        let commander = owned_commander_body(&mut state, "Commander", 4, 4, 4, 1);
        let bear = creature_body(&mut state, "Bear", 4, 4);

        let (bear_raw, bear_verdict) =
            sacrifice_policy_score_and_verdict(&state, &[commander, bear], vec![bear]);
        let (commander_raw, commander_verdict) =
            sacrifice_policy_score_and_verdict(&state, &[commander, bear], vec![commander]);
        let PolicyVerdict::Score {
            delta: bear_delta, ..
        } = bear_verdict
        else {
            panic!("sacrifice value policy must score the bear selection");
        };
        let PolicyVerdict::Score {
            delta: commander_delta,
            ..
        } = commander_verdict
        else {
            panic!("sacrifice value policy must score the commander selection");
        };

        assert_eq!(bear_raw, -10.0);
        assert_eq!(commander_raw, -16.0);
        assert!(
            bear_delta > commander_delta,
            "the bear candidate wins ({bear_delta}) over the commander candidate ({commander_delta}), so the AI gives up the BEAR and spares the commander"
        );
    }

    fn optional_sacrifice_for_card_state() -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sacrifice Engine".to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&source).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(3);
        object.toughness = Some(3);

        // Keep the accept branch engine-legal: drawing the payoff card must not
        // turn this policy regression into a draw-from-empty-library test.
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );

        let mut sacrifice = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        sacrifice.optional = true;
        sacrifice.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        )));
        state.push_optional_effect_frame(engine::types::OptionalEffectFrame {
            ability: Box::new(sacrifice),
            trigger_event: None,
            trigger_match_count: None,
        });
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: source,
            description: Some("You may sacrifice a creature. If you do, draw a card.".to_string()),
            may_trigger_key: None,
        };
        (state, source)
    }

    fn optional_verdict(state: &GameState, accept: bool, config: &AiConfig) -> PolicyVerdict {
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::DecideOptionalEffect { accept },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Utility),
        };
        let context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        SacrificeValuePolicy.verdict(&ctx)
    }

    #[test]
    fn easy_ai_declines_single_card_draw_when_only_filtered_sacrifice_is_source() {
        let (state, _) = optional_sacrifice_for_card_state();

        for difficulty in [AiDifficulty::VeryEasy, AiDifficulty::Easy] {
            let config = create_config(difficulty, Platform::Native).into_measurement(42);
            let mut rng = ChaCha20Rng::seed_from_u64(42);
            let action = crate::search::choose_action(&state, PlayerId(0), &config, &mut rng)
                .expect("the optional effect prompt has a legal decline action");
            assert_eq!(
                action,
                GameAction::DecideOptionalEffect { accept: false },
                "{difficulty:?} must preserve the filtered sacrifice source"
            );
        }
    }

    #[test]
    fn optional_source_preservation_guard_stands_down_with_another_sacrifice() {
        let (mut state, _) = optional_sacrifice_for_card_state();
        let config = create_config(AiDifficulty::VeryEasy, Platform::Native);
        let fodder = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Fodder".to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&fodder).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(1);
        object.toughness = Some(1);

        assert!(matches!(
            optional_verdict(&state, true, &config),
            PolicyVerdict::Score { .. }
        ));
    }

    #[test]
    fn optional_source_preservation_guard_does_not_block_explicit_self_sacrifice() {
        let (mut state, _) = optional_sacrifice_for_card_state();
        let config = create_config(AiDifficulty::VeryEasy, Platform::Native);
        let ability = &mut state
            .active_optional_effect_frame_mut()
            .expect("fixture parks an optional-effect frame")
            .ability;
        let Effect::Sacrifice { target, .. } = &mut ability.effect else {
            panic!("fixture must contain a sacrifice effect");
        };
        *target = TargetFilter::SelfRef;

        assert!(matches!(
            optional_verdict(&state, true, &config),
            PolicyVerdict::Score { .. }
        ));
    }

    #[test]
    fn search_enabled_ai_can_evaluate_single_source_sacrifice() {
        let (state, _) = optional_sacrifice_for_card_state();

        for difficulty in [
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
            AiDifficulty::CEDH,
        ] {
            let config = create_config(difficulty, Platform::Native);
            assert!(
                config.search.enabled,
                "test premise: {difficulty:?} must search continuations"
            );
            assert!(
                matches!(
                    optional_verdict(&state, true, &config),
                    PolicyVerdict::Score { .. }
                ),
                "{difficulty:?} must not hard-veto the sacrifice before search"
            );
        }
    }

    #[test]
    fn prefers_sacrificing_token_over_creature() {
        let mut state = GameState::new_two_player(42);

        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&creature).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(3);

        let token_card_id = CardId(state.next_object_id);
        let token = create_object(
            &mut state,
            token_card_id,
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&token).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.is_token = true;

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::PayCost {
                player: PlayerId(0),
                kind: PayCostKind::Sacrifice,
                choices: vec![creature, token],
                count: 1,
                min_count: 1,
                resume: CostResume::Spell {
                    spell: dummy_pending(),
                },
            },
            candidates: Vec::new(),
        };

        // Score sacrificing the creature
        let creature_candidate = CandidateAction {
            action: GameAction::SelectCards {
                cards: vec![creature],
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let creature_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &creature_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let creature_score = SacrificeValuePolicy.score(&creature_ctx);

        // Score sacrificing the token
        let token_candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![token] },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let token_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &token_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let token_score = SacrificeValuePolicy.score(&token_ctx);

        assert!(
            token_score > creature_score,
            "Should prefer sacrificing token ({token_score}) over creature ({creature_score})"
        );
    }

    /// Regression for #4282: sacrificing a high-value creature must not produce
    /// a scaled delta beyond the critical band ceiling. Before the fix, `verdict`
    /// returned the raw unbounded `-evaluate_creature` score and `activation`
    /// scaled it by `turn_phase_mult` (up to 1.3), so a single large creature
    /// tripped the registry's `debug_assert!(scaled_delta.abs() <= CRITICAL_MAX)`.
    ///
    /// **What now delivers that bound has changed, and this test does not see
    /// the difference** — recorded so it is not read as pinning the mechanism.
    /// `verdict` routes through `rescale_into_critical_band` before the clamp,
    /// so the ceiling is respected by *rescaling* rather than by *saturating*.
    /// This test asserts a magnitude bound on one candidate; it never compares
    /// two, so it is silent on order collapse. That is
    /// `verdict_preserves_the_land_guard_where_a_bare_clamp_collapses_it`.
    #[test]
    fn large_sacrifice_stays_within_critical_band() {
        use super::super::registry::CRITICAL_MAX;

        let mut state = GameState::new_two_player(42);

        // 8/8 => evaluate_creature = 8*1.5 + 8 = 20.0, comfortably over the
        // critical ceiling of 15, so the band clamp must actually engage.
        let big = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Colossus".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&big).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(8);
        obj.toughness = Some(8);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::PayCost {
                player: PlayerId(0),
                kind: PayCostKind::Sacrifice,
                choices: vec![big],
                count: 1,
                min_count: 1,
                resume: CostResume::Spell {
                    spell: dummy_pending(),
                },
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![big] },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        // The raw score must exceed the ceiling, proving the clamp is exercised.
        assert!(
            SacrificeValuePolicy.score(&ctx).abs() > CRITICAL_MAX,
            "test premise: raw sacrifice score should exceed the critical ceiling"
        );

        // The banded verdict must clamp magnitude into the critical band.
        let PolicyVerdict::Score { delta, .. } = SacrificeValuePolicy.verdict(&ctx) else {
            panic!("sacrifice value policy must return a Score verdict");
        };
        assert!(
            delta.abs() <= CRITICAL_MAX,
            "verdict delta {delta} must be clamped to the critical band ceiling {CRITICAL_MAX}"
        );

        // Activation is the constant 1.0, so the scaled delta the registry
        // asserts on equals the (already clamped) verdict delta — never above
        // the ceiling regardless of turn number.
        let activation = SacrificeValuePolicy
            .activation(&DeckFeatures::default(), &state, PlayerId(0))
            .expect("sacrifice value policy always activates");
        assert_eq!(
            activation, 1.0,
            "sacrifice valuation must not scale by phase"
        );
        assert!((delta * f64::from(activation)).abs() <= CRITICAL_MAX);
    }

    #[test]
    fn no_score_outside_sacrifice_context() {
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectCards {
                cards: vec![ObjectId(1)],
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = SacrificeValuePolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No score outside sacrifice, got {score}"
        );
    }

    /// The magnitude invariant the land guard rests on. `sacrifice_cost` caps
    /// every non-creature permanent at `NONCREATURE_SACRIFICE_CAP`, so a guard
    /// strictly above that cap outranks the most expensive artifact **even when
    /// `sacrifice_land_penalty` is trained all the way to zero** — which is the
    /// exposure the guard exists to close. Pinned rather than assumed, because
    /// a magic constant chosen to make one fixture pass is worthless.
    ///
    /// Deliberately NOT enforced at config load: turning a bad-but-legal trained
    /// config into a hard error is the wrong trade — the same call
    /// `land_penalty_strictly_exceeds_the_noncreature_cap` makes in `search.rs`.
    #[test]
    fn sacrifice_needed_land_penalty_outranks_the_noncreature_cap() {
        let cap = crate::policies::strategy_helpers::NONCREATURE_SACRIFICE_CAP;
        let guard = crate::config::PolicyPenalties::default().sacrifice_needed_land_penalty;
        assert!(
            guard < 0.0,
            "the guard is applied additively to a negated cost, so it must be \
             negative to be a penalty; got {guard}"
        );
        assert!(
            guard.abs() > cap,
            "sacrifice_needed_land_penalty magnitude ({}) must strictly exceed \
             NONCREATURE_SACRIFICE_CAP ({cap}), or a maximally-priced artifact \
             out-ranks a land whose trained scalar has fallen to zero — the \
             exact inversion this guard exists to prevent",
            guard.abs()
        );
    }

    /// **The discriminating test for the `PayCost{Sacrifice}` land guard.**
    ///
    /// This is the seam `SacrificeTier` could not reach: `PayCost{Sacrifice}`
    /// has no tiered `deterministic_choice` arm, so before this guard the
    /// ranking was the bare `sacrifice_cost` sum and a trained profile inverted
    /// it. Revert the `land_count * sacrifice_needed_land_penalty` term and the
    /// final assertion flips — under the trained profile the land scores −1.0
    /// against the artifact's −4.0, so the AI sacrifices the land.
    #[test]
    fn trained_land_penalty_under_the_cap_does_not_invert_the_sacrifice_choice() {
        let mut state = GameState::new_two_player(42);

        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let artifact_card = CardId(state.next_object_id);
        let artifact = create_object(
            &mut state,
            artifact_card,
            PlayerId(0),
            "Gilded Lotus".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.mana_cost = ManaCost::generic(5);
        }

        // `create_config` rather than `AiConfig::default()` + field assignment:
        // the latter is clippy's `field_reassign_with_default`.
        let mut config = create_config(AiDifficulty::VeryHard, Platform::Native);
        config.policy_penalties.sacrifice_land_penalty = 1.0;

        // Fixture premise: the trained penalty must sit UNDER the cap, or the
        // bare scalar already ranks these correctly and the test proves nothing.
        let cap = crate::policies::strategy_helpers::NONCREATURE_SACRIFICE_CAP;
        assert!(
            config.policy_penalties.sacrifice_land_penalty < cap,
            "fixture premise: trained land penalty must be under the cap ({cap})"
        );

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::PayCost {
                player: PlayerId(0),
                kind: PayCostKind::Sacrifice,
                choices: vec![land, artifact],
                count: 1,
                min_count: 1,
                resume: CostResume::Spell {
                    spell: dummy_pending(),
                },
            },
            candidates: Vec::new(),
        };

        let score_of = |cards: Vec<ObjectId>| {
            let candidate = CandidateAction {
                action: GameAction::SelectCards { cards },
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
            };
            SacrificeValuePolicy.score(&PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &crate::context::AiContext::empty(&config.weights),
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            })
        };

        let land_score = score_of(vec![land]);
        let artifact_score = score_of(vec![artifact]);

        // Reach guard: a non-zero score proves the `WaitingFor` guard was passed
        // and we are scoring the real arm, not short-circuiting at `return 0.0`.
        assert!(
            land_score < 0.0 && artifact_score < 0.0,
            "reach guard: both candidates must be scored by the sacrifice arm, \
             got land {land_score} and artifact {artifact_score}"
        );

        // DISCRIMINATION PROOF, not merely a reach guard. Recompute what the
        // score would be WITHOUT the guard term and assert that ordering is
        // inverted. This pins that the fixture actually reaches the defective
        // condition, so the assertion below cannot pass for an incidental
        // reason — and it stays true for future readers without anyone having
        // to revert the fix and watch it go red.
        let bare_land = -sacrifice_cost(&state, land, &config.policy_penalties);
        let bare_artifact = -sacrifice_cost(&state, artifact, &config.policy_penalties);
        assert!(
            bare_land > bare_artifact,
            "fixture premise: WITHOUT the land guard the ordering must invert \
             (bare land {bare_land} should outrank bare artifact {bare_artifact}), \
             or this fixture never exercised the bug and the assertion below \
             proves nothing"
        );

        assert!(
            artifact_score > land_score,
            "the artifact must be surrendered before the land. Got artifact \
             {artifact_score} vs land {land_score}. Without the land guard the \
             trained scalar prices the land at 1.0 against the artifact's 4.0 \
             and the AI sacrifices the land — the inversion this guard closes."
        );
    }

    /// The guard lives in [`SacrificeValuePolicy::score`], which all three
    /// sacrifice `WaitingFor` states share — but shared code is an argument,
    /// not coverage. This reaches the second untiered state,
    /// `WardSacrificeChoice`, through the real `WaitingFor` guard, so a future
    /// narrowing of that `matches!` to `PayCost` alone reddens here instead of
    /// silently dropping ward sacrifices back to bare-scalar ranking.
    #[test]
    fn the_land_guard_also_covers_ward_sacrifice_choice() {
        let mut state = GameState::new_two_player(42);

        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let artifact_card = CardId(state.next_object_id);
        let artifact = create_object(
            &mut state,
            artifact_card,
            PlayerId(0),
            "Gilded Lotus".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.mana_cost = ManaCost::generic(5);
        }

        let mut config = create_config(AiDifficulty::VeryHard, Platform::Native);
        config.policy_penalties.sacrifice_land_penalty = 1.0;

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::WardSacrificeChoice {
                player: PlayerId(0),
                permanents: vec![land, artifact],
                pending_effect: Box::new(ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: engine::types::ability::TargetFilter::Controller,
                    },
                    Vec::new(),
                    ObjectId(200),
                    PlayerId(1),
                )),
                remaining: 1,
                min_total_power: None,
            },
            candidates: Vec::new(),
        };

        let score_of = |cards: Vec<ObjectId>| {
            let candidate = CandidateAction {
                action: GameAction::SelectCards { cards },
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
            };
            SacrificeValuePolicy.score(&PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &crate::context::AiContext::empty(&config.weights),
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            })
        };

        let land_score = score_of(vec![land]);
        let artifact_score = score_of(vec![artifact]);

        // Reach guard: `WardSacrificeChoice` must actually pass the policy's
        // `WaitingFor` match, not fall through to `return 0.0`.
        assert!(
            land_score < 0.0 && artifact_score < 0.0,
            "reach guard: WardSacrificeChoice must be scored by the sacrifice \
             arm, got land {land_score} and artifact {artifact_score}"
        );
        assert!(
            artifact_score > land_score,
            "the ward sacrifice must give up the artifact, not the land. Got \
             artifact {artifact_score} vs land {land_score}"
        );
    }

    /// The guard is **bounded on purpose**, and this pins the boundary so the
    /// next reader does not "strengthen" it into a real tier.
    ///
    /// A tier would force sacrificing a large creature to save any land. That is
    /// bad play, so the guard must NOT outrank a big body. If someone replaces
    /// `sacrifice_needed_land_penalty` with a dominating band, this test reddens
    /// — which is the point: it is the counterweight to
    /// `trained_land_penalty_under_the_cap_does_not_invert_the_sacrifice_choice`,
    /// and the two together pin the guard's magnitude from both sides.
    #[test]
    fn the_land_guard_does_not_outrank_a_large_creature() {
        let mut state = GameState::new_two_player(42);

        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let colossus_card = CardId(state.next_object_id);
        let colossus = create_object(
            &mut state,
            colossus_card,
            PlayerId(0),
            "Colossus".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&colossus).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(8);
            obj.toughness = Some(8);
        }

        let config = AiConfig::default();
        let penalties = &config.policy_penalties;

        // Fixture premise: the body must out-price the land plus the whole
        // guard, or this test cannot distinguish "bounded" from "dominating".
        let body = crate::eval::evaluate_creature(&state, colossus);
        let land_total =
            penalties.sacrifice_land_penalty + penalties.sacrifice_needed_land_penalty.abs();
        assert!(
            body > land_total,
            "fixture premise: the 8/8 body ({body}) must exceed the land value \
             plus the guard ({land_total}), else the boundary is untestable"
        );

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::PayCost {
                player: PlayerId(0),
                kind: PayCostKind::Sacrifice,
                choices: vec![land, colossus],
                count: 1,
                min_count: 1,
                resume: CostResume::Spell {
                    spell: dummy_pending(),
                },
            },
            candidates: Vec::new(),
        };

        let score_of = |cards: Vec<ObjectId>| {
            let candidate = CandidateAction {
                action: GameAction::SelectCards { cards },
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
            };
            SacrificeValuePolicy.score(&PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &crate::context::AiContext::empty(&config.weights),
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            })
        };

        assert!(
            score_of(vec![land]) > score_of(vec![colossus]),
            "the land must still be surrendered before an 8/8 — the guard is a \
             bounded correction, not a tier. If this reddens, someone turned it \
             into a dominating band."
        );

        // The same boundary at the layer production ranks. `score` is pre-band;
        // `rescale_into_critical_band` is monotone, so this cannot disagree —
        // but "cannot disagree" is an argument and the reviewer's finding was
        // precisely that this file had only arguments at this layer.
        let verdict_delta_of = |cards: Vec<ObjectId>| {
            let candidate = CandidateAction {
                action: GameAction::SelectCards { cards },
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
            };
            let PolicyVerdict::Score { delta, .. } = SacrificeValuePolicy.verdict(&PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &crate::context::AiContext::empty(&config.weights),
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            }) else {
                panic!("sacrifice value policy must return a Score verdict");
            };
            delta
        };
        assert!(
            verdict_delta_of(vec![land]) > verdict_delta_of(vec![colossus]),
            "the bounded-guard boundary must hold at the banded verdict too, \
             not only at the raw score: got land {} vs 8/8 {}",
            verdict_delta_of(vec![land]),
            verdict_delta_of(vec![colossus])
        );
    }

    /// **Pins the cost of the ceiling, not merely its arithmetic.**
    ///
    /// An identity test (`ceiling == some_formula`) proves the constant matches
    /// a formula; it cannot tell you the formula was the right one, and it
    /// passes identically at 21.0 or 37.0. What a maintainer actually needs
    /// pinned is the *trade*: how much of this policy's voice the rescale gives
    /// up relative to sibling policies in `PolicyRegistry`'s sum, and how far
    /// the land guard still orders. Both are quoted in
    /// `SACRIFICE_VALUE_RAW_CEILING`'s docstring, so both are asserted here —
    /// change the ceiling and this reddens with the new numbers, forcing the
    /// docstring to be updated rather than silently going stale.
    #[test]
    fn sacrifice_value_ceiling_pins_the_compression_it_costs() {
        // The p95 printed body (a vanilla 6/6, `1.5*6 + 6`) is exactly
        // CRITICAL_MAX, which is why the bare clamp was invisible: it changed
        // nothing at 15.0 and everything above it.
        let p95_body = 1.5 * 6.0 + 6.0;
        assert_eq!(
            p95_body,
            super::super::registry::CRITICAL_MAX,
            "premise: a vanilla 6/6 must sit exactly on the critical ceiling, \
             which is what makes single-card saturation reachable at all"
        );

        for (raw, expected, what) in [
            (
                4.5,
                4.5,
                "the land guard's own default passes through untouched",
            ),
            (p95_body, 9.0, "p95 body: 15.0 -> 9.0 (docstring figure)"),
            (10.0, 7.0, "raw 10.0 -> 7.0 (docstring figure)"),
            (
                SACRIFICE_VALUE_RAW_CEILING,
                15.0,
                "the ceiling maps to CRITICAL_MAX",
            ),
        ] {
            let got = rescale_into_critical_band(-raw, SACRIFICE_VALUE_RAW_CEILING);
            assert!(
                (got + expected).abs() < 1e-9,
                "compression changed: {what} — expected {expected}, got {}. \
                 SACRIFICE_VALUE_RAW_CEILING is {SACRIFICE_VALUE_RAW_CEILING}; \
                 if you moved it, update the figures in its docstring, because \
                 they are what a maintainer re-tuning this will read.",
                -got
            );
        }

        // The other half of the trade: how far the land guard still orders.
        let penalties = crate::config::PolicyPenalties::default();
        let ordered_to = (1..=12)
            .take_while(|n| {
                let n = f64::from(*n);
                let land_free = -(NONCREATURE_SACRIFICE_CAP * n);
                let with_land = -(NONCREATURE_SACRIFICE_CAP * (n - 1.0)
                    + penalties.sacrifice_land_penalty
                    + penalties.sacrifice_needed_land_penalty.abs());
                rescale_into_critical_band(land_free, SACRIFICE_VALUE_RAW_CEILING)
                    > rescale_into_critical_band(with_land, SACRIFICE_VALUE_RAW_CEILING)
            })
            .count() as f64;
        assert_eq!(
            ordered_to, GUARDED_SELECTION_CARDINALITY,
            "the land guard orders all-non-creature selections up to {ordered_to} \
             cards, but GUARDED_SELECTION_CARDINALITY claims \
             {GUARDED_SELECTION_CARDINALITY}. Whichever you changed — the \
             ceiling, NONCREATURE_SACRIFICE_CAP, or a sacrifice penalty default \
             — the two must be re-derived together."
        );

        // The no-commander derivation above is intentionally constant-only. This
        // arm constructs the commander that invalidates its universal reading:
        // an uncrewed `{3}{W}` Vehicle commander cast once costs 4.0 on board
        // plus a 6.0 command-zone repurchase, so C is literally 10.0.
        let mut commander_state = GameState::new_two_player(42);
        commander_state.format_config.command_zone = true;
        let vehicle_card = CardId(commander_state.next_object_id);
        let vehicle = create_object(
            &mut commander_state,
            vehicle_card,
            PlayerId(0),
            "Vehicle Commander".to_string(),
            Zone::Battlefield,
        );
        {
            let object = commander_state.objects.get_mut(&vehicle).unwrap();
            object.card_types.core_types.push(CoreType::Artifact);
            object.is_commander = true;
            object.mana_cost = ManaCost::generic(4);
            object.base_mana_cost = ManaCost::generic(4);
        }
        commander_state.commander_cast_count.insert(vehicle, 1);
        let commander_price = sacrifice_cost(&commander_state, vehicle, &penalties);
        assert_eq!(
            commander_price, 10.0,
            "reach guard: the cardinality arm must use a real owned commander priced at C = 10.0"
        );

        let commander_ordered_to = (2..=12)
            .take_while(|n| {
                let n = f64::from(*n);
                let land_free = -(commander_price + NONCREATURE_SACRIFICE_CAP * (n - 1.0));
                let with_land = -(commander_price
                    + NONCREATURE_SACRIFICE_CAP * (n - 2.0)
                    + penalties.sacrifice_land_penalty
                    + penalties.sacrifice_needed_land_penalty.abs());
                rescale_into_critical_band(land_free, SACRIFICE_VALUE_RAW_CEILING)
                    > rescale_into_critical_band(with_land, SACRIFICE_VALUE_RAW_CEILING)
            })
            .last()
            .map(f64::from)
            .expect("a commander-present selection of two cards still orders");
        assert_eq!(
            commander_ordered_to, 5.0,
            "with C = 10.0, the commander-present bound is five: moving the \
             ceiling, cap, land penalty, or the live commander price requires \
             re-deriving this formula"
        );
    }

    /// **The gate test for the clamp seam: this asserts on `verdict`, which is
    /// what production ranks, not on `score`.**
    ///
    /// Every other land-guard test here reads the pre-band `score`. Production
    /// ranks `verdict` → `PolicyVerdict::score` → clamped to `CRITICAL_MAX` →
    /// summed by `PolicyRegistry::score`, and the clamp *saturates*: at four
    /// cards the land-free set (−16.0) and the land-bearing set (−21.0) both
    /// clamp to −15.0, the guard is erased, and the stable sort decides — the
    /// original defect, one cardinality later. The test proves that collapse
    /// in-line (it computes what a bare `PolicyVerdict::score` would return and
    /// asserts the two are equal) so the discrimination does not rest on anyone
    /// remembering to revert the fix.
    ///
    /// Revert `rescale_into_critical_band` in `verdict` and the final assertion
    /// flips from `−9.4 > −11.4` to `−15.0 > −15.0`.
    #[test]
    fn verdict_preserves_the_land_guard_where_a_bare_clamp_collapses_it() {
        use super::super::registry::CRITICAL_MAX;

        let mut state = GameState::new_two_player(42);

        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let artifacts: Vec<ObjectId> = (0..4)
            .map(|i| {
                let card = CardId(state.next_object_id);
                let id = create_object(
                    &mut state,
                    card,
                    PlayerId(0),
                    format!("Gilded Lotus {i}"),
                    Zone::Battlefield,
                );
                let obj = state.objects.get_mut(&id).unwrap();
                obj.card_types.core_types.push(CoreType::Artifact);
                obj.mana_cost = ManaCost::generic(5);
                id
            })
            .collect();

        let config = AiConfig::default();

        let mut choices = artifacts.clone();
        choices.push(land);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::PayCost {
                player: PlayerId(0),
                kind: PayCostKind::Sacrifice,
                choices,
                count: 4,
                min_count: 4,
                resume: CostResume::Spell {
                    spell: dummy_pending(),
                },
            },
            candidates: Vec::new(),
        };

        let candidate_for = |cards: Vec<ObjectId>| CandidateAction {
            action: GameAction::SelectCards { cards },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let score_of = |candidate: &CandidateAction| {
            SacrificeValuePolicy.score(&PolicyContext {
                state: &state,
                decision: &decision,
                candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &crate::context::AiContext::empty(&config.weights),
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            })
        };
        let verdict_delta_of = |candidate: &CandidateAction| {
            let PolicyVerdict::Score { delta, .. } = SacrificeValuePolicy.verdict(&PolicyContext {
                state: &state,
                decision: &decision,
                candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &crate::context::AiContext::empty(&config.weights),
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            }) else {
                panic!("sacrifice value policy must return a Score verdict");
            };
            delta
        };

        // A forced four-card sacrifice: keep the land, or give it up and keep
        // one more artifact. `min_count == count == 4` so the AI cannot dodge
        // by selecting fewer.
        let land_free = candidate_for(artifacts.clone());
        let with_land = candidate_for(vec![artifacts[0], artifacts[1], artifacts[2], land]);

        let raw_land_free = score_of(&land_free);
        let raw_with_land = score_of(&with_land);

        // Reach guard: both candidates cleared the `WaitingFor` match instead of
        // short-circuiting at `return 0.0`.
        assert!(
            raw_land_free < 0.0 && raw_with_land < 0.0,
            "reach guard: both selections must be scored by the sacrifice arm, \
             got land-free {raw_land_free} and with-land {raw_with_land}"
        );

        // Non-vacuity premise. If either raw magnitude sat inside the critical
        // band this fixture would never reach the saturating region and the
        // assertion below would pass for free.
        assert!(
            raw_land_free.abs() > CRITICAL_MAX && raw_with_land.abs() > CRITICAL_MAX,
            "fixture premise: BOTH selections must exceed the critical ceiling \
             ({CRITICAL_MAX}) or the clamp never engages and this test proves \
             nothing. Got land-free {raw_land_free} and with-land \
             {raw_with_land}"
        );
        assert!(
            raw_land_free > raw_with_land,
            "fixture premise: the raw score already orders these correctly \
             (land-free {raw_land_free} should outrank with-land \
             {raw_with_land}); this test is about whether the BAND preserves \
             that ordering, not about the score"
        );

        // The collapse, proven rather than asserted: this is exactly what
        // `verdict` returned before `rescale_into_critical_band` was introduced.
        let bare_clamp = |raw: f64| {
            let PolicyVerdict::Score { delta, .. } =
                PolicyVerdict::score(raw, PolicyReason::new("bare_clamp_probe"))
            else {
                panic!("PolicyVerdict::score never rejects");
            };
            delta
        };
        assert_eq!(
            bare_clamp(raw_land_free),
            bare_clamp(raw_with_land),
            "premise: a bare PolicyVerdict::score clamp must COLLAPSE these two \
             to the same delta — that is the defect under test. If they now \
             differ, either the fixture stopped saturating or CRITICAL_MAX moved."
        );

        let delta_land_free = verdict_delta_of(&land_free);
        let delta_with_land = verdict_delta_of(&with_land);

        assert!(
            delta_land_free.abs() <= CRITICAL_MAX && delta_with_land.abs() <= CRITICAL_MAX,
            "band contract: both deltas must stay inside the critical ceiling, \
             got {delta_land_free} and {delta_with_land}"
        );
        assert!(
            delta_land_free > delta_with_land,
            "THE LAND GUARD MUST SURVIVE THE BAND. Production ranks the verdict \
             delta, not the raw score: got land-free {delta_land_free} vs \
             with-land {delta_with_land}. Equal deltas mean the clamp saturated \
             both selections, the per-land penalty was erased, and the stable \
             sort now decides which set is sacrificed — the exact inversion \
             `sacrifice_needed_land_penalty` exists to prevent. Did you drop \
             `rescale_into_critical_band` from `verdict`, or lower \
             SACRIFICE_VALUE_RAW_CEILING under this selection's raw magnitude?"
        );
    }
}
