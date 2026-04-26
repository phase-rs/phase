//! `RedundancyAvoidancePolicy` — cross-cutting tactical signal that
//! penalises activated abilities and spell casts whose effects produce
//! already-active game state.
//!
//! Motivation. Prior to this policy, the AI's softmax treated "discard for
//! redundant effect" and "pump-already-pumped" activations as net-positive
//! because the discard cost was being refunded elsewhere (e.g., Monument to
//! Endurance replaces the discarded card) and the ability itself scored
//! weakly positive. The defence-in-depth per-source activation cap
//! (`MAX_ACTIVATIONS_PER_SOURCE_PER_TURN`) bounds the runaway, but the AI
//! still burns cycles searching loops whose gain is nil. This policy makes
//! the scoring honest by detecting the redundant-outcome shape and emitting
//! a typed negative `delta`.
//!
//! Design.
//! - The policy fires on `CastSpell` and `ActivateAbility`.
//! - `verdict()` walks the candidate's effect chain via `ctx.effects()`,
//!   dispatches per `Effect` variant in an exhaustive `match`, and sums the
//!   redundancy contribution from each arm.
//! - Exhaustiveness is the coverage tracker: new `Effect` variants force
//!   a compile-time decision about whether they admit a redundancy check.
//!
//! Shipped predicates (see `redundancy_delta` arms):
//! - `Tap` — every candidate target is already tapped.
//! - `Untap` — every candidate target is already untapped.
//! - `Pump` — every candidate target already has an active
//!   `UntilEndOfTurn` pump from this same source with matching P/T.
//! - `GainLife` — controller's life ≥ `LIFE_DIMINISHING_RETURNS`.
//! - `DealDamage` / `Draw` — the `QuantityExpr` resolves to 0.
//! - `GenericEffect` granting a keyword — every candidate target already
//!   has that keyword effectively.
//! - `Animate` granting keywords — every candidate target already has all
//!   granted keywords.
//!
//! TODOs for follow-up shipments (exhaustive-match arms intentionally
//! return `None` for these categories today):
//! - `AddCounter` — accumulating +1/+1 counters is almost always
//!   beneficial; would need a deeper "counter-doubling payoff absent"
//!   check before penalising.
//! - `Discard` — penalise "opponent discards" when opponent's hand is
//!   known-empty (requires information-asymmetry handling).
//! - Multi-turn projection (draw into a full hand when discard is coming).

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::keywords::has_keyword;
use engine::game::quantity::resolve_quantity;
use engine::types::ability::{
    ContinuousModification, Duration, Effect, GainLifePlayer, QuantityExpr, StaticDefinition,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, TransientContinuousEffect};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Life threshold at which further life gain is treated as redundant.
/// Chosen well above any opening-life total (20) so we never penalise early
/// stabilising lifegain; 30+ is deep into diminishing-returns territory
/// where an extra 2-3 life is unlikely to affect the winning line.
const LIFE_DIMINISHING_RETURNS: i32 = 30;

/// Reason kind emitted on every `Score` verdict. Per-arm detail lives in
/// `PolicyReason.facts` (`source_id`, `effect_kind`, etc.).
const REASON_KIND: &str = "redundancy_avoidance_score";

/// Effect-kind discriminant encoded into `PolicyReason.facts` so attribution
/// traces can distinguish which predicate fired without parsing free text.
/// These identifiers are frozen — do not renumber existing entries.
const KIND_TAP: i64 = 0;
const KIND_PUMP: i64 = 1;
const KIND_GAIN_LIFE: i64 = 2;
const KIND_DEAL_DAMAGE_ZERO: i64 = 3;
const KIND_DRAW_ZERO: i64 = 4;
const KIND_GENERIC_KEYWORD: i64 = 5;
const KIND_ANIMATE_KEYWORDS: i64 = 6;
const KIND_UNTAP: i64 = 7;

pub struct RedundancyAvoidancePolicy;

impl TacticalPolicy for RedundancyAvoidancePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::RedundancyAvoidance
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell, DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let source_id = match &ctx.candidate.action {
            GameAction::CastSpell { object_id, .. } => *object_id,
            GameAction::ActivateAbility { source_id, .. } => *source_id,
            _ => {
                return PolicyVerdict::Score {
                    delta: 0.0,
                    reason: PolicyReason::new(REASON_KIND),
                }
            }
        };

        // Sum redundancy contributions across the entire effect chain.
        // `last_fact` carries the terminal arm's facts for attribution —
        // multi-effect chains produce one representative fact entry, which
        // is enough context to find the culprit activation in traces
        // without bloating `PolicyReason` with per-arm arrays.
        let mut total = 0.0;
        let mut last_fact: Option<(i64, i64)> = None;
        for effect in ctx.effects() {
            if let Some((delta, kind_tag, extra)) =
                redundancy_delta(ctx.state, effect, source_id, ctx.ai_player)
            {
                total += delta;
                last_fact = Some((kind_tag, extra));
            }
        }

        let mut reason = PolicyReason::new(REASON_KIND);
        if let Some((kind_tag, extra)) = last_fact {
            reason = reason
                .with_fact("source_id", source_id.0 as i64)
                .with_fact("effect_kind", kind_tag)
                .with_fact("redundant_value", extra);
        }
        PolicyVerdict::Score {
            delta: total,
            reason,
        }
    }
}

/// Dispatch a single `Effect` to its redundancy predicate, returning
/// `Some((delta, kind_tag, extra))` when the effect is judged redundant.
///
/// The `match` is exhaustive on `Effect`: every variant that ships without
/// a redundancy check is listed explicitly and returns `None`. This is the
/// architectural coverage tracker — adding a new `Effect` variant forces a
/// compile-time decision here.
///
/// `extra` carries a per-arm-specific integer fact for attribution:
///   - Tap/Untap: count of matched tapped/untapped targets
///   - Pump: `power * 100 + toughness` (power dominates; tolerates ±99)
///   - GainLife: current life total
///   - DealDamage/Draw: resolved quantity (0)
///   - Generic/Animate keyword: count of granted keywords already present
fn redundancy_delta(
    state: &GameState,
    effect: &Effect,
    source_id: ObjectId,
    ai_player: PlayerId,
) -> Option<(f64, i64, i64)> {
    match effect {
        Effect::Tap { target } => tap_redundancy(state, source_id, target),
        Effect::Untap { target } => untap_redundancy(state, source_id, target),
        Effect::Pump {
            power,
            toughness,
            target,
        } => pump_redundancy(state, source_id, power, toughness, target),
        Effect::GainLife { amount, player } => {
            gain_life_redundancy(state, source_id, ai_player, amount, player)
        }
        Effect::DealDamage { amount, .. } => zero_quantity_redundancy(
            state,
            source_id,
            ai_player,
            amount,
            KIND_DEAL_DAMAGE_ZERO,
            /* delta= */ -3.0,
        ),
        Effect::Draw { count, .. } => zero_quantity_redundancy(
            state,
            source_id,
            ai_player,
            count,
            KIND_DRAW_ZERO,
            /* delta= */ -3.0,
        ),
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => generic_effect_keyword_redundancy(state, source_id, static_abilities, target.as_ref()),
        Effect::Animate {
            keywords, target, ..
        } => animate_keyword_redundancy(state, source_id, keywords, target),

        // ----- Variants with no shipped redundancy check -----
        //
        // Each arm below explicitly returns `None`. Adding a new `Effect`
        // variant without extending this list is a compile error — that's
        // the coverage tracker at work.
        Effect::StartYourEngines { .. }
        | Effect::IncreaseSpeed { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::Counter { .. }
        | Effect::Token { .. }
        | Effect::LoseLife { .. }
        | Effect::TapAll { .. }
        | Effect::UntapAll { .. }
        | Effect::AddCounter { .. }
        | Effect::RemoveCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::DestroyAll { .. }
        | Effect::BounceAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::Proliferate
        | Effect::Populate
        | Effect::Clash
        | Effect::Vote { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::BecomeCopy { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::RegisterBending { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::SearchLibrary { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::Suspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::SolveCase
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::PreventDamage { .. }
        | Effect::LoseTheGame
        | Effect::WinTheGame
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::Detain { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::ExtraTurn { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::AdditionalCombatPhase { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::Conjure { .. }
        | Effect::Tribute { .. }
        | Effect::Unimplemented { .. }
        // CR 702.85a: Cascade has no targets or redundancy — the redundancy
        // policy treats it as a no-op here; the cascade resolver handles the
        // cast-or-decline choice through its own WaitingFor state.
        | Effect::Cascade
        | Effect::Reveal { .. }
        // CR 702.xxx: Prepare (Strixhaven) — no redundancy detection.
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        // CR 702.94a: MiracleCast is an internal engine trigger effect — no redundancy.
        | Effect::MiracleCast { .. }
        // CR 702.35a: MadnessCast is an internal engine trigger effect — no redundancy.
        | Effect::MadnessCast { .. }
        // CR 122.1: LoseAllPlayerCounters is redundant only if no player in scope
        // has any counters. Not worth a dedicated predicate — fall through to None.
        | Effect::LoseAllPlayerCounters { .. }
        // CR 701.20a: RevealFromHand prompts a reveal-or-decline choice; its value
        // depends on the on_decline branch and game state — no simple redundancy signal.
        | Effect::RevealFromHand { .. }
        // CR 700.2: ChooseOneOf offers the controller a runtime choice between
        // branches — redundancy would require evaluating each branch in turn,
        // which is beyond this policy's scope. Fall through to None.
        | Effect::ChooseOneOf { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Predicate helpers
// ---------------------------------------------------------------------------

/// Collect the object IDs the given `TargetFilter` resolves to from the
/// ability's perspective. `SelfRef` short-circuits to the source; every
/// other filter enumerates battlefield matches via the unified
/// `matches_target_filter` entry point.
///
/// Returns an empty `Vec` if the filter matches nothing — callers interpret
/// this as "no redundancy signal" (the activation is already illegal or
/// edge-case).
fn resolved_candidate_targets(
    state: &GameState,
    source_id: ObjectId,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    if matches!(target, TargetFilter::SelfRef) {
        return vec![source_id];
    }
    let filter_ctx = FilterContext::from_source(state, source_id);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&obj_id| matches_target_filter(state, obj_id, target, &filter_ctx))
        .collect()
}

/// Tap-on-tapped: every candidate match is already `obj.tapped == true`.
fn tap_redundancy(
    state: &GameState,
    source_id: ObjectId,
    target: &TargetFilter,
) -> Option<(f64, i64, i64)> {
    let candidates = resolved_candidate_targets(state, source_id, target);
    if candidates.is_empty() {
        return None;
    }
    let all_tapped = candidates
        .iter()
        .all(|id| state.objects.get(id).is_some_and(|o| o.tapped));
    if all_tapped {
        Some((-3.0, KIND_TAP, candidates.len() as i64))
    } else {
        None
    }
}

/// Untap-on-untapped: symmetric to `tap_redundancy`. Every candidate match
/// is already untapped, so the Untap effect is a no-op on its target set.
fn untap_redundancy(
    state: &GameState,
    source_id: ObjectId,
    target: &TargetFilter,
) -> Option<(f64, i64, i64)> {
    let candidates = resolved_candidate_targets(state, source_id, target);
    if candidates.is_empty() {
        return None;
    }
    let all_untapped = candidates
        .iter()
        .all(|id| state.objects.get(id).is_some_and(|o| !o.tapped));
    if all_untapped {
        Some((-3.0, KIND_UNTAP, candidates.len() as i64))
    } else {
        None
    }
}

/// Pump-already-active: every candidate match already carries a
/// `UntilEndOfTurn` transient continuous effect from this same source whose
/// modifications include the requested AddPower/AddToughness values.
///
/// Narrow scope (same source only) is deliberate — cross-source pumps
/// stack legitimately and should not be penalised. The pathology this arm
/// exists to catch is the same ability re-activated within one turn.
fn pump_redundancy(
    state: &GameState,
    source_id: ObjectId,
    power: &engine::types::ability::PtValue,
    toughness: &engine::types::ability::PtValue,
    target: &TargetFilter,
) -> Option<(f64, i64, i64)> {
    use engine::types::ability::PtValue;
    // Only fixed P/T are handled — variable/quantity pumps may resolve
    // differently on each activation (depends on game state), so treating
    // them as "same modifications" would be unsafe.
    let (p, t) = match (power, toughness) {
        (PtValue::Fixed(p), PtValue::Fixed(t)) => (*p, *t),
        _ => return None,
    };
    // A zero-zero pump is already caught elsewhere if the quantity is 0;
    // skip here to keep arm semantics orthogonal.
    if p == 0 && t == 0 {
        return None;
    }
    let candidates = resolved_candidate_targets(state, source_id, target);
    if candidates.is_empty() {
        return None;
    }
    let all_redundant = candidates
        .iter()
        .all(|&obj_id| object_has_active_same_source_pump(state, source_id, obj_id, p, t));
    if all_redundant {
        // Encode (p, t) for attribution: power * 100 + toughness. Pump
        // values in practice are single-digit; the encoding tolerates ±99
        // either axis without overflow while staying readable in traces.
        Some((-1.5, KIND_PUMP, (p as i64) * 100 + (t as i64)))
    } else {
        None
    }
}

/// True iff `obj_id` is affected by an active UEOT transient continuous
/// effect sourced from `source_id` whose modifications match the given
/// `(power, toughness)` additive pair.
fn object_has_active_same_source_pump(
    state: &GameState,
    source_id: ObjectId,
    obj_id: ObjectId,
    power: i32,
    toughness: i32,
) -> bool {
    state
        .transient_continuous_effects
        .iter()
        .any(|tce| tce_matches_pump(tce, state, source_id, obj_id, power, toughness))
}

fn tce_matches_pump(
    tce: &TransientContinuousEffect,
    state: &GameState,
    source_id: ObjectId,
    obj_id: ObjectId,
    power: i32,
    toughness: i32,
) -> bool {
    if tce.source_id != source_id {
        return false;
    }
    if !matches!(tce.duration, Duration::UntilEndOfTurn) {
        return false;
    }
    let filter_ctx = FilterContext::from_source(state, source_id);
    if !matches_target_filter(state, obj_id, &tce.affected, &filter_ctx) {
        return false;
    }
    let has_power = power == 0
        || tce
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddPower { value } if *value == power));
    let has_toughness = toughness == 0
        || tce.modifications.iter().any(
            |m| matches!(m, ContinuousModification::AddToughness { value } if *value == toughness),
        );
    has_power && has_toughness
}

/// Gain-life-when-comfortable: controller's current life ≥
/// `LIFE_DIMINISHING_RETURNS`, and the life gain is directed at the
/// controller (the default `GainLifePlayer::Controller`).
fn gain_life_redundancy(
    state: &GameState,
    source_id: ObjectId,
    ai_player: PlayerId,
    amount: &QuantityExpr,
    player: &GainLifePlayer,
) -> Option<(f64, i64, i64)> {
    if !matches!(player, GainLifePlayer::Controller) {
        return None;
    }
    let controller = state
        .objects
        .get(&source_id)
        .map(|o| o.controller)
        .unwrap_or(ai_player);
    let life = state.players[controller.0 as usize].life;
    if life < LIFE_DIMINISHING_RETURNS {
        return None;
    }
    let resolved = resolve_quantity(state, amount, controller, source_id);
    if resolved <= 0 {
        return None;
    }
    Some((-0.5, KIND_GAIN_LIFE, life as i64))
}

/// Zero-quantity detector for damage/draw effects: the `QuantityExpr`
/// resolves to 0 given the current state. Applies equally to `DealDamage`
/// and `Draw` because both degenerate to no-ops at quantity 0.
fn zero_quantity_redundancy(
    state: &GameState,
    source_id: ObjectId,
    ai_player: PlayerId,
    amount: &QuantityExpr,
    kind_tag: i64,
    delta: f64,
) -> Option<(f64, i64, i64)> {
    let controller = state
        .objects
        .get(&source_id)
        .map(|o| o.controller)
        .unwrap_or(ai_player);
    let resolved = resolve_quantity(state, amount, controller, source_id);
    if resolved == 0 {
        Some((delta, kind_tag, 0))
    } else {
        None
    }
}

/// `GenericEffect` redundancy: the effect's static abilities grant one or
/// more keywords (via `ContinuousModification::AddKeyword`), and every
/// candidate target already effectively has each granted keyword.
fn generic_effect_keyword_redundancy(
    state: &GameState,
    source_id: ObjectId,
    static_abilities: &[StaticDefinition],
    target: Option<&TargetFilter>,
) -> Option<(f64, i64, i64)> {
    let target = target?;
    let granted = collect_keyword_grants(static_abilities);
    if granted.is_empty() {
        return None;
    }
    let candidates = resolved_candidate_targets(state, source_id, target);
    if candidates.is_empty() {
        return None;
    }
    let all_redundant = candidates.iter().all(|id| {
        state
            .objects
            .get(id)
            .is_some_and(|o| granted.iter().all(|k| has_keyword(o, k)))
    });
    if all_redundant {
        Some((-2.0, KIND_GENERIC_KEYWORD, granted.len() as i64))
    } else {
        None
    }
}

/// Walk `StaticDefinition.modifications` and collect the keywords that
/// would be granted. Other modification kinds (AddPower, GrantAbility,
/// etc.) are ignored here — this predicate is specifically about keyword
/// grants.
fn collect_keyword_grants(static_abilities: &[StaticDefinition]) -> Vec<Keyword> {
    let mut out = Vec::new();
    for stat in static_abilities {
        for modification in &stat.modifications {
            if let ContinuousModification::AddKeyword { keyword } = modification {
                out.push(keyword.clone());
            }
        }
    }
    out
}

/// `Animate` redundancy: every candidate target already has each of the
/// granted keywords. Mirrors the `GenericEffect` keyword arm but reads from
/// the `Animate.keywords` slice directly.
fn animate_keyword_redundancy(
    state: &GameState,
    source_id: ObjectId,
    keywords: &[Keyword],
    target: &TargetFilter,
) -> Option<(f64, i64, i64)> {
    if keywords.is_empty() {
        return None;
    }
    let candidates = resolved_candidate_targets(state, source_id, target);
    if candidates.is_empty() {
        return None;
    }
    let all_redundant = candidates.iter().all(|id| {
        state
            .objects
            .get(id)
            .is_some_and(|o| keywords.iter().all(|k| has_keyword(o, k)))
    });
    if all_redundant {
        Some((-2.0, KIND_ANIMATE_KEYWORDS, keywords.len() as i64))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::cast_facts::cast_facts_for_action;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::policies::registry::PolicyRegistry;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, PtValue, QuantityExpr, TargetFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::CardId;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

    fn mk_ctx<'a>(
        state: &'a GameState,
        decision: &'a AiDecisionContext,
        candidate: &'a CandidateAction,
        config: &'a AiConfig,
        ai_ctx: &'a AiContext,
    ) -> PolicyContext<'a> {
        let cast_facts = cast_facts_for_action(state, &candidate.action, PlayerId(0));
        PolicyContext {
            state,
            decision,
            candidate,
            ai_player: PlayerId(0),
            config,
            context: ai_ctx,
            cast_facts,
        }
    }

    fn make_creature_with_ability(state: &mut GameState, name: &str, effect: Effect) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(state.objects.len() as u64 + 1),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities)
            .push(AbilityDefinition::new(AbilityKind::Activated, effect));
        obj_id
    }

    fn activate_candidate(source_id: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Ability,
            },
        }
    }

    fn priority_decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        }
    }

    #[test]
    fn tap_on_tapped_source_penalized() {
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Tapper",
            Effect::Tap {
                target: TargetFilter::SelfRef,
            },
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(delta, -3.0, "tap on tapped should emit -3.0 delta");
    }

    #[test]
    fn tap_on_untapped_source_not_penalized() {
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Tapper",
            Effect::Tap {
                target: TargetFilter::SelfRef,
            },
        );
        // default tapped = false

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(delta, 0.0, "tap on untapped should not penalise");
    }

    #[test]
    fn untap_on_untapped_source_penalized() {
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Untapper",
            Effect::Untap {
                target: TargetFilter::SelfRef,
            },
        );
        // default tapped = false -- so untap is a no-op on this target set

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(delta, -3.0, "untap on untapped should emit -3.0 delta");
    }

    #[test]
    fn untap_on_tapped_source_not_penalized() {
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Untapper",
            Effect::Untap {
                target: TargetFilter::SelfRef,
            },
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(delta, 0.0, "untap on tapped should not penalise");
    }

    #[test]
    fn walking_ballista_deal_damage_not_penalized() {
        // Walking Ballista's ability is "Remove +1/+1 counter → deal 1 damage".
        // The DealDamage(Fixed(1)) must not trigger zero-quantity redundancy.
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Walking Ballista",
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(
            delta, 0.0,
            "Walking Ballista's 1-damage ability is not redundant"
        );
    }

    #[test]
    fn deal_damage_zero_penalized() {
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Zero Blast",
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(delta, -3.0, "DealDamage(0) should emit -3.0 delta");
    }

    #[test]
    fn gain_life_excess_penalized_above_threshold() {
        let mut state = GameState::new_two_player(0);
        state.players[0].life = LIFE_DIMINISHING_RETURNS + 5;
        let obj_id = make_creature_with_ability(
            &mut state,
            "Lifegainer",
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: GainLifePlayer::Controller,
            },
        );

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(delta, -0.5, "high-life lifegain should emit -0.5 delta");
    }

    #[test]
    fn gain_life_not_penalized_below_threshold() {
        let mut state = GameState::new_two_player(0);
        // default life is 20 — well below threshold
        let obj_id = make_creature_with_ability(
            &mut state,
            "Lifegainer",
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: GainLifePlayer::Controller,
            },
        );

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(delta, 0.0, "low-life lifegain should not penalise");
    }

    #[test]
    fn generic_effect_already_has_keyword_penalized() {
        let mut state = GameState::new_two_player(0);
        let stat = StaticDefinition::new(StaticMode::Continuous).modifications(vec![
            ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            },
        ]);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Gains Flying",
            Effect::GenericEffect {
                static_abilities: vec![stat],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::SelfRef),
            },
        );
        // Pre-existing flying on the source — the grant is redundant.
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(
            delta, -2.0,
            "redundant keyword grant should emit -2.0 delta"
        );
    }

    #[test]
    fn generic_effect_new_keyword_not_penalized() {
        let mut state = GameState::new_two_player(0);
        let stat = StaticDefinition::new(StaticMode::Continuous).modifications(vec![
            ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            },
        ]);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Gains Flying",
            Effect::GenericEffect {
                static_abilities: vec![stat],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::SelfRef),
            },
        );
        // No pre-existing flying on source — the grant is new value.

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(delta, 0.0, "new keyword grant should not penalise");
    }

    #[test]
    fn pump_already_active_ueot_penalized() {
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Self-Pumper",
            Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TargetFilter::SelfRef,
            },
        );
        // Simulate a prior activation having already registered a UEOT pump
        // from this same source.
        state.add_transient_continuous_effect(
            obj_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: obj_id },
            vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ],
            None,
        );

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(
            delta, -1.5,
            "re-activated same-source UEOT pump should emit -1.5 delta"
        );
    }

    #[test]
    fn pump_new_values_not_penalized() {
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Self-Pumper",
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::SelfRef,
            },
        );
        // Existing TCE is +1/+1; the candidate activation is +2/+2 — different
        // value, so it is NOT redundant.
        state.add_transient_continuous_effect(
            obj_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: obj_id },
            vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ],
            None,
        );

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(delta, 0.0, "different pump values should not penalise");
    }

    #[test]
    fn sub_ability_chain_redundancies_sum() {
        // Verify ctx.effects() walks sub_ability chains AND the policy sums
        // per-effect redundancy contributions. Build a chained ability where
        // BOTH the main effect AND sub-ability effect are redundant
        // (DealDamage(0) + Draw(0)) — expected total = -3.0 + -3.0 = -6.0.
        let mut state = GameState::new_two_player(0);
        let next_card_id = state.objects.len() as u64 + 1;
        let obj_id = create_object(
            &mut state,
            CardId(next_card_id),
            PlayerId(0),
            "Zero Everything".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        )));
        Arc::make_mut(&mut obj.abilities).push(ability);

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(
            delta, -6.0,
            "sub-ability chain with two zero-quantity effects should sum"
        );
    }

    #[test]
    fn end_to_end_via_policy_registry() {
        // Confirm the policy is wired into the default registry and produces
        // a RedundancyAvoidance verdict for a classifiable ActivateAbility.
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Zero Blast",
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(obj_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let registry = PolicyRegistry::default();
        let verdicts = registry.verdicts(&ctx);
        let found = verdicts.iter().any(|(id, v)| {
            matches!(id, PolicyId::RedundancyAvoidance)
                && matches!(v, PolicyVerdict::Score { delta, .. } if *delta < 0.0)
        });
        assert!(
            found,
            "RedundancyAvoidance should fire with a negative delta for DealDamage(0); \
             got verdicts: {:?}",
            verdicts
                .iter()
                .map(|(id, v)| (id, format!("{v:?}")))
                .collect::<Vec<_>>()
        );
    }
}
