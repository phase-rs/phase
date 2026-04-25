//! CR 701.38 + CR 207.2c: Council's-dilemma voting parser.
//!
//! # Shapes currently handled
//!
//! ```text
//! [<ability-word> — ] [Starting with you, ] each player [may] votes for <X> or <Y>[, or <Z>]+.
//! For each <X> vote, <effect-X>.
//! For each <Y> vote, <effect-Y>.
//! ```
//!
//! Output: a synthesized `Effect::Vote` whose `per_choice_effect` slots carry
//! the parsed sub-effects in `choices` declaration order.
//!
//! # Shapes not yet supported (deferred)
//!
//! 1. **Will-of-the-Council plurality** ("If X gets more votes, A. If Y gets
//!    more votes or the vote is tied, B."). Cards: Council's Judgment, Plea
//!    for Power, Coercive Portal, Magister of Worth, Selvala's Stampede.
//!    Requires a separate `Effect::VoteByPlurality` variant — the current
//!    `Effect::Vote` only models per-vote tally fan-out, not majority-wins
//!    resolution.
//! 2. **Compound single-sentence tally** ("Each opponent sacrifices a creature
//!    for each death vote and discards a card for each taxes vote."). Cards:
//!    Capital Punishment. Requires a `QuantityRef::VoteCount(choice)`
//!    variant so a single non-Vote effect can be quantified by a tally.
//! 3. **Vote-for-an-object** ("each player votes for a nonland permanent
//!    you don't control"). Requires a `VoteOption::TargetFilter` axis
//!    instead of fixed string options.
//!
//! # Architectural rules
//! * Nom combinators for ALL dispatch — `tag()`, `alt()`, `take_while1`,
//!   `take_until` (no `find`/`contains`/`split_once` for parsing decisions).
//! * Mixed-case handling via `nom_on_lower`: parsers operate on the
//!   lowercased text; the bridge maps remainders to the original-case slice
//!   so per-choice effect text is parsed in its source casing.
//! * Builds for the *class* of cards within the supported shape, not just
//!   one named card.
//! * The detector is pure: given vote text, returns the synthesized
//!   `AbilityDefinition`. Failure to match returns `None`, leaving the caller
//!   free to fall back to the standard chain parser.

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until, take_while1};
use nom::combinator::{opt, peek, value};
use nom::sequence::terminated;
use nom::Parser;
use nom_language::error::VerboseError;

use crate::types::ability::{AbilityDefinition, AbilityKind, ControllerRef, Effect};

use super::oracle_effect::{parse_effect_chain_with_context, ParseContext};
use super::oracle_nom::bridge::nom_on_lower;

type NomResult<'a, T> = nom::IResult<&'a str, T, VerboseError<&'a str>>;

/// Detect and parse a Council's-dilemma vote block. Returns a single
/// `AbilityDefinition` whose `effect` is `Effect::Vote` populated with the
/// per-choice sub-effects, or `None` if the input doesn't match the supported
/// shape (see module docs).
///
/// Accepts text with or without the leading "<ability-word> — " prefix —
/// callers don't need to strip "Council's dilemma — " first.
pub(crate) fn parse_vote_block(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    let lower = text.to_lowercase();

    // Phase 1: consume the entire intro sentence and produce the ordered
    // choice list. nom_on_lower hands back the original-case remainder so
    // per-choice effect text parses in its source casing.
    let ((starting_with, choices), rest_after_intro) = nom_on_lower(text, &lower, parse_intro)?;
    if choices.len() < 2 {
        return None;
    }

    // Phase 2: walk the per-choice sentences. Each iteration consumes one
    // "For each <choice> vote[,]? <effect>." clause and stores the parsed
    // sub-effect in `slots[choices.position(choice)]`. Order of textual
    // appearance is allowed to differ from the choice declaration order;
    // duplicate references are rejected (shape we don't yet model).
    let mut slots: Vec<Option<Box<AbilityDefinition>>> = (0..choices.len()).map(|_| None).collect();
    let mut walk_text = rest_after_intro;
    let mut walk_lower = lower_tail(&lower, walk_text);
    while !walk_text.trim().is_empty() {
        let (choice, effect_text, next_text) =
            parse_one_for_each_clause(walk_text, walk_lower, &choices)?;
        let idx = choices.iter().position(|c| c == &choice)?;
        if slots[idx].is_some() {
            return None;
        }
        let parsed = parse_effect_chain_with_context(effect_text, kind, &ParseContext::default());
        slots[idx] = Some(Box::new(parsed));
        walk_text = next_text;
        walk_lower = lower_tail(&lower, walk_text);
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

/// Slice of `lower` that aligns positionally with `tail` in the original
/// text. Both strings have identical byte lengths and a 1:1 char mapping
/// (lowercasing of ASCII Oracle text is byte-preserving), so `tail`'s suffix
/// position into `text` maps directly into `lower`.
fn lower_tail<'a>(lower: &'a str, tail: &str) -> &'a str {
    // `tail` was produced from a slice of the *original* text via
    // nom_on_lower's offset bridging, so its length in bytes is the bytes
    // remaining at the end of `lower` too.
    &lower[lower.len() - tail.len()..]
}

/// Nom: optional ability word, optional "starting with you", then the
/// "each player votes for <list>." prompt. Returns `(starting_with, choices)`.
fn parse_intro(i: &str) -> NomResult<'_, (ControllerRef, Vec<String>)> {
    let (i, _) = opt_ability_word(i)?;
    let (i, _) = opt_starting_with_you(i)?;
    let (i, _) = alt((
        tag("each player votes for "),
        tag("each player may vote for "),
    ))
    .parse(i)?;
    // Capture the choice list up to the closing period. `take_until(".")`
    // is a structural sentence-boundary scan, not parsing dispatch — the
    // dispatch happened above when we recognised the prompt prefix.
    let (i, list) = terminated(take_until("."), tag(". ")).parse(i)?;
    let choices = match split_choices(list) {
        Some(c) => c,
        None => {
            return Err(nom::Err::Error(VerboseError {
                errors: vec![(
                    i,
                    nom_language::error::VerboseErrorKind::Context("split_choices"),
                )],
            }))
        }
    };
    Ok((i, (ControllerRef::You, choices)))
}

/// Optional "<ability-word> — " prefix. Consumes both the word and the
/// em-dash (or hyphen) separator; no-op when absent.
fn opt_ability_word(i: &str) -> NomResult<'_, ()> {
    value(
        (),
        opt((
            alt((tag("council's dilemma"), tag("will of the council"))),
            opt(tag(" ")),
            alt((tag("— "), tag("- "))),
        )),
    )
    .parse(i)
}

/// Optional "starting with you[,]? " prefix. Consumed silently — currently
/// the only supported `ControllerRef` for the starting voter is
/// `ControllerRef::You`, so non-`you` phrasings ("starting with the player
/// to your left") are not detected here.
fn opt_starting_with_you(i: &str) -> NomResult<'_, ()> {
    value(
        (),
        opt(alt((tag("starting with you, "), tag("starting with you ")))),
    )
    .parse(i)
}

/// Parse one "For each <choice> vote[,]? <effect>." clause from `walk_text`.
/// Returns `(choice_lowercase, effect_text_in_original_case, next_text_in_original_case)`.
///
/// The effect text terminator is the next "for each <known-choice> vote"
/// boundary — implemented by composing `take_until(".")` against a
/// peek-checked sentence boundary that recognises the next clause.
fn parse_one_for_each_clause<'a>(
    walk_text: &'a str,
    walk_lower: &str,
    choices: &[String],
) -> Option<(String, &'a str, &'a str)> {
    // Step 1: consume "for each <choice> vote[,]? " on the lowercase view to
    // identify the choice and remember how many bytes were consumed.
    let (after_intro_lower, choice_lower) = parse_for_each_intro(walk_lower, choices).ok()?;
    let intro_consumed = walk_lower.len() - after_intro_lower.len();
    let after_intro_text = &walk_text[intro_consumed..];

    // Step 2: take the effect text up to the next "for each <known-choice>
    // vote" boundary or end of input. Period before the boundary is stripped
    // when present.
    let (effect_text, next_text) =
        take_effect_until_next_clause(after_intro_text, after_intro_lower, choices);

    Some((choice_lower, effect_text.trim(), next_text))
}

/// Lowercase intro consumer: "for each <choice> vote[,]? ". Returns the
/// remainder and the lowercase choice string.
fn parse_for_each_intro<'a>(
    lower: &'a str,
    choices: &[String],
) -> Result<(&'a str, String), nom::Err<VerboseError<&'a str>>> {
    let (i, _) = tag::<_, _, VerboseError<&str>>("for each ").parse(lower)?;
    let (i, word) = take_while1::<_, &str, VerboseError<&str>>(|c: char| {
        c.is_alphanumeric() || c == '\'' || c == '-'
    })
    .parse(i)?;
    if !choices.iter().any(|c| c == word) {
        return Err(nom::Err::Error(VerboseError {
            errors: vec![(
                i,
                nom_language::error::VerboseErrorKind::Context("unknown choice"),
            )],
        }));
    }
    let (i, _) = alt((tag(" vote, "), tag(" vote "))).parse(i)?;
    Ok((i, word.to_string()))
}

/// Walk forward from `text_in` (original case, paired with `lower_in`) until
/// either: end of input, OR the start of the next "for each <known-choice>
/// vote" sentence. Returns `(effect_text_original_case, remainder_text_original_case)`.
///
/// The boundary preamble (". " or " ") is consumed so the remainder begins
/// at "for each", letting the caller's outer loop succeed on its next intro
/// match without further fixups.
fn take_effect_until_next_clause<'a>(
    text_in: &'a str,
    lower_in: &str,
    choices: &[String],
) -> (&'a str, &'a str) {
    // Search nom-side for the next boundary; falls through to end-of-input
    // when no further clause is present.
    let mut probe_offset: usize = 0;
    let mut preamble_skip: usize = 0;
    while probe_offset < lower_in.len() {
        let probe = &lower_in[probe_offset..];
        if let Some(skip) = boundary_preamble_len(probe, choices) {
            preamble_skip = skip;
            break;
        }
        let step = probe.chars().next().map(char::len_utf8).unwrap_or(1);
        probe_offset += step;
    }
    let head_text = &text_in[..probe_offset];
    let tail_start = probe_offset + preamble_skip;
    let tail_text = &text_in[tail_start..];

    // Trim trailing period off the head — it's the sentence terminator,
    // not part of the effect text.
    let head_no_period = strip_trailing_period(head_text);
    (head_no_period, tail_text.trim_start())
}

/// Strip a trailing "." from a sentence-extracted slice. Structural
/// punctuation cleanup on already-tokenized text — not a parsing decision.
fn strip_trailing_period(s: &str) -> &str {
    let trimmed = s.trim_end();
    // allow-noncombinator: structural period strip on pre-extracted sentence text (PATTERNS.md §9)
    trimmed.strip_suffix('.').unwrap_or(trimmed).trim_end()
}

/// If `lower` starts with a "for each <known-choice> vote" boundary —
/// optionally preceded by ". " or a single space — return the byte length
/// of the preamble that must be consumed so the tail begins at "for each".
fn boundary_preamble_len(lower: &str, choices: &[String]) -> Option<usize> {
    // Try each possible preamble. Composed via nom alt() so the dispatch
    // stays inside the combinator framework.
    for &(plen, preamble) in &[(2usize, ". "), (1, " "), (0, "")] {
        let res: NomResult<()> = value(
            (),
            (tag(preamble), peek(parse_for_each_intro_check(choices))),
        )
        .parse(lower);
        if res.is_ok() {
            return Some(plen);
        }
    }
    None
}

/// Build a parser that succeeds only when the input begins with
/// "for each <known-choice> vote[,]? " — used as a `peek()` predicate so
/// the boundary scanner can detect the next clause without consuming it.
fn parse_for_each_intro_check<'a>(
    choices: &'a [String],
) -> impl FnMut(&'a str) -> NomResult<'a, ()> + 'a {
    move |i: &'a str| {
        let (i, _) = tag::<_, _, VerboseError<&str>>("for each ").parse(i)?;
        let (i, word) = take_while1::<_, &str, VerboseError<&str>>(|c: char| {
            c.is_alphanumeric() || c == '\'' || c == '-'
        })
        .parse(i)?;
        if !choices.iter().any(|c| c == word) {
            return Err(nom::Err::Error(VerboseError {
                errors: vec![(
                    i,
                    nom_language::error::VerboseErrorKind::Context("unknown choice"),
                )],
            }));
        }
        let (i, _) = alt((tag(" vote, "), tag(" vote "))).parse(i)?;
        Ok((i, ()))
    }
}

/// Split a list like "evidence or bribery" or "guards, hounds, or dragons"
/// into individual lowercase choices. Returns `None` if fewer than two
/// choices were found. Fully nom-driven: `take_while1` for word tokens,
/// `alt()` for separators, `many1`-style loop until input exhausts.
fn split_choices(input: &str) -> Option<Vec<String>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    let mut choices: Vec<String> = Vec::new();
    let mut rest = lower.as_str();
    loop {
        let word_res: NomResult<&str> =
            take_while1(|c: char| c.is_alphanumeric() || c == '\'' || c == '-').parse(rest);
        let (after_word, word) = word_res.ok()?;
        choices.push(word.to_string());
        rest = after_word;
        if rest.is_empty() {
            break;
        }
        let sep_res: NomResult<()> = value(
            (),
            // Longest separator first so ", or " wins over " or " / ", "
            // when both could match.
            alt((tag(", or "), tag(" or "), tag(", "))),
        )
        .parse(rest);
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
                assert!(matches!(*per_choice_effect[0].effect, Effect::Investigate));
                assert!(matches!(*per_choice_effect[1].effect, Effect::Token { .. }));
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// Council's-dilemma SORCERIES start with the ability word "Council's
    /// dilemma — " before the vote prompt. The parser must strip it so the
    /// dispatch path that hits `parse_effect_chain_impl` (instead of the
    /// trigger pre-pass) still recognizes the block.
    #[test]
    fn parses_with_ability_word_prefix() {
        let text = "Council's dilemma — Starting with you, each player votes for left or right. For each left vote, investigate. For each right vote, scry 1.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("ability-word block parses");
        match *def.effect {
            Effect::Vote { ref choices, .. } => {
                assert_eq!(choices, &vec!["left".to_string(), "right".to_string()]);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// Three-or-more-choice ballots ("vote for X, Y, or Z") must produce all
    /// declared options so the resolver can build the right tally array.
    #[test]
    fn parses_three_choice_vote() {
        let text = "starting with you, each player votes for guards, hounds, or dragons. For each guards vote, investigate. For each hounds vote, scry 1. For each dragons vote, draw a card.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("three-choice parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                ref per_choice_effect,
                ..
            } => {
                assert_eq!(choices.len(), 3);
                assert_eq!(per_choice_effect.len(), 3);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// CR 207.2c: The "Council's dilemma — " prefix is an ability word with
    /// no rules meaning. Parser must accept its absence (some printings drop
    /// the prefix when text is reproduced in compact contexts).
    #[test]
    fn parses_without_starting_with_prefix() {
        let text = "each player votes for evidence or bribery. For each evidence vote, investigate. For each bribery vote, scry 1.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("no-starting-with form parses");
        match *def.effect {
            Effect::Vote { starting_with, .. } => {
                assert_eq!(starting_with, ControllerRef::You);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    #[test]
    fn rejects_non_vote_text() {
        assert!(parse_vote_block("Draw a card.", AbilityKind::Spell).is_none());
    }

    /// Will-of-the-Council plurality cards are explicitly out of scope (see
    /// module docs). Parser must REJECT them so the chain parser can produce
    /// a clean Unimplemented diagnostic instead of a wrong-shape Vote.
    #[test]
    fn rejects_will_of_the_council_plurality() {
        // Plea for Power's Oracle text shape.
        let text = "starting with you, each player votes for time or knowledge. If time gets more votes, take an extra turn after this one.";
        assert!(parse_vote_block(text, AbilityKind::Spell).is_none());
    }
}
