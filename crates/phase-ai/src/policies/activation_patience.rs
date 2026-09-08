//! Activation-patience tactical policy.
//!
//! Report (Discord #ai-suggestions): "The AI, in general, should not make blind
//! decisions during random phases, such as upkeep. If I have a 5-toughness
//! creature in play and my opponent has a Grim Lavamancer, there are very few
//! situations where the AI should activate the Grim Lavamancer to deal 2 damage
//! to my 5-toughness creature **without drawing a card first**."
//!
//! The observation generalises past that one card. The upkeep step, taken with
//! an empty stack, is the AI's LOWEST-INFORMATION window of the turn: it has
//! not drawn yet this turn, no spell has been cast, and combat has not been
//! declared. For an ability that is still going to be there in the main phase,
//! activating during upkeep spends a resource to buy strictly less information
//! than waiting would. Nothing in the corpus modelled that: `TacticalWindow`
//! (`tactical_gate.rs`) has no upkeep variant, and `card_hints` gives every
//! `ActivateAbility` a flat base score with no timing term at all.
//!
//! The draw step is deliberately NOT one of the gated windows, even though it
//! looks like the same class. CR 117.3a and CR 504.1/504.2 (matched by the
//! engine's own comment beside `execute_draw` in `crates/engine/src/game/turns.rs`):
//! the active player receives priority during the draw step only AFTER the
//! turn-based draw has been dealt with — a skipped draw still runs that step
//! first. So whenever this policy could observe draw-step priority, the card
//! is already in hand; gating there would hold an activation back for
//! information the AI already has. The untap step is excluded for a stronger
//! reason: CR 502.4 means no player ever receives priority in it at all, so
//! `WaitingFor::Priority` cannot occur there to gate on.
//!
//! So this policy asks one question — **does waiting cost anything?** — and
//! penalises the activation when the answer is no.
//!
//! ## Why a soft penalty and not a `Reject`
//!
//! "Later is better" is a judgement about information, not a rule-derived
//! impossibility, and `tactical_gate` owns the latter (see its layering note).
//! A `Reject` would also blind search lookahead, which can legitimately find
//! that activating now enables a line the AI cannot see from the root. The
//! penalty is sized to lose to a real payoff and win against an idle ping.
//!
//! ## The three escape hatches
//!
//! The report names them: activate early when the source "is going to die or
//! leave play before the AI draws a card", when the damage is part of a lethal
//! line, and (implicitly) when the ability's own condition is met *now* and
//! might not be later. Those are [`any_immediate_threat`], `PushLethal` /
//! lethal-damage, and `ability.condition`, in that order below.

use engine::game::mana_abilities;
use engine::types::ability::{AbilityDefinition, AbilityTag};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::anti_self_harm::extract_damage_amount;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::self_protection_classify::any_immediate_threat;
use crate::eval::StrategicIntent;
use crate::features::DeckFeatures;
use crate::search::ability_is_temporary_combat_modifier;
use engine::game::players;

/// Base magnitude of the patience penalty, before the profile's interaction
/// patience scales it. Preference-band: enough to lose to any real payoff the
/// other policies price, enough to beat an idle activation's flat base score.
const PATIENCE_PENALTY_BASE: f64 = 1.2;

pub struct ActivationPatiencePolicy;

/// Does deferring this activation to a later window cost anything **structurally**?
///
/// Deliberately narrow: it answers only what can be read off the ability
/// itself, never board-state judgement. The escape hatches live in
/// [`ActivationPatiencePolicy::verdict`], on top of this.
///
/// Note this is NOT the same question `search::empty_stack_activation_is_low_value`
/// asks, and the two must not be merged. That predicate feeds the auto-pass
/// fast path and means "not worth searching for" — for which a mana ability and
/// a temporary combat modifier are the two YES answers, because both are
/// pointless in an empty-stack upkeep. Here those same two shapes are the two
/// NO answers, because waiting is not free for them. Same shapes, opposite
/// polarity. Folding this into that fast path would additionally let it skip a
/// lethal line, since the fast path bypasses policy scoring and would inherit
/// none of the escape hatches below.
///
/// Two shapes are NOT deferrable:
///
/// * A mana ability (CR 605.1a). It does not use the stack, produces mana that
///   empties at the end of the step (CR 106.4), and is answered by the payment
///   pipeline rather than by timing.
/// * A temporary combat modifier (CR 611.2 + CR 514.2 — the resolution
///   generates the continuous effect, and the cleanup step ends it). Waiting
///   is not "free" for those; they are window-bound, and `EffectTimingPolicy`'s
///   `combat_trick_score` already owns their timing.
///
/// Cycling is excluded so this does not double-charge alongside
/// `CyclingDisciplinePolicy`, which owns that class's timing.
fn activation_is_deferrable(state: &GameState, source_id: ObjectId, ability_index: usize) -> bool {
    state
        .objects
        .get(&source_id)
        .and_then(|object| object.abilities.get(ability_index))
        .is_some_and(ability_is_deferrable)
}

fn ability_is_deferrable(ability: &AbilityDefinition) -> bool {
    !mana_abilities::is_mana_ability(ability)
        && !ability_is_temporary_combat_modifier(ability)
        && ability.ability_tag != Some(AbilityTag::Cycling)
}

/// CR 503.1: the upkeep step has no turn-based actions of its own, so taken
/// with an empty stack it is the turn's low-information window — nothing has
/// been drawn, cast, or declared yet.
///
/// The draw step and the untap step are NOT included, and both exclusions are
/// load-bearing rather than incidental — see the module doc for the CR
/// citations. In short: any draw-step priority this predicate could see is
/// already post-draw (CR 504.1/504.2), so gating there would be pointless: the
/// AI has exactly the information waiting would have bought it. And the untap
/// step never grants priority at all (CR 502.4), so a `Phase::Untap` arm here
/// would be dead code that misleadingly reads as though it were a live window.
///
/// The end step is deliberately NOT here either, but for a different reason —
/// it is the *patient* window, the one a held ability is being saved for, and
/// `FetchLandPatiencePolicy` already treats it as the correct time to act.
fn is_low_information_window(state: &GameState) -> bool {
    state.phase == Phase::Upkeep
        && state.stack.is_empty()
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
}

/// Is this activation part of a line that can end the game now? CR 104.3b — a
/// player at 0 or less life loses, so damage that reaches a live opponent's life
/// total is never worth deferring.
fn is_lethal_line(ctx: &PolicyContext<'_>) -> bool {
    if matches!(ctx.strategic_intent(), StrategicIntent::PushLethal) {
        return true;
    }
    let Some(damage) = extract_damage_amount(&ctx.effects()) else {
        return false;
    };
    players::opponents(ctx.state, ctx.ai_player)
        .iter()
        .any(|&opponent| ctx.state.players[opponent.0 as usize].life <= damage)
}

impl TacticalPolicy for ActivationPatiencePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ActivationPatience
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        super::activation::turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::neutral(PolicyReason::new("activation_patience_na"));

        let GameAction::ActivateAbility {
            source_id,
            ability_index,
        } = &ctx.candidate.action
        else {
            return na();
        };

        if !is_low_information_window(ctx.state) {
            return na();
        }

        if !activation_is_deferrable(ctx.state, *source_id, *ability_index) {
            return na();
        }

        // Escape hatch 1 — the source may not survive to the patient window, so
        // "wait" is not actually on offer. Also covers life pressure and an
        // opponent board that can act before the AI gets another window.
        if any_immediate_threat(ctx.state, ctx.ai_player) {
            return PolicyVerdict::neutral(PolicyReason::new("activation_patience_threatened"));
        }

        // Escape hatch 2 — a lethal line beats any information the AI could buy
        // by waiting.
        if is_lethal_line(ctx) {
            return PolicyVerdict::neutral(PolicyReason::new("activation_patience_lethal_line"));
        }

        // Escape hatch 3 — CR 602.5: an activated ability's condition is checked
        // when activation begins. One that holds now may not hold later, so
        // waiting is not free. `ConditionGatedActivationPolicy` owns that
        // judgement.
        if ctx
            .effective_activated_ability()
            .is_some_and(|ability| ability.condition.is_some())
        {
            return PolicyVerdict::neutral(PolicyReason::new("activation_patience_conditional"));
        }

        let patience = ctx.config.profile.interaction_patience;
        PolicyVerdict::score(
            -PATIENCE_PENALTY_BASE * (0.5 + patience),
            PolicyReason::new("activation_patience_hold")
                .with_fact("phase", ctx.state.phase as i64),
        )
    }
}
