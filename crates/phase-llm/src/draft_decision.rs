//! The draft-side LLM decision: a seat's pack in, pack indices out.
//!
//! The same contract as [`crate::game_decision`]: the model never names a card
//! id, it names an INDEX into the pack it was shown. The caller maps indices to
//! `instance_id`s against the live pack and applies the pick through the normal
//! draft reducer, so an LLM drafter cannot take a card that is not in front of
//! it.

use std::collections::BTreeMap;

use draft_core::types::DraftCardInstance;
use draft_core::view::DraftPlayerView;
use engine::database::CardDatabase;
use phase_ai::config::AiDifficulty;

use crate::error::{LlmError, LlmResult};
use crate::fingerprint::fingerprint_of;
use crate::prompt::{
    decode_choice, difficulty_brief, multi_response_contract, LlmPrompt, RESPONSE_CONTRACT,
};
use crate::render::draft::{card_line, format_context, pool_context, progress_context, SetNames};

/// Everything a transport needs to run one LLM pick round trip for one seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftPickRequest {
    pub prompt: LlmPrompt,
    /// Identity of the pack this prompt was built over. A pack that has since
    /// passed or changed will not match, and the pick falls back.
    pub fingerprint: String,
    pub option_count: usize,
    /// CR 903.13b: how many cards this seat's pick step takes.
    pub required_pick_count: usize,
}

/// Oracle-text budget per pack entry. A drafter reads the whole card, so this
/// is more generous than the in-game board render, but it is still bounded:
/// a 15-card pack at full text would dominate the prompt.
fn oracle_budget(difficulty: AiDifficulty) -> usize {
    match difficulty {
        // A brand-new drafter picks off the picture and the rarity.
        AiDifficulty::VeryEasy => 0,
        AiDifficulty::Easy => 160,
        AiDifficulty::Medium => 280,
        AiDifficulty::Hard | AiDifficulty::VeryHard | AiDifficulty::CEDH => 420,
    }
}

fn option_lines(
    pack: &[DraftCardInstance],
    db: Option<&CardDatabase>,
    difficulty: AiDifficulty,
) -> Vec<String> {
    pack.iter()
        .map(|card| card_line(card, db, oracle_budget(difficulty)))
        .collect()
}

/// The fingerprint of the pack a prompt was built over.
pub fn pick_fingerprint(seat: u8, pack: &[DraftCardInstance]) -> String {
    let seat = seat.to_string();
    fingerprint_of(
        std::iter::once(seat.as_str()).chain(pack.iter().map(|card| card.instance_id.as_str())),
    )
}

/// Build the pick prompt for one seat.
///
/// `view` must be that seat's own projection
/// (`draft_core::view::filter_for_player`), never the session: the projection is
/// what makes an LLM drafter blind to other seats' pools and to unopened packs.
pub fn build_draft_pick_prompt(
    seat: u8,
    view: &DraftPlayerView,
    difficulty: AiDifficulty,
    db: Option<&CardDatabase>,
    set_names: &SetNames,
) -> LlmResult<DraftPickRequest> {
    let pack = view.current_pack.as_deref().unwrap_or_default();
    if pack.is_empty() {
        return Err(LlmError::UndecodableChoice {
            detail: "this seat has no pack to pick from".to_string(),
        });
    }
    let required = view.required_pick_count.clamp(1, pack.len());
    let options = option_lines(pack, db, difficulty);

    let system = format!(
        "You are drafting a Magic: The Gathering limited deck. You are one seat \
         in the pod and you are building the best 40-card deck you can from what \
         you take.\n\n{}\n\nYou will be shown the format, your pool so far, and \
         the pack in front of you as a numbered list. Pick from that list only.\n\n{}",
        difficulty_brief(difficulty),
        if required > 1 {
            multi_response_contract(required)
        } else {
            RESPONSE_CONTRACT.to_string()
        },
    );

    let instruction = if required > 1 {
        // CR 903.13b: a Commander Draft seat takes two cards per step.
        format!("Take {required} cards from this pack, best first.")
    } else {
        "Take one card from this pack.".to_string()
    };

    let user = format!(
        "=== DRAFT ===\n{}\n\n{}\n\n{}\n--- PACK ---\n{}\n\n{instruction}\n",
        format_context(view, set_names),
        progress_context(view),
        pool_context(&view.pool),
        options
            .iter()
            .enumerate()
            .map(|(index, line)| format!("  [{index}] {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    Ok(DraftPickRequest {
        fingerprint: pick_fingerprint(seat, pack),
        option_count: pack.len(),
        required_pick_count: required,
        prompt: LlmPrompt { system, user },
    })
}

/// The cards an LLM reply selects, as `instance_id`s resolved against the live
/// pack, plus the model's stated reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmPickSelection {
    pub card_instance_ids: Vec<String>,
    pub reasoning: Option<String>,
}

/// Bind a completion back to pack cards.
///
/// The pack is re-read from the live session at resolve time, so a reply that
/// arrives after the pack has passed is refused as stale rather than applied to
/// whatever now sits at that index.
pub fn select_picks(
    seat: u8,
    pack: &[DraftCardInstance],
    required: usize,
    expected_fingerprint: &str,
    completion_text: &str,
) -> LlmResult<LlmPickSelection> {
    if pick_fingerprint(seat, pack) != expected_fingerprint {
        return Err(LlmError::StaleDecision);
    }
    let wanted = required.clamp(1, pack.len());
    let choice = decode_choice(completion_text, pack.len(), wanted)?;
    // A model that named fewer cards than the step takes has not answered the
    // question; the caller completes the step with the heuristic bot rather
    // than guessing which extra card it meant.
    if choice.indices.len() < wanted {
        return Err(LlmError::UndecodableChoice {
            detail: format!(
                "pick step takes {wanted} cards but the reply named {}",
                choice.indices.len()
            ),
        });
    }
    Ok(LlmPickSelection {
        card_instance_ids: choice
            .indices
            .iter()
            .map(|index| pack[*index].instance_id.clone())
            .collect(),
        reasoning: choice.reasoning,
    })
}

/// Convenience constructor for the code -> name map a caller passes in.
pub fn set_names_from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> SetNames {
    pairs
        .into_iter()
        .map(|(code, name)| (code.to_uppercase(), name))
        .collect::<BTreeMap<_, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, name: &str) -> DraftCardInstance {
        DraftCardInstance {
            instance_id: id.to_string(),
            name: name.to_string(),
            set_code: "MRD".to_string(),
            collector_number: "1".to_string(),
            rarity: "common".to_string(),
            colors: vec!["R".to_string()],
            cmc: 3,
            type_line: "Creature".to_string(),
            draft_effect: None,
        }
    }

    fn pack() -> Vec<DraftCardInstance> {
        vec![card("a", "Alpha"), card("b", "Beta"), card("c", "Gamma")]
    }

    #[test]
    fn a_single_pick_reply_resolves_to_one_instance_id() {
        let pack = pack();
        let fingerprint = pick_fingerprint(3, &pack);
        let selection = select_picks(
            3,
            &pack,
            1,
            &fingerprint,
            r#"{"choice":1,"reason":"on colour"}"#,
        )
        .unwrap();
        assert_eq!(selection.card_instance_ids, vec!["b".to_string()]);
        assert_eq!(selection.reasoning.as_deref(), Some("on colour"));
    }

    #[test]
    fn a_two_card_step_requires_two_named_cards() {
        let pack = pack();
        let fingerprint = pick_fingerprint(0, &pack);
        let selection = select_picks(0, &pack, 2, &fingerprint, r#"{"choice":[2,0]}"#).unwrap();
        assert_eq!(
            selection.card_instance_ids,
            vec!["c".to_string(), "a".to_string()]
        );
        assert!(matches!(
            select_picks(0, &pack, 2, &fingerprint, r#"{"choice":[2]}"#),
            Err(LlmError::UndecodableChoice { .. })
        ));
    }

    #[test]
    fn a_pack_that_has_moved_on_is_refused_as_stale() {
        let original = pack();
        let fingerprint = pick_fingerprint(0, &original);
        let passed = vec![card("x", "Delta"), card("y", "Epsilon")];
        assert_eq!(
            select_picks(0, &passed, 1, &fingerprint, r#"{"choice":0}"#),
            Err(LlmError::StaleDecision)
        );
    }

    #[test]
    fn the_same_pack_at_a_different_seat_is_a_different_decision() {
        let pack = pack();
        assert_ne!(pick_fingerprint(1, &pack), pick_fingerprint(2, &pack));
    }

    #[test]
    fn an_out_of_range_pick_never_resolves_to_a_card() {
        let pack = pack();
        let fingerprint = pick_fingerprint(0, &pack);
        assert!(matches!(
            select_picks(0, &pack, 1, &fingerprint, r#"{"choice":9}"#),
            Err(LlmError::ChoiceOutOfRange { .. })
        ));
    }

    #[test]
    fn set_names_are_keyed_case_insensitively() {
        let names = set_names_from_pairs([("mrd".to_string(), "Mirrodin".to_string())]);
        assert_eq!(names.get("MRD").map(String::as_str), Some("Mirrodin"));
    }

    #[test]
    fn the_lowest_difficulty_drafts_without_oracle_text() {
        assert_eq!(oracle_budget(AiDifficulty::VeryEasy), 0);
        assert!(oracle_budget(AiDifficulty::VeryHard) > 0);
    }
}
