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
//! The draw step is gated for a different reason. CR 117.3a and CR 504.1/504.2
//! (matched by the engine's own comment beside `execute_draw` in
//! `crates/engine/src/game/turns.rs`): the active player receives priority
//! during the draw step only AFTER the turn-based draw, so waiting there buys no
//! information. But it gives nothing up either: the ability can still be
//! activated in the precombat main phase, and the mana and untapped creatures it
//! would spend now are still free for the sorcery-speed plays only the main phase
//! allows (CR 307.1). The same holds in an upkeep whose triggers have already
//! resolved. It fails only when the main phase opens with something an earlier
//! activation could still change (CR 505.3-505.5 turn-based actions, CR 117.3a
//! triggers), so those boards are not gated.
//! The untap step is excluded because CR 502.4 means no player ever receives
//! priority in it at all, so `WaitingFor::Priority` cannot occur there to gate on.
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
//! ## The escape hatches
//!
//! Floating mana the activation may spend comes first: it empties as the step
//! ends (CR 500.5), so for it waiting is not free. A draw replacement made in the
//! upkeep comes next: it must exist before the draw it replaces (CR 614.4). The
//! report names the other two times to activate early: when the damage is part of a lethal
//! line, and (implicitly) when the ability's own condition is met *now* and might
//! not be later. Those are lethal damage and `ability.condition`, in that order
//! below. Its third exception, a source that "is going to die or leave play
//! before the AI draws a card", has no reading in these windows: both are the
//! AI's own turn with an empty stack, so nothing is targeting the source and no
//! opponent attacks before the main phase.

use engine::game::{attractions, mana_abilities};
use engine::types::ability::{
    AbilityDefinition, AbilityTag, ActivationRestriction, DelayedTriggerCondition, Effect,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

use super::anti_self_harm::extract_damage_amount;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;
use crate::search::ability_is_temporary_combat_modifier;
use engine::game::players;

/// Base magnitude of the patience penalty, before the profile's interaction
/// patience scales it: preference band at the default patience (strong band at
/// 0.75 and up), enough to lose to any real payoff the other policies price and
/// to beat an idle activation's flat base score.
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
        // CR 602.5: an ability restricted to "your upkeep" can't be activated in the main
        // phase, so waiting would give it up.
        && !ability
            .activation_restrictions
            .contains(&ActivationRestriction::DuringYourUpkeep)
}

/// The AI's own windows before its precombat main phase in which it holds priority with an empty
/// stack, and why waiting for the main phase costs nothing there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatientWindow {
    /// CR 503.1: the upkeep step has no turn-based actions of its own, so taken with an empty
    /// stack it is the turn's low-information window: nothing has been drawn, cast, or declared.
    Upkeep,
    /// The draw step (CR 504.2: its priority comes after the draw) or an upkeep whose triggers
    /// have resolved: waiting buys no information, but it gives nothing up either, provided
    /// nothing happens as the main phase begins (see [`main_phase_opens_with_events`]). Every
    /// instant-speed use of the ability is still on offer in the main phase, and the mana and
    /// untapped permanents it would spend now are still there for the sorcery-speed plays
    /// (CR 307.1) that only the main phase allows.
    BeforeMain,
}

/// The [`PatientWindow`] `ai_player` holds priority in, if any.
///
/// The untap step is excluded because CR 502.4 means no player ever receives priority in it, so
/// `WaitingFor::Priority` cannot occur there. The end step is the *patient* window itself, the
/// one a held ability is being saved for (`FetchLandPatiencePolicy` treats it the same way).
///
/// # Why this must be `ai_player`'s OWN turn
///
/// CR 117.3d passes priority to the next player in turn order
/// (`crates/engine/src/game/priority.rs`'s `priority_pass_participants`), not
/// just the active player — so the AI can hold `WaitingFor::Priority` with an
/// empty stack during an OPPONENT's upkeep or draw step too. During an opponent's turn the AI's
/// next draw and main phase are no closer for having waited, and deferring costs something
/// real: this priority window is the AI's chance to act BEFORE the opponent's draw and
/// combat, and passing it up trades away a live interaction window for nothing.
/// `state.active_player == ai_player` is therefore a hard requirement, not a refinement.
///
/// # Why no beginning-of-upkeep trigger may reach `ai_player`'s upkeep
///
/// CR 503.1a: "at the beginning of your upkeep" triggers are put on the stack
/// BEFORE the active player ever receives priority for the step — and once
/// they're on the stack, `state.stack` is non-empty, so THIS predicate does
/// not fire yet (correctly: a live stack is `StackResponse` territory, not a
/// quiet window). But every one of those triggers must still be responded to
/// and resolved before the stack empties again, and priority then returns to
/// exactly the same shape this predicate matches — `Phase::Upkeep`, empty
/// stack, `Priority` — this time AFTER something already happened. Left
/// unguarded, that second window reads as identically "nothing has happened
/// yet" as the pristine first one, even when the resolved trigger drew a
/// card (Phyrexian Arena), scried, or revealed information. Excluding it
/// whenever ANY such trigger fires in `ai_player`'s upkeep (its own "your" triggers and
/// anyone's "each player's" triggers) is deliberately
/// coarse rather than trying to classify which triggers are
/// information-producing: CR 503.1a guarantees such a trigger is ALREADY
/// queued (and, by the time the stack is next empty, already resolved)
/// before the truly first grant, so the pristine window this predicate wants
/// is provably unreachable whenever one exists — there is no non-conservative
/// case being given up. Such an upkeep is still a [`PatientWindow::BeforeMain`], whose case
/// never rested on information.
fn patient_window(state: &GameState, ai_player: PlayerId) -> Option<PatientWindow> {
    if state.active_player != ai_player
        || !state.stack.is_empty()
        || !matches!(state.waiting_for, WaitingFor::Priority { .. })
    {
        return None;
    }
    // A main phase that opens with events stands both windows down: even a pristine upkeep can
    // run straight into it when the draw step is skipped (CR 614.10).
    if !matches!(state.phase, Phase::Upkeep | Phase::Draw)
        || main_phase_opens_with_events(state, ai_player)
    {
        return None;
    }
    if state.phase == Phase::Upkeep
        && !beginning_of_phase_trigger_reaches(state, ai_player, Phase::Upkeep)
    {
        Some(PatientWindow::Upkeep)
    } else {
        Some(PatientWindow::BeforeMain)
    }
}

/// Does `player`'s precombat main phase open with something that happens before they receive
/// priority in it, which an activation now could still change? CR 505.3: they are the archenemy
/// and set a scheme in motion. CR 505.4: they put a lore counter on each of their Sagas.
/// CR 505.5: they roll to visit their Attractions. CR 117.3a: "at the beginning of your
/// precombat main phase" triggers go on the stack, and so do CR 603.7 delayed triggers set for
/// the next precombat main phase (Mana Drain's "at the beginning of your next main phase").
/// Coarse in the safe direction, like
/// [`beginning_of_phase_trigger_reaches`]: any such permanent stands the window down. Triggers
/// worded "at the beginning of each of your main phases" (parsed without a phase) are not
/// counted; today they only add mana, which the main phase does not need earlier.
///
/// Sagas are read the way `turns::add_lore_counters_to_sagas` reads them, and Attractions
/// through the engine's own `is_attraction_permanent`.
fn main_phase_opens_with_events(state: &GameState, player: PlayerId) -> bool {
    state.archenemy == Some(player)
        || beginning_of_phase_trigger_reaches(state, player, Phase::PreCombatMain)
        || state
            .delayed_triggers
            .iter()
            .any(|delayed| match delayed.condition {
                DelayedTriggerCondition::AtNextPhase { phase } => phase == Phase::PreCombatMain,
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase,
                    player: for_player,
                    ..
                } => phase == Phase::PreCombatMain && for_player == player,
                DelayedTriggerCondition::AtBeginningOfAddedPhase { .. }
                | DelayedTriggerCondition::WhenLeavesPlay { .. }
                | DelayedTriggerCondition::WhenDies { .. }
                | DelayedTriggerCondition::WhenLeavesPlayFiltered { .. }
                | DelayedTriggerCondition::WhenEntersBattlefield { .. }
                | DelayedTriggerCondition::WhenDiesOrExiled { .. }
                | DelayedTriggerCondition::WheneverEvent { .. }
                | DelayedTriggerCondition::WhenNextEvent { .. } => false,
            })
        || state.battlefield.iter().any(|&id| {
            state.objects.get(&id).is_some_and(|object| {
                object.controller == player
                    && (object.card_types.subtypes.iter().any(|s| s == "Saga")
                        || attractions::is_attraction_permanent(object))
            })
        })
}

/// Does a printed "at the beginning of [phase]" trigger that fires on `player`'s turn sit on any
/// battlefield permanent (CR 503.1a for the upkeep, CR 117.3a in general)? A "your" trigger counts
/// for its controller only; an "each player's" trigger (Blinkmoth Urn) counts for every player,
/// whoever controls it, through the engine's `phase_trigger_scoped_to_turn_of`. Deliberately NOT
/// gated on the trigger currently functioning or on its intervening-if (CR 603.4: an activation
/// now may be what makes it true) — `Definitions::iter_unchecked` is the sanctioned
/// classification-only accessor (engine runtime paths use the CR-gated `functioning_*` helpers
/// instead; this is neither, so a source under a static suppression still counts). That is the
/// safe direction: this predicate exists only to EXCLUDE a window from patience-gating, so
/// over-including a source that could not actually have fired only means the policy declines to
/// nudge a few boards it safely could have — never the reverse.
fn beginning_of_phase_trigger_reaches(state: &GameState, player: PlayerId, phase: Phase) -> bool {
    state.battlefield.iter().any(|&id| {
        state.objects.get(&id).is_some_and(|object| {
            object.trigger_definitions.iter_unchecked().any(|entry| {
                let def = entry.definition();
                def.mode == TriggerMode::Phase
                    && def.phase == Some(phase)
                    && engine::game::triggers::phase_trigger_scoped_to_turn_of(
                        state,
                        def,
                        object.controller,
                        player,
                    )
            })
        })
    })
}

/// Does this activation's own damage end the game now? CR 104.3b — a player at 0
/// or less life loses, so damage that reaches a live opponent's life total is
/// never worth deferring.
///
/// A lethal intent alone does not make an activation part of the line: the line
/// is still there in the precombat main phase, before any combat it depends on.
fn is_lethal_line(ctx: &PolicyContext<'_>) -> bool {
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
            ..
        } = &ctx.candidate.action
        else {
            return na();
        };

        let Some(window) = patient_window(ctx.state, ctx.ai_player) else {
            return na();
        };

        if !activation_is_deferrable(ctx.state, *source_id, *ability_index) {
            return na();
        }

        // Escape hatch 1 — CR 500.5: mana in the pool empties as this step ends, so
        // when this activation may spend it (Thran Turbine's upkeep {C}{C}, which can't
        // be spent on spells) waiting can destroy it rather than save it for the main
        // phase. The engine answers eligibility only; whether this cost would actually
        // use the mana is left to the payment pipeline, and the hatch errs toward acting.
        if engine::game::casting::pool_has_expiring_mana_for_activation(
            ctx.state,
            ctx.ai_player,
            *source_id,
            *ability_index,
        ) {
            return PolicyVerdict::neutral(PolicyReason::new("activation_patience_floating_mana"));
        }

        // Escape hatch 2 — CR 614.4: a draw replacement ("the next time you would draw a
        // card this turn", the Words cycle) must exist before the draw it replaces, so in the
        // upkeep, with the draw step's turn-based draw still ahead, waiting for the main phase
        // forfeits it.
        if ctx.state.phase == Phase::Upkeep
            && ctx
                .effects()
                .iter()
                .any(|effect| matches!(effect, Effect::CreateDrawReplacement { .. }))
        {
            return PolicyVerdict::neutral(PolicyReason::new(
                "activation_patience_draw_replacement",
            ));
        }

        // Escape hatch 3 — a lethal line beats any information the AI could buy
        // by waiting.
        if is_lethal_line(ctx) {
            return PolicyVerdict::neutral(PolicyReason::new("activation_patience_lethal_line"));
        }

        // Escape hatch 4 — CR 608.2c: `ability.condition` on an ACTIVATED
        // ability (as opposed to a triggered ability's CR 603.4 intervening-if)
        // gates the PAYOFF at resolution, not the activation itself — CR 602.5
        // has no bearing here; per the Shelldock Isle ruling `condition_gated_
        // activation.rs`'s own module doc already cites, activating is legal
        // regardless of whether the condition holds (the effect simply does
        // nothing if it's false at resolution). What makes patience wrong here
        // is narrower: a condition that's TRUE now may not still be true after
        // waiting, so deferring risks losing a real payoff for a purely
        // informational gain. `ConditionGatedActivationPolicy` is this hatch's
        // mirror image — it prices activating while the condition is FALSE.
        if ctx
            .effective_activated_ability()
            .is_some_and(|ability| ability.condition.is_some())
        {
            return PolicyVerdict::neutral(PolicyReason::new("activation_patience_conditional"));
        }

        let patience = ctx.config.profile.interaction_patience;
        PolicyVerdict::score(
            -PATIENCE_PENALTY_BASE * (0.5 + patience),
            PolicyReason::new(match window {
                PatientWindow::Upkeep => "activation_patience_hold",
                PatientWindow::BeforeMain => "activation_patience_hold_for_main",
            })
            .with_fact("phase", ctx.state.phase as i64),
        )
    }
}
