//! Hard-veto gate for a cast whose **mandatory** non-mana cost is a sacrifice
//! that can only be paid by spending mana sources the plan still needs.
//!
//! The class, never a card: any cast where paying the cost would force the AI to
//! surrender a land or accelerant its own development schedule is still short of.
//! Detection is structural throughout — `AdditionalCost` variant, `FlashbackCost`
//! variant, `SacrificeRequirement` variant, `Zone`, `CoreType`, and `ManaRole`.
//! No card name appears anywhere below.
//!
//! # Two extraction sites, one mechanism
//!
//! A mandatory sacrifice can reach a cast from two places, and they are the same
//! payment pipeline rather than two features. An `AdditionalCost::Required`
//! sacrifice is an *additional* cost (CR 601.2f: "The player determines the total
//! cost of the spell… Some spells have additional or alternative costs"). A
//! flashback cost is an *alternative* cost, and CR 702.34a routes it through that
//! same CR 601.2b / 601.2f–h announcement-and-payment sequence. So one predicate
//! serves both, with two extraction sites feeding it.
//!
//! # Why the requirement's count is load-bearing
//!
//! An earlier design asked "is *every* eligible permanent a needed mana source?".
//! That is wrong in both directions: vacuously true on an empty eligible set, and
//! wrongly false whenever one spare exists but the cost demands more than one. A
//! cost reading "sacrifice three creatures" walks straight through such a gate
//! while the AI holds a single spare — which is the reported bug's own species at
//! `count > 1`. The predicate therefore counts:
//!
//! ```text
//! reject  ⟺  required > 0  ∧  eligible.len() >= required  ∧  spare < required
//! ```
//!
//! # Fail-open holes, each deliberate and each named
//!
//! * **`SacrificeRequirement::Aggregate`** — a count predicate cannot answer an
//!   aggregate constraint ("total power 4 or greater"), so the `let … else`
//!   declines. Engineering fail-open, not a rules decision; it carries no CR.
//! * **The `u32::MAX` X sentinel.** `casting::sacrifice_cost_bounds` hands back
//!   `min = 0`, and `spare < 0` is unsatisfiable on `usize`, so no reject can be
//!   produced — the fail-open is *arithmetic*. The `required == 0` disjunct in
//!   `gate_rejects` makes that intent legible and skips a plan realization plus a
//!   filter pass; it is not what makes the fail-open hold, and no revert of it can
//!   change a verdict. What *is* structural is the **delegation**: the sentinel
//!   encoding is never re-spelled here. CR 601.2b is the rules ground for why
//!   X = 0 must stay castable — the caster announces X, and announcing 0 is a
//!   legal and free announcement, so gating would forbid a legal play.
//! * **`AdditionalCost::OneOf` / `Choice` / `Optional` / `Kicker`** — the payer
//!   picks the leg or declines entirely (CR 601.2b), so there is no forced loss.
//! * **The two-site collision.** When a graveyard object carries *both* a
//!   `Required` sacrifice and a flashback sacrifice, CR 601.2f **composes** them —
//!   such a cast pays both. This gate returns a single `SacrificeCost`, and the two
//!   costs carry different `TargetFilter`s and therefore different eligible sets,
//!   which one `SacrificeCost` cannot represent. Rather than pin a precedence the
//!   rules do not grant, it stands down.
//!
//! Fail-open is the house style here, not an invention: `self_cost.rs` records
//! that its own catch-all is fail-open so "a new cost variant simply gets no gate
//! rather than a spurious veto." A gate that guesses wrong silently forbids
//! correct plays, which is worse than a gate with a documented hole.
//!
//! # Fail-safe when there is no plan
//!
//! With no plan authority in the session, `keep_tier` returns `Ordinary` for every
//! card, so nothing is ever a needed mana source and the gate is inert. That is a
//! property of the tier, not a special case here.
//!
//! # Why the commitment and not the payment seam
//!
//! `GameAction::CancelCast` genuinely *is* available at the downstream
//! `WaitingFor::PayCost` prompt, and `cancelled_casts` prevents a tight livelock,
//! so a payment-seam design would work mechanically. It is still the wrong seam:
//! cancelling there announces the spell (CR 601.2a) and then rewinds it
//! (CR 601.2e → CR 733.1, "the entire action is reversed and any payments already
//! made are canceled"), which is a wasted decision with observable side effects
//! once per priority round, and it splits the authority across two files.
//!
//! # No blanket `at_root()` guard, unlike `x_cast_gate`
//!
//! `x_cast_gate` skips itself outside the root because in lookahead an X=0 cast is
//! already dominated by its resulting-state eval. That justification is *absent*
//! here and provably so: `quiesce` halts at `WaitingFor::PayCost` — there is no
//! `PayCost` arm in `deterministic_choice` — so the rollout evaluates the leaf in a
//! state where the sacrifice has not happened and never charges for it. Copying the
//! guard would leave the search-prior path, the only path that runs at lookahead,
//! completely ungated.
//!
//! # Per-arm cost model — read this before "optimizing" the graveyard arm
//!
//! Not every arm is cheap, and the expensive one is affordable for a specific
//! reason. A cast from hand / exile / command costs one `Option` field read plus an
//! enum match — genuinely trivial. A cast from the **graveyard** additionally
//! resolves `keywords::effective_flashback_cost`, which off the battlefield is a
//! full **uncached** CR 613 Layer-6 walk: a state-wide continuous-effect collection
//! that sweeps every object in every zone, plus `O(n²)` dependency ordering. It is
//! affordable only because candidate generation has *already made that same call*
//! to produce the candidate at all — `prepare_spell_cast` resolves it zone-guarded
//! (so it fires for every graveyard object) and `can_cast_prepared_now_with_probe`
//! resolves it again variant-guarded on `CastingVariant::Flashback`. The gate's
//! marginal cost is therefore **+1 of 2 or 3** on a population that has already
//! paid it, not a new per-node sweep. The extraction is **zone-split**, so no
//! hand cast reaches the keyword resolver at all — and that split is a rules fact
//! (CR 702.34a: flashback functions only from the graveyard), with the perf saving
//! as its consequence.
//!
//! **Two cheap early-outs were evaluated and both are rejected on correctness.**
//! A card-local `obj.base_keywords` probe is unsound as a *negative* early-out,
//! because flashback is granted at runtime (the Snapcaster / Lier class) — taking
//! it would delete exactly the capability this module's runtime-grant test pins.
//! The repo's O(1) `functioning_abilities::static_kind_present` presence gate is
//! unsound here too: that index is refreshed only from battlefield/command
//! `static_definitions`, while the walk it would skip also consumes
//! `state.transient_continuous_effects`, `base_static_definitions`, and off-zone
//! opt-in-zone statics — `off_zone_characteristics::transient_add_keyword_applies_to_graveyard_card`
//! is a shipped test in which a live Flashback grant coexists with an all-false
//! index. A sound index would need the union of those sources, keyed on
//! `ContinuousModification` discriminants rather than `StaticModeKind`; that is an
//! engine unit with its own owner, not two visibility words in this one's budget.

use engine::game::casting::{find_eligible_sacrifice_targets, sacrifice_cost_bounds};
use engine::game::game_object::GameObject;
use engine::game::keywords::effective_flashback_cost;
use engine::types::ability::{AbilityCost, AdditionalCost, SacrificeCost, SacrificeRequirement};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::FlashbackCost;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::card_value::{keep_tier, KeepTier};
use crate::features::DeckFeatures;
use crate::plan::PlanState;

pub struct SacrificeCostManaGatePolicy;

impl TacticalPolicy for SacrificeCostManaGatePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SacrificeCostManaGate
    }

    /// `CastSpell` **only**, and that narrowness is load-bearing rather than
    /// incidental. `GameAction::PassPriority` classifies as
    /// `DecisionKind::ActivateAbility` at `WaitingFor::Priority`, so declaring
    /// only `CastSpell` is what keeps the Pass candidate out of this policy's
    /// reach — and therefore what keeps `PolicyRegistry::priors`' all-rejected
    /// uniform fallback from ever firing on this gate's account. **Widening this
    /// to `ActivateAbility` would break that argument.**
    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // activation-constant: unconditional Reject backstop; gating in `verdict`.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        gate_rejects(ctx).map_or_else(
            || PolicyVerdict::neutral(PolicyReason::new("sacrifice_cost_mana_gate_na")),
            PolicyVerdict::reject,
        )
    }
}

/// The shared `AbilityCost` → `SacrificeCost` reduction both extraction sites
/// feed.
///
/// `Composite` recurses and yields `Some` only when **exactly one** leg reduces
/// to a sacrifice. Two reducible legs are two independent eligible sets, which
/// the single-`SacrificeCost` return type cannot represent, and returning the
/// first would silently gate on one and ignore the other. `OneOf` yields `None`
/// outright: per CR 601.2b the payer chooses which leg to pay, so if any leg is
/// not a needed-source sacrifice the cast is fine. Every other cost shape is
/// fail-open.
fn sacrifice_leg(cost: &AbilityCost) -> Option<&SacrificeCost> {
    match cost {
        AbilityCost::Sacrifice(sacrifice) => Some(sacrifice),
        AbilityCost::Composite { costs } => {
            let mut legs = costs.iter().filter_map(sacrifice_leg);
            let first = legs.next()?;
            legs.next().is_none().then_some(first)
        }
        AbilityCost::OneOf { .. } => None,
        _ => None,
    }
}

/// The `AdditionalCost` → `SacrificeCost` reduction. Exhaustive over all four
/// variants with **no `_` arm**, so a fifth variant is a compile error rather
/// than a silent gate hole.
fn required_sacrifice_leg(additional_cost: &AdditionalCost) -> Option<SacrificeCost> {
    match additional_cost {
        // CR 601.2f: a mandatory additional cost is part of the total cost — the
        // caster cannot decline it. The only gated shape.
        AdditionalCost::Required(cost) => sacrifice_leg(cost).cloned(),
        // Declinable: the AI is never forced into the loss, and declining is
        // already modelled in `search.rs`'s optional-cost-choice arm, so gating
        // the cast here would double-count.
        AdditionalCost::Optional { .. } => None,
        // CR 702.33a: kicker is an optional additional cost.
        AdditionalCost::Kicker { .. } => None,
        // CR 601.2b: the payer picks a leg, same reasoning as `OneOf`.
        AdditionalCost::Choice(_, _) => None,
    }
}

/// The single mandatory sacrifice a prospective cast would have to pay, or
/// `None` when the cost shape is not one this gate models.
///
/// Owned rather than borrowed: the flashback arm resolves through
/// `effective_flashback_cost`, which returns an owned `FlashbackCost` (it must —
/// it composes runtime grants), so a borrow cannot outlive it. `SacrificeCost`
/// is `Clone`; the clone is one small struct (a `TargetFilter` plus a two-variant
/// enum) and is reached only after the variant match has already selected a
/// sacrifice-bearing cost. See the module docstring for the per-arm cost model —
/// the graveyard arm is NOT cheap, and the reason it is nonetheless affordable is
/// that candidate generation already made the same call: once for any graveyard
/// object (zone-guarded), twice when the candidate resolved to flashback
/// (variant-guarded).
fn mandatory_sacrifice(state: &GameState, object: &GameObject) -> Option<SacrificeCost> {
    // CR 702.34a: flashback functions ONLY from the graveyard, so a hand / exile /
    // command cast can never have a flashback cost — the keyword resolver is not
    // merely skipped as an optimization, it is inapplicable. This is also what
    // keeps the expensive off-zone keyword walk off every hand cast.
    if object.zone != Zone::Graveyard {
        return object
            .additional_cost
            .as_ref()
            .and_then(required_sacrifice_leg);
    }

    let from_additional = object
        .additional_cost
        .as_ref()
        .and_then(required_sacrifice_leg);
    let from_flashback = effective_flashback_cost(state, object.id).and_then(|fb| match fb {
        FlashbackCost::NonMana(cost) => sacrifice_leg(&cost).cloned(),
        FlashbackCost::Mana(_) => None,
    });

    match (from_additional, from_flashback) {
        (Some(cost), None) | (None, Some(cost)) => Some(cost),
        (None, None) => None,
        // CR 601.2f: "The player determines the total cost of the spell… Some
        // spells have additional or alternative costs." Flashback occupies the
        // ALTERNATIVE-cost slot (CR 702.34a routes it through 601.2b and 601.2f-h);
        // a `Required` additional cost is an ADDITIONAL cost. 601.2f therefore
        // COMPOSES them — such a cast pays BOTH sacrifices, not one of them.
        //
        // This gate returns a SINGLE `SacrificeCost`, and the two costs carry
        // different `TargetFilter`s and therefore different eligible sets, so their
        // requirements cannot be summed into one count against one set. Rather than
        // pin a precedence the rules do not grant, stand down.
        (Some(_), Some(_)) => None,
    }
}

/// Whether losing this permanent would deprive the plan of a mana source it
/// still needs.
///
/// CR 701.21a: to sacrifice a permanent, "its controller moves it from the
/// battlefield directly to its owner's graveyard" — so for a one-shot
/// self-sacrificing source (Treasure, Gold, Lotus Petal) sacrificing it **is**
/// using it, and paying a cost with it is its intended use rather than a loss of
/// development. `mana_role` uses the same intrinsic-source population as
/// `plan::controlled_mana_sources`, so the gate cannot promote a source the
/// deficit does not count. That is a class — every present and future one-shot
/// mana token — not a carve-out.
fn deprives_the_plan(state: &GameState, id: ObjectId, plan: Option<PlanState>) -> bool {
    keep_tier(state, id, plan) == KeepTier::NeededManaSource
}

fn gate_rejects(ctx: &PolicyContext<'_>) -> Option<PolicyReason> {
    // Ordering is load-bearing, not stylistic: this gate runs at every search
    // depth (see the module docstring), so the board scan below must be reached
    // only by candidates that actually carry a mandatory sacrifice cost. AND is
    // commutative, so putting the card-local predicates first cannot change any
    // verdict — it only spares the scan. Mirrors x_cast_gate.rs's cheap-first
    // ordering.
    let GameAction::CastSpell { .. } = &ctx.candidate.action else {
        return None;
    };
    let facts = ctx.cast_facts()?;
    let object = facts.object;
    // Zone-split inside: a non-graveyard cast does NO keyword resolution.
    let cost = mandatory_sacrifice(ctx.state, object)?;

    // An aggregate constraint cannot be answered by a count — fail open. No CR:
    // this is an engineering fail-open, not a rules implementation.
    let SacrificeRequirement::Count { count } = cost.requirement else {
        return None;
    };

    // CR 701.21a + CR 118.3: the engine's single eligibility authority —
    // controller check plus the can't-sacrifice-as-a-cost static. Do NOT
    // re-derive it here.
    let eligible =
        find_eligible_sacrifice_targets(ctx.state, ctx.ai_player, object.id, &cost.target);

    // CR 107.3a + CR 118.3: the engine's own minimum for this cost. Delegating is
    // what is structural — the u32::MAX sentinel encoding is never re-spelled
    // here, so it cannot drift from the engine. The X fail-open itself is
    // ARITHMETIC: bounds returns min = 0 and `spare < 0` is unsatisfiable on
    // usize, so CR 601.2b's "X = 0 is a legal free announcement" holds with or
    // without the `required == 0` disjunct below; that disjunct makes the intent
    // legible and skips a plan realization and a filter pass.
    //
    // The second conjunct is anti-vacuity: when the cost is not payable at all
    // the CAST is illegal (CR 601.2e -> CR 733.1), and legality, not tactics, is
    // the right authority. Without it, `spare < required` is trivially true on an
    // empty set and the gate would veto every cast whose cost it cannot pay.
    // This is the same pair, in the same order, that `cost_payability.rs` uses.
    let (required, _) = sacrifice_cost_bounds(count, eligible.len());
    if required == 0 || eligible.len() < required {
        return None;
    }

    let plan = ctx
        .context
        .session
        .plan
        .get(&ctx.ai_player)
        .map(|snapshot| PlanState::realize(ctx.state, ctx.ai_player, snapshot));

    let spare = eligible
        .iter()
        .filter(|&&id| !deprives_the_plan(ctx.state, id, plan))
        .count();

    // `required` / `eligible` / `spare` are all emitted because `priors` logs only
    // a candidate count when its uniform fallback fires; these are what make a
    // reject attributable post-hoc. Emitting `eligible` alone cannot distinguish
    // "no spare at all" from "not enough spares".
    (spare < required).then(|| {
        PolicyReason::new("sacrifice_cost_spends_needed_mana")
            .with_fact("required", required as i64)
            .with_fact("eligible", eligible.len() as i64)
            .with_fact("spare", spare as i64)
            .with_fact("lands_behind", plan.map_or(0, |p| p.lands_behind as i64))
            .with_fact("mana_behind", plan.map_or(0, |p| p.mana_behind as i64))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_value::{mana_role, ManaRole};
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::policies::context::{PriorsEnv, SearchDepth};
    use crate::policies::registry::PolicyRegistry;
    use crate::test_support::{context_with_plans, default_deck_plan};
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::static_abilities::player_cant_sacrifice_as_cost;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, Duration, Effect, ManaContribution,
        ManaProduction, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{CastPaymentMode, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost};
    use engine::types::statics::{CostPaymentProhibition, ProhibitionScope, StaticMode};
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    // --- fixture builders -------------------------------------------------

    /// Trap 3 (fixture terminality): `GameState::new_two_player` leaves both
    /// libraries empty, and per CR 704.5b a deep-enough search then finds a
    /// forced win and the test flakes. Every fixture here fills both libraries.
    ///
    /// No row below currently *depends* on it — these tests call `verdict` and
    /// `priors` directly, neither of which advances state or reaches a
    /// state-based action. It is applied unconditionally as future-proofing, so
    /// the fixtures stay safe if anyone later routes them through
    /// `deterministic_choice` or the planner.
    fn base_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.priority_player = AI;
        state.waiting_for = WaitingFor::Priority { player: AI };
        for player in [AI, OPP] {
            for _ in 0..10 {
                let card_id = CardId(state.next_object_id);
                create_object(
                    &mut state,
                    card_id,
                    player,
                    "Filler".to_string(),
                    Zone::Library,
                );
            }
        }
        state
    }

    fn permanent(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        create_object(state, card_id, player, name.to_string(), Zone::Battlefield)
    }

    /// `core_types = [Artifact, Land]` — the Great Furnace shape. Note it is
    /// deliberately NOT a Mountain: an artifact land carries no basic land
    /// subtype, which is why Lava Dart's flashback cost eats a basic instead.
    fn artifact_land(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = permanent(state, player, "Artifact Land");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.core_types.push(CoreType::Land);
        id
    }

    fn plain_land(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = permanent(state, player, "Plains");
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    fn basic_mountain(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = permanent(state, player, "Mountain");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Mountain".to_string());
        id
    }

    fn vanilla_creature(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = permanent(state, player, "Bear");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);
        id
    }

    /// An untargeted `{T}: Add {B}` — CR 605.1a mana ability whose cost does not
    /// sacrifice its own source, so it is *renewable* and
    /// `mana_role` classifies it as an accelerant.
    fn renewable_mana_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Black],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        ability
    }

    /// `{T}, Sacrifice this: Add {B}` — the Treasure / Gold / Lotus Petal shape.
    /// `cost_sacrifices_self` sees the `Sacrifice(SelfRef)` leg, so this is a
    /// mana ability (`mana_role` → `Accelerant`) that is NOT renewable.
    fn one_shot_mana_ability() -> AbilityDefinition {
        let mut ability = renewable_mana_ability();
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        ability
    }

    fn mana_rock(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = permanent(state, player, "Mana Rock");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut obj.abilities).push(renewable_mana_ability());
        id
    }

    fn mana_dork(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = permanent(state, player, "Mana Dork");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);
        Arc::make_mut(&mut obj.abilities).push(renewable_mana_ability());
        id
    }

    fn treasure_token(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = permanent(state, player, "Treasure");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.is_token = true;
        Arc::make_mut(&mut obj.abilities).push(one_shot_mana_ability());
        id
    }

    /// The filter both report cards use, read from `data/card-data.json`:
    /// `Or([Typed(Artifact), Typed(Creature)])`. Written as the real `Or` shape
    /// rather than a single-type convenience filter so the fixtures exercise
    /// what production actually emits.
    fn artifact_or_creature() -> TargetFilter {
        TargetFilter::Or {
            filters: vec![typed(TypeFilter::Artifact), typed(TypeFilter::Creature)],
        }
    }

    fn typed(type_filter: TypeFilter) -> TargetFilter {
        TargetFilter::Typed(TypedFilter::new(type_filter))
    }

    /// CR 205.3: `Typed[Subtype("Mountain")]` — Lava Dart's measured filter
    /// shape. Deliberately a subtype filter and not `TypeFilter::Land`: an
    /// artifact land carries no basic land subtype, which is why this cost eats
    /// a basic.
    fn mountain_filter() -> TargetFilter {
        typed(TypeFilter::Subtype("Mountain".to_string()))
    }

    fn sac(filter: TargetFilter, count: u32) -> AbilityCost {
        AbilityCost::Sacrifice(SacrificeCost::count(filter, count))
    }

    /// A castable object in `zone` carrying an optional additional cost.
    fn spell(
        state: &mut GameState,
        zone: Zone,
        additional_cost: Option<AdditionalCost>,
    ) -> (ObjectId, CardId) {
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, AI, "Spell".to_string(), zone);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.additional_cost = additional_cost;
        *Arc::make_mut(&mut obj.abilities) = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
        )];
        (id, card_id)
    }

    /// Push a PRINTED flashback keyword. Off the battlefield the keyword
    /// resolver reads `base_keywords`, not `keywords` — see
    /// `off_zone_characteristics::effective_off_zone_keyword_contributions`.
    fn print_flashback(state: &mut GameState, id: ObjectId, cost: AbilityCost) {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(FlashbackCost::NonMana(cost)));
    }

    // --- verdict plumbing -------------------------------------------------

    fn cast_candidate(object_id: ObjectId, card_id: CardId) -> CandidateAction {
        CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
        }
    }

    fn verdict_with_context(
        state: &GameState,
        context: &AiContext,
        config: &AiConfig,
        object_id: ObjectId,
        card_id: CardId,
    ) -> PolicyVerdict {
        let candidate = cast_candidate(object_id, card_id);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        SacrificeCostManaGatePolicy.verdict(&PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config,
            context,
            cast_facts: None,
            search_depth: SearchDepth::Root,
        })
    }

    /// Trap 2 (fixture unreachability): the plan is a **derived** snapshot via
    /// `context_with_plans` + `default_deck_plan`, never a hand-poked
    /// `PlanState { lands_behind: 3, .. }`. The derived land schedule plateaus
    /// at 6, so a fixture asserting a value `derive_snapshot` cannot emit would
    /// prove nothing.
    fn verdict_with_plan(state: &GameState, object_id: ObjectId, card_id: CardId) -> PolicyVerdict {
        let config = AiConfig::default();
        let context = context_with_plans(state, AI, &config, &[(AI, default_deck_plan())]);
        verdict_with_context(state, &context, &config, object_id, card_id)
    }

    fn realized_plan(state: &GameState) -> PlanState {
        PlanState::realize(state, AI, &default_deck_plan())
    }

    // --- assertions -------------------------------------------------------

    fn fact(verdict: &PolicyVerdict, key: &str) -> i64 {
        let reason = match verdict {
            PolicyVerdict::Reject { reason } => reason,
            PolicyVerdict::Score { reason, .. } => reason,
        };
        reason
            .facts
            .iter()
            .find(|(k, _)| *k == key)
            .unwrap_or_else(|| panic!("verdict carries no `{key}` fact: {verdict:?}"))
            .1
    }

    /// §8.3: a reject row asserts the reason KIND plus the arithmetic, so a
    /// reject arriving from the wrong branch is caught rather than counted.
    fn assert_rejected(verdict: &PolicyVerdict, required: i64, eligible: i64, spare: i64) {
        match verdict {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "sacrifice_cost_spends_needed_mana");
            }
            PolicyVerdict::Score { delta, reason } => panic!(
                "expected a reject, got Score {{ delta: {delta}, kind: {} }}",
                reason.kind
            ),
        }
        assert_eq!(fact(verdict, "required"), required, "required");
        assert_eq!(fact(verdict, "eligible"), eligible, "eligible");
        assert_eq!(fact(verdict, "spare"), spare, "spare");
    }

    /// §8.3: every negative assertion carries the positive reach-guard
    /// `reason.kind == "sacrifice_cost_mana_gate_na"` — NOT a bare
    /// `matches!(v, Score { .. })` — so an upstream short-circuit (a wrong
    /// `let … else`, a missing `cast_facts`, a mis-declared `decision_kinds`)
    /// cannot satisfy it vacuously.
    fn assert_stood_down(verdict: &PolicyVerdict) {
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "sacrifice_cost_mana_gate_na");
                assert_eq!(*delta, 0.0, "the stand-down is neutral, not a nudge");
            }
            PolicyVerdict::Reject { reason } => {
                panic!(
                    "expected a stand-down, got Reject {{ kind: {} }}",
                    reason.kind
                )
            }
        }
    }

    // --- F1: the report's own shape ---------------------------------------

    /// F1. A cast whose only eligible sacrifice is a needed artifact land is
    /// rejected. This is the reported bug (Fanatical Offering / Reckoner's
    /// Bargain), at the measured cost shape
    /// `Required(Sacrifice(Or[Artifact, Creature], count 1))`.
    ///
    /// REVERT THAT REDDENS IT: delete the `(spare < required)` reject in
    /// `gate_rejects` so the gate returns neutral.
    #[test]
    fn cast_rejected_when_only_fodder_is_a_needed_artifact_land() {
        let mut state = base_state();
        let land = artifact_land(&mut state, AI);
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Required(sac(artifact_or_creature(), 1))),
        );

        // §8.4 fixture-premise guards: if the derived schedule ever changes,
        // these fail with a diagnosis rather than passing for the wrong reason.
        assert_eq!(mana_role(&state, land), ManaRole::LandDrop);
        assert!(
            realized_plan(&state).lands_behind > 0,
            "fixture premise: the plan must be behind on lands"
        );

        assert_rejected(&verdict_with_plan(&state, obj, card), 1, 1, 0);
    }

    // --- F2: B2's rebuilt board — needed < required <= spare ---------------

    /// F2. Allowed when spares outnumber the requirement.
    ///
    /// REVERT THAT REDDENS IT: count *needed* instead of *spare* (drop the `!`
    /// in the `filter`) → `needed(1) < required(2)` → a reject appears.
    ///
    /// The board is the smallest one on which that swap is observable: the swap
    /// is inert wherever `needed == spare`, so this needs `needed < required <=
    /// spare`, i.e. 1 needed artifact land + 2 spare creatures at `count 2`.
    #[test]
    fn cast_allowed_when_spare_creatures_cover_the_requirement() {
        let mut state = base_state();
        let land = artifact_land(&mut state, AI);
        vanilla_creature(&mut state, AI);
        vanilla_creature(&mut state, AI);
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Required(sac(artifact_or_creature(), 2))),
        );

        assert_eq!(mana_role(&state, land), ManaRole::LandDrop);
        assert!(realized_plan(&state).lands_behind > 0);

        assert_stood_down(&verdict_with_plan(&state, obj, card));
    }

    // --- F3 / F4: two genuinely distinct plan seams ------------------------

    /// F3. Allowed when the plan says the player is on curve.
    ///
    /// REVERT THAT REDDENS IT: in `card_value.rs`, split the combined arm
    /// `Some(Ordering::Equal) | None => KeepTier::Ordinary` into
    /// `Some(Ordering::Equal) => KeepTier::NeededManaSource,
    ///  None => KeepTier::Ordinary`.
    ///
    /// CO-ASSERTION (N1): **F4 must stay GREEN under that revert.** That is the
    /// executable proof the two rows pin distinct seams — F4's fixture has no
    /// plan at all, so it takes the `None` half, which the revert leaves alone.
    #[test]
    fn cast_allowed_when_on_curve() {
        let mut state = base_state();
        let land = artifact_land(&mut state, AI);
        // Five more lands to reach the derived plateau of 6. They are plain
        // lands, not artifacts, so they never enter the eligible set — the
        // eligible set stays exactly {artifact land} and only the TIER moves.
        for _ in 0..5 {
            plain_land(&mut state, AI);
        }
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Required(sac(artifact_or_creature(), 1))),
        );

        assert_eq!(mana_role(&state, land), ManaRole::LandDrop);
        assert_eq!(
            realized_plan(&state).lands_behind,
            0,
            "fixture premise: exactly on curve, so the tier reads the \
             `Ordering::Equal` arm"
        );

        assert_stood_down(&verdict_with_plan(&state, obj, card));
    }

    /// F4. Inert when no plan authority exists — the gate's own fail-safe.
    ///
    /// Its seam is `gate_rejects`'s **plan lookup**, not a `keep_tier` arm.
    /// REVERT THAT REDDENS IT: replace
    /// `ctx.context.session.plan.get(&ctx.ai_player).map(..)` with a synthesized
    /// always-behind `PlanState` → the no-plan fixture gates → a reject appears.
    #[test]
    fn cast_inert_when_no_plan_authority() {
        let mut state = base_state();
        let land = artifact_land(&mut state, AI);
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Required(sac(artifact_or_creature(), 1))),
        );

        assert_eq!(mana_role(&state, land), ManaRole::LandDrop);

        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        assert!(
            !context.session.plan.contains_key(&AI),
            "fixture premise: this context must carry NO plan authority"
        );
        assert_stood_down(&verdict_with_context(&state, &context, &config, obj, card));
    }

    // --- F5: the count is load-bearing (Dread Return's species) ------------

    /// F5. `count > 1`: three needed dorks plus one spare, cost demands three →
    /// rejected. This is the case an "is EVERY eligible permanent needed?"
    /// predicate walks straight through.
    ///
    /// REVERT THAT REDDENS IT: replace `spare < required` with `spare == 0`
    /// (the earlier `.all()` semantics) → `spare(1) != 0` → the gate goes silent.
    #[test]
    fn multi_count_sacrifice_rejected_when_spares_are_insufficient() {
        let mut state = base_state();
        for _ in 0..3 {
            mana_dork(&mut state, AI);
        }
        vanilla_creature(&mut state, AI);
        let (obj, card) = spell(&mut state, Zone::Graveyard, None);
        print_flashback(&mut state, obj, sac(typed(TypeFilter::Creature), 3));

        assert!(
            realized_plan(&state).mana_behind > 0,
            "fixture premise: the plan must be behind on mana so the dorks tier \
             as needed sources"
        );

        assert_rejected(&verdict_with_plan(&state, obj, card), 3, 4, 1);
    }

    // --- F6: the report limb, raised above the policy unit -----------------

    /// F6. The Lava Dart flashback cast gets a **zero prior** from the real
    /// `PolicyRegistry::priors` aggregation, while `PassPriority` keeps a
    /// positive one.
    ///
    /// This row is above `verdict`: it drives `PolicyRegistry::score` →
    /// `priors`, the production entry point `planner/mod.rs` uses.
    ///
    /// REVERT THAT REDDENS IT: make `verdict` return
    /// `PolicyVerdict::neutral(PolicyReason::new("sacrifice_cost_mana_gate_na"))`
    /// unconditionally — dropping the `map_or_else` reject arm while **leaving
    /// the registration intact** → the cast's prior becomes positive.
    /// Deliberately NOT "delete the registration": that is F15's revert and
    /// reddens nearly every row, proving nothing about *this* seam.
    ///
    /// The second assertion is the executable form of the claim that this gate
    /// can never be the last rejector: `PassPriority` classifies as
    /// `DecisionKind::ActivateAbility`, which this policy does not declare, so
    /// the all-rejected uniform fallback must NOT have fired.
    #[test]
    fn flashback_sacrifice_of_needed_land_gets_zero_prior_and_pass_survives() {
        let mut state = base_state();
        basic_mountain(&mut state, AI);
        let (obj, card) = spell(&mut state, Zone::Graveyard, None);
        print_flashback(&mut state, obj, sac(mountain_filter(), 1));

        let config = AiConfig::default();
        let context = context_with_plans(&state, AI, &config, &[(AI, default_deck_plan())]);
        let candidates = vec![
            CandidateAction {
                action: GameAction::PassPriority,
                metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Pass),
            },
            cast_candidate(obj, card),
        ];
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: candidates.clone(),
        };
        let priors = PolicyRegistry::shared().priors(
            &PriorsEnv {
                state: &state,
                decision: &decision,
                ai_player: AI,
                config: &config,
                context: &context,
                search_depth: SearchDepth::Root,
            },
            &candidates,
        );

        assert_eq!(priors.len(), 2);
        assert_eq!(
            priors[1].prior, 0.0,
            "the vetoed flashback cast must carry a zero prior"
        );
        assert!(
            priors[0].prior > 0.0,
            "PassPriority must keep a positive prior — a uniform fallback here \
             would mean every candidate was rejected, which would re-enable the \
             vetoed cast at 1/n"
        );
    }

    // --- F7: Composite recursion, then the X sentinel ----------------------

    /// F7. Firecat Blitz's `Composite[Mana{RR}, Sacrifice(Mountain, X)]` reaches
    /// the sacrifice leg, and the X sentinel then stands the gate down.
    ///
    /// REVERT THAT REDDENS IT: delete the `Composite` recursion in
    /// `sacrifice_leg` → the extraction returns `None` → the reach-guard fails.
    ///
    /// The X half is a **direct assertion** on `sacrifice_cost_bounds` rather
    /// than a named revert, because the `required == 0` disjunct cannot change a
    /// verdict: the fail-open is arithmetic (`spare < 0` is unsatisfiable on
    /// `usize`), so no revert of it can be reddened.
    ///
    /// The positive reach-guard is mandatory here: "not rejected" would
    /// otherwise be satisfied vacuously by the extraction returning `None`.
    #[test]
    fn composite_flashback_cost_reaches_the_sacrifice_leg_then_x_stands_down() {
        let mut state = base_state();
        basic_mountain(&mut state, AI);
        let (obj, card) = spell(&mut state, Zone::Graveyard, None);
        print_flashback(
            &mut state,
            obj,
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::generic(2),
                    },
                    sac(mountain_filter(), u32::MAX),
                ],
            },
        );

        // Reach-guard: the recursion really does reach the sacrifice leg.
        let extracted = mandatory_sacrifice(&state, state.objects.get(&obj).unwrap())
            .expect("the Composite recursion must reach the sacrifice leg");
        assert_eq!(
            extracted.requirement,
            SacrificeRequirement::Count { count: u32::MAX },
            "and it must carry the X sentinel"
        );

        // CR 601.2b: the caster announces X, and X = 0 is legal and free, so the
        // engine's own minimum is 0 and no reject can be produced.
        assert_eq!(sacrifice_cost_bounds(u32::MAX, 1), (0, 1));

        assert_stood_down(&verdict_with_plan(&state, obj, card));
    }

    // --- F8 / F8b: two multi-authority rows, deliberately distinct ---------

    /// F8. Multi-authority (controller): an opponent's matching artifact is not
    /// the AI's fodder. CR 701.21a — "a player can't sacrifice … something
    /// that's a permanent they don't control."
    ///
    /// REVERT THAT REDDENS IT: in `casting::find_eligible_sacrifice_targets`,
    /// delete `if obj.controller != player { return false; }`.
    ///
    /// SCOPE: this row does **not** justify Step 3's `pub`. Every hand-rolled
    /// eligible-set copy already has a controller check, including the shipped
    /// sibling `self_cost::sacrifice_leaf_cost`. F8b is the row that does.
    #[test]
    fn opponent_permanent_is_not_eligible_fodder() {
        let mut state = base_state();
        artifact_land(&mut state, AI);
        let opp_artifact = permanent(&mut state, OPP, "Opposing Relic");
        state
            .objects
            .get_mut(&opp_artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Required(sac(artifact_or_creature(), 1))),
        );

        // eligible == 1 is the discriminator: the opponent's matching artifact
        // was excluded, so the AI's needed land is the only fodder.
        assert_rejected(&verdict_with_plan(&state, obj, card), 1, 1, 0);
    }

    /// F8b. **The row that justifies Step 3's `pub`.** An Angel-of-Jubilation
    /// shaped `CantPayCost` static removes an otherwise-eligible spare, and the
    /// gate sees the exclusion because it delegates to the engine's eligibility
    /// authority instead of copying it.
    ///
    /// REVERT THAT REDDENS IT: in `crates/engine/src/game/casting.rs`, delete
    /// `if super::static_abilities::player_cant_sacrifice_as_cost(state, player, id)
    /// { return false; }` → the prohibited creature is counted → `spare(1) >=
    /// required(1)` → the reject vanishes. **This revert is inside
    /// `crates/engine/`, so `test-engine` is watched too, not only `test-ai`.**
    ///
    /// The paired stand-down at the top is what makes the reject attributable to
    /// the static rather than to the board: same board, static absent → neutral;
    /// static present → reject.
    ///
    /// Real-card anchor: Angel of Jubilation prohibits sacrificing **creatures**
    /// while an artifact land — not a creature — stays eligible, so the eligible
    /// set collapses to exactly the needed mana source.
    #[test]
    fn cant_sacrifice_static_excludes_the_spare_and_the_gate_still_fires() {
        let build = |with_static: bool| {
            let mut state = base_state();
            artifact_land(&mut state, AI);
            let creature = vanilla_creature(&mut state, AI);
            if with_static {
                let hoser = permanent(&mut state, OPP, "Angel of Jubilation");
                state
                    .objects
                    .get_mut(&hoser)
                    .unwrap()
                    .static_definitions
                    .push(StaticDefinition::new(StaticMode::CantPayCost {
                        who: ProhibitionScope::AllPlayers,
                        cost: CostPaymentProhibition::Sacrifice {
                            filter: TargetFilter::Typed(TypedFilter::creature()),
                        },
                    }));
            }
            let (obj, card) = spell(
                &mut state,
                Zone::Hand,
                Some(AdditionalCost::Required(sac(artifact_or_creature(), 1))),
            );
            (state, creature, obj, card)
        };

        // Paired negative control: without the static the creature IS a spare.
        let (state, creature, obj, card) = build(false);
        assert!(!player_cant_sacrifice_as_cost(&state, AI, creature));
        assert_stood_down(&verdict_with_plan(&state, obj, card));

        // §8.4 premise guard: a fixture that silently failed to install the
        // static must read as a diagnosis, not as a pass.
        let (state, creature, obj, card) = build(true);
        assert!(
            player_cant_sacrifice_as_cost(&state, AI, creature),
            "fixture premise: the CantPayCost static must be in force"
        );
        assert_rejected(&verdict_with_plan(&state, obj, card), 1, 1, 0);
    }

    // --- F9: runtime-granted flashback ------------------------------------

    /// F9. A **runtime-granted** flashback cost is seen. A graveyard card with
    /// no printed flashback is granted one by a Snapcaster-shaped transient
    /// continuous effect, and the gate resolves it.
    ///
    /// REVERT THAT REDDENS IT: replace `effective_flashback_cost(state, id)`
    /// with a direct `object.base_keywords.iter()` scan (or add the
    /// `base_keywords` negative early-out the module docstring rejects) → the
    /// granted keyword is invisible → the gate goes silent.
    ///
    /// This is the row that makes the docstring's rejection of that "cheap
    /// prefilter" executable rather than rhetorical.
    #[test]
    fn runtime_granted_flashback_sacrifice_is_gated() {
        let mut state = base_state();
        let granter = permanent(&mut state, AI, "Snapcaster Mage");
        plain_land(&mut state, AI);
        let (obj, card) = spell(&mut state, Zone::Graveyard, None);

        assert!(
            state.objects.get(&obj).unwrap().base_keywords.is_empty(),
            "fixture premise: the card must carry NO printed flashback, so only \
             the runtime grant can supply it"
        );
        state.add_transient_continuous_effect(
            granter,
            AI,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: obj },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flashback(FlashbackCost::NonMana(sac(
                    typed(TypeFilter::Land),
                    1,
                ))),
            }],
            None,
        );

        assert!(
            engine::game::keywords::effective_flashback_cost(&state, obj).is_some(),
            "fixture premise: the grant must be live at the engine's authority"
        );
        assert_rejected(&verdict_with_plan(&state, obj, card), 1, 1, 0);
    }

    // --- F10 / F11: the renewable split, as a matched pair ------------------

    /// F10. A needed **Treasure** as the only fodder does NOT gate: CR 701.21a —
    /// sacrificing a one-shot self-sacrificing source *is* using it, so paying a
    /// cost with it is its intended use, not a loss of development.
    ///
    /// The shared `mana_role` and plan predicate both classify the Treasure as a
    /// non-source, so it cannot be promoted by a development deficit it cannot
    /// reduce.
    #[test]
    fn one_shot_mana_token_is_spare_not_a_needed_source() {
        let mut state = base_state();
        let treasure = treasure_token(&mut state, AI);
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Required(sac(
                typed(TypeFilter::Artifact),
                1,
            ))),
        );

        assert_eq!(
            mana_role(&state, treasure),
            ManaRole::None,
            "fixture premise: a one-shot mana token is not standing development"
        );
        assert!(realized_plan(&state).mana_behind > 0);

        assert_stood_down(&verdict_with_plan(&state, obj, card));
    }

    /// F11. Positive reach-guard for F10: a needed **renewable** mana rock as the
    /// only fodder DOES gate. Without this row, F10's stand-down would be
    /// satisfiable by a gate that never fires at all.
    ///
    /// NO REVERT OF ITS OWN, by design (NIT-5): F11 is the positive half of a
    /// matched pair and is discharged by its partner. F10's revert is what makes
    /// the pair meaningful.
    ///
    /// WORLD CHECK (§8.2b), read from `card_value.rs` at implementation time
    /// rather than assumed: `keep_tier`'s `ManaRole::Accelerant` arm reads
    /// `plan.map(|p| p.mana_behind)` — **World A holds**, so the premise guard
    /// below is `mana_behind > 0`. If that arm is ever re-gated to
    /// `lands_behind`, this is the row to rewrite.
    #[test]
    fn needed_renewable_mana_rock_as_only_fodder_is_rejected() {
        let mut state = base_state();
        let rock = mana_rock(&mut state, AI);
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Required(sac(
                typed(TypeFilter::Artifact),
                1,
            ))),
        );

        assert_eq!(mana_role(&state, rock), ManaRole::Accelerant);
        assert!(realized_plan(&state).mana_behind > 0);

        assert_rejected(&verdict_with_plan(&state, obj, card), 1, 1, 0);
    }

    // --- F12: anti-vacuity -------------------------------------------------

    /// F12. An **unpayable** cost yields no gate. When the cost cannot be paid at
    /// all the CAST is illegal (CR 601.2e → CR 733.1), and legality, not tactics,
    /// is the right authority.
    ///
    /// REVERT THAT REDDENS IT: delete the `eligible.len() < required` guard →
    /// `spare(0) < required(1)` is trivially true on an empty set → a reject
    /// appears.
    #[test]
    fn unpayable_sacrifice_cost_does_not_reject() {
        let mut state = base_state();
        artifact_land(&mut state, AI);
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Required(sac(
                typed(TypeFilter::Enchantment),
                1,
            ))),
        );

        assert!(
            realized_plan(&state).lands_behind > 0,
            "fixture premise: the plan is behind, so a non-empty eligible set \
             WOULD have gated — the stand-down is caused by emptiness alone"
        );
        assert_stood_down(&verdict_with_plan(&state, obj, card));
    }

    // --- F13: the two-site collision --------------------------------------

    /// F13. A graveyard card carrying **both** a `Required` additional-cost
    /// sacrifice and a flashback sacrifice stands the gate down. CR 601.2f
    /// COMPOSES an alternative and an additional cost — such a cast pays both —
    /// and one `SacrificeCost` cannot represent two eligible sets, so the gate
    /// declines rather than pinning a precedence the rules do not grant.
    ///
    /// REVERT THAT REDDENS IT: change `(Some(_), Some(_)) => None` to
    /// `(Some(cost), Some(_)) => Some(cost)` → the additional-cost leg gates → a
    /// reject appears.
    ///
    /// THREE-PART REACH-GUARD, because "neutral" is also what a broken
    /// extraction produces: (i) the verdict stands down; (ii) with the flashback
    /// removed the same object yields `Sacrifice(Land, 1)`; (iii) with the
    /// additional cost removed it yields `Sacrifice(Creature, 2)`.
    ///
    /// **Synthetic by construction** — measured population is 0 shipped cards.
    /// Every real co-occurrence in the corpus is Buyback with an `Optional`
    /// additional cost, which `required_sacrifice_leg` already declines.
    #[test]
    fn two_cost_sites_on_one_object_stand_the_gate_down() {
        let mut state = base_state();
        plain_land(&mut state, AI);
        mana_dork(&mut state, AI);
        mana_dork(&mut state, AI);
        let (obj, card) = spell(
            &mut state,
            Zone::Graveyard,
            Some(AdditionalCost::Required(sac(typed(TypeFilter::Land), 1))),
        );
        print_flashback(&mut state, obj, sac(typed(TypeFilter::Creature), 2));

        // (i) the collision stands the gate down.
        assert_stood_down(&verdict_with_plan(&state, obj, card));

        // (ii) the additional-cost site alone parses.
        let mut only_additional = state.clone();
        only_additional
            .objects
            .get_mut(&obj)
            .unwrap()
            .base_keywords
            .clear();
        assert_eq!(
            mandatory_sacrifice(&only_additional, only_additional.objects.get(&obj).unwrap()),
            Some(SacrificeCost::count(typed(TypeFilter::Land), 1)),
            "reach-guard (ii): the additional-cost site must parse on its own"
        );

        // (iii) the flashback site alone parses.
        let mut only_flashback = state.clone();
        only_flashback
            .objects
            .get_mut(&obj)
            .unwrap()
            .additional_cost = None;
        assert_eq!(
            mandatory_sacrifice(&only_flashback, only_flashback.objects.get(&obj).unwrap()),
            Some(SacrificeCost::count(typed(TypeFilter::Creature), 2)),
            "reach-guard (iii): the flashback site must parse on its own"
        );
    }

    // --- F14: the declinable sibling ---------------------------------------

    /// F14. An `Optional` sacrifice additional cost is never gated — the AI is
    /// not forced into the loss, and declining is already modelled in
    /// `search.rs`'s optional-cost-choice arm, so gating would double-count.
    ///
    /// REVERT THAT REDDENS IT: make the `Optional { .. }` arm of
    /// `required_sacrifice_leg` fall through to the `Required` behaviour.
    #[test]
    fn optional_sacrifice_cost_is_not_gated() {
        let mut state = base_state();
        artifact_land(&mut state, AI);
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Optional {
                cost: sac(artifact_or_creature(), 1),
                repeatability: Default::default(),
            }),
        );

        assert!(
            realized_plan(&state).lands_behind > 0,
            "fixture premise: the identical board DOES gate under `Required` \
             (F1), so the stand-down is caused by the variant alone"
        );
        assert_stood_down(&verdict_with_plan(&state, obj, card));
    }

    // --- F15: registration --------------------------------------------------

    /// F15. The policy is actually registered and routed. A `verdict()`-level
    /// test cannot catch a missing registration — every other row here would
    /// still pass with the policy unregistered.
    ///
    /// REVERT THAT REDDENS IT: delete the `Box::new(SacrificeCostManaGatePolicy)`
    /// push in `registry.rs`.
    ///
    /// It also pins §4.4's load-bearing `decision_kinds` narrowness: widening to
    /// `ActivateAbility` would put `PassPriority` in this policy's reach and
    /// break the argument that this gate can never trigger the uniform-priors
    /// fallback.
    #[test]
    fn gate_is_registered_and_routed_to_cast_spell() {
        assert!(PolicyRegistry::shared().has_policy(PolicyId::SacrificeCostManaGate));
        assert_eq!(
            SacrificeCostManaGatePolicy.decision_kinds(),
            &[DecisionKind::CastSpell]
        );
    }

    // --- F16: the zone split ------------------------------------------------

    /// F16. A **hand** cast of a card that also carries flashback reads only the
    /// additional cost — the executable form of CR 702.34a's zone restriction,
    /// and of the claim that a non-graveyard cast performs NO keyword
    /// resolution.
    ///
    /// REVERT THAT REDDENS IT: delete the `object.zone != Zone::Graveyard` early
    /// return so both sites are consulted in every zone. The hand card then
    /// reaches `match (from_additional, from_flashback)` with BOTH arms `Some`,
    /// which returns `None` → the verdict becomes neutral. The row asserts a
    /// reject carrying `required == 1`, so it reddens **as a reject-vs-neutral
    /// failure** — not, as one might expect, by the `required` fact changing
    /// from 1 to 2. The collision arm fires first and there is no reject left to
    /// carry a fact.
    ///
    /// Asserting on `required` is what distinguishes "the zone split works" from
    /// "the two-site collision fired".
    #[test]
    fn hand_cast_of_a_flashback_card_reads_only_the_additional_cost() {
        let mut state = base_state();
        plain_land(&mut state, AI);
        let (obj, card) = spell(
            &mut state,
            Zone::Hand,
            Some(AdditionalCost::Required(sac(typed(TypeFilter::Land), 1))),
        );
        print_flashback(&mut state, obj, sac(typed(TypeFilter::Creature), 2));

        assert!(realized_plan(&state).lands_behind > 0);
        // required == 1 is the discriminator: the LAND leg fired, not the
        // creature leg, and not the collision stand-down.
        assert_rejected(&verdict_with_plan(&state, obj, card), 1, 1, 0);
    }
}
