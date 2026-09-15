//! The prompt pair handed to a provider, the difficulty persona that shapes it,
//! and the decoder that turns a free-text reply back into engine option indices.

use phase_ai::config::AiDifficulty;
use serde::Serialize;
use serde_json::Value;

use crate::error::{LlmError, LlmResult};

/// A system/user message pair. Every protocol in [`crate::wire`] carries these
/// two fields, whatever it calls them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmPrompt {
    pub system: String,
    pub user: String,
}

/// What the model was asked to do, decoded back out of its reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmChoice {
    /// Engine option indices, in the order the model named them, deduplicated
    /// and already bounds-checked against the offered option count.
    pub indices: Vec<usize>,
    /// The model's own one-line justification, when it supplied one. Surfaced
    /// in local diagnostics; never fed back into the engine.
    pub reasoning: Option<String>,
}

/// How hard the LLM opponent is asked to play.
///
/// Difficulty is not decoration here: it is the single lever that makes an LLM
/// seat comparable to a heuristic seat at the same setting, so it shapes the
/// persona, the strategic instruction, and how much of the board the model is
/// asked to reason over.
pub fn difficulty_brief(difficulty: AiDifficulty) -> &'static str {
    match difficulty {
        AiDifficulty::VeryEasy => {
            "You are a brand-new player who has just learned the rules. Play the \
             first reasonable-looking option. Do not plan ahead, do not count \
             your opponent's open mana, and do not hold cards back for later \
             turns. Making a clearly suboptimal but legal play is expected and \
             correct at this level."
        }
        AiDifficulty::Easy => {
            "You are a casual kitchen-table player. Play your cards roughly on \
             curve and attack when it looks safe, but do not calculate exact \
             combat math, do not play around cards your opponent might be \
             holding, and do not build multi-turn plans."
        }
        AiDifficulty::Medium => {
            "You are a solid regular player. Develop your board on curve, make \
             favourable trades, count lethal damage, and hold up interaction \
             when it is cheap to do so. Play around the most obvious cards your \
             opponent could have, but do not agonise over unlikely lines."
        }
        AiDifficulty::Hard => {
            "You are a strong competitive player. Sequence your plays to \
             maximise mana efficiency, count exact combat and racing math, \
             track what your opponent has shown and what their open mana \
             represents, and pick the line that wins fastest while losing to \
             the fewest outs."
        }
        AiDifficulty::VeryHard => {
            "You are an expert tournament player. Evaluate every legal option \
             for its effect on the whole game, not just this turn. Count exact \
             damage, track every card revealed so far, infer your opponent's \
             hand from their mana and play pattern, and deliberately play \
             around the specific cards that beat you. Never make a play that is \
             merely 'fine' when a better one exists."
        }
        AiDifficulty::CEDH => {
            "You are a competitive Commander (cEDH) specialist. Assume every \
             opponent is playing a tuned, fast combo deck. Prioritise \
             assembling or protecting your own win condition, holding \
             interaction for opposing combo attempts, and denying the fastest \
             opponent. Value card selection, fast mana, and stack interaction \
             far above incremental board presence."
        }
    }
}

/// Whether this difficulty should be shown the full move history and the full
/// public board, or only the immediate position.
///
/// Returned as a line count rather than a bool so the caller has the actual
/// budget: the two lowest difficulties deliberately reason from a near-term
/// window, which is a large part of what makes them beatable.
pub fn history_window(difficulty: AiDifficulty) -> usize {
    match difficulty {
        AiDifficulty::VeryEasy => 0,
        AiDifficulty::Easy => 10,
        AiDifficulty::Medium => 30,
        AiDifficulty::Hard => 60,
        AiDifficulty::VeryHard | AiDifficulty::CEDH => 100,
    }
}

/// The reply contract, appended to every decision prompt. Kept in one constant
/// because [`decode_choice`] is written against exactly this shape.
pub const RESPONSE_CONTRACT: &str =
    "Reply with ONLY a JSON object and nothing else, in this form:\n\
     {\"choice\": <the number of the option you pick>, \"reason\": \"<one short sentence>\"}\n\
     Do not wrap it in markdown. Do not explain outside the JSON. The \"choice\" \
     value must be one of the option numbers listed above.";

/// The multi-pick variant of [`RESPONSE_CONTRACT`], for a draft step that takes
/// more than one card (CR 903.13b).
pub fn multi_response_contract(required: usize) -> String {
    format!(
        "Reply with ONLY a JSON object and nothing else, in this form:\n\
         {{\"choice\": [<{required} option numbers, best first>], \"reason\": \"<one short sentence>\"}}\n\
         Do not wrap it in markdown. Do not explain outside the JSON. Every value \
         must be one of the option numbers listed above, and they must be distinct."
    )
}

/// Decode a model's reply into option indices.
///
/// Deliberately forgiving in every way that cannot produce a wrong *legal*
/// answer: fenced JSON, prose around the JSON, a bare array, and a bare integer
/// all decode. What it will not do is guess — an index outside the offered
/// domain is an error, never a clamp, because clamping would silently convert
/// "the model misread the board" into "the engine took an action nobody chose".
pub fn decode_choice(text: &str, option_count: usize, wanted: usize) -> LlmResult<LlmChoice> {
    if option_count == 0 {
        return Err(LlmError::UndecodableChoice {
            detail: "no options were offered".to_string(),
        });
    }

    let stripped = strip_code_fences(text);
    let object = extract_json_object(stripped);

    let reasoning = object
        .as_ref()
        .and_then(reasoning_field)
        .map(str::to_string);

    let raw = object
        .as_ref()
        .and_then(choice_field)
        .map(collect_numbers)
        .filter(|numbers| !numbers.is_empty())
        .or_else(|| {
            // No usable JSON: fall back to the first integers in the reply. A
            // model that answers "I'll take option 3" is still unambiguous.
            let numbers = scan_integers(stripped);
            (!numbers.is_empty()).then_some(numbers)
        })
        .ok_or_else(|| LlmError::UndecodableChoice {
            detail: format!("no option number found in {:?}", truncate(text, 200)),
        })?;

    let mut indices: Vec<usize> = Vec::with_capacity(wanted.max(1));
    for number in raw {
        let index = usize::try_from(number).map_err(|_| LlmError::ChoiceOutOfRange {
            choice: number,
            option_count,
        })?;
        if index >= option_count {
            return Err(LlmError::ChoiceOutOfRange {
                choice: number,
                option_count,
            });
        }
        if !indices.contains(&index) {
            indices.push(index);
        }
        if indices.len() == wanted.max(1) {
            break;
        }
    }

    if indices.is_empty() {
        return Err(LlmError::UndecodableChoice {
            detail: "reply named no distinct in-range option".to_string(),
        });
    }

    Ok(LlmChoice { indices, reasoning })
}

/// Remove a surrounding ```/```json fence, which models add despite being asked
/// not to. Returns the original text when there is no fence to strip.
fn strip_code_fences(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // The opening fence may carry a language tag; the body starts at the newline.
    let body = after_open
        .split_once('\n')
        .map_or(after_open, |(_tag, rest)| rest);
    body.rsplit_once("```")
        .map_or(body, |(inner, _)| inner)
        .trim()
}

/// The first balanced `{...}` span that parses as JSON. Scans rather than
/// requiring the whole reply to be JSON so prose around the object is harmless.
fn extract_json_object(text: &str) -> Option<Value> {
    let bytes = text.as_bytes();
    for (start, _) in text.char_indices().filter(|(_, c)| *c == '{') {
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, byte) in bytes[start..].iter().enumerate() {
            if in_string {
                match byte {
                    _ if escaped => escaped = false,
                    b'\\' => escaped = true,
                    b'"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let end = start + offset + 1;
                        if let Ok(value) = serde_json::from_str::<Value>(&text[start..end]) {
                            return Some(value);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// The keys a model plausibly uses for its answer, in preference order.
fn choice_field(object: &Value) -> Option<&Value> {
    [
        "choice", "choices", "option", "options", "index", "pick", "picks",
    ]
    .iter()
    .find_map(|key| object.get(key))
}

fn reasoning_field(object: &Value) -> Option<&str> {
    ["reason", "reasoning", "why", "explanation"]
        .iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
}

/// Numbers carried by a `choice` field, whatever container it arrived in: a
/// number, a numeric string, or an array of either.
fn collect_numbers(value: &Value) -> Vec<i64> {
    match value {
        Value::Number(number) => number.as_i64().into_iter().collect(),
        Value::String(text) => scan_integers(text),
        Value::Array(items) => items.iter().flat_map(collect_numbers).collect(),
        _ => Vec::new(),
    }
}

/// Every standalone non-negative integer in `text`, in order.
///
/// A digit run is standalone when it is not part of a longer token. That rules
/// out card stats and model names that could otherwise be mistaken for an
/// answer (`2/2`, `gpt-4`, `2.5`), while still accepting a number that merely
/// ends a sentence (`I'll take option 2.`) — the separator characters only bind
/// when a digit sits on the other side of them.
fn scan_integers(text: &str) -> Vec<i64> {
    let chars: Vec<char> = text.chars().collect();
    let mut numbers = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
        let before_free = start == 0 || !binds_left(&chars, start);
        let after_free = index == chars.len() || !binds_right(&chars, index);
        if before_free && after_free {
            if let Ok(number) = chars[start..index]
                .iter()
                .collect::<String>()
                .parse::<i64>()
            {
                numbers.push(number);
            }
        }
    }
    numbers
}

/// Whether the character before a digit run joins it into a longer token.
fn binds_left(chars: &[char], start: usize) -> bool {
    let previous = chars[start - 1];
    if previous.is_alphanumeric() || previous == '_' {
        return true;
    }
    // `/`, `.` and `-` bind only with a digit on their far side: `2/2` and `2.5`
    // are one token, `option 2.` is not.
    matches!(previous, '/' | '.' | '-') && start >= 2 && chars[start - 2].is_ascii_digit()
}

/// Whether the character after a digit run joins it into a longer token.
fn binds_right(chars: &[char], end: usize) -> bool {
    let next = chars[end];
    if next.is_alphanumeric() || next == '_' {
        return true;
    }
    matches!(next, '/' | '.' | '-') && chars.get(end + 1).is_some_and(char::is_ascii_digit)
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_json_reply_decodes() {
        let choice = decode_choice(r#"{"choice": 2, "reason": "best blocker"}"#, 5, 1).unwrap();
        assert_eq!(choice.indices, vec![2]);
        assert_eq!(choice.reasoning.as_deref(), Some("best blocker"));
    }

    #[test]
    fn a_fenced_json_reply_decodes() {
        let choice = decode_choice("```json\n{\"choice\": 0}\n```", 3, 1).unwrap();
        assert_eq!(choice.indices, vec![0]);
    }

    #[test]
    fn prose_around_the_object_is_ignored() {
        let choice = decode_choice(
            "Thinking about it...\n{\"choice\": 1}\nHope that helps!",
            4,
            1,
        )
        .unwrap();
        assert_eq!(choice.indices, vec![1]);
    }

    #[test]
    fn a_bare_integer_reply_decodes() {
        assert_eq!(decode_choice("3", 5, 1).unwrap().indices, vec![3]);
        assert_eq!(
            decode_choice("I'll take option 2.", 5, 1).unwrap().indices,
            vec![2]
        );
    }

    #[test]
    fn a_multi_pick_reply_keeps_order_and_drops_repeats() {
        let choice = decode_choice(r#"{"choice": [4, 4, 1]}"#, 6, 2).unwrap();
        assert_eq!(choice.indices, vec![4, 1]);
    }

    #[test]
    fn an_out_of_range_choice_is_an_error_not_a_clamp() {
        assert_eq!(
            decode_choice(r#"{"choice": 9}"#, 3, 1),
            Err(LlmError::ChoiceOutOfRange {
                choice: 9,
                option_count: 3
            })
        );
    }

    #[test]
    fn a_reply_with_no_number_is_undecodable() {
        assert!(matches!(
            decode_choice("I am not sure what to do here.", 3, 1),
            Err(LlmError::UndecodableChoice { .. })
        ));
    }

    #[test]
    fn power_toughness_in_prose_is_not_mistaken_for_an_answer() {
        let choice = decode_choice("The 2/2 trades with their 3/3. {\"choice\": 1}", 4, 1).unwrap();
        assert_eq!(choice.indices, vec![1]);
        // With no JSON at all the same guard still holds: the stats bind into
        // longer tokens, so only the standalone number is an answer.
        assert_eq!(
            decode_choice("My 2/2 blocks; I choose 3", 5, 1)
                .unwrap()
                .indices,
            vec![3]
        );
    }

    #[test]
    fn a_decimal_is_never_read_as_two_answers() {
        assert_eq!(
            decode_choice("confidence 0.85 — I pick 1", 4, 1)
                .unwrap()
                .indices,
            vec![1]
        );
    }

    #[test]
    fn a_numeric_string_choice_decodes() {
        assert_eq!(
            decode_choice(r#"{"choice": "2"}"#, 4, 1).unwrap().indices,
            vec![2]
        );
    }

    #[test]
    fn an_empty_option_domain_never_decodes() {
        assert!(matches!(
            decode_choice(r#"{"choice": 0}"#, 0, 1),
            Err(LlmError::UndecodableChoice { .. })
        ));
    }

    #[test]
    fn every_difficulty_has_a_distinct_brief_and_a_history_window() {
        let difficulties = [
            AiDifficulty::VeryEasy,
            AiDifficulty::Easy,
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
            AiDifficulty::CEDH,
        ];
        let mut briefs: Vec<&str> = difficulties.iter().copied().map(difficulty_brief).collect();
        briefs.sort_unstable();
        briefs.dedup();
        assert_eq!(briefs.len(), difficulties.len());
        // Monotonic: a harder seat never sees less history than an easier one.
        for pair in difficulties.windows(2) {
            assert!(history_window(pair[0]) <= history_window(pair[1]));
        }
    }
}
