//! The game-side LLM decision: engine position in, engine `GameAction` out.
//!
//! The model never authors an action. It is shown the engine-issued candidate
//! domain — the same `AiDecisionContract` the heuristic AI selects from — and
//! returns an INDEX into it. Everything the engine already enforces about AI
//! actions (contract issuance, authority binding, re-validation on submit) is
//! untouched, so an LLM seat cannot reach an action a heuristic seat could not.

use engine::ai_support::AiDecisionContract;
use engine::database::CardDatabase;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::log::GameLogEntry;
use phase_ai::config::AiDifficulty;

use crate::error::{LlmError, LlmResult};
use crate::fingerprint::fingerprint_of;
use crate::prompt::{
    decode_choice, difficulty_brief, history_window, LlmPrompt, RESPONSE_CONTRACT,
};
use crate::render::action::{describe_action, describe_waiting_for, primary_object_name};
use crate::render::game::{render_board, GameRenderOptions};

/// Everything a transport needs to run one LLM decision round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameDecisionRequest {
    pub prompt: LlmPrompt,
    /// Identity of the option domain the prompt was built over. Hand this back
    /// to [`select_action`] so a decision that moved on is refused instead of
    /// misapplied.
    pub fingerprint: String,
    pub option_count: usize,
}

/// Difficulties below this see the board without Oracle text. They are meant to
/// misplay in the way a new player misplays — off the board, not off exact card
/// text — and withholding the text is the lever that produces that.
fn render_options(difficulty: AiDifficulty) -> GameRenderOptions {
    GameRenderOptions {
        history_lines: history_window(difficulty),
        include_oracle_text: !matches!(difficulty, AiDifficulty::VeryEasy),
        oracle_text_budget: match difficulty {
            AiDifficulty::VeryEasy | AiDifficulty::Easy => 160,
            AiDifficulty::Medium => 240,
            AiDifficulty::Hard | AiDifficulty::VeryHard | AiDifficulty::CEDH => 400,
        },
    }
}

/// Render the option domain exactly once, so the prompt the model reads and the
/// fingerprint that guards it are derived from the same strings.
fn option_lines(state: &GameState, contract: &AiDecisionContract) -> Vec<String> {
    contract
        .candidates
        .iter()
        .map(|candidate| {
            let described = describe_action(state, &candidate.action);
            match primary_object_name(state, &candidate.action) {
                Some(name) => format!("{name} — {described}"),
                None => described,
            }
        })
        .collect()
}

/// The fingerprint of a contract's option domain.
pub fn decision_fingerprint(state: &GameState, contract: &AiDecisionContract) -> String {
    let revision = contract.state_revision.to_string();
    let owner = contract.semantic_owner.0.to_string();
    let actor = contract.authorized_actor.0.to_string();
    let lines = option_lines(state, contract);
    fingerprint_of(
        [revision.as_str(), owner.as_str(), actor.as_str()]
            .into_iter()
            .chain(lines.iter().map(String::as_str)),
    )
}

/// Build the prompt for one engine decision.
///
/// `history` is the engine-authored game log the transport has accumulated from
/// prior `ActionResult`s. It is engine-authored data being handed back, not a
/// display-layer derivation: this module renders it, the transport only stores
/// it.
pub fn build_game_decision_prompt(
    state: &GameState,
    contract: &AiDecisionContract,
    difficulty: AiDifficulty,
    db: Option<&CardDatabase>,
    history: &[GameLogEntry],
) -> LlmResult<GameDecisionRequest> {
    let options = option_lines(state, contract);
    if options.is_empty() {
        return Err(LlmError::UndecodableChoice {
            detail: "the engine issued no candidate actions".to_string(),
        });
    }

    let viewer = contract.semantic_owner;
    // The engine's own visibility authority decides what this seat may read.
    let visible = filter_state_for_viewer(state, viewer);
    let board = render_board(&visible, viewer, db, history, &render_options(difficulty));

    let system = format!(
        "You are playing a game of Magic: The Gathering as Player {}. You are one \
         seat at the table and you play to win.\n\n{}\n\nYou will be shown the \
         position and a numbered list of the ONLY legal options available to you \
         right now. The list is complete and authoritative: an option that is not \
         listed is not legal, and every listed option is legal. Choose exactly one \
         by its number.\n\n{}",
        viewer.0,
        difficulty_brief(difficulty),
        RESPONSE_CONTRACT,
    );

    let user = format!(
        "{board}\n--- DECISION ---\nThe game is waiting on you for: {}\n\nYour legal options:\n{}\n",
        describe_waiting_for(&state.waiting_for),
        options
            .iter()
            .enumerate()
            .map(|(index, line)| format!("  [{index}] {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    Ok(GameDecisionRequest {
        fingerprint: decision_fingerprint(state, contract),
        option_count: options.len(),
        prompt: LlmPrompt { system, user },
    })
}

/// The action an LLM reply selects, plus the model's stated reason.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmActionSelection {
    pub action: GameAction,
    pub reasoning: Option<String>,
}

/// Bind a completion back to an engine action.
///
/// Refuses on any mismatch rather than approximating: a moved-on decision, an
/// out-of-range index, or an undecodable reply all surface as errors so the
/// caller falls back to the heuristic AI.
pub fn select_action(
    state: &GameState,
    contract: &AiDecisionContract,
    expected_fingerprint: &str,
    completion_text: &str,
) -> LlmResult<LlmActionSelection> {
    if decision_fingerprint(state, contract) != expected_fingerprint {
        return Err(LlmError::StaleDecision);
    }
    let choice = decode_choice(completion_text, contract.candidates.len(), 1)?;
    let index = *choice
        .indices
        .first()
        .ok_or_else(|| LlmError::UndecodableChoice {
            detail: "reply named no option".to_string(),
        })?;
    Ok(LlmActionSelection {
        action: contract.candidates[index].action.clone(),
        reasoning: choice.reasoning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, CandidateAction, TacticalClass};
    use engine::types::player::PlayerId;

    fn contract(actions: Vec<GameAction>) -> AiDecisionContract {
        AiDecisionContract {
            semantic_owner: PlayerId(1),
            authorized_actor: PlayerId(1),
            state_revision: 42,
            candidates: actions
                .into_iter()
                .map(|action| CandidateAction {
                    action,
                    metadata: ActionMetadata::for_actor(Some(PlayerId(1)), TacticalClass::Utility),
                })
                .collect(),
        }
    }

    fn two_option_contract() -> AiDecisionContract {
        contract(vec![
            GameAction::PassPriority,
            GameAction::ChoosePlayDraw { play_first: true },
        ])
    }

    #[test]
    fn the_prompt_numbers_every_issued_candidate() {
        let state = GameState::default();
        let request = build_game_decision_prompt(
            &state,
            &two_option_contract(),
            AiDifficulty::Medium,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(request.option_count, 2);
        assert!(request.prompt.user.contains("[0] Pass Priority"));
        assert!(request.prompt.user.contains("[1] Choose Play Draw"));
    }

    #[test]
    fn the_difficulty_brief_reaches_the_system_prompt() {
        let state = GameState::default();
        for difficulty in [AiDifficulty::VeryEasy, AiDifficulty::CEDH] {
            let request =
                build_game_decision_prompt(&state, &two_option_contract(), difficulty, None, &[])
                    .unwrap();
            assert!(request.prompt.system.contains(difficulty_brief(difficulty)));
        }
    }

    #[test]
    fn an_empty_candidate_domain_is_refused_before_any_network_call() {
        let state = GameState::default();
        assert!(matches!(
            build_game_decision_prompt(&state, &contract(vec![]), AiDifficulty::Medium, None, &[]),
            Err(LlmError::UndecodableChoice { .. })
        ));
    }

    #[test]
    fn a_valid_reply_selects_the_named_candidate() {
        let state = GameState::default();
        let contract = two_option_contract();
        let fingerprint = decision_fingerprint(&state, &contract);
        let selection = select_action(
            &state,
            &contract,
            &fingerprint,
            r#"{"choice":1,"reason":"on the play"}"#,
        )
        .unwrap();
        assert_eq!(
            selection.action,
            GameAction::ChoosePlayDraw { play_first: true }
        );
        assert_eq!(selection.reasoning.as_deref(), Some("on the play"));
    }

    #[test]
    fn a_changed_decision_is_refused_as_stale() {
        let state = GameState::default();
        let issued = two_option_contract();
        // The fingerprint the request was built over, taken on a DIFFERENT
        // option domain: exactly what a decision that moved on looks like.
        let stale = decision_fingerprint(&state, &contract(vec![GameAction::PassPriority]));
        assert_eq!(
            select_action(&state, &issued, &stale, r#"{"choice":0}"#),
            Err(LlmError::StaleDecision)
        );
    }

    #[test]
    fn an_out_of_range_reply_never_reaches_an_action() {
        let state = GameState::default();
        let contract = two_option_contract();
        let fingerprint = decision_fingerprint(&state, &contract);
        assert!(matches!(
            select_action(&state, &contract, &fingerprint, r#"{"choice":7}"#),
            Err(LlmError::ChoiceOutOfRange { .. })
        ));
    }

    #[test]
    fn the_lowest_difficulty_sees_no_history_and_no_oracle_text() {
        let options = render_options(AiDifficulty::VeryEasy);
        assert_eq!(options.history_lines, 0);
        assert!(!options.include_oracle_text);
        assert!(render_options(AiDifficulty::VeryHard).include_oracle_text);
    }
}
