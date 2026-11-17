use std::collections::{HashMap, HashSet};

use engine::ai_support::AiDecisionContract;
use engine::game::engine::{
    apply_interaction, apply_verified_ai_priority_pass, verified_ai_stack_pass_player, EngineError,
};
use engine::game::turn_control;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::log::GameLogEntry;
use engine::types::player::PlayerId;
use rand::Rng;
use std::sync::Arc;

use crate::config::AiConfig;
use crate::search::choose_action_with_issued_contract;
use crate::session::AiSession;

/// Maximum AI actions before forcing a stop (safety invariant — not CR-derived).
/// Typical AI sequences (mulligans + full turn) are 30–50 actions.
pub const MAX_AI_ACTIONS_PER_SEQUENCE: usize = 200;

/// Maximum host contest settlements one headless batch may perform before it
/// gives up (safety invariant — not CR-derived). Settlement is host work, not an
/// AI action, so it is bounded separately from the caller's action budget: a
/// batch that spent its budget on settlements would report `ActionBudgetReached`
/// with no applied action, which a driver cannot tell apart from a stall.
const MAX_HOST_SETTLEMENTS_PER_SEQUENCE: usize = 200;

/// Identifies whether a batch bound belongs to the caller or to the module's
/// infinite-loop safety cap. A caller requesting exactly the cap remains a
/// caller budget; only a larger request is truncated by the safety cap.
#[derive(Clone, Copy)]
enum ActionLimit {
    SafetyCap,
    CallerBudget { requested: usize },
}

impl ActionLimit {
    fn effective(self) -> usize {
        match self {
            Self::SafetyCap => MAX_AI_ACTIONS_PER_SEQUENCE,
            Self::CallerBudget { requested } => requested.min(MAX_AI_ACTIONS_PER_SEQUENCE),
        }
    }

    fn is_safety_cap(self) -> bool {
        match self {
            Self::SafetyCap => true,
            Self::CallerBudget { requested } => requested > MAX_AI_ACTIONS_PER_SEQUENCE,
        }
    }
}

/// Who drives a game parked on `WaitingFor::PhysicalContest`, where no seat acts.
///
/// A live host (phase-server, the WASM engine) owns a real clock and real input, so the
/// batch stops there and the host's own driver settles the contest. A headless host
/// (self-play, benchmarks, gates) has neither, so the batch settles an all-AI contest
/// itself on a deterministic virtual clock and plays on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ContestHosting {
    External,
    Headless,
}

/// Result of a single AI action: the action taken and the resulting events.
pub struct AiActionResult {
    pub action: GameAction,
    pub state: GameState,
    pub events: Vec<GameEvent>,
    pub log_entries: Vec<GameLogEntry>,
}

/// Why an AI action batch stopped.
///
/// Diagnostic surface for phase#6080 (the driver-stall family): today the only
/// signal at these break points is a `tracing::error`/`tracing::warn` that no
/// harness subscriber captures. Exposing the reason as typed data lets a
/// caller like `ai_commander` print it instead of installing a subscriber.
#[derive(Debug, Clone)]
pub enum AiActionsStop {
    /// No AI seat can currently act. Two causes are still folded together
    /// here: `WaitingFor::acting_players()` returned empty (`GameOver`, or an
    /// empty pending set), or it returned one or more players and none of
    /// their `turn_control::authorized_submitter_for_player` mappings is in
    /// `ai_players` (a human seat, or a human turn-decision controller).
    /// Deliberately carries no `PlayerId`: the first cause has no player at
    /// all, and the simultaneous-decision variants (`MulliganDecision`,
    /// `OpeningHandBottomCards`) can pend several at once, so naming one
    /// would be arbitrary. A missing AI *configuration* is `MissingAiConfig`.
    NoEligibleAiActor,
    /// `player` is in `ai_players` but has no entry in `ai_configs`. Distinct
    /// from `NoActor`: an actor *was* found and *is* AI-controlled, so the
    /// remedy is caller wiring (register a config for this seat), not "wait
    /// for a human" or "the game ended".
    MissingAiConfig { player: PlayerId },
    /// `choose_action_with_issued_contract` returned `None` for `player` — the AI
    /// policy stack produced no legal action for a decision it was asked to
    /// make.
    ChooseActionNone { player: PlayerId },
    /// `apply()` rejected `player`'s chosen `action`. `action` is boxed because
    /// `GameAction` is large relative to the other variants (clippy
    /// `large_enum_variant`); `EngineError` is four small variants (largest
    /// payload a `String`) and needs no box.
    ApplyFailed {
        player: PlayerId,
        action: Box<GameAction>,
        error: EngineError,
    },
    /// The module-wide safety budget was exhausted while an AI-controlled
    /// submitter still had work. This is a driver failure, not a benign handoff.
    ActionSafetyCapReached { limit: usize },
    /// A caller-provided smaller budget was exhausted while an AI-controlled
    /// submitter still had work. The caller owns continuation from this point.
    ActionBudgetReached { limit: usize },
}

/// Outcome of a `run_ai_actions` batch.
///
/// `Deref`s to `Vec<AiActionResult>` so callers that only care about actions
/// taken (`.is_empty()`, `.len()`, indexing, iterating by reference) remain
/// source-compatible.
pub struct AiActionsRun {
    pub results: Vec<AiActionResult>,
    pub stop: AiActionsStop,
}

/// Resolve the next AI-owned decision using the driver's authorization boundary.
///
/// CR 805.5a: under shared team turns every living teammate may act when the
/// team has priority, so an AI teammate is eligible even while the window is
/// parked on the human representative.
pub fn eligible_ai_decision(
    state: &GameState,
    ai_players: &HashSet<PlayerId>,
) -> Option<(PlayerId, PlayerId)> {
    // CR 805.5a: team priority windows list every living member as a decision
    // owner; other windows list the engine's acting players unchanged.
    // CR 805.5b: a member who already passed this window has no new decision
    // until someone acts (which clears passes) — skip them so the driver hands
    // the window to an unpassed teammate instead of re-prompting the passer
    // until the safety cap.
    let owners = turn_control::decision_owners(state);
    let unpassed: Vec<PlayerId> = match &state.waiting_for {
        WaitingFor::Priority { .. } => owners
            .iter()
            .copied()
            .filter(|owner| !state.priority_passes.contains(owner))
            .collect(),
        // Every other window resolves to the engine's acting players
        // unchanged. Exhaustive by construction: a new `WaitingFor` variant
        // fails to compile here until it is classified.
        WaitingFor::ResolveAllConsent { .. } => owners.clone(),
        WaitingFor::ResolveAllReady { .. } => owners.clone(),
        WaitingFor::MeldPairChoice { .. } => owners.clone(),
        WaitingFor::MeldAttackTargetChoice { .. } => owners.clone(),
        WaitingFor::EntryAttackTargetChoice { .. } => owners.clone(),
        WaitingFor::MulliganDecision { .. } => owners.clone(),
        WaitingFor::OpeningHandBottomCards { .. } => owners.clone(),
        WaitingFor::ManaPayment { .. } => owners.clone(),
        WaitingFor::ManaSourceSelection { .. } => owners.clone(),
        WaitingFor::AssistChoosePlayer { .. } => owners.clone(),
        WaitingFor::AssistPayment { .. } => owners.clone(),
        WaitingFor::ChooseXValue { .. } => owners.clone(),
        WaitingFor::TargetSelection { .. } => owners.clone(),
        WaitingFor::DeclareAttackers { .. } => owners.clone(),
        WaitingFor::DeclareBlockers { .. } => owners.clone(),
        WaitingFor::UntapChoice { .. } => owners.clone(),
        WaitingFor::ChooseUntapSubset { .. } => owners.clone(),
        WaitingFor::ExertChoice { .. } => owners.clone(),
        WaitingFor::EnlistChoice { .. } => owners.clone(),
        WaitingFor::GameOver { .. } => owners.clone(),
        WaitingFor::ReplacementChoice { .. } => owners.clone(),
        WaitingFor::EntryControllerChoice { .. } => owners.clone(),
        WaitingFor::OrderTriggers { .. } => owners.clone(),
        WaitingFor::CopyTargetChoice { .. } => owners.clone(),
        WaitingFor::ExploreChoice { .. } => owners.clone(),
        WaitingFor::ReturnAsAuraTarget { .. } => owners.clone(),
        WaitingFor::ChooseTokenHost { .. } => owners.clone(),
        WaitingFor::EquipTarget { .. } => owners.clone(),
        WaitingFor::CrewVehicle { .. } => owners.clone(),
        WaitingFor::StationTarget { .. } => owners.clone(),
        WaitingFor::SaddleMount { .. } => owners.clone(),
        WaitingFor::ScryChoice { .. } => owners.clone(),
        WaitingFor::RippleRevealChoice { .. } => owners.clone(),
        WaitingFor::RippleBottomOrder { .. } => owners.clone(),
        WaitingFor::RevealUntilBottomOrder { .. } => owners.clone(),
        WaitingFor::DigBottomOrder { .. } => owners.clone(),
        WaitingFor::ArrangePlanarDeckTopChoice { .. } => owners.clone(),
        WaitingFor::RedistributeLifeTotals { .. } => owners.clone(),
        WaitingFor::CoinFlipKeepChoice { .. } => owners.clone(),
        WaitingFor::DieKeepChoice { .. } => owners.clone(),
        WaitingFor::DigChoice { .. } => owners.clone(),
        WaitingFor::DigRestSplitChoice { .. } => owners.clone(),
        WaitingFor::SurveilChoice { .. } => owners.clone(),
        WaitingFor::RevealChoice { .. } => owners.clone(),
        WaitingFor::SearchChoice { .. } => owners.clone(),
        WaitingFor::SearchPartitionChoice { .. } => owners.clone(),
        WaitingFor::OutsideGameChoice { .. } => owners.clone(),
        WaitingFor::ChooseFromZoneChoice { .. } => owners.clone(),
        WaitingFor::BeholdChoice { .. } => owners.clone(),
        WaitingFor::EmpowerJaceChoice { .. } => owners.clone(),
        WaitingFor::SpellCopyOrderChoice { .. } => owners.clone(),
        WaitingFor::ChooseOneOfBranch { .. } => owners.clone(),
        WaitingFor::ConniveDiscard { .. } => owners.clone(),
        WaitingFor::DiscardChoice { .. } => owners.clone(),
        WaitingFor::HeroEventChoice { .. } => owners.clone(),
        WaitingFor::EffectZoneChoice { .. } => owners.clone(),
        WaitingFor::DrawnThisTurnTopdeckChoice { .. } => owners.clone(),
        WaitingFor::LearnChoice { .. } => owners.clone(),
        WaitingFor::ManifestDreadChoice { .. } => owners.clone(),
        WaitingFor::TriggerTargetSelection { .. } => owners.clone(),
        WaitingFor::BetweenGamesSideboard { .. } => owners.clone(),
        WaitingFor::BetweenGamesChoosePlayDraw { .. } => owners.clone(),
        WaitingFor::ChooseStartingPlayer { .. } => owners.clone(),
        WaitingFor::NamedChoice { .. } => owners.clone(),
        WaitingFor::OpponentGuess { .. } => owners.clone(),
        WaitingFor::SpellbookDraft { .. } => owners.clone(),
        WaitingFor::DamageSourceChoice { .. } => owners.clone(),
        WaitingFor::ModeChoice { .. } => owners.clone(),
        WaitingFor::DiscardToHandSize { .. } => owners.clone(),
        WaitingFor::OptionalCostChoice { .. } => owners.clone(),
        WaitingFor::ChooseGiftRecipient { .. } => owners.clone(),
        WaitingFor::SpliceOffer { .. } => owners.clone(),
        WaitingFor::DefilerPayment { .. } => owners.clone(),
        WaitingFor::OrderCostReductions { .. } => owners.clone(),
        WaitingFor::CastOffer { .. } => owners.clone(),
        WaitingFor::ModalFaceChoice { .. } => owners.clone(),
        WaitingFor::AlternativeCastChoice { .. } => owners.clone(),
        WaitingFor::MutateMergeChoice { .. } => owners.clone(),
        WaitingFor::CipherEncodeChoice { .. } => owners.clone(),
        WaitingFor::CastingVariantChoice { .. } => owners.clone(),
        WaitingFor::ChoosePermanentTypeSlot { .. } => owners.clone(),
        WaitingFor::MultiTargetSelection { .. } => owners.clone(),
        WaitingFor::AbilityModeChoice { .. } => owners.clone(),
        WaitingFor::OptionalEffectChoice { .. } => owners.clone(),
        WaitingFor::ResolutionOptionalPaymentChoice { .. } => owners.clone(),
        WaitingFor::PairChoice { .. } => owners.clone(),
        WaitingFor::TributeChoice { .. } => owners.clone(),
        WaitingFor::MiracleReveal { .. } => owners.clone(),
        WaitingFor::OpponentMayChoice { .. } => owners.clone(),
        WaitingFor::LoopShortcut { .. } => owners.clone(),
        WaitingFor::RespondToShortcut { .. } => owners.clone(),
        WaitingFor::PrecastCopyShortcutOffer { .. } => owners.clone(),
        WaitingFor::RespondToPrecastCopyShortcut { .. } => owners.clone(),
        WaitingFor::UnlessPayment { .. } => owners.clone(),
        WaitingFor::UnlessPaymentChooseCost { .. } => owners.clone(),
        WaitingFor::WardDiscardChoice { .. } => owners.clone(),
        WaitingFor::WardSacrificeChoice { .. } => owners.clone(),
        WaitingFor::UnlessBounceChoice { .. } => owners.clone(),
        WaitingFor::ChooseRingBearer { .. } => owners.clone(),
        WaitingFor::ChooseRoomDoor { .. } => owners.clone(),
        WaitingFor::ChooseDungeon { .. } => owners.clone(),
        WaitingFor::ChooseDungeonRoom { .. } => owners.clone(),
        WaitingFor::SpecializeColor { .. } => owners.clone(),
        WaitingFor::PayCost { .. } => owners.clone(),
        WaitingFor::ActivationCostOneOfChoice { .. } => owners.clone(),
        WaitingFor::CostTypeChoice { .. } => owners.clone(),
        WaitingFor::BlightChoice { .. } => owners.clone(),
        WaitingFor::PayManaAbilityMana { .. } => owners.clone(),
        WaitingFor::ManaAbilityManaPayment { .. } => owners.clone(),
        WaitingFor::ChooseManaColor { .. } => owners.clone(),
        WaitingFor::CollectEvidenceChoice { .. } => owners.clone(),
        WaitingFor::HarmonizeTapChoice { .. } => owners.clone(),
        WaitingFor::RevealUntilKeptChoice { .. } => owners.clone(),
        WaitingFor::RepeatDecision { .. } => owners.clone(),
        WaitingFor::TopOrBottomChoice { .. } => owners.clone(),
        WaitingFor::PopulateChoice { .. } => owners.clone(),
        WaitingFor::ClashChooseOpponent { .. } => owners.clone(),
        WaitingFor::ChooseFromZoneOpponentChooser { .. } => owners.clone(),
        WaitingFor::ChooseAnnouncingOpponent { .. } => owners.clone(),
        WaitingFor::ClashCardPlacement { .. } => owners.clone(),
        WaitingFor::PhysicalContest { .. } => owners.clone(),
        WaitingFor::VoteChoice { .. } => owners.clone(),
        WaitingFor::SeparatePilesChooseOpponent { .. } => owners.clone(),
        WaitingFor::SeparatePilesPartition { .. } => owners.clone(),
        WaitingFor::SeparatePilesChoice { .. } => owners.clone(),
        WaitingFor::SecretAgendaChoice { .. } => owners.clone(),
        WaitingFor::CompanionReveal { .. } => owners.clone(),
        WaitingFor::ChooseLegend { .. } => owners.clone(),
        WaitingFor::CommanderZoneChoice { .. } => owners.clone(),
        WaitingFor::BattleProtectorChoice { .. } => owners.clone(),
        WaitingFor::ProliferateChoice { .. } => owners.clone(),
        WaitingFor::TimeTravelChoice { .. } => owners.clone(),
        WaitingFor::ChooseObjectsSelection { .. } => owners.clone(),
        WaitingFor::CategoryChoice { .. } => owners.clone(),
        WaitingFor::EachPlayerCopyChosenSelection { .. } => owners.clone(),
        WaitingFor::KeepWithinTotalPowerChoice { .. } => owners.clone(),
        WaitingFor::KeepExactPermanentsChoice { .. } => owners.clone(),
        WaitingFor::CopyRetarget { .. } => owners.clone(),
        WaitingFor::AssignCombatDamage { .. } => owners.clone(),
        WaitingFor::AssignBlockerDamage { .. } => owners.clone(),
        WaitingFor::DistributeAmong { .. } => owners.clone(),
        WaitingFor::MoveCountersDistribution { .. } => owners.clone(),
        WaitingFor::RemoveCountersChoice { .. } => owners.clone(),
        WaitingFor::PayAmountChoice { .. } => owners.clone(),
        WaitingFor::RetargetChoice { .. } => owners.clone(),
        WaitingFor::CombatTaxPayment { .. } => owners.clone(),
        WaitingFor::PhyrexianPayment { .. } => owners.clone(),
    };
    let owners = if unpassed.is_empty() {
        owners
    } else {
        unpassed
    };
    owners.into_iter().find_map(|semantic_owner| {
        let actor = turn_control::authorized_submitter_for_player(state, semantic_owner);
        ai_players
            .contains(&actor)
            .then_some((semantic_owner, actor))
    })
}

impl std::ops::Deref for AiActionsRun {
    type Target = Vec<AiActionResult>;
    fn deref(&self) -> &Vec<AiActionResult> {
        &self.results
    }
}

impl IntoIterator for AiActionsRun {
    type Item = AiActionResult;
    type IntoIter = std::vec::IntoIter<AiActionResult>;
    fn into_iter(self) -> Self::IntoIter {
        self.results.into_iter()
    }
}

impl<'a> IntoIterator for &'a AiActionsRun {
    type Item = &'a AiActionResult;
    type IntoIter = std::slice::Iter<'a, AiActionResult>;
    fn into_iter(self) -> Self::IntoIter {
        self.results.iter()
    }
}

impl<'a> IntoIterator for &'a mut AiActionsRun {
    type Item = &'a mut AiActionResult;
    type IntoIter = std::slice::IterMut<'a, AiActionResult>;
    fn into_iter(self) -> Self::IntoIter {
        self.results.iter_mut()
    }
}

/// Run AI actions on the game state until the next actor is human or the game is over.
///
/// Returns one `AiActionResult` per AI action taken, preserving granularity for
/// the caller to broadcast individual state updates with animation timing.
///
/// # Arguments
/// * `state` — mutable game state (modified in place)
/// * `ai_players` — set of AI-controlled player IDs
/// * `ai_configs` — per-player AI configuration
///
/// CR 117.3d: A player who passes gives priority to the next player in turn order.
/// The loop terminates when a non-AI player receives priority or the game ends.
pub fn run_ai_actions(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
) -> AiActionsRun {
    // Thin delegate: existing callers get the full safety-cap budget and
    // exactly the prior semantics.
    run_ai_actions_with_limit(
        state,
        ai_players,
        ai_configs,
        rng,
        session,
        ActionLimit::SafetyCap,
        ContestHosting::External,
    )
}

/// Run AI actions like [`run_ai_actions`], but with a caller-supplied upper
/// bound on how many actions the batch may take.
///
/// The effective bound is `min(max_actions, MAX_AI_ACTIONS_PER_SEQUENCE)`: the
/// module's safety cap remains the single authority — a caller can *shrink* a
/// batch below it (to honor an action budget) but never *enlarge* one past it.
/// This function never returns more than that many `AiActionResult`s.
///
/// `max_actions == 0` returns `ActionBudgetReached { limit: 0 }` — no actor is
/// inspected. A caller that loops on this function must therefore guarantee a
/// positive budget before each call.
///
/// The "hit safety cap" warning stays keyed to `MAX_AI_ACTIONS_PER_SEQUENCE`,
/// not `max_actions`: a small operator budget reaching its bound is expected,
/// not a pathological infinite loop, so the warning is naturally silent whenever
/// the clamp is the lower of the two.
pub fn run_ai_actions_bounded(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
    max_actions: usize,
) -> AiActionsRun {
    run_ai_actions_with_limit(
        state,
        ai_players,
        ai_configs,
        rng,
        session,
        ActionLimit::CallerBudget {
            requested: max_actions,
        },
        ContestHosting::External,
    )
}

/// Choose and commit one AI action through the sequence driver's existing
/// decision contract and apply boundaries, without a one-element result vector.
/// The sequence caller owns its cumulative `MAX_AI_ACTIONS_PER_SEQUENCE` budget.
pub fn run_ai_action(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
) -> std::ops::ControlFlow<AiActionsStop, AiActionResult> {
    // CR 723.5: The controller makes the controlled player's decisions, so the
    // authorized submitter, not the semantic owner, selects the AI config.
    let Some((semantic_owner, actor)) = eligible_ai_decision(state, ai_players) else {
        return std::ops::ControlFlow::Break(AiActionsStop::NoEligibleAiActor);
    };
    let config = match ai_configs.get(&actor) {
        Some(config) => config,
        None => {
            tracing::warn!(player = ?actor, "AI seat has no config — stopping AI loop");
            return std::ops::ControlFlow::Break(AiActionsStop::MissingAiConfig { player: actor });
        }
    };
    let contract = AiDecisionContract::issue(state, semantic_owner);
    let action = match choose_action_with_issued_contract(state, config, rng, session, &contract) {
        Some(action) => action,
        None => {
            tracing::warn!(player = ?actor, "choose_action returned None — stopping AI loop");
            return std::ops::ControlFlow::Break(AiActionsStop::ChooseActionNone { player: actor });
        }
    };
    // The verified helper is the single classifier and checks that boundary
    // again when applying. Everything else, including payment finalize, uses
    // the ordinary interaction boundary and its issued revision/domain check.
    // CR 601.2h: The reducer owns payment of the spell's total cost.
    let is_stack_recheck_pass = verified_ai_stack_pass_player(state, &action).is_some();
    if !is_stack_recheck_pass && !contract.permits(state, actor, &action) {
        let error = EngineError::InvalidAction(
            "AI chose an action outside its issued decision contract".to_string(),
        );
        tracing::error!(
            ?semantic_owner,
            ?actor,
            "AI action violated decision contract"
        );
        return std::ops::ControlFlow::Break(AiActionsStop::ApplyFailed {
            player: actor,
            action: Box::new(action),
            error,
        });
    }
    let applied = if is_stack_recheck_pass {
        apply_verified_ai_priority_pass(state, actor, &contract, action.clone())
    } else {
        apply_interaction(state, actor, semantic_owner, action.clone())
    };
    match applied {
        Ok(result) => std::ops::ControlFlow::Continue(AiActionResult {
            action,
            state: state.clone(),
            events: result.events,
            log_entries: result.log_entries,
        }),
        Err(error) => {
            tracing::error!(player = ?actor, %error, "AI action apply failed — stopping");
            std::ops::ControlFlow::Break(AiActionsStop::ApplyFailed {
                player: actor,
                action: Box::new(action),
                error,
            })
        }
    }
}

/// [`run_ai_actions`] for a headless host. All-AI physical contests use a
/// deterministic virtual clock; contests with a human still hand off.
pub fn run_ai_actions_headless(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
) -> AiActionsRun {
    run_ai_actions_with_limit(
        state,
        ai_players,
        ai_configs,
        rng,
        session,
        ActionLimit::SafetyCap,
        ContestHosting::Headless,
    )
}

/// Headless host work does not spend the caller's AI action budget.
pub fn run_ai_actions_bounded_headless(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
    max_actions: usize,
) -> AiActionsRun {
    run_ai_actions_with_limit(
        state,
        ai_players,
        ai_configs,
        rng,
        session,
        ActionLimit::CallerBudget {
            requested: max_actions,
        },
        ContestHosting::Headless,
    )
}

/// Host settlements have their own finite budget, independent of AI actions.
fn settle_headless_host_contests(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    contest_hosting: ContestHosting,
    settlements: &mut usize,
    rng: &mut impl Rng,
) -> Result<bool, ()> {
    if contest_hosting != ContestHosting::Headless {
        return Ok(false);
    }
    let mut settled_any = false;
    while matches!(state.waiting_for, WaitingFor::PhysicalContest { .. }) {
        if *settlements >= MAX_HOST_SETTLEMENTS_PER_SEQUENCE {
            tracing::warn!(
                count = *settlements,
                "AI action loop hit host settlement cap — possible infinite loop"
            );
            return Err(());
        }
        if !crate::contest::settle_headless_contest(state, ai_players, rng) {
            break;
        }
        *settlements += 1;
        settled_any = true;
    }
    Ok(settled_any)
}

fn run_ai_actions_with_limit(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
    action_limit: ActionLimit,
    contest_hosting: ContestHosting,
) -> AiActionsRun {
    let mut results = Vec::new();
    let limit = action_limit.effective();

    if limit == 0 {
        return AiActionsRun {
            results,
            stop: AiActionsStop::ActionBudgetReached { limit },
        };
    }

    // Only committed AI actions spend the caller's budget. Host settlement may
    // resume another action at this same ordinal, even for a cap-one caller.
    let mut settlements = 0;
    while results.len() < limit {
        match run_ai_action(state, ai_players, ai_configs, rng, session) {
            std::ops::ControlFlow::Continue(result) => results.push(result),
            std::ops::ControlFlow::Break(AiActionsStop::NoEligibleAiActor) => {
                match settle_headless_host_contests(
                    state,
                    ai_players,
                    contest_hosting,
                    &mut settlements,
                    rng,
                ) {
                    Ok(true) => continue,
                    Ok(false) => {
                        return AiActionsRun {
                            results,
                            stop: AiActionsStop::NoEligibleAiActor,
                        };
                    }
                    Err(()) => {
                        return AiActionsRun {
                            results,
                            stop: AiActionsStop::ActionSafetyCapReached {
                                limit: MAX_HOST_SETTLEMENTS_PER_SEQUENCE,
                            },
                        };
                    }
                }
            }
            std::ops::ControlFlow::Break(stop) => return AiActionsRun { results, stop },
        }
    }

    // The budget counts applied AI actions only; host settlements are counted apart.
    // When the final budgeted AI action creates an all-AI contest, settle it now so
    // headless play does not hand off an unfinished contest as terminal NoEligibleAiActor.
    if let Err(()) =
        settle_headless_host_contests(state, ai_players, contest_hosting, &mut settlements, rng)
    {
        return AiActionsRun {
            results,
            stop: AiActionsStop::ActionSafetyCapReached {
                limit: MAX_HOST_SETTLEMENTS_PER_SEQUENCE,
            },
        };
    }

    if action_limit.is_safety_cap() {
        tracing::warn!(
            count = limit,
            "AI action loop hit safety cap — possible infinite loop"
        );
    }

    AiActionsRun {
        results,
        stop: if eligible_ai_decision(state, ai_players).is_none() {
            AiActionsStop::NoEligibleAiActor
        } else if action_limit.is_safety_cap() {
            AiActionsStop::ActionSafetyCapReached { limit }
        } else {
            AiActionsStop::ActionBudgetReached { limit }
        },
    }
}

/// Driver-relevant outcome of processing one `run_ai_actions` batch: how many
/// actions to add to a caller's running total and its exact stop condition.
///
/// phase#6080 follow-up: a batch can complete one or more actions (`results`
/// non-empty) and *still* carry a stop reason — e.g. it applies two
/// actions, then the third choice is `ChooseActionNone` or the fourth
/// `apply()` call fails. A driver that only inspects the stop reason when
/// `results.is_empty()` silently discards the diagnostic for exactly that
/// case, loops again, and may report a misleading `NoActor`/unknown reason
/// once a later, unrelated batch happens to come back empty. `driver_step`
/// is the single place that decision is made, so callers (and tests) don't
/// re-derive it ad hoc.
pub struct DriverStep {
    pub actions_taken: usize,
    pub stop: AiActionsStop,
}

/// Extracts the [`DriverStep`] for one batch. Callers should process
/// `results`'s individual `AiActionResult`s (logging, animation, dumps)
/// before or after calling this — it only reports the count/stop decision.
pub fn driver_step(results: AiActionsRun) -> DriverStep {
    DriverStep {
        actions_taken: results.results.len(),
        stop: results.stop,
    }
}

/// Why [`run_driver_loop`] returned: it either hit the action cap exactly, or a
/// batch carried a stop reason ([`AiActionsStop`]) at its boundary. One
/// fact, one type — not an `aborted: bool` plus an `Option<AiActionsStop>`
/// pair whose two illegal combinations (aborted with a reason, not-aborted with
/// none) a caller would have to defend against.
#[derive(Debug)]
pub enum DriverExit {
    /// `total_actions` reached `action_cap` with no break door firing first.
    CapReached,
    /// A batch stopped early at its boundary; carries the reason to report.
    BatchBreak(AiActionsStop),
}

/// Outcome of a [`run_driver_loop`] run: the total actions taken and why it
/// stopped.
#[derive(Debug)]
pub struct DriverOutcome {
    pub total_actions: usize,
    pub exit: DriverExit,
}

/// Drives repeated [`run_ai_actions_bounded`] batches until the action cap is
/// reached or a batch breaks, threading the remaining-budget arithmetic that
/// keeps a small `action_cap` from being overshot. This is the single authority
/// for the batch / remaining-budget boundary: `ai_commander`'s `main` and the
/// regression tests both drive it, so the exact-cap contract is exercised on the
/// production path rather than re-derived in a test-only mirror loop.
///
/// # Exact-cap contract
/// `DriverOutcome::total_actions` never exceeds `action_cap`. Each batch is
/// bounded to the *remaining* budget (`action_cap - total`), so the loop stops
/// exactly at the cap instead of overshooting by up to
/// `MAX_AI_ACTIONS_PER_SEQUENCE` within a final batch. This deliberately differs
/// from `duel_suite`'s `drive_game_observed`, whose documented contract checks
/// the cap only at batch boundaries and may overshoot within a batch — that
/// overshoot is baseline-witnessed and intentional there, so this helper must
/// not be "helpfully" unified with it.
///
/// # Observer contract
/// `on_batch(&mut results, &state, total)` fires exactly once per batch, with:
/// the batch results *mutably* (so the observer can `mem::take` per-action log
/// vectors while draining dumps — this is why the seam is `&mut`, unlike
/// `drive_game_observed`'s immutable `&GameState`-only observer); the
/// post-batch `state` immutably; and the *pre-batch* running `total` (turn and
/// ELIMINATED numbering in `ai_commander` depend on the pre-batch total, so the
/// observer must see the count as it stood before this batch's actions).
///
/// # Caller contract
/// `action_cap >= 1`. `ai_commander`'s CLI parsing guarantees this; the
/// `debug_assert!` enforces it in tests. `remaining` is a plain subtraction (not
/// `saturating_sub`): the `CapReached` abort door below breaks at
/// `total >= action_cap`, so `remaining >= 1` whenever the loop body runs. An
/// underflow here would be a real invariant violation and must panic rather than
/// be silently masked into a zero-budget no-op.
pub fn run_driver_loop(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
    action_cap: usize,
    on_batch: &mut dyn FnMut(&mut AiActionsRun, &GameState, usize),
) -> DriverOutcome {
    debug_assert!(action_cap > 0);
    let mut total: usize = 0;
    loop {
        let remaining = action_cap - total;
        let mut results =
            run_ai_actions_bounded_headless(state, ai_players, ai_configs, rng, session, remaining);
        on_batch(&mut results, &*state, total);

        let step = driver_step(results);
        total += step.actions_taken;
        match step.stop {
            AiActionsStop::ActionBudgetReached { .. }
            | AiActionsStop::ActionSafetyCapReached { .. }
                if total >= action_cap =>
            {
                return DriverOutcome {
                    total_actions: total,
                    exit: DriverExit::CapReached,
                };
            }
            AiActionsStop::ActionSafetyCapReached { .. } if total < action_cap => continue,
            stop => {
                return DriverOutcome {
                    total_actions: total,
                    exit: DriverExit::BatchBreak(stop),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policies::mulligan::{registry_keeps, FREE_REVEAL_BOUND};
    use engine::ai_support::legal_actions_for_viewer;
    use engine::game::deck_loading::{
        load_and_hydrate_decks, resolve_deck_list, DeckList, PlayerDeckList,
    };
    use engine::game::engine::{apply, start_game_with_starting_player};
    use engine::game::mulligan::free_reveal_futile_for;
    use engine::game::topology::shared_poison_members;
    use engine::game::zones::{add_to_zone, create_object, remove_from_zone};
    use engine::types::ability::EffectKind;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, ResolvedAbility, TargetFilter,
    };
    use engine::types::actions::MulliganChoice;
    use engine::types::card_type::CoreType;
    use engine::types::format::{ChallengeDeck, FormatConfig};
    use engine::types::game_state::{
        AutoMayChoice, MayTriggerAutoChoiceKey, MayTriggerOrigin, StackEntry, StackEntryKind,
        StackResolutionPolicy, UntapStepProgress, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use rand::SeedableRng;

    fn recheck_priority_state() -> GameState {
        recheck_priority_state_with_top_controller(PlayerId(0))
    }

    /// P0 is active and holds priority over one NoOp activated ability
    /// (70_001) controlled by `top_controller`; P1 controls "AI Recheck
    /// Action", a free activated draw.
    fn recheck_priority_state_with_top_controller(top_controller: PlayerId) -> GameState {
        let mut state = GameState::new_two_player(1);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.stack.push_back(StackEntry {
            id: ObjectId(70_001),
            source_id: ObjectId(70_001),
            controller: top_controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: ObjectId(70_001),
                ability: Box::new(engine::types::ability::ResolvedAbility::new(
                    Effect::NoOp,
                    Vec::new(),
                    ObjectId(70_001),
                    top_controller,
                )),
            },
        });
        let ability_source = create_object(
            &mut state,
            CardId(70_002),
            PlayerId(1),
            "AI Recheck Action".to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&ability_source)
            .expect("created battlefield object");
        object.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        state
    }

    fn push_no_op_stack_entry(state: &mut GameState, id: u64) {
        state.stack.push_back(StackEntry {
            id: ObjectId(id),
            source_id: ObjectId(id),
            controller: PlayerId(1),
            kind: StackEntryKind::ActivatedAbility {
                source_id: ObjectId(id),
                ability: Box::new(engine::types::ability::ResolvedAbility::new(
                    Effect::NoOp,
                    Vec::new(),
                    ObjectId(id),
                    PlayerId(1),
                )),
            },
        });
    }

    /// An optional "you may" NoOp trigger controlled by P1 whose auto-choice
    /// is Decline: an inert entry the stack resolver may batch.
    fn push_declined_no_op_trigger(state: &mut GameState, id: u64) {
        let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
        let mut ability = ResolvedAbility::new(Effect::NoOp, Vec::new(), ObjectId(id), PlayerId(1));
        ability.optional = true;
        ability.may_trigger_origin = Some(origin.clone());
        state.stack.push_back(StackEntry {
            id: ObjectId(id),
            source_id: ObjectId(id),
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(id),
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Declined Trigger".to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: PlayerId(1),
                source_id: ObjectId(id),
                origin,
            },
            AutoMayChoice::Decline,
        );
    }

    fn dummy_result(state: &GameState) -> AiActionResult {
        AiActionResult {
            action: GameAction::PassPriority,
            state: state.clone(),
            events: Vec::new(),
            log_entries: Vec::new(),
        }
    }

    #[test]
    fn driver_step_preserves_break_reason_from_non_empty_batch() {
        // The exact regression: a batch that completed an action must not
        // have its stop reason discarded just because `results` isn't
        // empty.
        let state = GameState::new_two_player(1);
        let run = AiActionsRun {
            results: vec![dummy_result(&state)],
            stop: AiActionsStop::ChooseActionNone {
                player: PlayerId(1),
            },
        };
        let step = driver_step(run);
        assert_eq!(step.actions_taken, 1);
        assert!(
            matches!(step.stop, AiActionsStop::ChooseActionNone { .. }),
            "stop reason from a non-empty batch must survive driver_step"
        );
    }

    #[test]
    fn driver_step_empty_batch_behavior_is_unchanged() {
        // Existing behavior (empty batch + break reason) must still work.
        let run = AiActionsRun {
            results: Vec::new(),
            stop: AiActionsStop::NoEligibleAiActor,
        };
        let step = driver_step(run);
        assert_eq!(step.actions_taken, 0);
        assert!(matches!(step.stop, AiActionsStop::NoEligibleAiActor));
    }

    #[test]
    fn driver_step_preserves_budget_stop() {
        let state = GameState::new_two_player(1);
        let run = AiActionsRun {
            results: vec![dummy_result(&state), dummy_result(&state)],
            stop: AiActionsStop::ActionBudgetReached { limit: 2 },
        };
        let step = driver_step(run);
        assert_eq!(step.actions_taken, 2);
        assert!(matches!(
            step.stop,
            AiActionsStop::ActionBudgetReached { limit: 2 }
        ));
    }

    #[test]
    fn zero_sized_bounded_batch_is_a_normal_budget_stop() {
        let mut state = GameState::new_two_player(1);
        let mut rng = rand::rng();
        let session = AiSession::arc_from_game(&state);
        let run = run_ai_actions_bounded(
            &mut state,
            &HashSet::new(),
            &HashMap::new(),
            &mut rng,
            &session,
            0,
        );

        assert!(run.is_empty());
        assert!(matches!(
            run.stop,
            AiActionsStop::ActionBudgetReached { limit: 0 }
        ));
    }

    #[test]
    fn exact_cap_caller_budget_is_not_a_safety_cap() {
        assert!(!ActionLimit::CallerBudget {
            requested: MAX_AI_ACTIONS_PER_SEQUENCE,
        }
        .is_safety_cap());
        assert!(ActionLimit::SafetyCap.is_safety_cap());
    }

    #[test]
    fn native_ai_pass_on_the_stack_uses_the_verified_recheck_seam() {
        let mut state = recheck_priority_state();
        let ai_players = HashSet::from([PlayerId(0)]);
        let ai_configs = HashMap::from([(PlayerId(0), AiConfig::default())]);
        let session = AiSession::arc_from_game(&state);
        let mut rng = rand::rng();

        let run =
            run_ai_actions_bounded(&mut state, &ai_players, &ai_configs, &mut rng, &session, 1);

        assert!(matches!(
            run.results.as_slice(),
            [AiActionResult {
                action: GameAction::PassPriority,
                ..
            }]
        ));
        assert_eq!(
            state
                .stack_resolution_session
                .as_ref()
                .map(|session| session.policy),
            Some(StackResolutionPolicy::RecheckNoMeaningfulPriorityAction),
            "the native AI runner must use the verified stack-pass seam"
        );
    }

    /// Adjudication (b) pin: an AI representative that has not verified a pass
    /// in the Recheck cohort is engine-passed over its own top entry by its
    /// standing pass (`priority::standing_priority_pass`), although it holds a
    /// meaningful activated ability, so it gets no decision before that entry
    /// resolves.
    #[test]
    fn unverified_ai_representative_is_standing_passed_over_its_own_top() {
        let ai_players = HashSet::from([PlayerId(0), PlayerId(1)]);
        let ai_configs = HashMap::from([
            (PlayerId(0), AiConfig::default()),
            (PlayerId(1), AiConfig::default()),
        ]);
        let mut rng = rand::rng();

        // Paired positive control: with the top under P0, P1 has no standing
        // pass, and the same one-action run stops at P1's window.
        let mut control = recheck_priority_state_with_top_controller(PlayerId(0));
        let control_session = AiSession::arc_from_game(&control);
        let control_run = run_ai_actions_bounded(
            &mut control,
            &ai_players,
            &ai_configs,
            &mut rng,
            &control_session,
            1,
        );
        assert!(matches!(
            control_run.results.as_slice(),
            [AiActionResult {
                action: GameAction::PassPriority,
                ..
            }]
        ));
        assert!(matches!(
            control.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        assert_eq!(
            control.stack.len(),
            1,
            "reach guard: P1's window is offered"
        );
        assert!(
            engine::ai_support::legal_actions(&control)
                .iter()
                .any(|action| matches!(action, GameAction::ActivateAbility { .. })),
            "reach guard: P1 holds a meaningful activated ability at its window"
        );

        let mut state = recheck_priority_state_with_top_controller(PlayerId(1));
        let session = AiSession::arc_from_game(&state);
        let run =
            run_ai_actions_bounded(&mut state, &ai_players, &ai_configs, &mut rng, &session, 1);
        let [AiActionResult {
            action: GameAction::PassPriority,
            events,
            ..
        }] = run.results.as_slice()
        else {
            panic!("only P0's verified pass is taken");
        };
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::StackResolved {
                    object_id: ObjectId(70_001)
                }
            )),
            "P1 is standing-passed over its own entry inside P0's dispatch"
        );
        assert!(state.stack.is_empty());
    }

    #[test]
    fn verified_pass_cache_drains_a_large_ai_stack_without_action_cap() {
        let mut state = recheck_priority_state();
        state.objects.clear();
        for id in 70_003..70_204 {
            push_no_op_stack_entry(&mut state, id);
        }
        let ai_players = HashSet::from([PlayerId(0), PlayerId(1)]);
        let ai_configs = HashMap::from([
            (PlayerId(0), AiConfig::default()),
            (PlayerId(1), AiConfig::default()),
        ]);
        let session = AiSession::arc_from_game(&state);
        let mut rng = rand::rng();

        engine::game::perf_counters::reset();
        let run = run_ai_actions(&mut state, &ai_players, &ai_configs, &mut rng, &session);
        let counters = engine::game::perf_counters::snapshot();

        assert!(
            matches!(run.stop, AiActionsStop::NoEligibleAiActor),
            "a completed fenced stack must not hit the generic AI action cap: {:?}",
            run.stop
        );
        assert_eq!(
            run.results.len(),
            26,
            "the fenced cache must keep a 201-entry stack well below the generic action cap"
        );
        assert!(state.stack.is_empty());
        assert!(state.stack_resolution_session.is_none());
        assert_eq!(
            counters.priority_cast_probe_builds, 0,
            "cached verified passes must avoid the recheck probe on every stack entry"
        );
    }

    /// CR 117.3b + CR 117.4 + CR 732.2b: P0's verified pass and P1's standing
    /// pass over its own triggers (`priority::standing_priority_pass`) authorize
    /// every skipped window over P1's run, so the declined trigger run resolves
    /// as one proven inert batch.
    #[test]
    fn verified_pass_cohort_batches_a_declined_trigger_stack() {
        let mut state = recheck_priority_state();
        state.objects.clear();
        for id in 70_003..70_204 {
            push_declined_no_op_trigger(&mut state, id);
        }
        let ai_players = HashSet::from([PlayerId(0), PlayerId(1)]);
        let ai_configs = HashMap::from([
            (PlayerId(0), AiConfig::default()),
            (PlayerId(1), AiConfig::default()),
        ]);
        let session = AiSession::arc_from_game(&state);
        let mut rng = rand::rng();

        engine::game::perf_counters::reset();
        let run = run_ai_actions(&mut state, &ai_players, &ai_configs, &mut rng, &session);
        let counters = engine::game::perf_counters::snapshot();

        assert!(
            matches!(run.stop, AiActionsStop::NoEligibleAiActor),
            "the drained stack ends the AI run: {:?}",
            run.stop
        );
        assert!(state.stack.is_empty());
        assert!(state.stack_resolution_session.is_none());
        assert_eq!(counters.priority_cast_probe_builds, 0);
        assert_eq!(counters.stack_inert_noop_batches, 1);
        assert_eq!(counters.stack_inert_noop_entries, 201);
    }

    /// The mulligan is over: no seat owes a declaration or a bottoming.
    fn left_the_mulligan(state: &GameState) -> bool {
        !matches!(
            state.waiting_for,
            WaitingFor::MulliganDecision { .. } | WaitingFor::OpeningHandBottomCards { .. }
        )
    }

    fn integration_db() -> engine::database::card_db::CardDatabase {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../engine/tests/fixtures/integration_cards.json.gz");
        let file = std::fs::File::open(path).expect("integration fixture should open");
        engine::database::card_db::CardDatabase::from_export_reader(flate2::read::GzDecoder::new(
            std::io::BufReader::new(file),
        ))
        .expect("integration fixture should load")
    }

    fn dandan_pile(
        db: &engine::database::card_db::CardDatabase,
        pile: &[(&str, usize)],
    ) -> engine::game::deck_loading::DeckPayload {
        let main_deck = pile
            .iter()
            .flat_map(|&(name, copies)| std::iter::repeat_n(name.to_string(), copies))
            .collect();
        resolve_deck_list(
            db,
            &engine::database::ComboTable::default(),
            &DeckList {
                player: PlayerDeckList {
                    main_deck,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
    }

    fn free_reveal() -> GameAction {
        GameAction::MulliganDecision {
            choice: engine::types::actions::MulliganChoice::FreeReveal,
        }
    }

    /// Both seats AI on Dandan piles whose every redraw is futile: the native
    /// driver leaves the mulligan without taking a free reveal.
    #[test]
    fn native_ai_pair_leaves_the_mulligan_on_a_futile_dandan_pile() {
        let db = integration_db();
        let (p0, p1) = (PlayerId(0), PlayerId(1));

        for pile in [&[("Island", 80)][..], &[("Island", 79), ("Opt", 1)][..]] {
            let payload = dandan_pile(&db, pile);
            let mut state = GameState::new(FormatConfig::dandan(), 2, 0);
            load_and_hydrate_decks(&mut state, &payload, Some(&db));
            start_game_with_starting_player(&mut state, p0);
            for seat in [p0, p1] {
                assert!(
                    legal_actions_for_viewer(&state, seat)
                        .0
                        .contains(&free_reveal()),
                    "reach: {seat:?} is issued the reveal"
                );
                assert!(free_reveal_futile_for(&state, seat), "{pile:?} {seat:?}");
            }

            let ai_players = HashSet::from([p0, p1]);
            let ai_configs = HashMap::from([(p0, AiConfig::default()), (p1, AiConfig::default())]);
            let session = AiSession::arc_from_game(&state);
            let mut rng = rand::rngs::SmallRng::seed_from_u64(0);
            let run = run_ai_actions_bounded(
                &mut state,
                &ai_players,
                &ai_configs,
                &mut rng,
                &session,
                20,
            );

            assert!(
                left_the_mulligan(&run.results.last().expect("the AI acted").state),
                "{pile:?}: the mulligan ends within 20 actions"
            );
            assert!(left_the_mulligan(&state), "{pile:?}");
            assert!(
                run.results.iter().all(|r| r.action != free_reveal()),
                "{pile:?}"
            );
        }
    }

    /// A 78 Island + 2 Opt pile with both hands returned to it, the pile ordered
    /// by object id, seat 0 dealt seven Islands and seat 1 `ai_opts` Opts among
    /// seven cards, and the RNG reseeded: the whole chain is a function of `seed`.
    fn pinned_game(
        db: &engine::database::card_db::CardDatabase,
        seed: u64,
        ai_opts: usize,
    ) -> GameState {
        let (p0, p1) = (PlayerId(0), PlayerId(1));
        let mut state = GameState::new(FormatConfig::dandan(), 2, seed);
        load_and_hydrate_decks(
            &mut state,
            &dandan_pile(db, &[("Island", 78), ("Opt", 2)]),
            Some(db),
        );
        start_game_with_starting_player(&mut state, p0);
        let holder = state.zone_storage_seat(Zone::Library, p0);
        for seat in [p0, p1] {
            let hand: Vec<ObjectId> = state.players[seat.0 as usize]
                .hand
                .iter()
                .copied()
                .collect();
            for id in hand {
                remove_from_zone(&mut state, id, Zone::Hand, seat);
                add_to_zone(&mut state, id, Zone::Library, holder);
                state.objects.get_mut(&id).unwrap().zone = Zone::Library;
            }
        }
        let mut pile: Vec<ObjectId> = state.library_of(p0).iter().copied().collect();
        pile.sort();
        *state.library_of_mut(p0) = pile.iter().copied().collect();
        let named = |state: &GameState, name: &str| -> Vec<ObjectId> {
            pile.iter()
                .copied()
                .filter(|id| state.objects[id].name == name)
                .collect()
        };
        let (islands, opts) = (named(&state, "Island"), named(&state, "Opt"));
        let mut ai_hand = islands[7..14 - ai_opts].to_vec();
        ai_hand.extend_from_slice(&opts[..ai_opts]);
        for (seat, cards) in [(p0, islands[..7].to_vec()), (p1, ai_hand)] {
            for id in cards {
                remove_from_zone(&mut state, id, Zone::Library, holder);
                add_to_zone(&mut state, id, Zone::Hand, seat);
                let object = state.objects.get_mut(&id).unwrap();
                object.zone = Zone::Hand;
                object.owner = seat;
            }
        }
        state.rng_seed = seed;
        state.rng = engine::types::game_state::GameRng::seed_from_u64(seed);
        state
    }

    /// One AI seat against a human who kept seven Islands: every redraw the
    /// pile can deal is still offered the reveal, so only the bound ends the chain.
    #[test]
    fn native_ai_leaves_the_mulligan_against_a_kept_hand_that_leaves_the_reveal_open() {
        let db = integration_db();
        let (human, ai) = (PlayerId(0), PlayerId(1));
        for ai_opts in [0, 1] {
            for seed in [8, 11] {
                let mut state = pinned_game(&db, seed, ai_opts);
                apply(
                    &mut state,
                    human,
                    GameAction::MulliganDecision {
                        choice: MulliganChoice::Keep,
                    },
                )
                .expect("the human keeps");
                assert!(
                    legal_actions_for_viewer(&state, ai)
                        .0
                        .contains(&free_reveal()),
                    "reach: the AI is issued the reveal"
                );
                assert!(!free_reveal_futile_for(&state, ai));
                assert!(!registry_keeps(&state, ai), "reach: the registry rejects");

                let ai_players = HashSet::from([ai]);
                let ai_configs = HashMap::from([(ai, AiConfig::default())]);
                let session = AiSession::arc_from_game(&state);
                let mut rng = rand::rngs::SmallRng::seed_from_u64(seed);
                let run = run_ai_actions(&mut state, &ai_players, &ai_configs, &mut rng, &session);

                let label = format!("{ai_opts} Opts seed {seed}");
                assert!(left_the_mulligan(&state), "{label}");
                assert!(
                    matches!(run.stop, AiActionsStop::NoEligibleAiActor),
                    "{label}: {:?}",
                    run.stop
                );
                let reveals = run
                    .results
                    .iter()
                    .filter(|r| r.action == free_reveal())
                    .count();
                assert!(
                    (1..=usize::from(FREE_REVEAL_BOUND)).contains(&reveals),
                    "{label}: {reveals} reveals"
                );
            }
        }
    }

    /// CR 805.5a: on a shared survivor turn parked on the human
    /// representative, AI allies are driven — their actions pass the
    /// decision-contract gate and apply. Pre-fix, `permits` required the
    /// semantic owner to be the single holder, so every ally action was
    /// rejected and the driver stopped with `ApplyFailed`: allies never
    /// played a land or passed (the Horde idle-allies report).
    #[test]
    fn ai_ally_acts_on_shared_survivor_turn() {
        let rep = PlayerId(1);
        let ally = PlayerId(2);
        let ally2 = PlayerId(3);
        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::ZombiesHorde.default_ruleset()),
            4,
            42,
        );
        state.phase = Phase::PreCombatMain;
        state.active_player = rep;
        state.priority_player = rep;
        state.waiting_for = WaitingFor::Priority { player: rep };
        for (seat, name) in [(ally, "Ally Forest"), (ally2, "Ally2 Forest")] {
            let forest = create_object(&mut state, CardId(1), seat, name.to_string(), Zone::Hand);
            state
                .objects
                .get_mut(&forest)
                .expect("created land must exist")
                .card_types
                .core_types
                .push(CoreType::Land);
            state.players[seat.0 as usize].hand.push_back(forest);
        }

        let ai_players = HashSet::from([ally, ally2]);
        let ai_configs = HashMap::from([(ally, AiConfig::default()), (ally2, AiConfig::default())]);
        let session = AiSession::arc_from_game(&state);
        let mut rng = rand::rngs::SmallRng::seed_from_u64(42);
        let run = run_ai_actions(&mut state, &ai_players, &ai_configs, &mut rng, &session);

        assert!(
            !matches!(run.stop, AiActionsStop::ApplyFailed { .. }),
            "ally actions must apply, got {:?}",
            run.stop
        );
        let ally_passed = state.priority_passes.contains(&ally);
        let ally_land_played = state
            .battlefield
            .iter()
            .any(|id| state.objects.get(id).is_some_and(|o| o.controller == ally));
        assert!(
            ally_passed || ally_land_played,
            "the ally must act or pass on its team's turn"
        );

        // S1 context (labelled, never asserted): Stage 1's unbounded drain
        // ends this horde fixture's game (`GameOver`), so no same-fixture
        // Stage-2 presence is asserted here — a presence on the drained
        // fixture is unachievable. The Stage-2 honesty proof lives in
        // `ai_ally_pass_records_on_live_team_stack`, on a dedicated live 2HG
        // fixture, instead.
    }

    /// CR 805.5b Stage-2 honesty on a live stack: the AI ally's pass records
    /// on a dedicated 2HG fixture. Stage 1's horde fixture cannot carry this
    /// proof — its unbounded drain ends the game — so this test parks a live
    /// window on the human representative with the ally holding an empty hand:
    /// the ally's only legal action is the pass, and the human rep never
    /// passes, so unanimous pass is unreachable and the stack must stay.
    #[test]
    fn ai_ally_pass_records_on_live_team_stack() {
        let rep = PlayerId(0);
        let ally = PlayerId(1);
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 7);
        state.phase = Phase::PreCombatMain;
        state.active_player = rep;
        state.priority_player = rep;
        state.waiting_for = WaitingFor::Priority { player: rep };
        push_no_op_stack_entry(&mut state, 91_001);
        // Layout probe: the 2HG family pools poison per team off the same
        // `team_members` authority (`topology.rs`), so the public
        // `shared_poison_members` read-out proves the ally shares the rep's
        // team. If the 2HG seat layout ever differs, this reds first.
        assert!(
            shared_poison_members(&state, rep).contains(&ally),
            "layout: the ally must share the representative's team"
        );
        // Pre-run liveness quartet: a dead fixture must red here, never as a
        // vacuous post-run green.
        assert!(
            state.game_end.is_none(),
            "the fixture must be live before the driver runs"
        );
        assert!(
            matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == rep),
            "the window must be parked on the human rep, got {:?}",
            state.waiting_for
        );
        assert!(!state.stack.is_empty(), "the stack must be live");
        assert!(
            state.priority_passes.is_empty(),
            "no passes may be recorded before the driver runs"
        );
        // Absence: construction yields empty passes; pre-seeding the ally
        // reds this and proves the presence below is driver-produced.
        assert!(
            !state.priority_passes.contains(&ally),
            "absence: the ally must hold no pass before the driver runs"
        );
        let top_id = state.stack.back().map(|entry| entry.id);

        let ai_players = HashSet::from([ally]);
        let ai_configs = HashMap::from([(ally, AiConfig::default())]);
        let session = AiSession::arc_from_game(&state);
        let mut rng = rand::rngs::SmallRng::seed_from_u64(7);
        let run = run_ai_actions(&mut state, &ai_players, &ai_configs, &mut rng, &session);

        assert!(
            !matches!(run.stop, AiActionsStop::ApplyFailed { .. }),
            "ally stack passes must apply, got {:?}",
            run.stop
        );
        // Presence is `contains`, not equality: the driver's verified-pass
        // session path may also account the parked representative's window,
        // while a direct `apply` records exactly the acting teammate. The
        // test proves the ally's pass records; it does not lock in the
        // session path's extra record.
        assert!(
            state.priority_passes.contains(&ally),
            "presence: the ally's pass must record on the live stack"
        );
        assert_eq!(
            run.results.len(),
            1,
            "exactly the ally's single pass — Stage-2 honesty, not a vacuous green"
        );
        assert!(
            matches!(run.stop, AiActionsStop::NoEligibleAiActor),
            "clean handoff once no AI seat can act, got {:?}",
            run.stop
        );
        assert!(
            state.game_end.is_none(),
            "anti-S1 tripwire: the game must still be live after the run"
        );
        assert_eq!(state.stack.len(), 1, "the stack must stay");
        assert_eq!(
            state.stack.back().map(|entry| entry.id),
            top_id,
            "the same top entry must survive — no resolution ran"
        );
    }

    /// U6 hardening (no revert claim): outside `Priority`, an already-passed
    /// member stays eligible — the unpassed filter lives only on the
    /// `Priority` arm. Exhaustiveness is compile-time (no fallback arm); this
    /// pins dispatch parity for one window per family (turn action, step
    /// choice, resolution choice).
    #[test]
    fn non_priority_windows_ignore_pass_records() {
        let rep = PlayerId(1);
        let ally = PlayerId(2);
        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::ZombiesHorde.default_ruleset()),
            4,
            42,
        );
        state.phase = Phase::PreCombatMain;
        state.active_player = rep;
        state.priority_player = rep;
        state.priority_passes.insert(ally);
        state.priority_pass_count = 1;
        let ai_players = HashSet::from([ally]);

        // Contrast: on `Priority` the passed ally is filtered out, so nobody
        // is eligible.
        state.waiting_for = WaitingFor::Priority { player: rep };
        assert!(
            eligible_ai_decision(&state, &ai_players).is_none(),
            "the unpassed filter must hide a passed member on Priority"
        );

        let windows = [
            (
                "DeclareAttackers",
                WaitingFor::DeclareAttackers {
                    player: ally,
                    valid_attacker_ids: Vec::new(),
                    valid_attack_targets: Vec::new(),
                    valid_attack_targets_by_attacker: None,
                    attacker_constraints: Default::default(),
                },
            ),
            (
                "ChooseUntapSubset",
                WaitingFor::ChooseUntapSubset {
                    player: ally,
                    group: Vec::new(),
                    max: 1,
                    progress: UntapStepProgress::AfterTurnBasedActions,
                },
            ),
            (
                "DiscardChoice",
                WaitingFor::DiscardChoice {
                    player: ally,
                    count: 1,
                    cards: Vec::new(),
                    source_id: ObjectId(9),
                    effect_kind: EffectKind::Discard,
                    up_to: false,
                    unless_filter: None,
                    discard_frame: None,
                },
            ),
        ];
        for (label, waiting) in windows {
            state.waiting_for = waiting;
            assert_eq!(
                eligible_ai_decision(&state, &ai_players),
                Some((ally, ally)),
                "{label} must resolve to the engine's acting players unchanged"
            );
        }
    }
}
