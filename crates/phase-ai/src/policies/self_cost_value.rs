//! Net-value gate policy for self-cost ability activations.
//!
//! Thin adapter over `self_cost.rs`: fetches the activated ability, confirms its
//! cost spends a self-resource (sacrifice / pay-life / discard / self-exile),
//! stands down when off-ability deck synergy justifies the cost, then prices the
//! cost against the ability's immediate payoff. A real cost with a trivial
//! payoff is rejected (scoring `-inf`, so Pass wins); a cheap cost is merely
//! deprioritized; a payoff that can be confidently and completely priced is
//! compared against the cost — and a payoff **certified smaller than the cost is
//! rejected**, not discounted — and a real payoff that cannot be soundly priced
//! (unpriceable effect, or an unmodeled rider in the chain) is left alone.
//!
//! This policy is the **single authority** for the cost-vs-benefit question on
//! every self-cost activation, mana-costed sacrifice outlets included.
//!
//! # Why the underwater arm is categorical and not graduated
//!
//! An earlier revision scored the underwater case in proportion to the
//! shortfall, on the argument that search should stay able to override it. That
//! was **falsified by measurement** (see `policies::tests::sac_outlet_drain_repro`
//! for the executed record). The final decision is a softmax *sample*
//! (`search::softmax_select_pairs`) at Medium `temperature = 1.0`, repeated over
//! ~100 priority windows per game: repricing a repeatable outlet from `+0.85` to
//! `-0.65` cut the per-window activation probability 63.9% → 28.3%, a real 2.3×
//! improvement, and the board drained **identically**, because P(at least one
//! selection over that many windows) ≈ 1.0 either way.
//!
//! The general law, worth carrying to any policy verdict on a *repeatable*
//! candidate: **a graduated penalty is a rate, a `Reject` is a bound, and a rate
//! cannot enforce a bound over unbounded trials.** Only `-inf` is categorical —
//! its softmax weight is `exp(-inf) = 0`. So a graduated "discouragement" on a
//! trade this policy has certified as a loss means "do it eventually", which is
//! the opposite of what the certification says.
//!
//! # Why the trivial-payoff bound is a life COUNT and not a price
//!
//! The law above says a bound cannot be enforced by a rate — but stating a bound
//! still requires choosing the unit it is expressed in, and for the trivial
//! branch that unit is the resource, not its price.
//!
//! `self_cost::real_self_cost` prices pay-life at `points *
//! self_cost_pay_life_per_point` (0.15/point), multiplied by 4.0 only when life
//! is already a pressured resource. An unpressured Adanto Vanguard ("Pay 4 life:
//! this creature gains indestructible until end of turn") therefore prices at
//! 0.60, under `REAL_COST_FLOOR`, and lands on the graduated `self_cost_marginal`
//! branch — a −0.60 rate against an ability that can be activated in every
//! priority window, which is exactly the shape the law above forbids. Raising
//! `self_cost_pay_life_per_point` until 4 life clears the floor would reach that
//! case, but only by moving every *priced* comparison a life leg participates in
//! (the `Priced` arm's net, every mixed-cost outlet boundary) — a measured AI
//! behaviour change that needs an ai-gate calibration to justify.
//!
//! The veto side needs no calibration at all, because it never compares against
//! a benefit: the payoff has already been certified trivial. So the question it
//! asks is the one the player actually faces — **is this at least
//! `self_cost_material_life` life?** A count is the right unit for the same
//! reason `-inf` is the right verdict: the answer must not move when the rate
//! moves, and must not drift when CMA-ES retrains the per-point price.
//!
//! **The veto fires only on a Trivial-proper chain**
//! (`BenefitAppraisal::Trivial { any_unmodeled: false }`), where every effect
//! was explicitly classified trivial. A chain carrying an unmodeled effect —
//! `Effect::Pump` among them, so the whole Desolation Prowler class ("Pay 2
//! life: this creature gets +2/+2 until end of turn") — reports "no payoff" only
//! as an absence of evidence, so it keeps the graduated branch. Its activation
//! *timing* is a separate policy's job, not this one's.

use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::self_cost::{
    appraise_benefit, cost_is_material, real_self_cost, self_cost_in_scope,
    self_counter_cost_preview, synergy_justifies_self_cost, BenefitAppraisal,
    SelfCounterCostPreview,
};
use crate::features::DeckFeatures;

/// At or above this priced self-cost, a trivial-benefit activation is a real
/// loss and is rejected. Below it, a trivial-benefit activation is only
/// deprioritized (never hard-rejected) — but it is still never treated as a
/// benefit-present play.
const REAL_COST_FLOOR: f64 = 1.0;

pub struct SelfCostValuePolicy;

impl TacticalPolicy for SelfCostValuePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SelfCostValue
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
        // activation-constant: cost-axis backstop for every activated-ability candidate; scope gating happens in `verdict`.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::ActivateAbility {
            source_id,
            ability_index: _,
        } = &ctx.candidate.action
        else {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_value_na"));
        };

        let Some(ability) = ctx.effective_activated_ability() else {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_value_na"));
        };

        if let Some(verdict) = counter_replenishment_verdict(
            self_counter_cost_preview(ctx.state, ctx.ai_player, *source_id, &ability),
            ctx.penalties(),
        ) {
            return verdict;
        }

        if ability.ability_tag == Some(AbilityTag::Cycling) {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_cycling_deferred"));
        }

        let Some(cost) = ability.cost.as_ref() else {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_value_na"));
        };

        if !self_cost_in_scope(cost) {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_value_na"));
        }

        let features = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .cloned()
            .unwrap_or_default();

        if synergy_justifies_self_cost(&features, &ability) {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_synergy_justified"));
        }

        let cost_value =
            real_self_cost(ctx.state, ctx.ai_player, *source_id, cost, ctx.penalties());

        let cost_milli = (cost_value * 1000.0) as i64;

        match appraise_benefit(
            ctx.state,
            ctx.ai_player,
            *source_id,
            &ability,
            ctx.penalties(),
        ) {
            BenefitAppraisal::Trivial { any_unmodeled } => {
                // Two independent grounds for the same categorical veto. The
                // priced one applies to every chain that reached here. The
                // materiality one is restricted to Trivial-PROPER chains: with
                // an unmodeled effect in the chain, "trivial" is an absence of
                // evidence rather than a finding, and a bound may not rest on
                // it. See the module docs for why the second is a life count.
                if cost_value >= REAL_COST_FLOOR
                    || (!any_unmodeled
                        && cost_is_material(
                            ctx.state,
                            ctx.ai_player,
                            *source_id,
                            cost,
                            ctx.penalties(),
                        ))
                {
                    return PolicyVerdict::reject(
                        PolicyReason::new("self_cost_trivial_benefit")
                            .with_fact("cost_milli", cost_milli)
                            .with_fact("benefit", 0),
                    );
                }
                // Trivial payoff, but the priced self-cost is below the
                // real-loss floor: deprioritize with an auto-banded negative
                // delta. No trivial self-cost play may resolve to
                // `self_cost_benefit_present`, and the `self_cost_marginal`
                // reason deliberately does NOT claim a benefit.
                PolicyVerdict::score(
                    -cost_value,
                    PolicyReason::new("self_cost_marginal").with_fact("cost_milli", cost_milli),
                )
            }
            BenefitAppraisal::Priced { value } => {
                let net = value - cost_value;
                let benefit_milli = (value * 1000.0) as i64;
                // Cost and benefit are summed from different coefficients, so an
                // exact-cover trade can land a few ULPs below zero. This veto is
                // categorical, so that rounding must not decide the boundary.
                if net >= -f64::EPSILON * value.abs().max(cost_value.abs()).max(1.0) {
                    // Inclusive boundary: net == 0 covers. A 0/1 creature token
                    // prices at `max(creature_combat_value(0,1) = 1.0, 0.5) =
                    // 1.0` against draw(1) = 1.0 — exactly 0. Cracking it is
                    // intended; the comparison means "not a loss", and an
                    // exact-cover crack is allowed. Neutral rather than
                    // positive: `CardAdvantagePolicy`/`DrawPayoffPolicy` already
                    // reward the draw on this same candidate, so a positive
                    // delta here would double-count.
                    //
                    // NOTE: tap state is deliberately NOT part of this boundary.
                    // `sacrifice_cost` prices the permanent intrinsically, so a
                    // tapped 1/1 token still costs 2.5 and stays underwater. The
                    // earlier tapped-discounted reading put it at exactly 0 here
                    // and made this arm the escape hatch a whole board drained
                    // through. Fixed at the give-up authority, not at this
                    // boundary.
                    PolicyVerdict::neutral(
                        PolicyReason::new("self_cost_benefit_covers_cost")
                            .with_fact("cost_milli", cost_milli)
                            .with_fact("benefit_milli", benefit_milli),
                    )
                } else {
                    // net < 0: a CERTIFIED losing trade — the chain was fully
                    // priced, the quantity read, the cost bound
                    // filter-faithfully, and every modeled justification
                    // (synergy payoff on board, life pressure, unpriceable or
                    // unmodeled value, counter replenishment, cycling, cEDH
                    // bracket) already declined to stand the comparison down.
                    //
                    // Categorical, not graduated: under repeated softmax
                    // sampling any finite negative is eventually selected while
                    // fodder remains (measured — see the module docs and
                    // `policies::tests::sac_outlet_drain_repro`), so a graduated
                    // "discouragement" here means "do it eventually", the
                    // opposite of what this verdict certifies.
                    //
                    // There is no threshold constant in the restraint: the
                    // boundary is the sign of the net, inclusive at zero. The
                    // magnitudes that set WHERE that sign flips
                    // (`creature_combat_value`'s 1.5*P + T, `SINGLE_CARD_VALUE`,
                    // `sacrifice_token_cost`, the per-life coefficient) live in
                    // eval, which is where calibration belongs.
                    PolicyVerdict::reject(
                        PolicyReason::new("self_cost_benefit_underwater")
                            .with_fact("cost_milli", cost_milli)
                            .with_fact("benefit_milli", benefit_milli),
                    )
                }
            }
            BenefitAppraisal::Unpriced => {
                PolicyVerdict::neutral(PolicyReason::new("self_cost_benefit_present"))
            }
        }
    }
}

/// Conservatively prices the exact self-counter-replenishment preview.
///
/// CR 614.1: A replacement can prevent the counter event or redirect it to an
/// unsupported event class. Either outcome means this policy cannot assume the
/// activation repays its counter cost, so both receive the bounded penalty.
/// Applied, choice-required, and transformed outcomes remain neutral because
/// they do not establish that replenishment has failed.
fn counter_replenishment_verdict(
    preview: Option<SelfCounterCostPreview>,
    penalties: &crate::config::PolicyPenalties,
) -> Option<PolicyVerdict> {
    let reason = match preview {
        Some(SelfCounterCostPreview::Prevented) => "self_cost_counter_replacement_prevented",
        Some(SelfCounterCostPreview::Unsupported) => "self_cost_counter_replacement_unsupported",
        Some(
            SelfCounterCostPreview::Applied
            | SelfCounterCostPreview::ChoiceRequired
            | SelfCounterCostPreview::Transformed,
        )
        | None => return None,
    };
    Some(PolicyVerdict::strong(
        -penalties.self_cost_counter_replacement_prevented_penalty,
        PolicyReason::new(reason),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::aristocrats::AristocratsFeature;
    use crate::features::landfall::LandfallFeature;
    use crate::features::lifegain::LifegainFeature;
    use crate::features::reanimator::ReanimatorFeature;
    use crate::features::DeckFeatures;
    use crate::policies::self_cost::{chain_effect_trivialities, EffectTriviality};
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::bracket_estimate::CommanderBracketTier;
    use engine::game::effects::draw::can_draw_at_least_one;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, ContinuousModification,
        ControllerRef, DiscardSelfScope, DrawReplacementScope, Duration, Effect, ManaContribution,
        ManaProduction, ObjectScope, PlayerScope, PtValue, QuantityExpr, QuantityModification,
        QuantityRef, ReplacementCondition, ReplacementDefinition, ReplacementMode,
        ReplacementPlayerScope, SacrificeCost, StaticDefinition, TargetFilter, TypeFilter,
        TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::counter::{CounterMatch, CounterType};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::{Keyword, KeywordKind};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::statics::{ProhibitionScope, StaticMode};
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    // --- fixture builders -------------------------------------------------

    fn activated(effect: Effect, cost: AbilityCost) -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, effect);
        ability.cost = Some(cost);
        ability
    }

    fn sac_creature_cost() -> AbilityCost {
        AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            1,
        ))
    }

    fn sac_land_cost() -> AbilityCost {
        AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            1,
        ))
    }

    fn gain_life(amount: i32) -> Effect {
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: amount },
            player: TargetFilter::Controller,
        }
    }

    fn draw(count: i32) -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: count },
            target: TargetFilter::Controller,
        }
    }

    fn deal_fixed(value: i32) -> Effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        }
    }

    fn deal_dynamic() -> Effect {
        // Fling shape: damage equal to a creature's power (non-Fixed quantity).
        Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            },
            target: TargetFilter::Player,
            damage_source: None,
            excess: None,
        }
    }

    fn add_two_colorless() -> Effect {
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: Vec::new(),
                contribution: ManaContribution::Base,
            },
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        }
    }

    fn search_for_land() -> Effect {
        Effect::SearchLibrary {
            source_zones: vec![Zone::Library],
            filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
            split: None,
        }
    }

    fn shroud_self_grant() -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Shroud,
                }])],
            target: Some(TargetFilter::SelfRef),
            duration: None,
            end_cost: None,
        }
    }

    /// Adanto Vanguard's payoff: "This creature gains indestructible until end
    /// of turn."
    ///
    /// Rebuilt from the parsed shape in `data/card-data.json`
    /// (`.["adanto vanguard"].abilities[0].effect`): `target: null` and
    /// `duration: "UntilEndOfTurn"`, with the static's `affected: SelfRef`. The
    /// `affected` field is the load-bearing one — `is_self_protection_effect`
    /// reads the outer `target` only as the fallback for an inner
    /// `ParentTarget`, so this classifies as self-protection on `affected`
    /// alone, exactly as the printed card does.
    fn indestructible_self_grant() -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Indestructible,
                }])],
            target: None,
            duration: Some(Duration::UntilEndOfTurn),
            end_cost: None,
        }
    }

    /// Selenia, Dark Angel / Crovax, Ascendant Hero: "Pay 2 life: Return ~ to
    /// its owner's hand."
    ///
    /// Rebuilt from the parsed shape in `data/card-data.json`
    /// (`.["selenia, dark angel"].abilities[0]`): `Bounce` with
    /// `target: SelfRef` and `destination: null`, cost `PayLife { Fixed 2 }`.
    fn self_bounce() -> Effect {
        Effect::Bounce {
            target: TargetFilter::SelfRef,
            destination: None,
            selection: Default::default(),
        }
    }

    /// Puts an opponent-controlled `Destroy` on the stack aimed at `victim`, so
    /// `any_immediate_threat` sees a live threat to an AI permanent.
    fn stack_removal_targeting(state: &mut GameState, victim: ObjectId) {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};

        let spell_id = create_object(
            state,
            CardId(next_id()),
            OPP,
            "Doom Blade".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(victim)],
            spell_id,
            OPP,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: OPP,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(Box::new(ability)),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
    }

    /// Desolation Prowler's payoff: "This creature gets +2/+2 until end of
    /// turn." `Effect::Pump` has no `effect_triviality` arm, so this is the
    /// Unmodeled side of the Trivial-proper split.
    fn pump_self(power: i32, toughness: i32) -> Effect {
        Effect::Pump {
            power: PtValue::Fixed(power),
            toughness: PtValue::Fixed(toughness),
            target: TargetFilter::SelfRef,
        }
    }

    fn pay_life_cost(amount: i32) -> AbilityCost {
        AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: amount },
        }
    }

    fn put_counter(counter: CounterType, target: TargetFilter) -> Effect {
        Effect::PutCounter {
            counter_type: counter,
            count: QuantityExpr::Fixed { value: 1 },
            target,
        }
    }

    fn self_counter_replenisher() -> AbilityDefinition {
        activated(
            put_counter(CounterType::Plus1Plus1, TargetFilter::SelfRef),
            AbilityCost::RemoveCounter {
                count: 1,
                counter_type: CounterMatch::OfType(CounterType::Plus1Plus1),
                target: None,
                selection: Default::default(),
            },
        )
    }

    fn install_counter_replacement(state: &mut GameState, modification: QuantityModification) {
        let replacement = create_object(
            state,
            CardId(next_id()),
            AI,
            "Counter replacement".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&replacement)
            .expect("replacement exists")
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(modification),
            );
    }

    /// `GameState::new_two_player(42)` — the state every row in this module
    /// builds on — **with a real AI library**.
    ///
    /// `new_two_player` seeds NO library, and a draw from an empty library
    /// delivers no card (CR 704.5b), so a draw fixture built on the bare
    /// constructor prices its payoff at zero for the empty-library reason no
    /// matter what else the fixture says. Every row whose claim depends on a
    /// draw being WORTH something must start here, or it certifies something
    /// other than what its comment says. Three cards — more than any fixture's
    /// draw count — so library size is never the discriminator.
    fn state_with_library() -> GameState {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(next_id()),
                AI,
                format!("Library Card {i}"),
                Zone::Library,
            );
        }
        state
    }

    /// Notion Thief on the OPPONENT's battlefield: "If an opponent would draw a
    /// card except the first one they draw in each of their draw steps, instead
    /// that player skips that draw and you draw a card."
    ///
    /// Rebuilt VERBATIM from the parsed shape in `data/card-data.json`
    /// (`.["notion thief"].replacements[0]`): `event: Draw`, `mode: Mandatory`,
    /// `valid_player: Opponent`, `condition: ExceptFirstDrawInDrawStep`,
    /// `draw_scope: IndividualDraw`, and an `execute` whose head effect is the
    /// `Unimplemented("draw")` gap node carrying the `Draw{1, Controller}`
    /// sub-ability. That `Unimplemented` head is LOAD-BEARING — it is what makes
    /// the branch a non-Draw substitution for
    /// `replacement::draw_is_substituted_away`, and the engine is runtime-proven
    /// correct on this card (`notion_thief_opponent_draw_redirect.rs`). It is
    /// reproduced, never "fixed", and is built through the single authority
    /// `Effect::unimplemented` rather than a hand-written literal.
    ///
    /// The condition is a LIVE gate, not a decoration: it exempts the active
    /// player's first draw of their own draw step, so the caller must be in the
    /// main phase — which is where the reported drain happened — for the
    /// replacement to apply at all.
    fn opposing_notion_thief(state: &mut GameState) -> ObjectId {
        state.phase = Phase::PreCombatMain;
        let id = create_object(
            state,
            CardId(next_id()),
            OPP,
            "Notion Thief".to_string(),
            Zone::Battlefield,
        );
        let mut execute =
            AbilityDefinition::new(AbilityKind::Spell, Effect::unimplemented("draw", "draw"));
        execute.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            draw(1),
        )));
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Draw)
            .draw_scope(DrawReplacementScope::IndividualDraw)
            .execute(execute)
            .condition(ReplacementCondition::ExceptFirstDrawInDrawStep);
        replacement.valid_player = Some(ReplacementPlayerScope::Opponent);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.replacement_definitions.push(replacement);
        id
    }

    /// A draw-restricting static (Spirit of the Labyrinth / Narset shape) on an
    /// OPPONENT permanent, scoped to all players so it covers the AI.
    fn add_draw_restricting_static(state: &mut GameState, mode: StaticMode) {
        let id = create_object(
            state,
            CardId(next_id()),
            OPP,
            "Draw Hoser".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.static_definitions.push(StaticDefinition::new(mode));
    }

    /// Install a typed individual-draw replacement owned by the AI. These
    /// fixtures exercise the same engine preview that production pricing calls;
    /// none reproduces replacement applicability in phase-AI.
    fn install_ai_draw_replacement(state: &mut GameState, replacement: ReplacementDefinition) {
        let id = create_object(
            state,
            CardId(next_id()),
            AI,
            "AI Draw Replacement".to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&id)
            .expect("replacement source exists");
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(1);
        object.toughness = Some(1);
        object.replacement_definitions.push(replacement);
    }

    fn mandatory_draw_prevent() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Draw)
            .draw_scope(DrawReplacementScope::IndividualDraw)
            .quantity_modification(QuantityModification::Prevent)
    }

    fn optional_draw_prevent() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Draw)
            .draw_scope(DrawReplacementScope::IndividualDraw)
            .quantity_modification(QuantityModification::Prevent)
            .mode(ReplacementMode::Optional { decline: None })
    }

    fn mandatory_search_draw_substitute() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Draw)
            .draw_scope(DrawReplacementScope::IndividualDraw)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                search_for_land(),
            ))
    }

    fn add_library_land(state: &mut GameState) {
        let id = create_object(
            state,
            CardId(next_id()),
            AI,
            "Search Fixture Land".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&id)
            .expect("library land exists")
            .card_types
            .core_types
            .push(CoreType::Land);
    }

    // --- state / context helpers -----------------------------------------

    fn creature(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        p: i32,
        t: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(next_id()),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(p);
        obj.toughness = Some(t);
        id
    }

    fn owned_commander_creature(
        state: &mut GameState,
        name: &str,
        power: i32,
        toughness: i32,
        command_zone_casts: u32,
    ) -> ObjectId {
        let id = creature(state, AI, name, power, toughness);
        let object = state.objects.get_mut(&id).unwrap();
        object.is_commander = true;
        object.mana_cost = engine::types::mana::ManaCost::generic(4);
        object.base_mana_cost = engine::types::mana::ManaCost::generic(4);
        state.format_config.command_zone = true;
        if command_zone_casts > 0 {
            state.commander_cast_count.insert(id, command_zone_casts);
        }
        id
    }

    fn token_creature(state: &mut GameState, name: &str, p: i32, t: i32) -> ObjectId {
        let id = creature(state, AI, name, p, t);
        state.objects.get_mut(&id).unwrap().is_token = true;
        id
    }

    fn artifact_token(state: &mut GameState, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(next_id()),
            AI,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.is_token = true;
        id
    }

    fn sac_artifact_cost() -> AbilityCost {
        AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
            ),
            1,
        ))
    }

    fn put_counter_all(counter: CounterType, target: TargetFilter) -> Effect {
        Effect::PutCounterAll {
            counter_type: counter,
            count: QuantityExpr::Fixed { value: 1 },
            target,
        }
    }

    fn land(state: &mut GameState, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(next_id()),
            AI,
            name.to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    fn source_with(
        state: &mut GameState,
        name: &str,
        core: &[CoreType],
        ability: AbilityDefinition,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(next_id()),
            AI,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        for &ct in core {
            obj.card_types.core_types.push(ct);
        }
        Arc::make_mut(&mut obj.abilities).push(ability);
        id
    }

    fn next_id() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1000);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn features_with(
        landfall: f32,
        lifegain: f32,
        reanimator: f32,
        death_triggers: Vec<String>,
        bracket: CommanderBracketTier,
    ) -> DeckFeatures {
        DeckFeatures {
            landfall: LandfallFeature {
                commitment: landfall,
                ..Default::default()
            },
            lifegain: LifegainFeature {
                commitment: lifegain,
                ..Default::default()
            },
            reanimator: ReanimatorFeature {
                commitment: reanimator,
                ..Default::default()
            },
            aristocrats: AristocratsFeature {
                death_trigger_count: death_triggers.len() as u32,
                death_trigger_names: death_triggers,
                ..Default::default()
            },
            bracket_tier: bracket,
            ..DeckFeatures::default()
        }
    }

    fn plain_features() -> DeckFeatures {
        features_with(0.0, 0.0, 0.0, Vec::new(), CommanderBracketTier::Core)
    }

    fn verdict_for(
        state: &GameState,
        source_id: ObjectId,
        features: DeckFeatures,
    ) -> PolicyVerdict {
        let config = AiConfig::default();
        let context = context_for(&config, features);
        verdict_for_in(&context, &config, state, source_id)
    }

    fn context_for(config: &AiConfig, features: DeckFeatures) -> AiContext {
        let mut session = AiSession::empty();
        session.features.insert(AI, features);
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        context
    }

    fn verdict_for_in(
        context: &AiContext,
        config: &AiConfig,
        state: &GameState,
        source_id: ObjectId,
    ) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config,
            context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        SelfCostValuePolicy.verdict(&ctx)
    }

    fn assert_reject(verdict: &PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Reject { reason } => assert_eq!(reason.kind, kind, "reject kind"),
            PolicyVerdict::Score { delta, reason } => {
                panic!(
                    "expected reject {kind}, got Score {{ delta: {delta}, kind: {} }}",
                    reason.kind
                )
            }
        }
    }

    /// Pins the arithmetic of a verdict that carries no delta. A `Reject`
    /// propagates to `-inf`, so the comparison it certifies is only observable
    /// through `PolicyReason::facts` — these are `(value * 1000.0) as i64`, so
    /// the expectations are exact, not epsilon'd.
    fn assert_facts(verdict: &PolicyVerdict, cost_milli: i64, benefit_milli: i64) {
        let reason = match verdict {
            PolicyVerdict::Reject { reason } | PolicyVerdict::Score { reason, .. } => reason,
        };
        assert_eq!(
            reason.facts,
            vec![("cost_milli", cost_milli), ("benefit_milli", benefit_milli)],
            "verdict facts"
        );
    }

    fn assert_trivial_facts(verdict: &PolicyVerdict, cost_milli: i64) {
        let reason = match verdict {
            PolicyVerdict::Reject { reason } | PolicyVerdict::Score { reason, .. } => reason,
        };
        assert_eq!(
            reason.facts,
            vec![("cost_milli", cost_milli), ("benefit", 0)],
            "trivial verdict facts"
        );
    }

    fn assert_neutral(verdict: &PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, kind, "neutral kind");
                assert_eq!(*delta, 0.0, "neutral delta");
            }
            PolicyVerdict::Reject { reason } => {
                panic!("expected neutral {kind}, got Reject {}", reason.kind)
            }
        }
    }

    fn assert_not_reject(verdict: &PolicyVerdict) {
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "expected a Score (not a hard veto)"
        );
    }

    // --- Row 1: sac-creature trivial lifegain rejected --------------------

    #[test]
    fn sac_creature_for_small_lifegain_rejected() {
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(gain_life(1), sac_creature_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn sac_creature_for_draw_is_vetoed_underwater() {
        // Row 1's positive reach-guard, now pinning the VETO contract:
        // identical cost to `sac_creature_for_small_lifegain_rejected`, real
        // payoff (a card), so the input passed `self_cost_in_scope` and the
        // chain walk classified the draw NON-trivial — reaching `underwater` at
        // all proves both, which is the reach-guard this test carries.
        //
        // The comparison: the Bear prices at `evaluate_creature_intrinsic(2,2)`
        // = 1.5*2+2 = 5.0 against `draw(1)` = 1.0 → net -4.0, certified losing.
        //
        // HISTORY: before the veto this arm emitted a graduated `Score` with
        // delta -4.0. The graduated shape was falsified by measurement (module
        // docs) — a rate cannot bound a repeatable candidate. Reverting to a
        // graduated score flips this to `Score` and the test goes red on shape;
        // reverting the pricing entirely flips the kind to
        // `self_cost_benefit_present` and it goes red on kind.
        //
        // The library is seeded because the 1000 is the whole claim: on the bare
        // constructor's empty library the draw would price 0 and this row would
        // certify the empty-library rule instead of the arithmetic it names.
        let mut state = state_with_library();
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 5000, 1000);
    }

    // --- Row 2: Fling-class dynamic damage NOT rejected -------------------

    #[test]
    fn dynamic_power_damage_not_rejected() {
        let mut state = GameState::new_two_player(42);
        state.players[OPP.0 as usize].life = 12;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Fling-like",
            &[CoreType::Artifact],
            activated(deal_dynamic(), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn fixed_one_face_ping_rejected() {
        // Hostile boundary for row 2: same sac cost, a fixed 1 to face with no
        // kill is trivial → reject.
        let mut state = GameState::new_two_player(42);
        state.players[OPP.0 as usize].life = 12;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Pinger",
            &[CoreType::Artifact],
            activated(deal_fixed(1), sac_creature_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    // --- Row 3: burn above the ceiling NOT rejected, boundary at 2 --------

    #[test]
    fn fixed_three_face_damage_not_rejected() {
        let mut state = GameState::new_two_player(42);
        state.players[OPP.0 as usize].life = 20;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Burn",
            &[CoreType::Artifact],
            activated(deal_fixed(3), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn fixed_two_face_damage_no_kill_rejected() {
        let mut state = GameState::new_two_player(42);
        state.players[OPP.0 as usize].life = 20;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Weak Burn",
            &[CoreType::Artifact],
            activated(deal_fixed(2), sac_creature_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    // --- Row 4 / 4b: Zuran Orb rejected, land-search allowed --------------

    #[test]
    fn zuran_orb_land_sac_lifegain_rejected() {
        let mut state = GameState::new_two_player(42);
        land(&mut state, "Forest");
        let source = source_with(
            &mut state,
            "Zuran Orb",
            &[CoreType::Artifact],
            activated(gain_life(2), sac_land_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn zuran_orb_still_rejected_in_landfall_deck() {
        // NEW-1 regression guard: landfall commitment above the synergy floor
        // must NOT stand Zuran Orb down — landfall triggers on a land entering,
        // never on one being sacrificed.
        let mut state = GameState::new_two_player(42);
        land(&mut state, "Forest");
        let source = source_with(
            &mut state,
            "Zuran Orb",
            &[CoreType::Artifact],
            activated(gain_life(2), sac_land_cost()),
        );
        let features = features_with(0.9, 0.0, 0.0, Vec::new(), CommanderBracketTier::Core);
        assert_reject(
            &verdict_for(&state, source, features),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn land_sac_search_for_land_allowed_even_in_landfall_deck() {
        // Reach-guard for 4b: a real "sacrifice a land: search a land" ramp line
        // reaches scoring (in-scope land sacrifice) and is allowed via the
        // SearchLibrary-for-land arm, NOT a synergy stand-down.
        let mut state = GameState::new_two_player(42);
        land(&mut state, "Forest");
        let source = source_with(
            &mut state,
            "Ramp Land",
            &[CoreType::Land],
            activated(search_for_land(), sac_land_cost()),
        );
        let features = features_with(0.9, 0.0, 0.0, Vec::new(), CommanderBracketTier::Core);
        assert_neutral(
            &verdict_for(&state, source, features),
            "self_cost_benefit_present",
        );
    }

    // --- Row 5: Ashnod's Altar (mana) allowed, cEDH stand-down ------------

    #[test]
    fn sac_for_mana_not_rejected() {
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Ashnod's Altar",
            &[CoreType::Artifact],
            activated(add_two_colorless(), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn cedh_bracket_does_not_waive_a_trivial_sacrifice_cost() {
        // Bracket classification is deck metadata, not a payment exemption:
        // a trivial sacrifice activation remains a real loss in CEDH too.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        creature(&mut state, AI, "Diregraf Captain", 2, 2);
        let source = source_with(
            &mut state,
            "Spark Reaper",
            &[CoreType::Creature],
            activated(gain_life(1), sac_creature_cost()),
        );
        let source_object = state
            .objects
            .get_mut(&source)
            .expect("Spark Reaper source exists");
        source_object.power = Some(1);
        source_object.toughness = Some(1);
        let features = features_with(
            0.0,
            0.0,
            0.0,
            vec!["Diregraf Captain".to_string()],
            CommanderBracketTier::Cedh,
        );
        assert_reject(
            &verdict_for(&state, source, features),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn cedh_aristocrats_never_waives_a_sacrifice_leaf_direct_or_nested() {
        let features = features_with(
            0.0,
            1.0,
            0.0,
            vec!["Diregraf Captain".to_string()],
            CommanderBracketTier::Cedh,
        );
        let costs = [
            sac_creature_cost(),
            AbilityCost::Composite {
                costs: vec![AbilityCost::Tap, sac_creature_cost()],
            },
            AbilityCost::OneOf {
                costs: vec![
                    sac_creature_cost(),
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            },
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::PerCounter {
                        counter: CounterType::Age,
                        target: TargetFilter::SelfRef,
                        base: Box::new(sac_creature_cost()),
                    },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            },
        ];

        for cost in costs {
            let ability = activated(gain_life(1), cost);
            assert!(
                !synergy_justifies_self_cost(&features, &ability),
                "a sacrifice leaf must remain priced even in a CEDH aristocrats deck"
            );
        }
    }

    // --- Row 6: discard-to-grant no-threat rejected -----------------------

    #[test]
    fn discard_for_self_protection_no_threat_rejected() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        // Give the AI a spare card so the discard cost is meaningful context.
        create_object(
            &mut state,
            CardId(next_id()),
            AI,
            "Filler".to_string(),
            Zone::Hand,
        );
        let cost = AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            selection: Default::default(),
            self_scope: Default::default(),
        };
        let source = source_with(
            &mut state,
            "Loopy Creature",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn discard_stands_down_in_reanimator_deck() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        let cost = AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            selection: Default::default(),
            self_scope: Default::default(),
        };
        let source = source_with(
            &mut state,
            "Loopy Creature",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        let features = features_with(0.0, 0.0, 0.9, Vec::new(), CommanderBracketTier::Core);
        assert_neutral(
            &verdict_for(&state, source, features),
            "self_cost_synergy_justified",
        );
    }

    // --- Row 7: self-exile-graveyard priced cheap (marginal, not reject) --

    #[test]
    fn self_exile_graveyard_single_card_is_marginal_not_rejected() {
        // DEVIATION from matrix row 7 ("reject"): the plan prices graveyard
        // exile at 0.15/card, well below the 0.5 marginal floor, so a single
        // self-exile is deprioritized, never hard-vetoed. Multi-card exiles
        // (>=7 cards) would clear the reject floor.
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Graveyard),
            filter: Some(TargetFilter::SelfRef),
        };
        let source = source_with(
            &mut state,
            "Psychic Frog",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_not_reject(&verdict);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "self_cost_marginal");
                assert!(delta < 0.0, "expected a deprioritizing nudge, got {delta}");
            }
            PolicyVerdict::Reject { .. } => unreachable!(),
        }
    }

    #[test]
    fn self_exile_hand_is_in_scope_and_priced_as_discard() {
        // Exile{Hand} is priced as a discard (1.0/card), so a trivial-benefit
        // hand-exile clears the reject floor — proves Exile{Hand} reaches scoring.
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Hand),
            filter: None,
        };
        let source = source_with(
            &mut state,
            "Hand Exiler",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    // --- Row 8: ExilesCards siblings never fire the gate ------------------

    #[test]
    fn exile_cost_siblings_out_of_scope() {
        // CollectEvidence / ExileWithAggregate / Behold are structurally
        // distinct from a self-resource exile — the gate must not fire.
        assert!(!self_cost_in_scope(&AbilityCost::CollectEvidence {
            amount: 3
        }));
        assert!(!self_cost_in_scope(&AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Library),
            filter: None,
        }));
        assert!(!self_cost_in_scope(&AbilityCost::Exile {
            count: 1,
            zone: None,
            filter: None,
        }));
        // A Composite of only out-of-scope costs stays out of scope.
        assert!(!self_cost_in_scope(&AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::CollectEvidence { amount: 2 },],
        }));
        // Selective, not blanket: a graveyard/hand exile IS in scope.
        assert!(self_cost_in_scope(&AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Graveyard),
            filter: None,
        }));
    }

    #[test]
    fn collect_evidence_cost_yields_na() {
        let mut state = GameState::new_two_player(42);
        let source = source_with(
            &mut state,
            "Evidence Card",
            &[CoreType::Creature],
            activated(gain_life(1), AbilityCost::CollectEvidence { amount: 3 }),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_value_na",
        );
    }

    // --- Row 9: Tyrite-Sanctum-class beneficial counter allowed (M2) ------

    #[test]
    fn beneficial_indestructible_counter_not_rejected() {
        // M2: real card Tyrite Sanctum parses this as PutCounter{Keyword(
        // Indestructible)} on a target God — a beneficial counter, non-trivial.
        let mut state = GameState::new_two_player(42);
        let effect = put_counter(
            CounterType::Keyword(KeywordKind::Indestructible),
            TargetFilter::Typed(TypedFilter::default()),
        );
        let cost = AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, sac_land_cost()],
        };
        land(&mut state, "Forest");
        let source = source_with(
            &mut state,
            "Tyrite Sanctum",
            &[CoreType::Land],
            activated(effect, cost),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    // --- Row 10: Carrion Feeder fizzle rejected, multi-authority guards ---

    #[test]
    fn self_counter_fizzles_when_source_is_only_sac_target() {
        // Sacrifice a creature: +1/+1 on itself. With only the source creature
        // on board, paying the cost removes the counter's only recipient →
        // trivial → reject.
        let mut state = GameState::new_two_player(42);
        let effect = put_counter(CounterType::Plus1Plus1, TargetFilter::SelfRef);
        let source = source_with(
            &mut state,
            "Carrion Feeder",
            &[CoreType::Creature],
            activated(effect, sac_creature_cost()),
        );
        // Make the source itself a creature that matches the sac filter.
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn self_counter_does_not_fizzle_with_other_fodder() {
        // Multi-authority: a separate token can be sacrificed instead, so the
        // +1/+1 counter lands → non-trivial → not rejected.
        let mut state = GameState::new_two_player(42);
        let effect = put_counter(CounterType::Plus1Plus1, TargetFilter::SelfRef);
        let source = source_with(
            &mut state,
            "Carrion Feeder",
            &[CoreType::Creature],
            activated(effect, sac_creature_cost()),
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        token_creature(&mut state, "Zombie Token", 1, 1);
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn counter_on_other_creature_does_not_fizzle() {
        // recipient != source: even with the source as the only sac target, a
        // counter aimed at a different creature filter is not a fizzle.
        let mut state = GameState::new_two_player(42);
        let effect = put_counter(
            CounterType::Plus1Plus1,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        );
        let source = source_with(
            &mut state,
            "Counter Sac",
            &[CoreType::Creature],
            activated(effect, sac_creature_cost()),
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    // --- Row 11: non-self-cost / OneOf-min untouched ----------------------

    #[test]
    fn tap_only_ability_yields_na() {
        let mut state = GameState::new_two_player(42);
        let source = source_with(
            &mut state,
            "Tapper",
            &[CoreType::Artifact],
            activated(gain_life(1), AbilityCost::Tap),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_value_na",
        );
    }

    #[test]
    fn one_of_min_picks_free_alternative_never_rejects() {
        // OneOf{ pay 3 life | {2} } — the cheapest branch is the mana cost (0),
        // so the priced self-cost is 0 and the gate never rejects.
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::OneOf {
            costs: vec![
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
                AbilityCost::Mana {
                    cost: engine::types::mana::ManaCost::generic(2),
                },
            ],
        };
        let source = source_with(
            &mut state,
            "Flexible",
            &[CoreType::Artifact],
            activated(gain_life(1), cost),
        );
        assert_not_reject(&verdict_for(&state, source, plain_features()));
    }

    // --- Marginal branch: cheap pay-life deprioritized, never vetoed ------

    #[test]
    fn cheap_pay_life_trivial_is_marginal() {
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 1 },
        };
        let source = source_with(
            &mut state,
            "Life Sink",
            &[CoreType::Artifact],
            activated(gain_life(1), cost),
        );
        let verdict = verdict_for(&state, source, plain_features());
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "self_cost_marginal");
                assert!(
                    delta < 0.0 && delta > -0.5,
                    "expected small nudge, got {delta}"
                );
            }
            PolicyVerdict::Reject { .. } => panic!("cheap pay-life must never be vetoed"),
        }
    }

    // --- MED-1: trivial self-costs in [0.5, 1.0) deprioritize, never neutral --

    #[test]
    fn pay_five_life_trivial_deprioritizes_not_neutral() {
        // RE-CHARTERED. This row was written for MED-1, which only had to prove
        // that pay 5 life (0.75 priced, inside the [0.5, 1.0) sub-veto range)
        // for a trivial 1 lifegain stopped resolving to
        // `self_cost_benefit_present` — a losing play mislabeled as a benefit.
        // It asserted `self_cost_marginal` because that was the strongest
        // verdict the priced floor could reach at 0.75.
        //
        // The materiality veto now reaches further on this exact input: the
        // chain is Trivial-proper (`gain_life(1)` is classified trivial by an
        // explicit arm, not left unmodeled) and 5 life is at or above
        // `self_cost_material_life`, so the correct verdict is `Reject`. The
        // MED-1 claim is strictly preserved — a reject is even further from
        // `self_cost_benefit_present` than the marginal score was — and the
        // assertion is strengthened rather than relaxed.
        //
        // A verdict of `self_cost_marginal` here now means the materiality veto
        // is gone; `self_cost_benefit_present` means MED-1's own fix regressed.
        // The name is kept so the MED-1 audit trail stays greppable — read it as
        // "not neutral", which is still exactly what the row certifies.
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 5 },
        };
        let source = source_with(
            &mut state,
            "Life Sink",
            &[CoreType::Artifact],
            activated(gain_life(1), cost),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_trivial_benefit");
        assert_trivial_facts(&verdict, 750);
    }

    // --- S19: pay-life materiality veto on Trivial-PROPER chains -----------

    #[test]
    fn pay_four_life_for_trivial_payoff_is_rejected() {
        // Adanto Vanguard, Oracle text confirmed against Scryfall: "Pay 4 life:
        // This creature gains indestructible until end of turn."
        //
        // The chain is Trivial-PROPER, not merely unmodeled: the grant matches
        // `is_self_protection_effect`, so the classifier's explicit
        // self-protection arm judges it — and with the AI active, an empty
        // stack, and full life there is no threat for indestructible to answer,
        // so that arm returns Trivial. (Same fixture conditions as
        // `discard_for_self_protection_no_threat_rejected`.)
        //
        // REVERT-FAILING: 4 life prices at 4 * 0.15 = 0.60, below
        // `REAL_COST_FLOOR`, so with the materiality veto removed this input
        // falls through to `self_cost_marginal` — a −0.60 rate on an ability
        // with no activation limit, which is the shape the module's law forbids.
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        let source = source_with(
            &mut state,
            "Adanto Vanguard",
            &[CoreType::Creature],
            activated(indestructible_self_grant(), pay_life_cost(4)),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_trivial_benefit");
        // Pinned so the row certifies the MATERIALITY ground and not the priced
        // one: 600 is well below `REAL_COST_FLOOR * 1000`, so a reject carrying
        // this fact cannot have come from `cost_value >= REAL_COST_FLOOR`.
        assert_trivial_facts(&verdict, 600);
    }

    #[test]
    fn pay_one_life_for_trivial_payoff_is_marginal_not_rejected() {
        // Hostile boundary for the row above: the identical chain at 1 life is
        // below `self_cost_material_life`, so the bound does not apply and the
        // graduated branch keeps it. Without this pair the veto could be
        // "reject every pay-life trivial activation" and still pass.
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        let source = source_with(
            &mut state,
            "Adanto Vanguard",
            &[CoreType::Creature],
            activated(indestructible_self_grant(), pay_life_cost(1)),
        );
        let verdict = verdict_for(&state, source, plain_features());
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "self_cost_marginal");
                assert!(delta < 0.0, "expected a deprioritizing nudge, got {delta}");
            }
            PolicyVerdict::Reject { .. } => {
                panic!("one life is below the materiality bound and must not hard-veto")
            }
        }
    }

    #[test]
    fn pay_life_with_priced_benefit_unchanged() {
        // The materiality bound belongs to the trivial branch alone. The same 4
        // life that is vetoed above, against a payoff the module CAN price,
        // still resolves through the net comparison: 0.60 cost vs a 1.0 draw is
        // not a loss, so the verdict is `covers_cost` — the facts pin both
        // sides, so a materiality check leaking into the `Priced` arm would show
        // up as a `Reject` here rather than as a silent re-band.
        let mut state = state_with_library();
        let source = source_with(
            &mut state,
            "Necropotence-ish",
            &[CoreType::Enchantment],
            activated(draw(1), pay_life_cost(4)),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_neutral(&verdict, "self_cost_benefit_covers_cost");
        assert_facts(&verdict, 600, 1000);
    }

    #[test]
    fn prowler_shape_unmodeled_pump_is_not_rejected() {
        // Desolation Prowler, Oracle text confirmed against Scryfall: "Pay 2
        // life: This creature gets +2/+2 until end of turn."
        //
        // 2 life IS material, so this row isolates the OTHER half of the gate:
        // `Effect::Pump` has no classifier arm, so the chain is
        // `Trivial { any_unmodeled: true }` and "no payoff" is an absence of
        // evidence, not a finding. A bound may not rest on that, so the veto
        // stands down and the graduated branch keeps the candidate. Whether a
        // pump is worth activating outside a combat window is a timing question
        // owned by the tactical gate, not a cost-materiality one.
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        let source = source_with(
            &mut state,
            "Desolation Prowler",
            &[CoreType::Creature],
            activated(pump_self(2, 2), pay_life_cost(2)),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_not_reject(&verdict);
        match verdict {
            PolicyVerdict::Score { reason, .. } => assert_eq!(reason.kind, "self_cost_marginal"),
            PolicyVerdict::Reject { .. } => unreachable!(),
        }
    }

    #[test]
    fn unmodeled_chain_still_rejects_on_the_priced_floor() {
        // The stand-down above is scoped to the MATERIALITY ground only — the
        // pre-existing priced veto still applies to an unmodeled chain. Same
        // unmodeled pump payoff, but discarding a card prices at 1.0, which is
        // at `REAL_COST_FLOOR`. Reverting the `!any_unmodeled` guard would leave
        // this green; weakening the priced arm to Trivial-proper chains only
        // would turn it red, which is the regression this row exists to catch.
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        create_object(
            &mut state,
            CardId(next_id()),
            AI,
            "Filler".to_string(),
            Zone::Hand,
        );
        let cost = AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            selection: Default::default(),
            self_scope: Default::default(),
        };
        let source = source_with(
            &mut state,
            "Pump Loop",
            &[CoreType::Creature],
            activated(pump_self(2, 2), cost),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_trivial_benefit");
        assert_trivial_facts(&verdict, 1000);
    }

    #[test]
    fn self_bounce_save_under_hostile_removal_is_not_rejected() {
        // Selenia, Dark Angel: "Pay 2 life: Return Selenia to its owner's hand."
        // A self-bounce is a SAVE (CR 400.7 — it comes back a new object), so
        // with opposing removal on the stack aimed at it the dodge is a real
        // payoff and the veto must stand down.
        //
        // REVERT-FAILING for the routing fix: `Bounce` matches the removal arm,
        // where `targetable_threat_value` prices the AI's own creature at 0 →
        // Trivial-proper, and 2 life is material, so without the self-scoped
        // save arm this is a `-inf` veto on the save.
        let mut state = GameState::new_two_player(42);
        state.active_player = OPP;
        let source = source_with(
            &mut state,
            "Selenia, Dark Angel",
            &[CoreType::Creature],
            activated(self_bounce(), pay_life_cost(2)),
        );
        stack_removal_targeting(&mut state, source);
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn self_bounce_with_no_threat_pays_material_life_is_rejected() {
        // Hostile boundary for the row above, and the reason the fix routes
        // through the threat gate rather than blanket-exempting self-bounces:
        // the identical ability with an empty stack, the AI active and full life
        // has nothing to dodge, so returning Selenia to hand for 2 life is the
        // repeatable no-payoff activation S19 exists to veto.
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        let source = source_with(
            &mut state,
            "Selenia, Dark Angel",
            &[CoreType::Creature],
            activated(self_bounce(), pay_life_cost(2)),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_trivial_benefit");
        // 300 is far below `REAL_COST_FLOOR * 1000`, so the reject provably
        // comes from the materiality ground, not the priced floor.
        assert_trivial_facts(&verdict, 300);
    }

    #[test]
    fn one_of_with_a_free_alternative_is_not_material() {
        // `cost_is_material` mirrors `real_self_cost`'s `OneOf` fold: the payer
        // picks, so the cost is material only when EVERY alternative is. This is
        // the same input as `one_of_min_picks_free_alternative_never_rejects`
        // read at the materiality seam, and it is what keeps that row green by
        // construction rather than by luck.
        let mut state = GameState::new_two_player(42);
        let penalties = crate::config::PolicyPenalties::default();
        // Any AI-controlled object serves as the quantity-resolution source; the
        // costs under test carry `Fixed` amounts.
        let source = land(&mut state, "Forest");

        let mixed = AbilityCost::OneOf {
            costs: vec![
                pay_life_cost(3),
                AbilityCost::Mana {
                    cost: engine::types::mana::ManaCost::generic(2),
                },
            ],
        };
        assert!(
            !cost_is_material(&state, AI, source, &mixed, &penalties),
            "a payable-with-mana alternative is not material"
        );

        // Both alternatives material → the payer has no escape, so the cost is.
        let both = AbilityCost::OneOf {
            costs: vec![pay_life_cost(3), pay_life_cost(2)],
        };
        assert!(
            cost_is_material(&state, AI, source, &both, &penalties),
            "every alternative material means the cost is material"
        );

        // Composite charges every leg, so one material leg is enough.
        let composite = AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, pay_life_cost(2)],
        };
        assert!(
            cost_is_material(&state, AI, source, &composite, &penalties),
            "a composite is material when any leg is"
        );
    }

    #[test]
    fn non_creature_token_sac_trivial_deprioritizes_not_neutral() {
        // MED-1: sacrifice a non-creature token (0.5 priced, the lower edge of the
        // [0.5, 1.0) range) for a trivial 1 lifegain must deprioritize, not resolve
        // to `self_cost_benefit_present`.
        let mut state = GameState::new_two_player(42);
        artifact_token(&mut state, "Treasure");
        // The source is an enchantment (not an artifact) so the sole artifact the
        // "sacrifice an artifact" cost can consume is the 0.5-priced token.
        let source = source_with(
            &mut state,
            "Token Sink",
            &[CoreType::Enchantment],
            activated(gain_life(1), sac_artifact_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(
                    reason.kind, "self_cost_marginal",
                    "must not be benefit_present"
                );
                assert!(delta < 0.0, "expected a deprioritizing nudge, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("0.5 priced cost must not hard-veto"),
        }
    }

    // --- MED-2: harmful mass counter with a worthwhile target is non-trivial --

    #[test]
    fn mass_harmful_counter_hitting_opponent_creature_not_rejected() {
        // MED-2: "Sacrifice a creature: put a -1/-1 counter on each creature" with a
        // worthwhile opponent creature present is real board interaction — it must
        // NOT be auto-classified trivial and hard-vetoed. Reverting the fix (the old
        // `counter_is_harmful(counter_type)` arm returns true → trivial) turns this
        // into a `self_cost_trivial_benefit` reject.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        creature(&mut state, OPP, "Ogre", 4, 4);
        let effect = put_counter_all(
            CounterType::Minus1Minus1,
            TargetFilter::Typed(TypedFilter::creature()),
        );
        let source = source_with(
            &mut state,
            "Mass Wither",
            &[CoreType::Artifact],
            activated(effect, sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn mass_harmful_counter_no_worthwhile_target_rejected() {
        // Hostile boundary for MED-2: the same mass -1/-1 with no opponent creature
        // on board has no worthwhile board impact → trivial → reject. This pairs
        // with the positive row above so neither is a vacuous assertion.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let effect = put_counter_all(
            CounterType::Minus1Minus1,
            TargetFilter::Typed(TypedFilter::creature()),
        );
        let source = source_with(
            &mut state,
            "Mass Wither",
            &[CoreType::Artifact],
            activated(effect, sac_creature_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    // --- Row 6 threat waiver: self-protection under threat allowed --------

    #[test]
    fn discard_for_self_protection_allowed_under_threat() {
        let mut state = GameState::new_two_player(42);
        state.active_player = OPP;
        let cost = AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            selection: Default::default(),
            self_scope: Default::default(),
        };
        let source = source_with(
            &mut state,
            "Loopy Creature",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        // Opponent removal on the stack targeting the creature that receives
        // shroud makes the protection grant a live payoff.
        stack_removal_targeting(&mut state, source);
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    // --- Parsed-Oracle reach-guards (production parser AST) ----------------

    #[test]
    fn parsed_zuran_orb_rejected() {
        use engine::parser::oracle::parse_oracle_text;

        let mut state = GameState::new_two_player(42);
        land(&mut state, "Forest");
        let parsed = parse_oracle_text(
            "Sacrifice a land: You gain 2 life.",
            "Zuran Orb",
            &[],
            &["Artifact".to_string()],
            &[],
        );
        let ability = parsed
            .abilities
            .into_iter()
            .next()
            .expect("one activated ability");
        let source = create_object(
            &mut state,
            CardId(next_id()),
            AI,
            "Zuran Orb".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            *Arc::make_mut(&mut obj.abilities) = vec![ability];
        }
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn parsed_ashnods_altar_not_rejected() {
        use engine::parser::oracle::parse_oracle_text;

        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let parsed = parse_oracle_text(
            "Sacrifice a creature: Add {C}{C}.",
            "Ashnod's Altar",
            &[],
            &["Artifact".to_string()],
            &[],
        );
        let ability = parsed
            .abilities
            .into_iter()
            .next()
            .expect("one activated ability");
        let source = create_object(
            &mut state,
            CardId(next_id()),
            AI,
            "Ashnod's Altar".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            *Arc::make_mut(&mut obj.abilities) = vec![ability];
        }
        assert_not_reject(&verdict_for(&state, source, plain_features()));
    }

    #[test]
    fn parsed_tyrite_sanctum_indestructible_counter_not_rejected() {
        // LOW: production-parser reach guard for the M2 beneficial-counter path.
        // Tyrite Sanctum's third ability parses as a Composite{Mana, Tap,
        // Sacrifice(SelfRef)} cost with a PutCounter{indestructible} payoff on a
        // target God — a beneficial counter, so the self-cost activation must NOT
        // be vetoed even though the sacrificed land prices at 4.0. Guards the M2
        // classification against future parser AST changes.
        use engine::parser::oracle::parse_oracle_text;

        let mut state = GameState::new_two_player(42);
        let parsed = parse_oracle_text(
            "{T}: Add {C}.\n{2}, {T}: Target legendary creature becomes a God in addition to its other types. Put a +1/+1 counter on it.\n{4}, {T}, Sacrifice this land: Put an indestructible counter on target God.",
            "Tyrite Sanctum",
            &[],
            &["Land".to_string()],
            &[],
        );
        let ability = parsed
            .abilities
            .into_iter()
            .find(|a| a.cost.as_ref().is_some_and(self_cost_in_scope))
            .expect("the sacrifice-this-land activation");
        let source = create_object(
            &mut state,
            CardId(next_id()),
            AI,
            "Tyrite Sanctum".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            *Arc::make_mut(&mut obj.abilities) = vec![ability];
        }
        assert_not_reject(&verdict_for(&state, source, plain_features()));
    }

    #[test]
    fn self_counter_replenishment_preview_outcomes_only_penalize_prevention() {
        let applied = |_state: &mut GameState| {};
        let transformed = |state: &mut GameState| {
            install_counter_replacement(state, QuantityModification::DOUBLE);
        };
        let prevented = |state: &mut GameState| {
            install_counter_replacement(state, QuantityModification::Prevent);
        };
        let choice_required = |state: &mut GameState| {
            install_counter_replacement(state, QuantityModification::DOUBLE);
            install_counter_replacement(state, QuantityModification::Plus { value: 1 });
        };

        for (install, expected_preview, expected_reason, penalized) in [
            (
                applied as fn(&mut GameState),
                SelfCounterCostPreview::Applied,
                "self_cost_value_na",
                false,
            ),
            (
                transformed as fn(&mut GameState),
                SelfCounterCostPreview::Transformed,
                "self_cost_value_na",
                false,
            ),
            (
                prevented as fn(&mut GameState),
                SelfCounterCostPreview::Prevented,
                "self_cost_counter_replacement_prevented",
                true,
            ),
            (
                choice_required as fn(&mut GameState),
                SelfCounterCostPreview::ChoiceRequired,
                "self_cost_value_na",
                false,
            ),
        ] {
            let mut state = GameState::new_two_player(42);
            install(&mut state);
            let source = source_with(
                &mut state,
                "Counter Replenisher",
                &[CoreType::Creature],
                self_counter_replenisher(),
            );
            state
                .objects
                .get_mut(&source)
                .expect("source exists")
                .counters
                .insert(CounterType::Plus1Plus1, 1);

            let ability = state.objects[&source]
                .abilities
                .first()
                .expect("counter replenisher ability");
            assert_eq!(
                self_counter_cost_preview(&state, AI, source, ability),
                Some(expected_preview),
                "replacement preview must reach the expected outcome"
            );

            let result = verdict_for(&state, source, plain_features());
            if penalized {
                assert!(matches!(
                    result,
                    PolicyVerdict::Score { delta, reason }
                        if delta < 0.0 && reason.kind == expected_reason
                ));
            } else {
                assert_neutral(&result, expected_reason);
            }
        }
    }

    #[test]
    fn self_counter_replenishment_preview_accepts_single_cost_composite() {
        let mut state = GameState::new_two_player(42);
        let mut ability = self_counter_replenisher();
        let remove_counter = ability.cost.take().expect("counter payment");
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![remove_counter],
        });
        let source = source_with(
            &mut state,
            "Composite Counter Replenisher",
            &[CoreType::Creature],
            ability,
        );
        state
            .objects
            .get_mut(&source)
            .expect("source exists")
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let ability = state.objects[&source]
            .abilities
            .first()
            .expect("counter replenisher ability");
        assert_eq!(
            self_counter_cost_preview(&state, AI, source, ability),
            Some(SelfCounterCostPreview::Applied)
        );
    }

    #[test]
    fn self_counter_replenishment_preview_rejects_multi_cost_composite() {
        let mut state = GameState::new_two_player(42);
        let mut ability = self_counter_replenisher();
        let remove_counter = ability.cost.take().expect("counter payment");
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, remove_counter],
        });
        let source = source_with(
            &mut state,
            "Composite Counter Replenisher",
            &[CoreType::Creature],
            ability,
        );
        state
            .objects
            .get_mut(&source)
            .expect("source exists")
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let ability = state.objects[&source]
            .abilities
            .first()
            .expect("counter replenisher ability");
        assert_eq!(self_counter_cost_preview(&state, AI, source, ability), None);
    }

    #[test]
    fn self_counter_rewritten_preview_is_conservatively_deprioritized() {
        let config = AiConfig::default();

        assert!(matches!(
            counter_replenishment_verdict(
                Some(SelfCounterCostPreview::Unsupported),
                &config.policy_penalties,
            ),
            Some(PolicyVerdict::Score { delta, reason })
                if delta < 0.0 && reason.kind == "self_cost_counter_replacement_unsupported"
        ));
    }

    // --- The priced comparison: cost vs benefit ---------------------------
    //
    // Every test below reaches `BenefitAppraisal` through the real
    // `SelfCostValuePolicy::verdict` entry point via `verdict_for`, past the
    // scope gate and the synergy stand-down.

    /// Chain an extra effect onto an ability as its `sub_ability`, so
    /// `collect_chain_effects` yields both in order.
    fn with_rider(mut ability: AbilityDefinition, rider: Effect) -> AbilityDefinition {
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            rider,
        )));
        ability
    }

    fn with_riders(mut ability: AbilityDefinition, riders: Vec<Effect>) -> AbilityDefinition {
        let mut current = &mut ability;
        for rider in riders {
            current.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Activated,
                rider,
            )));
            current = current
                .sub_ability
                .as_deref_mut()
                .expect("rider was just attached");
        }
        ability
    }

    fn draw_to(count: i32, target: TargetFilter) -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: count },
            target,
        }
    }

    fn parent_discard(count: QuantityExpr) -> Effect {
        Effect::Discard {
            count,
            target: TargetFilter::ParentTarget,
            selection: CardSelectionMode::Chosen,
            unless_filter: None,
            filter: None,
        }
    }

    fn discard_cost(count: i32) -> AbilityCost {
        AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: count },
            filter: None,
            selection: CardSelectionMode::Chosen,
            self_scope: DiscardSelfScope::FromHand,
        }
    }

    fn sacrifice_self_cost() -> AbilityCost {
        AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1))
    }

    fn add_hand_cards(state: &mut GameState, player: PlayerId, count: usize) {
        for index in 0..count {
            create_object(
                state,
                CardId(next_id()),
                player,
                format!("Hand Card {index}"),
                Zone::Hand,
            );
        }
    }

    fn opponent_player_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
    }

    #[test]
    fn cephalid_coliseum_shape_rejects_trivial_opponent_churn() {
        let mut state = state_with_library();
        let source = source_with(
            &mut state,
            "Cephalid Coliseum",
            &[CoreType::Land],
            with_rider(
                activated(draw_to(3, TargetFilter::Player), sacrifice_self_cost()),
                parent_discard(QuantityExpr::Fixed { value: 3 }),
            ),
        );

        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_trivial_benefit");
        assert_trivial_facts(&verdict, 4500);
    }

    #[test]
    fn ai_draw_then_parent_discard_prices_the_drawback() {
        let mut state = state_with_library();
        add_hand_cards(&mut state, AI, 2);
        let source = source_with(
            &mut state,
            "Drawback Fixture",
            &[CoreType::Enchantment],
            with_rider(
                activated(draw_to(3, TargetFilter::Player), discard_cost(2)),
                parent_discard(QuantityExpr::Fixed { value: 2 }),
            ),
        );

        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 2000, 1000);
    }

    /// T15: delivery of all three cards makes the ordered mandatory discard cap
    /// two cards from a formerly empty hand: `3 - 2 = 1` benefit against a 2.0
    /// discard cost.
    #[test]
    fn full_ai_draw_delivery_feeds_parent_discard_cap() {
        let mut state = state_with_library();
        let source = source_with(
            &mut state,
            "Full Draw Delivery Cap",
            &[CoreType::Enchantment],
            with_rider(
                activated(draw_to(3, TargetFilter::Player), discard_cost(2)),
                parent_discard(QuantityExpr::Fixed { value: 2 }),
            ),
        );

        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 2000, 1000);
    }

    /// T16: CR 121.2b leaves only one delivered card under this draw limit, so
    /// the following mandatory discard can only discard that one card and the
    /// exact ordered benefit is zero, not the requested-three arithmetic.
    #[test]
    fn partial_ai_draw_delivery_feeds_parent_discard_cap() {
        let mut state = state_with_library();
        add_draw_restricting_static(
            &mut state,
            StaticMode::PerTurnDrawLimit {
                who: ProhibitionScope::AllPlayers,
                max: 1,
            },
        );
        let source = source_with(
            &mut state,
            "Partial Draw Delivery Cap",
            &[CoreType::Enchantment],
            with_rider(
                activated(draw_to(3, TargetFilter::Player), discard_cost(2)),
                parent_discard(QuantityExpr::Fixed { value: 2 }),
            ),
        );

        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 2000, 0);
    }

    /// T17a: a mandatory CR 614.6 prevention is a certified exact-zero draw,
    /// so it is priced as zero rather than treated as an unresolved branch.
    #[test]
    fn prevented_ai_draw_is_exact_zero_for_parent_discard_cap() {
        let mut state = state_with_library();
        install_ai_draw_replacement(&mut state, mandatory_draw_prevent());
        let source = source_with(
            &mut state,
            "Prevented Draw Delivery Cap",
            &[CoreType::Enchantment],
            with_rider(
                activated(draw_to(3, TargetFilter::Player), discard_cost(2)),
                parent_discard(QuantityExpr::Fixed { value: 2 }),
            ),
        );

        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 2000, 0);
    }

    /// T17b: the mandatory replacement is not enough to price its search
    /// continuation. The engine reaches `SearchChoice` (covered at the engine
    /// seam), so the policy must fail open as an unpriced benefit.
    #[test]
    fn mandatory_search_substitute_stands_down_parent_discard_cap() {
        let mut state = state_with_library();
        add_library_land(&mut state);
        install_ai_draw_replacement(&mut state, mandatory_search_draw_substitute());
        let source = source_with(
            &mut state,
            "Search Substitute Delivery Cap",
            &[CoreType::Enchantment],
            with_rider(
                activated(draw_to(3, TargetFilter::Player), discard_cost(2)),
                parent_discard(QuantityExpr::Fixed { value: 2 }),
            ),
        );

        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    /// T18: CR 614.11 offers the optional replacement even with no library
    /// cards. Its choice is unknown rather than an empty-library zero, so the
    /// benefit comparison remains neutral.
    #[test]
    fn empty_library_optional_draw_replacement_stands_down_parent_discard_cap() {
        let mut state = GameState::new_two_player(42);
        install_ai_draw_replacement(&mut state, optional_draw_prevent());
        let source = source_with(
            &mut state,
            "Empty Library Optional Replacement Cap",
            &[CoreType::Enchantment],
            with_rider(
                activated(draw_to(3, TargetFilter::Player), discard_cost(2)),
                parent_discard(QuantityExpr::Fixed { value: 2 }),
            ),
        );

        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn limestone_golem_shape_prices_a_player_draw_for_the_ai() {
        let mut state = state_with_library();
        let source = source_with(
            &mut state,
            "Limestone Golem",
            &[CoreType::Creature],
            activated(draw_to(1, TargetFilter::Player), sacrifice_self_cost()),
        );
        let object = state.objects.get_mut(&source).expect("source exists");
        object.power = Some(3);
        object.toughness = Some(4);

        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 8500, 1000);
    }

    #[test]
    fn opponent_only_draw_is_trivial() {
        let mut state = state_with_library();
        let source = source_with(
            &mut state,
            "Opponent Draw Fixture",
            &[CoreType::Creature],
            activated(draw_to(2, opponent_player_filter()), sacrifice_self_cost()),
        );
        let object = state.objects.get_mut(&source).expect("source exists");
        object.power = Some(3);
        object.toughness = Some(4);

        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_trivial_benefit");
        assert_trivial_facts(&verdict, 8500);
    }

    #[test]
    fn each_player_draw_stands_down_as_mixed() {
        let mut state = state_with_library();
        let source = source_with(
            &mut state,
            "Each Player Draw Fixture",
            &[CoreType::Land],
            activated(draw_to(1, TargetFilter::Any), sacrifice_self_cost()),
        );

        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn optional_parent_discard_is_not_charged() {
        let mut state = state_with_library();
        add_hand_cards(&mut state, AI, 2);
        let source = source_with(
            &mut state,
            "Optional Drawback Fixture",
            &[CoreType::Enchantment],
            with_rider(
                activated(draw_to(2, TargetFilter::Player), discard_cost(2)),
                parent_discard(QuantityExpr::up_to(QuantityExpr::Fixed { value: 2 })),
            ),
        );

        let verdict = verdict_for(&state, source, plain_features());
        assert_neutral(&verdict, "self_cost_benefit_covers_cost");
        assert_facts(&verdict, 2000, 2000);
    }

    #[test]
    fn survey_mechan_damage_rider_remains_unpriced() {
        let mut state = state_with_library();
        let source = source_with(
            &mut state,
            "Survey Mechan",
            &[CoreType::Land],
            with_rider(
                activated(deal_fixed(3), sacrifice_self_cost()),
                draw_to(3, TargetFilter::Player),
            ),
        );

        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn opponent_discard_beyond_draw_churn_stands_down() {
        let mut state = state_with_library();
        add_hand_cards(&mut state, AI, 3);
        let source = source_with(
            &mut state,
            "Bounded Opponent Churn",
            &[CoreType::Land],
            with_rider(
                activated(draw_to(1, TargetFilter::Player), sacrifice_self_cost()),
                parent_discard(QuantityExpr::Fixed { value: 3 }),
            ),
        );

        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn mandatory_parent_discard_caps_to_available_hand_cards() {
        for (hand_cards, benefit_milli) in [(3, -3000), (0, 0)] {
            let mut state = state_with_library();
            add_hand_cards(&mut state, AI, hand_cards);
            add_draw_restricting_static(
                &mut state,
                StaticMode::CantDraw {
                    who: ProhibitionScope::AllPlayers,
                },
            );
            let source = source_with(
                &mut state,
                "Mandatory Drawback Cap",
                &[CoreType::Land],
                with_rider(
                    activated(draw_to(4, TargetFilter::Player), sacrifice_self_cost()),
                    parent_discard(QuantityExpr::Fixed { value: 3 }),
                ),
            );

            let verdict = verdict_for(&state, source, plain_features());
            assert_reject(&verdict, "self_cost_benefit_underwater");
            assert_facts(&verdict, 4500, benefit_milli);
        }
    }

    #[test]
    fn controller_wheel_discard_remains_unmodeled() {
        let mut state = state_with_library();
        let wheel_discard = Effect::Discard {
            count: QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::Controller,
                },
            },
            target: TargetFilter::Controller,
            selection: CardSelectionMode::Chosen,
            unless_filter: None,
            filter: None,
        };
        let source = source_with(
            &mut state,
            "Wheel Fixture",
            &[CoreType::Land],
            with_rider(
                activated(wheel_discard, sacrifice_self_cost()),
                draw_to(7, TargetFilter::Controller),
            ),
        );

        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn conflicting_player_target_choices_stand_down_as_mixed() {
        let mut state = state_with_library();
        let source = source_with(
            &mut state,
            "Conflicting Player Targets",
            &[CoreType::Land],
            with_riders(
                activated(draw_to(1, TargetFilter::Player), sacrifice_self_cost()),
                vec![
                    Effect::ExtraTurn {
                        target: TargetFilter::Player,
                    },
                    Effect::LoseLife {
                        amount: QuantityExpr::Fixed { value: 20 },
                        target: Some(TargetFilter::Controller),
                    },
                ],
            ),
        );

        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn opponent_draw_churn_is_consumed_once_per_parent_discard() {
        let mut state = state_with_library();
        let ability = with_riders(
            activated(draw_to(1, TargetFilter::Player), sacrifice_self_cost()),
            vec![
                parent_discard(QuantityExpr::Fixed { value: 1 }),
                parent_discard(QuantityExpr::Fixed { value: 1 }),
            ],
        );
        let source = source_with(
            &mut state,
            "Churn Saturation Fixture",
            &[CoreType::Land],
            ability,
        );
        let ability = state.objects[&source].abilities[0].clone();

        assert_eq!(
            chain_effect_trivialities(&state, AI, source, &ability),
            vec![
                EffectTriviality::Trivial,
                EffectTriviality::Trivial,
                EffectTriviality::NonTrivial,
            ],
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn non_player_root_keeps_parent_target_mixed() {
        let mut state = state_with_library();
        let source = source_with(
            &mut state,
            "Non-Player Parent Target",
            &[CoreType::Land],
            with_rider(
                activated(draw_to(3, opponent_player_filter()), sacrifice_self_cost()),
                parent_discard(QuantityExpr::Fixed { value: 1 }),
            ),
        );

        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    /// A token-creation rider: no `effect_triviality` arm models `Effect::Token`,
    /// so it classifies `Unmodeled`. Benefit-signed.
    fn create_token_rider() -> Effect {
        Effect::Token {
            name: "Servo".to_string(),
            power: engine::types::ability::PtValue::Fixed(1),
            toughness: engine::types::ability::PtValue::Fixed(1),
            types: vec!["Artifact".to_string(), "Creature".to_string()],
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        }
    }

    /// A life-loss rider: also `Unmodeled`, but drawback-signed. The pair proves
    /// the stand-down is direction-independent.
    fn lose_life_rider() -> Effect {
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: None,
        }
    }

    #[test]
    fn noncreature_token_sac_for_draw_covers_cost() {
        // A Clue/Food/Treasure-class crack: the artifact token prices at
        // `sacrifice_token_cost` = 0.5 against draw(1) = 1.0 → net +0.5, so the
        // comparison must NOT deprioritize it. Source is an enchantment so it
        // cannot itself join the artifact cheapest-match pool. The draw is
        // DELIVERABLE (seeded library, no suppressor), which is what makes the
        // 1.0 real — see `thief_suppressed_draw_is_vetoed_underwater` for the
        // same board with the payoff removed by an opponent.
        let mut state = state_with_library();
        artifact_token(&mut state, "Clue");
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            activated(draw(1), sac_artifact_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_neutral(&verdict, "self_cost_benefit_covers_cost");
        // Facts pinned so this row is an EXACT negative control for
        // `thief_suppressed_draw_is_vetoed_underwater`: same 500 cost, and the
        // 1000 benefit that the thief takes away.
        assert_facts(&verdict, 500, 1000);
    }

    #[test]
    fn draw_quantity_scales_the_comparison() {
        // Same 1/1 fodder (2.5) as the underwater token case, but drawing THREE
        // cards (3.0) clears it. The quantity must be read, not assumed to be 1
        // — an implementation hardcoding SINGLE_CARD_VALUE reports `underwater`.
        let mut state = state_with_library();
        creature(&mut state, AI, "Squire", 1, 1);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(3), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_covers_cost",
        );
    }

    #[test]
    fn pricing_uses_cheapest_matching_fodder() {
        // MULTI-AUTHORITY hostile fixture for the identity contract: two legal
        // sacrifices are on board, a 1/1 token (2.5) and a Bear (5.0). draw(3)
        // = 3.0 covers the CHEAPEST but not the dearest, so a binding that
        // priced anything other than the cheapest live match reports
        // `underwater` here.
        let mut state = state_with_library();
        token_creature(&mut state, "Goblin Token", 1, 1);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(3), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_covers_cost",
        );
    }

    /// The filtered-sacrifice leaf is reached through `real_self_cost` and the
    /// `SelfCostValuePolicy` adapter. A draw is worth 1.0: an ordinary 4/4
    /// leaves a -9.0 margin, while the owned commander cast once costs 16.0 and
    /// leaves -15.0. Both are categorical rejects; the exact margins prevent a
    /// threshold change from hiding behind that shared verdict shape.
    #[test]
    fn commander_sacrifice_cost_widens_the_self_cost_veto_margin() {
        let config = AiConfig::default();

        let mut commander_state = state_with_library();
        let commander = owned_commander_creature(&mut commander_state, "Commander", 4, 4, 1);
        let commander_source = source_with(
            &mut commander_state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        let commander_ability = commander_state.objects[&commander_source]
            .abilities
            .first()
            .expect("fixture source has its sacrifice ability");
        let commander_cost = real_self_cost(
            &commander_state,
            AI,
            commander_source,
            commander_ability
                .cost
                .as_ref()
                .expect("fixture ability has a cost"),
            &config.policy_penalties,
        );

        assert_eq!(
            commander_cost, 16.0,
            "reach guard: the commander must be the sole matching sacrifice and carry its 6.0 premium"
        );
        assert_eq!(1.0 - commander_cost, -15.0);
        let commander_verdict = verdict_for(&commander_state, commander_source, plain_features());
        assert_reject(&commander_verdict, "self_cost_benefit_underwater");
        assert_facts(&commander_verdict, 16000, 1000);

        let mut ordinary_state = state_with_library();
        ordinary_state.format_config.command_zone = true;
        creature(&mut ordinary_state, AI, "Bear", 4, 4);
        let ordinary_source = source_with(
            &mut ordinary_state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        let ordinary_ability = ordinary_state.objects[&ordinary_source]
            .abilities
            .first()
            .expect("fixture source has its sacrifice ability");
        let ordinary_cost = real_self_cost(
            &ordinary_state,
            AI,
            ordinary_source,
            ordinary_ability
                .cost
                .as_ref()
                .expect("fixture ability has a cost"),
            &config.policy_penalties,
        );

        assert_eq!(
            ordinary_cost, 10.0,
            "commander-free control: command-zone format alone must not uplift an ordinary 4/4"
        );
        assert_eq!(1.0 - ordinary_cost, -9.0);
        let ordinary_verdict = verdict_for(&ordinary_state, ordinary_source, plain_features());
        assert_reject(&ordinary_verdict, "self_cost_benefit_underwater");
        assert_facts(&ordinary_verdict, 10000, 1000);

        assert_eq!(
            commander_cost - ordinary_cost,
            6.0,
            "the self-cost leaf reaches the premium exactly once"
        );
        assert_ne!(
            commander, commander_source,
            "the source must not join the creature fodder pool"
        );
    }

    /// At six prior command-zone casts, the same 4/4's 16.0 repurchase premium
    /// makes its sacrifice cost 26.0. Against draw(1), the verdict remains a
    /// reject with a -25.0 margin; the finite price is intentionally observed at
    /// the self-cost veto seam rather than assumed from the helper alone.
    #[test]
    fn high_tax_commander_reaches_the_self_cost_veto_with_its_live_margin() {
        let config = AiConfig::default();
        let mut state = state_with_library();
        let commander = owned_commander_creature(&mut state, "Commander", 4, 4, 6);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        assert_eq!(
            state.commander_cast_count.get(&commander),
            Some(&6),
            "reach guard: the high-tax fixture must record exactly six prior command-zone casts"
        );
        let ability = state.objects[&source]
            .abilities
            .first()
            .expect("fixture source has its sacrifice ability");
        let cost = real_self_cost(
            &state,
            AI,
            source,
            ability.cost.as_ref().expect("fixture ability has a cost"),
            &config.policy_penalties,
        );

        assert_eq!(cost, 26.0, "10.0 board value + 4.0 mana value + 12.0 tax");
        assert_eq!(1.0 - cost, -25.0);
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 26000, 1000);
        assert!(
            state.format_config.command_zone && state.objects[&commander].is_commander,
            "reach guard: this high-tax fixture must be an owned commander in a command-zone format"
        );
    }

    #[test]
    fn large_lifegain_is_vetoed_underwater() {
        // The second pricing arm. gain_life(10) exceeds TRIVIAL_LIFEGAIN_CEILING
        // so it classifies NON-trivial, then prices at 10 *
        // self_cost_pay_life_per_point (0.15) = 1.5 against the Bear's 5.0 →
        // net -3.5, certified losing. Pricing lifegain on the same per-point
        // axis the cost side already uses is what makes this comparable at all.
        //
        // HISTORY: graduated delta -3.5 before the veto.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(gain_life(10), sac_creature_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 5000, 1500);
    }

    #[test]
    fn large_lifegain_unpriced_under_life_pressure() {
        // NEGATIVE SIBLING of the row above: identical fixture, AI life dropped
        // to 4 so `ai_life_critical` holds. Life is then genuinely worth more
        // than the per-point axis can bound, so the pricing arm declines and the
        // comparison stands down to the pre-existing neutral — today's exact
        // behaviour, preserved. An implementation that priced lifegain
        // unconditionally reports `underwater` here.
        let mut state = GameState::new_two_player(42);
        state.players[AI.0 as usize].life = 4;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(gain_life(10), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn mixed_chain_with_unpriceable_effect_stands_down() {
        // Aggregation guard: `Effect::Mana` is classifier-NON-trivial but has no
        // confident price, so one unpriceable member must suppress the whole
        // comparison rather than let a partial sum (draw 1.0 vs Bear 5.0) go
        // underwater. Reaches the `None => Unpriced` early return.
        //
        // The library is seeded so the named partial sum (draw 1.0) is the real
        // one: the stand-down must be caused by the unpriceable member, not by a
        // payoff that was worth nothing anyway.
        let mut state = state_with_library();
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            with_rider(activated(draw(1), sac_creature_cost()), add_two_colorless()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn unmodeled_benefit_rider_stands_down_the_comparison() {
        // UNMODELED rider, BENEFIT direction. A rider-blind implementation
        // prices this chain at draw(1) = 1.0 against the Bear's 5.0 and reports
        // `underwater` with the token silently valued at 0 — understating the
        // payoff. The sum is not a lower bound, so no conclusion is drawn.
        // Library seeded so the "prices this chain at draw(1) = 1.0" story is
        // literally true of this fixture.
        let mut state = state_with_library();
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            with_rider(
                activated(draw(1), sac_creature_cost()),
                create_token_rider(),
            ),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn unmodeled_drawback_rider_blocks_covers_conclusion() {
        // UNMODELED rider, DRAWBACK direction — the paired sibling. Cheap
        // artifact-token fodder (0.5) vs draw(1) = 1.0, so the partial sum
        // "covers"; but the chain also loses 2 life, which the sum omits. An
        // implementation that helpfully concluded `covers_cost` from a partial
        // sum fails on the reason kind here. Together these two rows pin that
        // the stand-down never consults the net's sign.
        //
        // Library seeded: the "partial sum covers" half of that discrimination
        // story is only true when the draw is DELIVERABLE. On the bare
        // constructor's empty library the partial sum is 0.0 vs 0.5 —
        // underwater — and this row would keep passing while telling a false
        // story about why.
        let mut state = state_with_library();
        artifact_token(&mut state, "Clue");
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            with_rider(activated(draw(1), sac_artifact_cost()), lose_life_rider()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn underwater_veto_is_categorical_at_any_depth() {
        // DEPTH-INVARIANCE discriminator (replaces
        // `underwater_delta_routes_through_the_band_rescale`, whose whole
        // discrimination — WHERE in the critical band a shortfall lands — died
        // with the graduated arm). Sole fodder is an 8/8 non-token, untapped,
        // keywordless: cost = creature_combat_value(8,8) = 1.5*8 + 8 = 20.0;
        // benefit = 1.0; net = -19.0.
        //
        // The source MUST be a non-creature (the High Market land pattern):
        // `sac_creature_cost()` is Typed(creature, You) with no `Another`
        // property, so a creature-typed source would join the cheapest-match
        // pool and silently break the 20.0 arithmetic.
        //
        // What this now pins: a -19.0 shortfall and the -4.0 shortfall of
        // `sac_creature_for_draw_is_vetoed_underwater` share ONE categorical
        // fate. Any implementation that re-introduces depth sensitivity — a band
        // rescale, a clamp, a "only veto past N" threshold — produces a `Score`
        // here and goes red on shape. The facts still carry the depth, so the
        // magnitude remains observable without being actionable.
        let mut state = state_with_library();
        creature(&mut state, AI, "Colossus", 8, 8);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 20000, 1000);
    }

    #[test]
    fn tapped_fodder_still_prices_at_full_body_value() {
        // THE TAPPED-INHERITANCE DISCRIMINATOR. Sole fodder is a 1/1 creature
        // token that is TAPPED; source is a non-creature so it cannot join the
        // cheapest-match pool.
        //
        // Correct pricing: `max(evaluate_creature_intrinsic(1,1) = 2.5,
        // sacrifice_token_cost = 0.5) = 2.5` against draw(1) = 1.0 → net -1.5 →
        // vetoed.
        //
        // Revert image (this is the exact defect the unit exists to close):
        // routing `sacrifice_cost` back through `evaluate_creature` prices the
        // tapped token at `max(2.5 - 1.5, 0.5) = 1.0` against 1.0 → net exactly
        // 0 → `self_cost_benefit_covers_cost`. The test then goes red on BOTH
        // the verdict shape and the reason kind. That `covers_cost` boundary is
        // the escape hatch a five-body board measurably drained through the
        // moment its tokens attacked and tapped.
        let mut state = state_with_library();
        let fodder = token_creature(&mut state, "Goblin Token", 1, 1);
        state.objects.get_mut(&fodder).unwrap().tapped = true;
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        // Reach guard: the fixture is genuinely tapped, or it proves nothing
        // about tap inheritance.
        assert!(state.objects[&fodder].tapped);

        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 2500, 1000);
    }

    #[test]
    fn zero_power_token_crack_covers_at_the_boundary() {
        // The INCLUSIVE boundary's positive pin, replacing the tapped-1/1
        // example the boundary comment used to carry (that example is now false
        // — a tapped 1/1 is underwater). A 0/1 creature token prices at
        // `max(creature_combat_value(0,1) = 0*1.5 + 1 = 1.0, 0.5) = 1.0` against
        // draw(1) = 1.0 → net exactly 0.
        //
        // Revert image: an EXCLUSIVE boundary (`net > 0.0`) vetoes this
        // exact-cover crack and the test goes red on shape — the veto-overreach
        // direction, paired against `tapped_fodder_still_prices_at_full_body_value`
        // one arm over.
        let mut state = state_with_library();
        token_creature(&mut state, "Wall Token", 0, 1);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_neutral(&verdict, "self_cost_benefit_covers_cost");
        assert_facts(&verdict, 1000, 1000);
    }

    #[test]
    fn one_of_free_branch_still_covers() {
        // `OneOf` takes the payer's cheapest branch: the mana leg is out of
        // scope and prices 0, so the priced self-cost is 0 and draw(1) covers.
        // The comparison must not resurrect a cost the payer would never choose.
        //
        // Library seeded so the covering side is a real 1.0 against 0.0. Without
        // it this row would read 0.0 vs 0.0 and still pass — the free-branch
        // claim would be certified by "nothing for nothing", which is the
        // degenerate world, not the one under test.
        let mut state = state_with_library();
        let cost = AbilityCost::OneOf {
            costs: vec![
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
                AbilityCost::Mana {
                    cost: engine::types::mana::ManaCost::generic(2),
                },
            ],
        };
        let source = source_with(
            &mut state,
            "Flexible",
            &[CoreType::Artifact],
            activated(draw(1), cost),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_neutral(&verdict, "self_cost_benefit_covers_cost");
        // Facts pinned so this row is an EXACT test of the free branch, and so
        // the library seeding above cannot silently disarm it.
        //
        // The mutant this row exists to catch is `real_self_cost`'s `OneOf` arm
        // folding with `max` or `sum` instead of `min` (`self_cost.rs`). Under
        // that mutant the PayLife branch prices 3 * 0.15 = 0.45, which sits
        // BELOW `SINGLE_CARD_VALUE`, so against the seeded 1.0 benefit the net
        // is +0.55 and the verdict is still `covers_cost` — a kind-only
        // assertion stays green and the mutant escapes. Pinning
        // `cost_milli == 0` catches it, and is strictly better than raising the
        // PayLife amount because no `max`/`sum` fold can produce a zero.
        //
        // GENERAL RULE for anyone adding a row here: raising the benefit side
        // (e.g. by seeding a library) moves a row TOWARD `covers_cost`, so it is
        // safe only for rows that pin facts, or that decide before any price is
        // consulted (`BenefitAppraisal::Unpriced`) — a row asserting
        // `covers_cost` on kind alone is moved toward its own assertion, i.e.
        // away from failure, and silently stops discriminating.
        assert_facts(&verdict, 0, 1000);
    }

    #[test]
    fn token_cost_default_stays_below_single_card_value() {
        // The invariant that keeps Clue/Food/Treasure cracking profitable under
        // the comparison. STATED LIMITATION: this pins the shipped DEFAULT only.
        // `sacrifice_token_cost` is a CMA-ES-tuned parameter; a retrain that
        // pushed it to or above 1.0 would flip non-creature token cracking to
        // `underwater` without failing any test but this one.
        //
        // That consequence is now STRONGER, not weaker: since the underwater arm
        // became a categorical veto, crossing this constant does not merely
        // deprioritize Clue cracking — it forbids it outright, at every
        // difficulty, on every deck. This pin matters more after the veto than
        // it did before it.
        assert!(
            crate::config::PolicyPenalties::default().sacrifice_token_cost
                < crate::policies::strategy_helpers::SINGLE_CARD_VALUE,
            "a token must stay cheaper than the card a crack draws"
        );
    }

    // --- draw deliverability: a draw the pipeline removes buys nothing ------

    #[test]
    fn opposing_notion_thief_is_visible_to_the_draw_preflight() {
        // FIXTURE REACH PROBE — the non-vacuity guard every row below stands
        // on. A hand-built replacement that never reaches
        // `find_applicable_replacements` would make every "suppressed" row
        // below pass for the empty-library reason instead, so the fixture is
        // proven to move the engine's own predicate BEFORE any policy verdict
        // is consulted. Both directions are asserted in one state: seeded
        // library + no thief ⇒ deliverable; add the thief ⇒ not deliverable.
        let mut state = state_with_library();
        state.phase = Phase::PreCombatMain;
        assert!(
            can_draw_at_least_one(&state, AI),
            "positive control: a seeded library with no suppressor must deliver"
        );
        opposing_notion_thief(&mut state);
        assert!(
            !can_draw_at_least_one(&state, AI),
            "an opponent's Notion Thief must remove the AI's draw"
        );
    }

    #[test]
    fn thief_suppressed_draw_is_vetoed_underwater() {
        // THE REPORTED BUG, at the seam that decides it. Same Clue board as
        // `noncreature_token_sac_for_draw_covers_cost` — the paired negative
        // control, identical in every respect except the thief — so the ONLY
        // difference between "crack it" and "never crack it" is an opponent's
        // permanent that takes the card.
        //
        // Each activation costs a permanent and {1}{B}, draws the AI nothing,
        // and hands an opponent a card (CR 614.6: the replaced draw never
        // happens). Priced honestly the trade is 0.0 against 0.5 → net -0.5 →
        // categorical veto.
        //
        // Revert image: restore the unconditional `count * SINGLE_CARD_VALUE`
        // and this reads neutral `self_cost_benefit_covers_cost` with
        // `benefit_milli` 1000 — red on the verdict SHAPE, on the reason KIND,
        // and on the FACTS. That green-on-a-lie state is exactly what this test
        // was watched passing in, pre-fix, before the arm was changed.
        //
        // Reach: `opposing_notion_thief_is_visible_to_the_draw_preflight` proves
        // the fixture actually moves `can_draw_at_least_one`, so the zero here
        // cannot be the empty-library confound.
        let mut state = state_with_library();
        artifact_token(&mut state, "Clue");
        opposing_notion_thief(&mut state);
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            activated(draw(1), sac_artifact_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 500, 0);
    }

    #[test]
    fn draw_price_follows_the_suppressor_leaving_within_one_state() {
        // LIVENESS / NON-LATCHING. The deliverability gate is a LIVE predicate,
        // recomputed on every scoring pass and deliberately never snapshotted,
        // latched, or memoized. Every other row here builds a fresh
        // `GameState`, so none of them can observe the SAME source's price
        // CHANGE — this row is the only place that property is pinned.
        //
        // THIS TEST EXISTS TO MAKE A MEMOIZATION OF `can_draw_at_least_one`
        // FAIL LOUDLY. Caching it per turn, per game, or process-global is the
        // obvious perf follow-up — that is exactly the set the two red-watch
        // legs demonstrated, and it is the set that can produce the failure
        // described below. A memo scoped to a single DECISION is NOT caught:
        // `verdict_for_in` rebuilds `CandidateAction`/`AiDecisionContext`/
        // `PolicyContext` per call, and the house parks search memos on
        // `PlannerServices` (planner/mod.rs:474, `eval_cache` and
        // `transposition_table`), which this row never constructs. That scope
        // is harmless here: it is discarded between decisions, so it cannot
        // outlive the suppressor's departure. `AiContext` is in that same
        // harmless class in production — `PlannerServices` owns one per
        // decision (planner/mod.rs:478) — but this row shares one across BOTH
        // verdicts, so a memo parked there IS caught. `AiSession`, `GameState`
        // and process-global are caught for the same reason.
        //
        // ONE HOME ESCAPES, recorded rather than papered over: a memo field on
        // `SelfCostValuePolicy` ITSELF. This row calls
        // `SelfCostValuePolicy.verdict(&ctx)` on a fresh value per call, while
        // production reaches the policy through `PolicyRegistry::shared()` — a
        // `OnceLock` static (registry.rs:433-434) holding
        // `Box::new(SelfCostValuePolicy)` (:388) — so the policy value is
        // process-global there and reborn here. Policy-instance state is an
        // established pattern in this registry (`ComboLinePolicy::new()`),
        // though none is interior-mutable today. A future interior-mutable memo
        // on this struct would slip past this row; catching it would need a row
        // that scores through `PolicyRegistry::shared()`.
        // Nothing else in the suite would go red: the
        // end-to-end arm's `suppressed_activations == 0` becomes only MORE true
        // under a latch, and the thiefless control has no suppressor to lose.
        // The AI would silently keep pricing its cracks at 0.0 after the thief
        // died and decline correct play for the rest of the duration.
        //
        // The suppressor is removed from the LIVE state and the SAME source is
        // re-verdicted, so a stale value has nowhere to hide.
        let mut state = state_with_library();
        artifact_token(&mut state, "Clue");
        let thief = opposing_notion_thief(&mut state);
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            activated(draw(1), sac_artifact_cost()),
        );

        let config = AiConfig::default();
        let context = context_for(&config, plain_features());

        // Suppressed: the draw buys nothing, exactly as the row above.
        assert_facts(&verdict_for_in(&context, &config, &state, source), 500, 0);

        // The thief dies. The zone gate that decides whether a replacement is
        // even a candidate reads `obj.zone`
        // (`replacement.rs::object_replacement_candidate_applies`,
        // `zones_to_scan.contains(&obj.zone)`), NOT `state.battlefield` — so
        // retaining the id out of the battlefield vector alone would leave the
        // replacement live and make this row a no-op. Both are updated.
        state.battlefield.retain(|&id| id != thief);
        state.objects.get_mut(&thief).unwrap().zone = Zone::Graveyard;

        // Same source, same state: the price must FOLLOW the board.
        let revived = verdict_for_in(&context, &config, &state, source);
        assert_neutral(&revived, "self_cost_benefit_covers_cost");
        assert_facts(&revived, 500, 1000);
    }

    #[test]
    fn empty_library_draw_buys_nothing() {
        // CR 704.5b: drawing from an empty library delivers no card (it records
        // an attempted draw and loses the game at the next SBA check), so the
        // same Clue crack buys nothing. A DIFFERENT leg of
        // `can_draw_at_least_one` from the thief row — `select_cards_to_draw`,
        // not the replacement pipeline — reaching the same zero, which is what
        // makes the gate a class fix rather than a Notion Thief special case.
        // Deliberately built on the BARE constructor: this is the one row whose
        // subject IS the empty library.
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        artifact_token(&mut state, "Clue");
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            activated(draw(1), sac_artifact_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 500, 0);
    }

    #[test]
    fn cant_draw_static_makes_the_crack_underwater() {
        // The STATICS authority (`allowed_draw_count`), a third leg to the same
        // zero — an opponent's Spirit of the Labyrinth-class permanent, not a
        // replacement and not an empty library.
        let mut state = state_with_library();
        state.phase = Phase::PreCombatMain;
        artifact_token(&mut state, "Clue");
        add_draw_restricting_static(
            &mut state,
            StaticMode::CantDraw {
                who: ProhibitionScope::AllPlayers,
            },
        );
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            activated(draw(1), sac_artifact_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 500, 0);
    }

    #[test]
    fn per_turn_draw_limit_suppresses_only_once_exhausted() {
        // THE LIMIT AND ITS HOSTILE SIBLING, one fixture two states: the same
        // `PerTurnDrawLimit { max: 1 }` reads suppressing or harmless purely
        // from `cards_drawn_this_turn`. A gate that treated the mere PRESENCE of
        // a draw-limiting static as suppression passes the (a) leg and fails the
        // (b) leg — which is why both are here.
        //
        // CR 121.2 boundary, disclosed rather than tested: the price is binary,
        // so with headroom 1 a `draw(3)` would still price 3.0. Over-pricing is
        // the conservative direction (it can let a marginal crack through, never
        // forbid a paying one).
        for (drawn_this_turn, expect_veto) in [(1_u32, true), (0_u32, false)] {
            let mut state = state_with_library();
            state.phase = Phase::PreCombatMain;
            artifact_token(&mut state, "Clue");
            add_draw_restricting_static(
                &mut state,
                StaticMode::PerTurnDrawLimit {
                    who: ProhibitionScope::AllPlayers,
                    max: 1,
                },
            );
            state.players[AI.0 as usize].cards_drawn_this_turn = drawn_this_turn;
            let source = source_with(
                &mut state,
                "Token Cracker",
                &[CoreType::Enchantment],
                activated(draw(1), sac_artifact_cost()),
            );
            let verdict = verdict_for(&state, source, plain_features());
            if expect_veto {
                assert_reject(&verdict, "self_cost_benefit_underwater");
                assert_facts(&verdict, 500, 0);
            } else {
                assert_neutral(&verdict, "self_cost_benefit_covers_cost");
                assert_facts(&verdict, 500, 1000);
            }
        }
    }

    #[test]
    fn optional_thief_mode_stands_down_as_unpriced() {
        // An OPTIONAL replacement requires an accept/decline decision. The
        // preview must not select either branch, so delivery is Unknown rather
        // than the mandatory thief's settled zero. That makes the benefit
        // unpriced and leaves the cost comparison neutral.
        //
        // Multi-authority row: identical source, identical player scope,
        // identical substitute — only the MODE differs from
        // `thief_suppressed_draw_is_vetoed_underwater`.
        let mut state = state_with_library();
        artifact_token(&mut state, "Clue");
        let thief = opposing_notion_thief(&mut state);
        state
            .objects
            .get_mut(&thief)
            .unwrap()
            .replacement_definitions[0]
            .mode = ReplacementMode::Optional { decline: None };
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            activated(draw(1), sac_artifact_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_neutral(&verdict, "self_cost_benefit_present");
    }

    #[test]
    fn opponent_self_scoped_substitution_leaves_the_ai_draw_alone() {
        // The PLAYER-SCOPE authority. Same opponent, same mandatory non-Draw
        // substitution, but scoped to its own controller's draws (the Chains of
        // Mephistopheles shape, `valid_player: None` ⇒ source player only)
        // rather than to opponents. The AI's draw is untouched, so the crack
        // still pays.
        //
        // This is the row that proves the gate inherits the engine's LIVE
        // applicability decision instead of scanning definitions by event: an
        // implementation that asked "is there a mandatory non-Draw Draw
        // replacement anywhere on the board?" vetoes here and goes red.
        let mut state = state_with_library();
        artifact_token(&mut state, "Clue");
        let thief = opposing_notion_thief(&mut state);
        state
            .objects
            .get_mut(&thief)
            .unwrap()
            .replacement_definitions[0]
            .valid_player = None;
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            activated(draw(1), sac_artifact_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_neutral(&verdict, "self_cost_benefit_covers_cost");
        assert_facts(&verdict, 500, 1000);
    }

    #[test]
    fn suppressed_draw_beside_an_unmodeled_rider_still_stands_down() {
        // COMPOSITION GUARD 1. The zero must not be allowed to manufacture a
        // conclusion the module's own conservatism forbids: an unmodeled rider
        // beside the dead draw still yields `Unpriced` → neutral stand-down, not
        // a veto. The hostile version of
        // `unmodeled_benefit_rider_stands_down_the_comparison` (same row with
        // the draw alive), because a zeroed payoff makes the underwater
        // conclusion look MORE attractive, not less.
        let mut state = state_with_library();
        artifact_token(&mut state, "Clue");
        opposing_notion_thief(&mut state);
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            with_rider(
                activated(draw(1), sac_artifact_cost()),
                create_token_rider(),
            ),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn a_dead_draw_does_not_zero_its_priced_chain_mates() {
        // COMPOSITION GUARD 2. Pricing is PER EFFECT: the suppressed draw goes
        // to 0.0, but the lifegain beside it keeps its own price (10 *
        // self_cost_pay_life_per_point 0.15 = 1.5), so 1.5 against the Clue's
        // 0.5 still covers.
        //
        // Revert image, and the reason this row exists: an implementation that
        // took the "the draw is dead" fact and zeroed the whole CHAIN reports
        // `underwater` here and goes red on shape, kind, and facts.
        let mut state = state_with_library();
        artifact_token(&mut state, "Clue");
        opposing_notion_thief(&mut state);
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            with_rider(activated(draw(1), sac_artifact_cost()), gain_life(10)),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_neutral(&verdict, "self_cost_benefit_covers_cost");
        assert_facts(&verdict, 500, 1500);
    }
}
