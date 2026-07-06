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
//! - `DealDamage` / `Draw` / `AddCounter` — the `QuantityExpr` (`amount`/
//!   `count`) resolves to 0, so the effect is a strict no-op.
//! - `GenericEffect` granting a keyword — every candidate target already
//!   has that keyword effectively.
//! - `Animate` granting keywords — every candidate target already has all
//!   granted keywords.
//!
//! TODOs for follow-up shipments (exhaustive-match arms intentionally
//! return `None` for these categories today):
//! - `AddCounter` — the strictly-redundant zero-count sub-case ships above
//!   (count `QuantityExpr` resolves to 0). The broader case — accumulating
//!   +1/+1 counters is almost always beneficial — is still deferred; it would
//!   need a deeper "counter-doubling payoff absent" check before penalising a
//!   nonzero grant.
//! - `Discard` — penalise "opponent discards" when opponent's hand is
//!   known-empty (requires information-asymmetry handling).
//! - Multi-turn projection (draw into a full hand when discard is coming).
//!
//! Deferred (issue #563 — Whitemane Lion ETB self-bounce loop):
//! - **Per-turn cast cap (defence-in-depth)**: `MAX_ACTIVATIONS_PER_SOURCE_PER_TURN`
//!   today only bounds `ActivateAbility`. Mirroring it as a
//!   `spells_cast_this_turn_by_player[card_id]` cap would catch any future
//!   loop class that slips past content-level redundancy detection, including
//!   non-bounce self-undoing patterns. Tracked separately; not implemented here.
//! - **Parser non-targeting fix**: Whitemane Lion's ETB ("return a creature
//!   you control to its owner's hand") is non-targeting in Oracle terms,
//!   but currently parses as a targeted Bounce. Fixing the parser would make
//!   the AI choose correctly at resolution time without needing this policy
//!   to short-circuit casts. Tracked separately as an oracle-parser issue.

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::keywords::{has_flash, has_keyword};
use engine::game::quantity::resolve_quantity;
use engine::types::ability::{
    ContinuousModification, Duration, Effect, EffectScope, QuantityExpr, StaticDefinition,
    TapStateChange, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, TransientContinuousEffect};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::cast_facts::collect_definition_effects;
use crate::features::DeckFeatures;
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;

/// Life threshold at which further life gain is treated as redundant.
/// Chosen well above any opening-life total (20) so we never penalise early
/// stabilising lifegain; 30+ is deep into diminishing-returns territory
/// where an extra 2-3 life is unlikely to affect the winning line.
const LIFE_DIMINISHING_RETURNS: i32 = 30;

/// Penalty delta applied when the ETB self-bounce predicate fires (Whitemane
/// Lion class — see `bounce_self_undo_redundancy`). Promoted to a named
/// constant so the magnitude is documented and traceable rather than a bare
/// `-3.0` magic number at the predicate's `return` sites.
const BOUNCE_SELF_UNDO_DELTA: f64 = -3.0;

/// Origin layer for an effect being evaluated by the redundancy policy.
///
/// The redundancy semantics differ depending on whether the effect comes
/// from the spell's primary ability chain, or from an immediate ETB trigger
/// on a cast spell. Self-undo detection (Whitemane Lion class) only applies
/// to ETB triggers: an activated/triggered bounce on an already-resolved
/// permanent (Soulherder-style blink) is a legitimate value loop and must
/// not be penalised. A typed enum here keeps the call-site intent
/// self-documenting and leaves room to add further origin layers (e.g.,
/// `ResolutionReplacement`, `LeavesBattlefieldTrigger`) without retrofitting
/// a wider boolean parameter list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectOrigin {
    /// Effect from `ctx.effects()` — the candidate's primary ability chain.
    /// Used for activated abilities and the primary effects of cast spells.
    PrimaryAbility,
    /// Effect from `cast_facts.immediate_etb_triggers` on a `CastSpell`
    /// candidate — fires only on the first ETB after the cast resolves.
    CastImmediateEtbTrigger,
}

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
/// CR 603.6a + CR 400.7 + CR 608.2b: ETB-triggered self-bounce where the
/// only legal returnee is the source itself (or no creature qualifies). The
/// trigger fires when the permanent enters the battlefield (CR 603.6a); on
/// resolution, target legality is rechecked (CR 608.2b) and — because the
/// permanent that resolved on the battlefield is a new object distinct from
/// the spell that was cast (CR 400.7) — the only legal "creature you control"
/// is the source itself, so it re-enters the caster's hand. Whitemane Lion /
/// Stonecloaker class — left unchecked, the AI casts and re-casts in a
/// positive-scoring loop because `EtbValuePolicy` rewards the bounce without
/// distinguishing self-undo from genuine blink value.
const KIND_BOUNCE_SELF_UNDO: i64 = 8;
/// CR 122.1: An `AddCounter` whose count `QuantityExpr` resolves to 0 places
/// no counters — a strict no-op, mirroring the `DealDamage`/`Draw` zero-quantity
/// arms. The broader "diminishing returns on +1/+1 counters" case stays deferred
/// (see the module TODOs); only the strictly-redundant zero-count sub-case fires.
const KIND_ADD_COUNTER_ZERO: i64 = 9;
/// CR 601.3b: Activating a flash-cast permission (Alchemist's Refuge class)
/// when no hand spell would gain instant-speed timing.
const KIND_FLASH_CAST_PERMISSION: i64 = 10;

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
            if let Some((delta, kind_tag, extra)) = redundancy_delta(
                ctx.state,
                effect,
                source_id,
                ctx.ai_player,
                EffectOrigin::PrimaryAbility,
            ) {
                total += delta;
                last_fact = Some((kind_tag, extra));
            }
        }

        // CR 603.6a + CR 400.7: For permanent spell casts, also walk the
        // immediate ETB-trigger effect chain. CR 603.6a says ETB abilities
        // trigger when the permanent enters; CR 400.7 says the resolved
        // permanent is a new object distinct from the spell. `ctx.effects()`
        // only covers the spell's primary abilities — a vanilla creature
        // whose only on-cast interaction is an ETB trigger (e.g., Whitemane
        // Lion's "return a creature you control") would otherwise present an
        // empty effect list here, blinding the policy to ETB-self-undo loops.
        if matches!(ctx.candidate.action, GameAction::CastSpell { .. }) {
            if let Some(facts) = ctx.cast_facts() {
                for trigger in &facts.immediate_etb_triggers {
                    let Some(execute) = trigger.execute.as_deref() else {
                        continue;
                    };
                    for effect in collect_definition_effects(execute) {
                        if let Some((delta, kind_tag, extra)) = redundancy_delta(
                            ctx.state,
                            effect,
                            source_id,
                            ctx.ai_player,
                            EffectOrigin::CastImmediateEtbTrigger,
                        ) {
                            total += delta;
                            last_fact = Some((kind_tag, extra));
                        }
                    }
                }
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
///   - DealDamage/Draw/AddCounter: resolved quantity (0)
///   - Generic/Animate keyword: count of granted keywords already present
///   - Bounce self-undo: candidate-set size (0 = trigger fizzles, 1 = source-only)
///
/// `origin` records which layer the effect was pulled from. Most arms ignore
/// it; the `Bounce` arm uses it to detect the ETB-self-undo class (Whitemane
/// Lion) without penalising legitimate activated/triggered bounce abilities
/// like Soulherder.
fn redundancy_delta(
    state: &GameState,
    effect: &Effect,
    source_id: ObjectId,
    ai_player: PlayerId,
    origin: EffectOrigin,
) -> Option<(f64, i64, i64)> {
    match effect {
        // CR 701.26a/b: single-target tap/untap have redundancy checks; the
        // mass (`All`) scope has none (see the no-op list below), matching the
        // legacy `Tap`/`Untap` vs `TapAll`/`UntapAll` split.
        Effect::SetTapState {
            target,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        } => tap_redundancy(state, source_id, target),
        Effect::SetTapState {
            target,
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
        } => untap_redundancy(state, source_id, target),
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
        Effect::ApplyPostReplacementDamage { .. } => None,
        Effect::CombineHost { .. } | Effect::ChooseAugmentAndCombineWithHost { .. } => None,
        Effect::Draw { count, .. } => zero_quantity_redundancy(
            state,
            source_id,
            ai_player,
            count,
            KIND_DRAW_ZERO,
            /* delta= */ -3.0,
        ),
        // CR 122.1: An AddCounter whose count resolves to 0 places no counters
        // — a strict no-op, exactly like DealDamage(0)/Draw(0). Dynamic counts
        // (e.g. "a +1/+1 counter for each artifact you control" with no
        // artifacts) are reachable and resolve to 0 via `resolve_quantity`. The
        // broader "+1/+1 counters are almost always beneficial" / diminishing-
        // returns case remains deferred (see module TODOs) — only this strictly
        // redundant zero-count sub-case fires here.
        Effect::PutCounter { count, .. } => zero_quantity_redundancy(
            state,
            source_id,
            ai_player,
            count,
            KIND_ADD_COUNTER_ZERO,
            /* delta= */ -3.0,
        ),
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => generic_effect_keyword_redundancy(state, source_id, static_abilities, target.as_ref())
            .or_else(|| {
                generic_effect_flash_cast_permission_redundancy(state, ai_player, static_abilities)
            }),
        Effect::Animate {
            keywords, target, ..
        } => animate_keyword_redundancy(state, source_id, keywords, target),
        // CR 603.6a + CR 400.7 + CR 608.2b: ETB-triggered self-bounce is only
        // a self-undo loop when the destination is the owner's hand (the
        // default for `Effect::Bounce`). Library/Exile destinations have
        // their own value/concern axes — they don't refill the caster's hand
        // for an immediate recast — so we route only hand-destined bounces
        // through the self-undo predicate.
        Effect::Bounce {
            target,
            destination,
            ..
        } => match destination {
            None | Some(Zone::Hand) => {
                bounce_self_undo_redundancy(state, source_id, target, origin)
            }
            Some(_) => None,
        },

        // ----- Variants with no shipped redundancy check -----
        //
        // Each arm below explicitly returns `None`. Adding a new `Effect`
        // variant without extending this list is a compile error — that's
        // the coverage tracker at work.
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::Token { .. }
        | Effect::LoseLife { .. }
        // CR 701.26a/b: mass tap/untap (legacy `TapAll`/`UntapAll`) has no
        // shipped redundancy check.
        | Effect::SetTapState {
            scope: EffectScope::All,
            ..
        }
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
        | Effect::CounterAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Populate
        | Effect::Clash
        | Effect::Behold { .. }
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        // Random pool copy (Momir Basic) — not a "redundant if already
        // controlled" effect; the token's identity is chosen at resolution.
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        // CR 702.141a: Encore makes per-opponent copy tokens — like Myriad, it is
        // not a "redundant if already controlled" effect.
        | Effect::Encore
        // CR 701.42a: Meld exiles both halves of a meld pair and materializes a
        // single combined permanent — not a "redundant if already controlled" one.
        | Effect::Meld { .. }
        // CR 702.75a: HideawayConceal is an internal continuation step of the
        // Hideaway ETB trigger (turn the just-exiled card face down + link it);
        // it is never independently chosen, so it carries no redundancy signal.
        | Effect::HideawayConceal { .. }
        // CR 702.55a: ExileHaunting (the haunt ability — exile this card haunting
        // target creature) is a triggered death/resolution effect, not a
        // "redundant if already controlled" one.
        | Effect::ExileHaunting { .. }
        // CR 614.1a + CR 607.2b: Rod of Absorption's trigger stamps a resolving
        // spell with an exile-instead/linked-source rider. Its value is realized
        // by the stack resolution replacement path, so this policy has no static
        // redundancy signal to score.
        | Effect::ExileResolvingSpellInsteadOfGraveyard
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
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
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::PreventDamage { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Planeswalk
        // CR 311.7: ChaosEnsues fires the current plane's "whenever chaos ensues"
        // triggered ability — it has no target and no static redundancy signal.
        | Effect::ChaosEnsues
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::ForEachCategoryExile { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
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
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::Conjure { .. }
        | Effect::Intensify { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::Tribute { .. }
        | Effect::Unimplemented { .. }
        // CR 702.85a: Cascade has no targets or redundancy — the redundancy
        // policy treats it as a no-op here; the cascade resolver handles the
        // cast-or-decline choice through its own WaitingFor state.
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::Reveal { .. }
        // CR 702.xxx: Prepare (Strixhaven) — no redundancy detection.
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        // CR 702.171b: a permanent cannot become saddled if already saddled; no
        // static redundancy signal — leave to the resolver.
        | Effect::BecomeSaddled { .. }
        // CR 509.1h: "becomes blocked" has no static redundancy signal (the
        // target's blocked state is combat-scoped) — leave it to the resolver.
        | Effect::BecomeBlocked { .. }
        // CR 702.95c-d: PairWith mutates the source/target pair relationship;
        // redundancy depends on trigger timing and revalidation, so this policy
        // leaves it to the resolver.
        | Effect::PairWith { .. }
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
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        // CR 700.2: ChooseOneOf offers the controller a runtime choice between
        // branches — redundancy would require evaluating each branch in turn,
        // which is beyond this policy's scope. Fall through to None.
        | Effect::ChooseOneOf { .. }
        // CR 614.1a + CR 514.2: AddTargetReplacement registers a one-shot
        // replacement on the resolved target (e.g., "if that creature would
        // die this turn, exile it instead"). Its value depends on whether the
        // target later triggers the replacement event — no static redundancy
        // signal available.
        | Effect::AddTargetReplacement { .. }
        // CR 614.1 + CR 615: CreateDamageReplacement installs a one-shot
        // damage "shield" (modify/prevent/redirect the next matching damage
        // event this turn). Its value depends on whether that damage event
        // later occurs — no static redundancy signal, same as the target
        // replacement above.
        | Effect::CreateDamageReplacement { .. }
        // CR 614.11 + CR 614.6: CreateDrawReplacement installs a one-shot draw
        // "shield" ("the next time you would draw a card this turn, [effect]
        // instead"). Its value depends on whether a draw later occurs — no
        // static redundancy signal, same as the damage replacement above.
        | Effect::CreateDrawReplacement { .. }
        // CR 614.1a + CR 614.5: CreatePlaneswalkReplacement installs a continuous,
        // duration-bound planar-die planeswalk "shield" (Fixed Point in Time). Its
        // value depends on whether a planeswalk later occurs within the window — no
        // static redundancy signal, same as the draw replacement above.
        | Effect::CreatePlaneswalkReplacement { .. }
        // CR 614.12 + CR 303.4: ReturnAsAura installs an Aura conversion +
        // attach pick. Its redundancy is the new Aura's grants vs. the
        // existing static layer — out of scope for this policy.
        | Effect::ReturnAsAura { .. }
        // CR 701.12a: ExchangeLifeWithStat's value depends on the live gap
        // between a player's life and the source's stat — no static redundancy
        // signal (it never "does nothing" the way a duplicate keyword grant does).
        | Effect::ExchangeLifeWithStat { .. }
        // CR 701.12a: ExchangeLifeTotals's value depends on the live gap between
        // the two players' life totals — no static redundancy signal.
        | Effect::ExchangeLifeTotals { .. }
        // CR 701.51 + CR 701.52: Attraction open/visit — deck state dependent.
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        // Unstable Contraptions: assembly/crank/reassembly value depends on deck
        // contents, sprocket state, and board state; no static redundancy signal.
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        // CR 701.34a + CR 122.1: targeted proliferate adds one counter of each
        // kind already present — adding counters is virtually always beneficial,
        // so there is no "does nothing" static-redundancy signal here.
        | Effect::ProliferateTarget { .. }
        | Effect::ProcessRadCounters
        // Heist (MTG Arena digital-only) draws random nonland cards from the
        // opponent's library and exiles the controller's chosen one face down
        // with a permanent any-color cast-from-exile permission. The randomness
        // and the face-down / any-mana dimension make it state-dependent with
        // no static redundancy signal; the AI scores Heist via the generic
        // effect-rating path, not the redundancy-avoidance table.
        | Effect::Heist { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::RememberCard { .. }
        // CR 608.2d + CR 122.1: the counter-kind choice + its consume carry no
        // static redundancy signal (the value depends on the runtime choice).
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::HeistExile => None,
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

/// ETB-self-undo bounce: the cast spell's immediate ETB trigger returns a
/// creature to its owner's hand, but no other creature the AI controls is
/// a legal target. The trigger either fizzles (if zero legal targets exist
/// at resolution) or is forced to pick the source itself (Whitemane Lion
/// ruling: "If you don't control any other creature, you must return it").
/// Either way the cast does not stick — the source re-enters the caster's
/// hand or contributes no on-resolution value beyond a vanilla body that
/// didn't enter.
///
/// Why one branch suffices: this predicate is gated to
/// `EffectOrigin::CastImmediateEtbTrigger`, which means the source is being
/// evaluated *pre-cast* — still in hand, not yet on the battlefield.
/// `resolved_candidate_targets` queries `state.battlefield`, so the source
/// is never in the candidate set at cast-time. The "every candidate IS the
/// source" subcase is therefore unreachable at this evaluation point — the
/// `candidates.is_empty()` check is the correct and complete signal:
///   * Empty candidates pre-cast => no other creature controlled => when the
///     ETB resolves after the Lion enters, the Lion will be the only legal
///     target (or there will be none, if the source died/left). Either way
///     the cast is a no-op loop.
///   * Non-empty candidates pre-cast => at least one creature other than the
///     source is on the battlefield => legitimate target exists, no penalty.
///
/// Categorical scope:
///   - Only fires when `origin == EffectOrigin::CastImmediateEtbTrigger`.
///     Activated/triggered abilities that happen to bounce a creature you
///     control (Soulherder-style blink) must NOT be penalised — those are
///     legitimate value loops, not self-undo.
///   - The caller (the `Effect::Bounce` match arm in `redundancy_delta`) is
///     responsible for restricting this predicate to bounces whose
///     destination is the owner's hand (default `None` or explicit
///     `Some(Zone::Hand)`). Library/Exile destinations are not handled by
///     this predicate.
///
/// Returns `(BOUNCE_SELF_UNDO_DELTA, KIND_BOUNCE_SELF_UNDO, 0)` on a
/// confirmed self-undo; `None` otherwise. The `count` fact is 0 (no other
/// legal targets at cast-time) — sufficient signal for trace attribution.
fn bounce_self_undo_redundancy(
    state: &GameState,
    source_id: ObjectId,
    target: &TargetFilter,
    origin: EffectOrigin,
) -> Option<(f64, i64, i64)> {
    if origin != EffectOrigin::CastImmediateEtbTrigger {
        return None;
    }
    let candidates = resolved_candidate_targets(state, source_id, target);
    // CR 608.2b: if no other creature the AI controls is a legal target,
    // the trigger either fizzles (zero legal targets remain at resolution)
    // or is forced to return the source itself (Whitemane Lion ruling). Both
    // produce the same loop pathology; both are detected here by the
    // empty-set check at cast-time, since the source is still in hand and
    // cannot itself appear in the battlefield-queried candidate set.
    if candidates.is_empty() {
        return Some((BOUNCE_SELF_UNDO_DELTA, KIND_BOUNCE_SELF_UNDO, 0));
    }
    None
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
/// controller (the default `TargetFilter::Controller`).
fn gain_life_redundancy(
    state: &GameState,
    source_id: ObjectId,
    ai_player: PlayerId,
    amount: &QuantityExpr,
    player: &TargetFilter,
) -> Option<(f64, i64, i64)> {
    if !matches!(player, TargetFilter::Controller) {
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

/// Zero-quantity detector for damage/draw/counter effects: the `QuantityExpr`
/// resolves to 0 given the current state. Applies equally to `DealDamage`,
/// `Draw`, and `AddCounter` because each degenerates to a no-op at quantity 0.
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
/// recipient already effectively has each granted keyword.
///
/// Recipients are resolved by layer (CR 611.2 — a continuous effect's
/// affected set is defined by the ability, which may or may not be a chosen
/// target):
/// - A chosen `target` (e.g. "target creature gains flying") drives the set
///   directly — at decision time the legal-target *filter* stands in for the
///   not-yet-chosen object.
/// - A self/affected-scoped grant carries `target: None` (e.g. Prognostic
///   Sphinx's "~ gains hexproof until end of turn", whose lowered form has
///   `target: None` and a `StaticDefinition` with `affected: Some(SelfRef)`).
///   The recipients are then the union of each keyword-granting static's
///   `affected` filter. Without this fallback the policy is blind to redundant
///   self-buffs — the AI re-pays the cost (here: discarding a card) to grant a
///   keyword it already has (issue #1966).
fn generic_effect_keyword_redundancy(
    state: &GameState,
    source_id: ObjectId,
    static_abilities: &[StaticDefinition],
    target: Option<&TargetFilter>,
) -> Option<(f64, i64, i64)> {
    let granted = collect_keyword_grants(static_abilities);
    if granted.is_empty() {
        return None;
    }
    let candidates = match target {
        Some(target) => resolved_candidate_targets(state, source_id, target),
        None => resolve_affected_candidates(state, source_id, static_abilities),
    };
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

/// Resolve the recipients of a keyword-granting `GenericEffect` that carries
/// no chosen `target` — the self/affected-scoped case (Prognostic Sphinx's
/// "~ gains hexproof until end of turn"). Returns the dedup-preserving union of
/// objects matched by the `affected` filter of every static ability that
/// grants at least one keyword. A static whose `affected` is `None` defines no
/// recipient set and contributes nothing.
fn resolve_affected_candidates(
    state: &GameState,
    source_id: ObjectId,
    static_abilities: &[StaticDefinition],
) -> Vec<ObjectId> {
    let mut out = Vec::new();
    for stat in static_abilities {
        let grants_keyword = stat
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddKeyword { .. }));
        if !grants_keyword {
            continue;
        }
        let Some(affected) = stat.affected.as_ref() else {
            continue;
        };
        for id in resolved_candidate_targets(state, source_id, affected) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

fn static_grants_flash_cast(mode: &StaticMode) -> bool {
    matches!(
        mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Flash,
            ..
        } | StaticMode::CastWithFlash
    )
}

/// A sorcery-speed hand spell gains timing value from a flash-cast grant.
fn hand_spell_benefits_from_flash_grant(obj: &engine::game::game_object::GameObject) -> bool {
    if obj.card_types.core_types.contains(&CoreType::Land) {
        return false;
    }
    if obj.card_types.core_types.contains(&CoreType::Instant) {
        return false;
    }
    !has_flash(obj)
}

/// Issue #1528 — penalise activating Alchemist's Refuge-style flash grants
/// when the AI's hand has no spell that would actually gain instant speed.
fn generic_effect_flash_cast_permission_redundancy(
    state: &GameState,
    ai_player: PlayerId,
    static_abilities: &[StaticDefinition],
) -> Option<(f64, i64, i64)> {
    let flash_stats: Vec<_> = static_abilities
        .iter()
        .filter(|s| static_grants_flash_cast(&s.mode))
        .collect();
    if flash_stats.is_empty() {
        return None;
    }
    let player = state.players.iter().find(|p| p.id == ai_player)?;
    let has_beneficiary = player.hand.iter().any(|&id| {
        let Some(obj) = state.objects.get(&id) else {
            return false;
        };
        if !hand_spell_benefits_from_flash_grant(obj) {
            return false;
        }
        flash_stats.iter().any(|stat| {
            stat.affected.as_ref().is_none_or(|filter| {
                matches_target_filter(state, id, filter, &FilterContext::from_source(state, id))
            })
        })
    });
    if has_beneficiary {
        None
    } else {
        Some((-2.0, KIND_FLASH_CAST_PERMISSION, 0))
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
        AbilityDefinition, AbilityKind, BounceSelection, PtValue, QuantityExpr, TargetFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
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
                excess: None,
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
                excess: None,
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
    fn add_counter_zero_penalized() {
        // An AddCounter whose count resolves to 0 places no counters — a strict
        // no-op, exactly like DealDamage(0)/Draw(0). Must emit the -3.0 delta.
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Zero Counters",
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::SelfRef,
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
        assert_eq!(delta, -3.0, "AddCounter(0) should emit -3.0 delta");
    }

    #[test]
    fn add_counter_nonzero_not_penalized() {
        // A nonzero AddCounter places real counters — the zero-count arm must
        // NOT fire (the broader diminishing-returns case stays deferred).
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Real Counters",
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
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
            "nonzero AddCounter must not be flagged by the zero-count arm"
        );
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
                player: TargetFilter::Controller,
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
                player: TargetFilter::Controller,
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
    fn flash_cast_permission_without_sorcery_speed_hand_spell_penalized() {
        let mut state = GameState::new_two_player(0);
        let refuge_id = make_creature_with_ability(
            &mut state,
            "Alchemist's Refuge",
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::CastWithKeyword {
                    keyword: Keyword::Flash,
                })],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
        );
        let instant = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        state.players[0].hand.push_back(instant);

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(refuge_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(
            delta, -2.0,
            "flash permission with only instants in hand should be redundant"
        );
    }

    #[test]
    fn flash_cast_permission_with_sorcery_in_hand_not_penalized() {
        let mut state = GameState::new_two_player(0);
        let refuge_id = make_creature_with_ability(
            &mut state,
            "Alchemist's Refuge",
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::CastWithKeyword {
                    keyword: Keyword::Flash,
                })],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
        );
        let sorcery = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&sorcery)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
        state.players[0].hand.push_back(sorcery);

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(refuge_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(
            delta, 0.0,
            "flash permission should remain viable when a sorcery can use it"
        );
    }

    #[test]
    fn flash_cast_permission_with_already_flash_permanent_penalized() {
        let mut state = GameState::new_two_player(0);
        let refuge_id = make_creature_with_ability(
            &mut state,
            "Alchemist's Refuge",
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::CastWithKeyword {
                    keyword: Keyword::Flash,
                })],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
        );
        let artifact = create_object(
            &mut state,
            CardId(902),
            PlayerId(0),
            "Shimmer Myr".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&artifact).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.keywords.push(Keyword::Flash);
        state.players[0].hand.push_back(artifact);

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = activate_candidate(refuge_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(
            delta, -2.0,
            "flash permission should be redundant when the only affected permanent already has flash"
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

    /// Issue #1966 (Prognostic Sphinx): a self-scoped keyword grant lowers to
    /// `GenericEffect { target: None, static_abilities: [.. affected: SelfRef ..] }`.
    /// When the source already has the keyword, re-activating (paying the
    /// discard cost) is redundant and must be penalised. Before the
    /// `affected`-fallback fix the `target: None` shape returned `None`,
    /// blinding the policy to the redundant self-buff.
    #[test]
    fn generic_effect_self_scoped_already_has_keyword_penalized() {
        let mut state = GameState::new_two_player(0);
        let stat = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }]);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Prognostic Sphinx",
            Effect::GenericEffect {
                static_abilities: vec![stat],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
        );
        // Hexproof already active from a prior activation this turn — re-granting
        // is pure redundancy.
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

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
            "redundant self-scoped keyword grant should emit -2.0 delta"
        );
    }

    /// Companion to the issue #1966 case: the first activation, when the source
    /// does not yet have the keyword, grants real value and must NOT be
    /// penalised (otherwise the AI never gains hexproof at all).
    #[test]
    fn generic_effect_self_scoped_new_keyword_not_penalized() {
        let mut state = GameState::new_two_player(0);
        let stat = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }]);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Prognostic Sphinx",
            Effect::GenericEffect {
                static_abilities: vec![stat],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
        );
        // No pre-existing hexproof — the grant is new value.

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
            "first self-scoped keyword grant should not penalise"
        );
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
                excess: None,
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

    // -----------------------------------------------------------------------
    // Bounce-self-undo (issue #563) coverage — Whitemane Lion class.
    //
    // The Lion ETB returns "a creature you control" — if you control no other
    // creature, the Lion itself is the only legal target, so it re-enters
    // your hand. The AI was looping on this because EtbValuePolicy scores the
    // bounce positively, and no policy detected the self-undo.
    // -----------------------------------------------------------------------

    use engine::types::ability::{ControllerRef, TriggerDefinition, TypedFilter};
    use engine::types::triggers::TriggerMode;

    /// Helper: place a creature in hand with an ETB-triggered Bounce of the
    /// given target filter, matching the Whitemane Lion shape. Returns the
    /// hand object id and its card id, ready to construct a CastSpell candidate.
    fn make_etb_bouncer_in_hand(
        state: &mut GameState,
        name: &str,
        bounce_target: TargetFilter,
    ) -> (ObjectId, CardId) {
        make_etb_bouncer_in_hand_with_destination(state, name, bounce_target, None)
    }

    /// Variant of `make_etb_bouncer_in_hand` allowing the ETB-bounce
    /// destination to be set explicitly. `None` matches the default
    /// (owner's hand); `Some(Zone::Library)` covers top-of-library variants
    /// used to verify the destination-axis short-circuit in the bounce arm.
    fn make_etb_bouncer_in_hand_with_destination(
        state: &mut GameState,
        name: &str,
        bounce_target: TargetFilter,
        destination: Option<Zone>,
    ) -> (ObjectId, CardId) {
        let card_id = CardId(state.objects.len() as u64 + 1);
        let obj_id = create_object(state, card_id, PlayerId(0), name.to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Bounce {
                        target: bounce_target,
                        destination,
                        selection: BounceSelection::Targeted,
                    },
                )),
        );
        (obj_id, card_id)
    }

    fn cast_candidate(object_id: ObjectId, card_id: CardId) -> CandidateAction {
        CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        }
    }

    #[test]
    fn whitemane_lion_self_undo_etb_penalised() {
        // AI is evaluating a cast of Whitemane Lion from hand. The AI
        // currently controls no other creatures, so the "creature you
        // control" ETB bounce will, after the Lion resolves, either fizzle
        // or be forced to return the Lion itself (per the Lion ruling).
        // Either way: self-undo loop. The Lion stays in hand for this
        // test — `resolved_candidate_targets` queries the battlefield at
        // cast-time and returns an empty set, which is the exact signal
        // `bounce_self_undo_redundancy` keys off of.
        let mut state = GameState::new_two_player(0);
        let (obj_id, card_id) = make_etb_bouncer_in_hand(
            &mut state,
            "Whitemane Lion",
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        );

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = cast_candidate(obj_id, card_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert!(
            delta <= -3.0,
            "Whitemane Lion ETB-self-undo should emit delta <= -3.0; got {delta}"
        );
    }

    #[test]
    fn etb_bounce_with_other_creature_not_penalised() {
        // AI is casting Whitemane Lion from hand AND already controls
        // another creature on the battlefield. The ETB bounce has a
        // legitimate target other than the Lion itself, so it is NOT
        // self-undoing — the candidate set is non-empty at cast-time.
        let mut state = GameState::new_two_player(0);
        let (obj_id, card_id) = make_etb_bouncer_in_hand(
            &mut state,
            "Whitemane Lion",
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        );
        // The other creature lives on the battlefield as the legitimate
        // bounce target. `make_creature_with_ability` places it there.
        let _other = make_creature_with_ability(
            &mut state,
            "Other Creature",
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        );

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = cast_candidate(obj_id, card_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(
            delta, 0.0,
            "ETB-self-bounce with another legal target should not penalise; got {delta}"
        );
    }

    #[test]
    fn activated_bounce_yourcreature_not_penalised() {
        // Soulherder-style: an ACTIVATED ability (not an ETB trigger) that
        // bounces "a creature you control". This is a legitimate value loop,
        // not self-undo — the policy must NOT penalise. Even if the only
        // legal target is the source itself, the activated/spell path runs
        // `redundancy_delta` with `EffectOrigin::PrimaryAbility`, which
        // short-circuits the bounce arm to `None`.
        let mut state = GameState::new_two_player(0);
        let obj_id = make_creature_with_ability(
            &mut state,
            "Soulherder",
            Effect::Bounce {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                destination: None,
                selection: BounceSelection::Targeted,
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
            "activated bounce-your-creature must not be penalised (Soulherder class); got {delta}"
        );
    }

    #[test]
    fn etb_bounce_no_legal_target_penalised() {
        // AI has the Lion in hand but controls no creatures yet — the ETB
        // bounce trigger will fizzle (no legal target). Penalise: AI is
        // about to pay 2 mana for a vanilla 2/2 with a no-op trigger that
        // contributes no value beyond the body. Detecting this prevents
        // burning search budget on the cast when better lines exist.
        let mut state = GameState::new_two_player(0);
        let (obj_id, card_id) = make_etb_bouncer_in_hand(
            &mut state,
            "Whitemane Lion",
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        );
        // Do NOT push obj_id to battlefield — Lion is still in hand, and
        // there are no creatures of any kind on the battlefield. The
        // candidate target set is empty.

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = cast_candidate(obj_id, card_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert!(
            delta <= -3.0,
            "ETB bounce with no legal target should emit delta <= -3.0; got {delta}"
        );
    }

    #[test]
    fn etb_bounce_to_library_not_penalised() {
        // Categorical scope: only `destination == None` or
        // `Some(Zone::Hand)` are self-undo loops, because only those land the
        // source back where it can be immediately re-cast. A bounce to library
        // is a different category (top-of-library setup, e.g., Crystal Shard
        // variants) — `bounce_self_undo_redundancy` must NOT fire here.
        // Mirrors the Whitemane-Lion geometry (bouncer in hand, no other
        // creatures controlled): if the destination-axis short-circuit in
        // the `Effect::Bounce` match arm is missing, the predicate would
        // emit a penalty via the empty-candidates branch and this test
        // would fail.
        let mut state = GameState::new_two_player(0);
        let (obj_id, card_id) = make_etb_bouncer_in_hand_with_destination(
            &mut state,
            "Library Bouncer",
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            Some(Zone::Library),
        );

        let config = AiConfig::default();
        let ai_ctx = AiContext::empty(&config.weights);
        let decision = priority_decision();
        let candidate = cast_candidate(obj_id, card_id);
        let ctx = mk_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        let PolicyVerdict::Score { delta, .. } = RedundancyAvoidancePolicy.verdict(&ctx) else {
            panic!("expected Score verdict");
        };
        assert_eq!(
            delta, 0.0,
            "ETB-bounce with destination=Library is not the self-undo class; got {delta}"
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
                excess: None,
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
