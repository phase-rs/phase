//! CR 701.38 + CR 207.2c: Council's-dilemma / Will-of-the-Council voting parser.
//!
//! This module owns recognition of the full vote effect block:
//!
//! ```text
//! starting with you, each player votes for <choice-a> or <choice-b>.
//! For each <choice-a> vote, <effect-a>.
//! For each <choice-b> vote, <effect-b>.
//! ```
//!
//! Output: a synthesized `Effect::Vote` whose `per_choice_effect` slots carry
//! the parsed sub-effects in `choices` declaration order.
//!
//! Architectural rules:
//! * Nom combinators for ALL dispatch — never `find` / `contains` / `split_once`.
//! * Builds for the *class* of cards (every Will-of-the-Council / Council's-
//!   dilemma vote with two-or-more named choices), not just Tivit.
//! * The detector is pure: given vote text, it returns the synthesized
//!   `AbilityDefinition`. Failure to match returns `None`, leaving the caller
//!   free to fall back to the standard chain parser.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use crate::types::ability::{AbilityDefinition, AbilityKind, ControllerRef, Effect};

use super::oracle_effect::{parse_effect_chain_with_context, ParseContext};

/// Detect and parse the entire Council's-dilemma vote block. Returns a single
/// `AbilityDefinition` whose `effect` is `Effect::Vote` populated with the
/// per-choice sub-effects, or `None` if the input doesn't match the pattern.
///
/// The input is the trigger/effect *body* text — i.e., what comes after
/// "Whenever ~ enters or deals combat damage to a player, ". The "starting
/// with you, " prefix is consumed here (kept inside this module so chain-level
/// stripping in `parse_effect_chain_impl` doesn't interfere).
pub(crate) fn parse_vote_block(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    let lower = text.to_lowercase();
    // Phase 1: optional "starting with you," prefix.
    let (i, starting_with) =
        parse_starting_with(&lower).unwrap_or((lower.as_str(), ControllerRef::You));
    // Phase 2: "each player votes for <a> or <b>." (allowing "each player may vote").
    let (i, choices) = parse_each_player_votes_clause(i)?;
    if choices.len() < 2 {
        return None;
    }
    // Phase 3: "For each <choice> vote, <effect>." Once per choice. Walk the
    // text exactly once and key the parsed sub-effects by their canonical
    // `choices` index so the output array always matches declaration order.
    let mut slots: Vec<Option<Box<AbilityDefinition>>> = (0..choices.len()).map(|_| None).collect();
    let mut walk = i.trim_start();
    while !walk.is_empty() {
        let (rest, (choice, effect_text)) = parse_for_each_vote_clause(walk, &choices)?;
        let idx = choices.iter().position(|c| c == &choice)?;
        if slots[idx].is_some() {
            // Same choice referenced twice — shape we don't yet model.
            return None;
        }
        let parsed = parse_effect_chain_with_context(effect_text, kind, &ParseContext::default());
        slots[idx] = Some(Box::new(parsed));
        walk = rest.trim_start();
    }
    let per_choice_effect: Vec<Box<AbilityDefinition>> =
        slots.into_iter().collect::<Option<Vec<_>>>()?;

    Some(AbilityDefinition::new(
        kind,
        Effect::Vote {
            choices,
            per_choice_effect,
            starting_with,
        },
    ))
}

/// Parse the optional "starting with you, " prefix. Returns the unconsumed
/// remainder plus the resolved `ControllerRef`. Other phrasings ("starting
/// with the player to your left") map to `ControllerRef::You` until we model
/// player-position refs.
fn parse_starting_with(input: &str) -> Option<(&str, ControllerRef)> {
    let res: nom::IResult<&str, (), VerboseError<&str>> = value(
        (),
        alt((tag("starting with you, "), tag("starting with you "))),
    )
    .parse(input);
    match res {
        Ok((rest, ())) => Some((rest, ControllerRef::You)),
        Err(_) => None,
    }
}

/// Parse "each player votes for <a> or <b>." or its "may vote" cousin.
/// Returns the unconsumed remainder plus the lowercase choice list.
///
/// Generalized to N>=2 choices via repeated " or " / ", " separators —
/// covers cards like Capital Punishment that vote on three options.
fn parse_each_player_votes_clause(input: &str) -> Option<(&str, Vec<String>)> {
    let res: nom::IResult<&str, (), VerboseError<&str>> = value(
        (),
        alt((
            tag("each player votes for "),
            tag("each player may vote for "),
        )),
    )
    .parse(input);
    let (rest, ()) = res.ok()?;

    // Read the choice list: "<a>[, <b>][, <c>] or <last>." — allow "or"
    // separator for the last item, comma between earlier items.
    let (after, choice_list_text) = read_until_period(rest)?;
    let choices = split_choices(choice_list_text)?;
    Some((after, choices))
}

/// Parse "For each <choice> vote, <effect>." Returns the choice (lowercase)
/// and the inner effect text. Whitespace handling:
/// * Accepts both upper- and lowercase "For"/"for".
/// * Consumes a trailing period if present.
fn parse_for_each_vote_clause<'a>(
    input: &'a str,
    choices: &[String],
) -> Option<(&'a str, (String, &'a str))> {
    let lower = input.to_lowercase();
    let res: nom::IResult<&str, (), VerboseError<&str>> =
        value((), alt((tag("for each "),))).parse(lower.as_str());
    let (lower_rest, ()) = res.ok()?;
    // Slice the original input at the same offset.
    let consumed = input.len() - lower_rest.len();
    let original_rest = &input[consumed..];

    // Read the choice token (case-insensitive); choices are whitespace-free
    // single words in canonical Council's-dilemma cards.
    let (choice, after_choice) = read_word(original_rest)?;
    let choice_lower = choice.to_lowercase();
    if !choices.iter().any(|c| c == &choice_lower) {
        return None;
    }
    // Consume " vote, " (singular) — plural "votes" would imply the resolver
    // re-tally pattern that Council's dilemma never uses; reject to keep the
    // detector tight.
    let (after_vote, _): (&str, &str) = tag::<_, _, VerboseError<&str>>(" vote, ")
        .parse(after_choice)
        .ok()?;
    // Read up to terminator: either next "For each " OR end-of-string,
    // stripping trailing period.
    let (effect_text, rest) = read_effect_until_next_clause(after_vote);
    Some((rest, (choice_lower, effect_text)))
}

/// Read a maximal prefix up to (but not including) the next "For each "
/// clause or end of input. Strips a trailing period from the consumed slice.
fn read_effect_until_next_clause(input: &str) -> (&str, &str) {
    let lower = input.to_lowercase();
    // Find the next "for each " case-insensitively. structural: not dispatch
    // — this is a local sentence-boundary scanner, not a parser dispatch
    // decision. We use lowercase for the search but slice the original input
    // so casing is preserved in the returned effect text.
    let cut = lower
        .match_indices("for each ")
        .find(|(idx, _)| {
            *idx == 0
                || matches!(
                    lower.as_bytes().get(*idx - 1),
                    Some(b' ') | Some(b'.') | Some(b',')
                )
        })
        .map(|(idx, _)| idx)
        .unwrap_or(input.len());
    let head = &input[..cut];
    let tail = &input[cut..];
    let head_trimmed = head.trim_end();
    // allow-noncombinator: structural period strip on pre-extracted sentence clause
    let head_no_period = head_trimmed.strip_suffix('.').unwrap_or(head_trimmed);
    (head_no_period.trim(), tail.trim_start())
}

/// Read a word (alphanumeric + apostrophes). Returns (word, remainder).
fn read_word(input: &str) -> Option<(&str, &str)> {
    let end = input
        .char_indices()
        .find(|(_, c)| !c.is_alphanumeric() && *c != '\'' && *c != '-')
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    if end == 0 {
        return None;
    }
    Some((&input[..end], &input[end..]))
}

/// Read characters up to and including a period; return the substring before
/// the period and the remainder after it.
fn read_until_period(input: &str) -> Option<(&str, &str)> {
    let idx = input.find('.')?;
    Some((&input[idx + 1..], &input[..idx]))
}

/// Split a list like "evidence or bribery" or "guards, hounds, or dragons"
/// into individual lowercase choices. Returns `None` if fewer than two
/// choices were found.
///
/// Uses nom to consume word tokens separated by `", or "`, `" or "`, or `", "` —
/// handling the standard MTG list formats without string-splitting on raw bytes.
fn split_choices(input: &str) -> Option<Vec<String>> {
    let lower = input.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let word_chars = |c: char| c.is_alphanumeric() || c == '\'' || c == '-';
    let mut choices: Vec<String> = Vec::new();
    let mut rest: &str = lower.as_str();
    loop {
        let (after_word, word) =
            nom::bytes::complete::take_while1::<_, &str, VerboseError<&str>>(word_chars)
                .parse(rest)
                .ok()?;
        choices.push(word.to_string());
        rest = after_word;
        if rest.is_empty() {
            break;
        }
        // Consume separator; try longest match first to avoid partial matches.
        let sep_res: nom::IResult<&str, (), VerboseError<&str>> =
            value((), alt((tag(", or "), tag(" or "), tag(", ")))).parse(rest);
        let (after_sep, ()) = sep_res.ok()?;
        rest = after_sep;
    }
    if choices.len() < 2 {
        return None;
    }
    Some(choices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tivit_vote_block() {
        let text = "starting with you, each player votes for evidence or bribery. For each evidence vote, investigate. For each bribery vote, create a Treasure token.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                ref per_choice_effect,
                starting_with,
            } => {
                assert_eq!(
                    choices,
                    &vec!["evidence".to_string(), "bribery".to_string()]
                );
                assert_eq!(per_choice_effect.len(), 2);
                assert_eq!(starting_with, ControllerRef::You);
                // First per-choice = Investigate
                assert!(matches!(*per_choice_effect[0].effect, Effect::Investigate));
                // Second per-choice = Token (Treasure)
                assert!(matches!(*per_choice_effect[1].effect, Effect::Token { .. }));
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    #[test]
    fn rejects_non_vote_text() {
        assert!(parse_vote_block("Draw a card.", AbilityKind::Spell).is_none());
    }
}
