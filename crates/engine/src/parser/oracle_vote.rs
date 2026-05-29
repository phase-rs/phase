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

use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use crate::parser::oracle_nom::primitives::{parse_number, scan_preceded, scan_split_at_phrase};
use nom::branch::alt;
use nom::bytes::complete::{tag, tag_no_case, take_while1};
use nom::combinator::{map, success, value};
use nom::Parser;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, PlayerFilter, VoterScope,
};

use super::oracle_effect::parse_effect_chain_with_context;
use super::oracle_ir::context::ParseContext;

/// Detect and parse the entire Council's-dilemma vote block. Returns a single
/// `AbilityDefinition` whose `effect` is `Effect::Vote` populated with the
/// per-choice sub-effects, or `None` if the input doesn't match the pattern.
///
/// The input is the trigger/effect *body* text — i.e., what comes after
/// "Whenever ~ enters or deals combat damage to a player, ". The "starting
/// with you, " prefix is consumed here (kept inside this module so chain-level
/// stripping in `parse_effect_chain_ir` doesn't interfere).
pub(crate) fn parse_vote_block(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    // Case-insensitive nom tags (`tag_no_case`) match directly against the
    // original-case input, so the entire vote-detection pipeline operates on
    // `text` without an upfront `to_lowercase()` allocation. On the failure
    // path (every non-vote spell line that reaches this probe) the first
    // `tag_no_case` in `parse_each_player_votes_clause` short-circuits on the
    // first byte mismatch and no allocation is ever performed.
    let (i, starting_with) = parse_starting_with(text).unwrap_or((text, ControllerRef::You));
    // Phase 2: opener clause. Two shapes covered:
    //   * "each player votes for <a> or <b>."         → VoterScope::AllPlayers
    //   * "each player may vote for <a> or <b>."      → VoterScope::AllPlayers
    //   * "each player chooses <a> or <b>."           → VoterScope::AllPlayers
    //   * "each opponent chooses <a> or <b>."         → VoterScope::EachOpponent
    //   * "each opponent may choose <a> or <b>."      → VoterScope::EachOpponent
    // CR 701.38c: "chooses" patterns aren't strict votes per the rules but
    // are mechanically identical for the engine's purposes — the resolver
    // tallies and fans out per-choice effects the same way.
    let (i, choices, voter_scope) = parse_each_player_votes_clause(i)?;
    if choices.len() < 2 {
        return None;
    }
    // Phase 3: per-choice clauses. Three shapes covered, dispatched by scope:
    //   * "For each <choice> vote, <effect>."                     (Tivit / classic)
    //   * "For each player who chose <choice>, <effect>."          (Master of Ceremonies)
    //   * "Each <choice> <effect>."                                (Battlebond friend-or-foe)
    // For `ControllerLabels`, every per-class effect implicitly distributes
    // across labeled players and the body refers to "they" / "their" — these
    // are the labeled players, not the spell controller. Wire each parsed
    // sub-effect with `PlayerFilter::VotedFor { choice_index }` so the runtime
    // re-binds the sub-effect controller to each labeled player.
    let is_controller_labels = matches!(voter_scope, VoterScope::ControllerLabels);
    let mut slots: Vec<Option<Box<AbilityDefinition>>> = (0..choices.len()).map(|_| None).collect();
    let mut walk = i.trim_start();
    while !walk.is_empty() {
        // Each iteration consumes exactly one per-choice clause. Shapes are
        // tried in priority order (ControllerLabels is mutually exclusive with
        // the other two):
        //   1. ControllerLabels  → "Each <choice> <effect>."         (Battlebond)
        //   2. "For each <choice> vote, <effect>." / "...who chose..." (classic)
        //   3. Aggregate tally   → "<effect ...a number of...> equal to
        //      [<multiplier>] the number of <choice> votes."          (Emissary Green)
        // `voted_for` records whether the parsed sub-effect must fan out across
        // the players who chose this option (CR 701.38 + CR 101.4); the
        // aggregate-tally shape is controller-performed, so it stays `false`.
        let (rest, idx, mut parsed, voted_for) = if is_controller_labels {
            let (rest, (choice, effect_text, who_chose)) = parse_each_class_clause(walk, &choices)?;
            let idx = choices.iter().position(|c| c == &choice)?;
            let parsed =
                parse_effect_chain_with_context(effect_text, kind, &mut ParseContext::default());
            (rest, idx, parsed, who_chose)
        } else if let Some((rest, (choice, effect_text, who_chose))) =
            parse_for_each_vote_clause(walk, &choices)
        {
            let idx = choices.iter().position(|c| c == &choice)?;
            let parsed =
                parse_effect_chain_with_context(effect_text, kind, &mut ParseContext::default());
            (rest, idx, parsed, who_chose)
        } else {
            // CR 701.38 + CR 122.1: aggregate-tally shape (Emissary Green). The
            // per-vote multiplier folds into the sub-effect's fixed count, and
            // the Vote resolver runs each per-choice sub-effect once per tallied
            // vote, so `count = multiplier` yields `multiplier × votes` total.
            let (rest, choice, rewritten) = parse_aggregate_tally_clause(walk, &choices)?;
            let idx = choices.iter().position(|c| c == &choice)?;
            let parsed =
                parse_effect_chain_with_context(&rewritten, kind, &mut ParseContext::default());
            (rest, idx, parsed, false)
        };
        if slots[idx].is_some() {
            // Same choice referenced twice — shape we don't yet model.
            return None;
        }
        if voted_for {
            // CR 701.38 + CR 101.4: Wire the per-vote sub-effect to fan out
            // across the players who received this choice index.
            // - "for each player who chose <choice>, <effect>" (Master of
            //   Ceremonies-style) routes to controller + voters who picked
            //   the option.
            // - "Each <choice> <effect>" under ControllerLabels (Battlebond
            //   friend-or-foe; no explicit CR section) routes to every
            //   labeled player, re-binding the sub-effect controller to
            //   each labeled player so "they" / "their" refers correctly.
            //
            // u8 fits trivially: vote-choice cardinality is bounded by Magic
            // card design (no card has ever exceeded ~5 choices).
            parsed.player_scope = Some(PlayerFilter::VotedFor {
                choice_index: idx as u8,
            });
        }
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
            voter_scope,
        },
    ))
}

/// Parse the optional "starting with you, " prefix. Returns the unconsumed
/// remainder plus the resolved `ControllerRef`. Other phrasings ("starting
/// with the player to your left") map to `ControllerRef::You` until we model
/// player-position refs.
fn parse_starting_with(input: &str) -> Option<(&str, ControllerRef)> {
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((
            tag_no_case("starting with you, "),
            tag_no_case("starting with you "),
        )),
    )
    .parse(input);
    match res {
        Ok((rest, ())) => Some((rest, ControllerRef::You)),
        Err(_) => None,
    }
}

/// Parse the opener that precedes the vote choice list. Six shapes:
///
/// | Pattern                                      | `VoterScope`            |
/// |----------------------------------------------|-------------------------|
/// | `"each player votes for "`                   | `AllPlayers`            |
/// | `"each player may vote for "`                | `AllPlayers`            |
/// | `"each player chooses "`                     | `AllPlayers`            |
/// | `"each opponent chooses "`                   | `EachOpponent`          |
/// | `"each opponent may choose "`                | `EachOpponent`          |
/// | `"for each player, choose "`                 | `ControllerLabels`      |
///
/// Returns the unconsumed remainder, the lowercase choice list, and the
/// resolved voter scope.
///
/// Generalized to N>=2 choices via repeated " or " / ", " separators —
/// covers cards like Capital Punishment that vote on three options.
///
/// The `ControllerLabels` opener is Battlebond's friend-or-foe pattern
/// (no explicit CR section; resolution follows CR 101.4 APNAP + CR 608.2
/// general spell resolution). The leading `"for each player, "` is
/// consumed here (mirroring the `"starting with you, "` handling) so the
/// chain splitter does not bisect the opener.
fn parse_each_player_votes_clause(input: &str) -> Option<(&str, Vec<String>, VoterScope)> {
    let res: nom::IResult<&str, VoterScope, OracleError<'_>> = alt((
        value(
            VoterScope::AllPlayers,
            tag_no_case("each player votes for "),
        ),
        value(
            VoterScope::AllPlayers,
            tag_no_case("each player may vote for "),
        ),
        value(
            VoterScope::EachOpponent,
            tag_no_case("each opponent chooses "),
        ),
        value(
            VoterScope::EachOpponent,
            tag_no_case("each opponent may choose "),
        ),
        value(VoterScope::AllPlayers, tag_no_case("each player chooses ")),
        value(
            VoterScope::ControllerLabels,
            tag_no_case("for each player, choose "),
        ),
    ))
    .parse(input);
    let (rest, voter_scope) = res.ok()?;

    // Read the choice list: "<a>[, <b>][, <c>] or <last>." — allow "or"
    // separator for the last item, comma between earlier items.
    let (after, choice_list_text) = read_until_period(rest)?;
    let choices = split_choices(choice_list_text)?;
    Some((after, choices, voter_scope))
}

/// Parse a single "For each ..." clause. Two shapes are accepted:
///
/// 1. `"for each <choice> vote, <effect>."`            (Tivit / classic council's-dilemma)
/// 2. `"for each <player-noun> who chose <choice>, <effect>."` (Master of Ceremonies)
///
/// Returns the unconsumed remainder, the matched choice (lowercase), the
/// inner effect text, and a flag indicating whether the clause was the
/// "who chose" shape (which triggers `PlayerFilter::VotedFor` wiring on
/// the parsed sub-effect).
///
/// Whitespace handling:
/// * Accepts both upper- and lowercase "For"/"for".
/// * Consumes a trailing period if present.
fn parse_for_each_vote_clause<'a>(
    input: &'a str,
    choices: &[String],
) -> Option<(&'a str, (String, &'a str, bool))> {
    // Case-insensitive opener; operates directly on original-case input so
    // downstream slices preserve casing without offset arithmetic.
    let res: nom::IResult<&'a str, (), OracleError<'a>> =
        value((), tag_no_case("for each ")).parse(input);
    let (rest_after_for, ()) = res.ok()?;

    // Try the "<player-noun> who chose <choice>, " shape first — its prefix
    // is alphabetic-leading just like the simple "<choice> vote, " shape, so
    // a successful match here unambiguously routes to the VotedFor wiring.
    if let Some((after_clause, choice_lower)) =
        parse_who_chose_player_clause(rest_after_for, choices)
    {
        let (effect_text, rest) = read_effect_until_next_clause(after_clause);
        return Some((rest, (choice_lower, effect_text, true)));
    }

    // Fallback: classic "<choice> vote, <effect>" shape.
    // Read the choice token (case-insensitive); choices are whitespace-free
    // single words in canonical Council's-dilemma cards.
    let (choice, after_choice) = read_word(rest_after_for)?;
    let choice_lower = choice.to_lowercase();
    if !choices.iter().any(|c| c == &choice_lower) {
        return None;
    }
    // Consume " vote, " (singular) — plural "votes" would imply the resolver
    // re-tally pattern that Council's dilemma never uses; reject to keep the
    // detector tight.
    let (after_vote, _): (&str, &str) = tag::<_, _, OracleError<'_>>(" vote, ")
        .parse(after_choice)
        .ok()?;
    // Read up to terminator: either next "For each " OR end-of-string,
    // stripping trailing period.
    let (effect_text, rest) = read_effect_until_next_clause(after_vote);
    Some((rest, (choice_lower, effect_text, false)))
}

/// Parse a single "Each <choice> <effect>." clause used by Battlebond's
/// friend-or-foe cards (no explicit CR section): Pir's Whim, Khorvath's
/// Fury, Regna's Sanction, Virtus's Maneuver, Zndrsplt's Judgment. The
/// `<choice>` token must be a member of the parent vote's `choices` list
/// (canonically `["friend", "foe"]`).
///
/// Shape: `"Each <choice> <effect>."` — case-insensitive on `"Each"`.
///
/// Returns the unconsumed remainder, the matched choice (lowercase), the
/// inner effect text, and `who_chose=true` (the per-class fan-out always
/// routes via `PlayerFilter::VotedFor`).
///
/// Distinct from `parse_for_each_vote_clause`: that helper recognizes
/// `"For each <choice> vote, <effect>"` and `"For each player who chose
/// <choice>, <effect>"`. The bare-`"Each <choice>"` shape only fires
/// under `VoterScope::ControllerLabels`; otherwise it would false-match
/// general "Each creature..." imperatives.
fn parse_each_class_clause<'a>(
    input: &'a str,
    choices: &[String],
) -> Option<(&'a str, (String, &'a str, bool))> {
    // Case-insensitive opener; operates directly on original-case input.
    let res: nom::IResult<&'a str, (), OracleError<'a>> =
        value((), tag_no_case("each ")).parse(input);
    let (after_each, ()) = res.ok()?;
    // Read the choice token and confirm it's a valid class.
    let (choice, after_choice) = read_word(after_each)?;
    let choice_lower = choice.to_lowercase();
    if !choices.iter().any(|c| c == &choice_lower) {
        return None;
    }
    // Consume the single space between the class label and the verb. The
    // body extends until the next `"Each "` (start of the sibling class
    // clause) or end of input. Strip the trailing period.
    let (after_space, _): (&str, &str) =
        tag::<_, _, OracleError<'_>>(" ").parse(after_choice).ok()?;
    let (effect_text, rest) = read_effect_until_each_class(after_space, choices);
    Some((rest, (choice_lower, effect_text, true)))
}

/// Read maximally up to the next `"Each <choice>"` clause or end of input,
/// where `<choice>` is a member of the parent vote's `choices` list. Strips
/// trailing period.
///
/// Implementation: a single nom combinator (`tag_no_case("each ")` →
/// `take_while1` for the class word → verify membership and trailing space)
/// is tried at every word boundary in `input` via `scan_split_at_phrase`.
/// The dynamic `choices` vocabulary is handled by `take_while1` + an inline
/// membership check rather than `alt()`, because `alt()` requires a static
/// tuple of branches.
///
/// This prevents false positives on intra-body phrases like
/// `"on each creature they control"` (Regna's Sanction friend body) where
/// `"each"` is the distributive quantifier inside an imperative, not the
/// start of a sibling class clause: `take_while1` reads "creature", which
/// fails the class-label membership check.
fn read_effect_until_each_class<'a>(input: &'a str, choices: &[String]) -> (&'a str, &'a str) {
    let is_word_char = |c: char| c.is_alphanumeric() || c == '\'' || c == '-';
    let try_each_class_marker = |i: &'a str| -> nom::IResult<&'a str, (), OracleError<'a>> {
        let (after_each, _) = tag_no_case::<_, _, OracleError<'a>>("each ").parse(i)?;
        let (after_word, word) =
            take_while1::<_, _, OracleError<'a>>(is_word_char).parse(after_each)?;
        let (_, _) = tag::<_, _, OracleError<'a>>(" ").parse(after_word)?;
        if !choices.iter().any(|c| c.eq_ignore_ascii_case(word)) {
            return Err(nom::Err::Error(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Verify,
            )));
        }
        Ok((after_word, ()))
    };
    let (head, tail) = scan_split_at_phrase(input, try_each_class_marker).unwrap_or((input, ""));
    let head_trimmed = head.trim_end();
    // allow-noncombinator: structural period strip on pre-extracted sentence clause
    let head_no_period = head_trimmed.strip_suffix('.').unwrap_or(head_trimmed);
    (head_no_period.trim(), tail.trim_start())
}

/// Parse the "who chose" sub-shape of a `for each ...` clause:
///
///   `"<player-noun> who chose <choice>, "`
///
/// where `<player-noun>` is `"player"` or `"opponent"` and `<choice>` must
/// be a member of the parent vote's `choices` list. Returns the remainder
/// after the trailing `", "` and the matched choice (lowercase).
fn parse_who_chose_player_clause<'a>(
    input: &'a str,
    choices: &[String],
) -> Option<(&'a str, String)> {
    let res: nom::IResult<&'a str, (), OracleError<'a>> =
        value((), alt((tag_no_case("player"), tag_no_case("opponent")))).parse(input);
    let (after_noun, ()) = res.ok()?;
    let (after_who, _): (&str, &str) = tag_no_case::<_, _, OracleError<'_>>(" who chose ")
        .parse(after_noun)
        .ok()?;
    let (choice_word, after_choice) = read_word(after_who)?;
    let choice_lower = choice_word.to_lowercase();
    if !choices.iter().any(|c| c == &choice_lower) {
        return None;
    }
    let (after_comma, _): (&str, &str) = tag::<_, _, OracleError<'_>>(", ")
        .parse(after_choice)
        .ok()?;
    Some((after_comma, choice_lower))
}

/// Read a maximal prefix up to (but not including) the next "For each "
/// clause or end of input. Strips a trailing period from the consumed slice.
///
/// `scan_split_at_phrase` + `tag_no_case` is the idiomatic combinator pair
/// for "split at the next word-boundary occurrence of <phrase>": it tries
/// the combinator at every word boundary and returns the split point on
/// the first match.
fn read_effect_until_next_clause(input: &str) -> (&str, &str) {
    let (head, tail) = scan_split_at_phrase(input, |i| {
        tag_no_case::<_, _, OracleError<'_>>("for each ").parse(i)
    })
    .unwrap_or((input, ""));
    let head_trimmed = head.trim_end();
    // allow-noncombinator: structural period strip on pre-extracted sentence clause
    let head_no_period = head_trimmed.strip_suffix('.').unwrap_or(head_trimmed);
    (head_no_period.trim(), tail.trim_start())
}

/// Parse one aggregate-tally per-choice clause (Emissary Green):
///
///   `"<effect ...a number of <X>...> equal to [<multiplier>] the number of <choice> votes."`
///
/// Canonical bodies:
///   * "You create a number of Treasure tokens equal to twice the number of profit votes."
///   * "Put a number of +1/+1 counters on each creature you control equal to the number of security votes."
///
/// The per-vote multiplier ("twice" → 2, "<n> times" → n, absent → 1) folds
/// into the sub-effect's fixed count. The Vote resolver runs each per-choice
/// sub-effect once per tallied vote (CR 701.38), so a `count = multiplier`
/// sub-effect produces `multiplier × votes` total — exactly the aggregate the
/// Oracle text describes.
///
/// Returns `(remainder_after_sentence, choice_lowercase, rewritten_effect_text)`.
/// `None` when the clause is not in this shape, letting the caller fall through.
fn parse_aggregate_tally_clause<'a>(
    input: &'a str,
    choices: &[String],
) -> Option<(&'a str, String, String)> {
    let (sentence, rest) = read_sentence(input);
    // Locate "equal to [<multiplier>] the number of <choice> votes" at a word
    // boundary via a single combinator; `scan_preceded` returns the head before
    // the match, the parsed (choice, multiplier), and the post-match remainder.
    let suffix = |i: &'a str| -> nom::IResult<&'a str, (String, u32), OracleError<'a>> {
        let (i, _) = tag_no_case("equal to ").parse(i)?;
        let (i, multiplier) = parse_tally_multiplier(i)?;
        let (i, _) = tag_no_case("the number of ").parse(i)?;
        let (i, choice) =
            take_while1(|c: char| c.is_alphanumeric() || c == '\'' || c == '-').parse(i)?;
        if !choices.iter().any(|c| c.eq_ignore_ascii_case(choice)) {
            return Err(nom::Err::Error(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Verify,
            )));
        }
        let (i, _) = tag_no_case(" votes").parse(i)?;
        Ok((i, (choice.to_lowercase(), multiplier)))
    };
    let (head, (choice, multiplier), tail) = scan_preceded(sentence, suffix)?;
    // The tally clause must be the sentence suffix — reject trailing text we
    // are not modeling (keeps the detector tight).
    if !tail.trim().is_empty() {
        return None;
    }
    let rewritten = rewrite_a_number_of(head.trim_end(), multiplier)?;
    Some((rest, choice, rewritten))
}

/// Parse the optional per-vote multiplier preceding "the number of <choice>
/// votes": `"twice "` → 2, `"<n> times "` → n (digit or English word), and an
/// absent multiplier → 1. Always succeeds so it composes inside the tally
/// combinator.
fn parse_tally_multiplier(input: &str) -> OracleResult<'_, u32> {
    alt((
        value(2u32, tag_no_case("twice ")),
        map((parse_number, tag_no_case(" times ")), |(n, _)| n),
        success(1u32),
    ))
    .parse(input)
}

/// Rewrite an effect head of the form `"...a number of <X>..."` into the
/// fixed-count imperative `"...<count> <X>..."`, which the standard effect
/// chain parser maps to a `Fixed` count. Returns `None` when the head lacks the
/// "a number of " quantity phrase (i.e. not an aggregate-tally effect).
fn rewrite_a_number_of(head: &str, count: u32) -> Option<String> {
    let (before, _, after) = scan_preceded(head, |i| {
        tag_no_case::<_, _, OracleError<'_>>("a number of ").parse(i)
    })?;
    Some(format!("{before}{count} {after}"))
}

/// Split the input at the first period: returns `(sentence, remainder)` with
/// surrounding whitespace trimmed. The final clause may lack a trailing period,
/// in which case the whole input is the sentence and the remainder is empty.
fn read_sentence(input: &str) -> (&str, &str) {
    match input.find('.') {
        Some(idx) => (input[..idx].trim(), input[idx + 1..].trim_start()),
        None => (input.trim(), ""),
    }
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
            nom::bytes::complete::take_while1::<_, &str, OracleError<'_>>(word_chars)
                .parse(rest)
                .ok()?;
        choices.push(word.to_string());
        rest = after_word;
        if rest.is_empty() {
            break;
        }
        // Consume separator; try longest match first to avoid partial matches.
        let sep_res: nom::IResult<&str, (), OracleError<'_>> =
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
    use crate::types::ability::TargetFilter;

    #[test]
    fn parses_tivit_vote_block() {
        let text = "starting with you, each player votes for evidence or bribery. For each evidence vote, investigate. For each bribery vote, create a Treasure token.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                ref per_choice_effect,
                starting_with,
                voter_scope,
            } => {
                assert_eq!(
                    choices,
                    &vec!["evidence".to_string(), "bribery".to_string()]
                );
                assert_eq!(per_choice_effect.len(), 2);
                assert_eq!(starting_with, ControllerRef::You);
                assert_eq!(voter_scope, VoterScope::AllPlayers);
                // First per-choice = Investigate
                assert!(matches!(*per_choice_effect[0].effect, Effect::Investigate));
                // Second per-choice = Token (Treasure)
                assert!(matches!(*per_choice_effect[1].effect, Effect::Token { .. }));
                // Classic Tivit shape: per-choice sub-effects do not carry a
                // VotedFor scope (they fan out per-vote, not per-voter).
                assert!(per_choice_effect[0].player_scope.is_none());
                assert!(per_choice_effect[1].player_scope.is_none());
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// CR 800.4g: Master of Ceremonies's full upkeep-trigger body — three
    /// choices, `EachOpponent` voter scope, and "for each player who chose X,
    /// you and that player each Y" per-choice clauses. This is the canonical
    /// regression test for the bug fix this module was generalized to support.
    #[test]
    fn parses_master_of_ceremonies_vote_block() {
        let text = "each opponent chooses money, friends, or secrets. For each player who chose money, you and that player each create a Treasure token. For each player who chose friends, you and that player each create a 1/1 green and white Citizen creature token. For each player who chose secrets, you and that player each draw a card.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                ref per_choice_effect,
                voter_scope,
                ..
            } => {
                assert_eq!(
                    choices,
                    &vec![
                        "money".to_string(),
                        "friends".to_string(),
                        "secrets".to_string()
                    ]
                );
                assert_eq!(voter_scope, VoterScope::EachOpponent);
                assert_eq!(per_choice_effect.len(), 3);
                // Each per-choice sub-effect is wired to PlayerFilter::VotedFor
                // with its own choice index.
                assert_eq!(
                    per_choice_effect[0].player_scope,
                    Some(PlayerFilter::VotedFor { choice_index: 0 })
                );
                assert_eq!(
                    per_choice_effect[1].player_scope,
                    Some(PlayerFilter::VotedFor { choice_index: 1 })
                );
                assert_eq!(
                    per_choice_effect[2].player_scope,
                    Some(PlayerFilter::VotedFor { choice_index: 2 })
                );

                // CR 109.5: Each per-choice body has been distributed by the
                // compound-subject combinator. The top-level effect's recipient
                // is `OriginalController`; the second half is in `sub_ability`
                // with `ScopedPlayer`.
                let assert_distributed = |idx: usize, label: &str| {
                    let body = &per_choice_effect[idx];
                    let top_target = match &*body.effect {
                        Effect::Token { owner, .. } => owner.clone(),
                        Effect::Draw { target, .. } => target.clone(),
                        other => panic!("[{}] unexpected per_choice top effect {:?}", label, other),
                    };
                    assert_eq!(
                        top_target,
                        TargetFilter::OriginalController,
                        "[{}] top half must target OriginalController",
                        label
                    );
                    let sub = body
                        .sub_ability
                        .as_ref()
                        .unwrap_or_else(|| panic!("[{}] expected per_choice sub_ability", label));
                    let sub_target = match &*sub.effect {
                        Effect::Token { owner, .. } => owner.clone(),
                        Effect::Draw { target, .. } => target.clone(),
                        other => panic!("[{}] unexpected per_choice sub effect {:?}", label, other),
                    };
                    assert_eq!(
                        sub_target,
                        TargetFilter::ScopedPlayer,
                        "[{}] sub half must target ScopedPlayer",
                        label
                    );
                };
                assert_distributed(0, "money");
                assert_distributed(1, "friends");
                assert_distributed(2, "secrets");
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// Two-choice variant of the "each opponent chooses ..." pattern.
    #[test]
    fn parses_each_opponent_chooses_two_options() {
        let text = "each opponent chooses left or right. For each player who chose left, you and that player each draw a card. For each player who chose right, you and that player each draw a card.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                voter_scope,
                ..
            } => {
                assert_eq!(choices, &vec!["left".to_string(), "right".to_string()]);
                assert_eq!(voter_scope, VoterScope::EachOpponent);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// Three-choice variant of the "each opponent chooses ..." pattern.
    #[test]
    fn parses_each_opponent_chooses_three_options() {
        let text = "each opponent chooses one, two, or three. For each player who chose one, you and that player each draw a card. For each player who chose two, you and that player each draw a card. For each player who chose three, you and that player each draw a card.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                voter_scope,
                ref per_choice_effect,
                ..
            } => {
                assert_eq!(choices.len(), 3);
                assert_eq!(per_choice_effect.len(), 3);
                assert_eq!(voter_scope, VoterScope::EachOpponent);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// Single-choice opener must be rejected — `parse_vote_block` requires
    /// at least two choices to avoid false-positives on unrelated text.
    #[test]
    fn rejects_each_opponent_with_only_one_choice() {
        let text = "each opponent chooses money. For each player who chose money, you and that player each draw a card.";
        // `split_choices` requires N>=2 — single-choice input fails the
        // detector outright.
        assert!(parse_vote_block(text, AbilityKind::Spell).is_none());
    }

    /// Regression: serialized vote effects from the previous schema
    /// (without `voter_scope`) deserialize as `VoterScope::AllPlayers`.
    /// We don't have direct access to a stale JSON blob here; instead,
    /// confirm the classic "starting with you, each player votes for ..."
    /// path produces `AllPlayers`, which is what the serde default emits.
    #[test]
    fn tivit_test_still_passes_with_default_voter_scope() {
        let text = "starting with you, each player votes for evidence or bribery. For each evidence vote, investigate. For each bribery vote, create a Treasure token.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        if let Effect::Vote { voter_scope, .. } = *def.effect {
            assert_eq!(voter_scope, VoterScope::AllPlayers);
        } else {
            panic!("expected Vote effect");
        }
    }

    /// Direct unit test for the "<player-noun> who chose <choice>, " sub-clause.
    #[test]
    fn parses_for_each_player_who_chose_money_clause() {
        let choices = vec!["money".to_string(), "friends".to_string()];
        let (rest, choice) =
            parse_who_chose_player_clause("player who chose money, do stuff", &choices)
                .expect("clause parses");
        assert_eq!(choice, "money");
        assert_eq!(rest, "do stuff");
        // Same with "opponent".
        let (rest2, choice2) =
            parse_who_chose_player_clause("opponent who chose friends, draw a card", &choices)
                .expect("clause parses");
        assert_eq!(choice2, "friends");
        assert_eq!(rest2, "draw a card");
    }

    /// Regression: existing N=3 voting card (Capital Punishment is the public
    /// reference; here we use its grammatical shape with stand-in choices).
    #[test]
    fn parses_capital_punishment_three_choice_vote() {
        let text = "starting with you, each player votes for first, second, or third. For each first vote, draw a card. For each second vote, investigate. For each third vote, create a Treasure token.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                voter_scope,
                ref per_choice_effect,
                ..
            } => {
                assert_eq!(choices.len(), 3);
                assert_eq!(per_choice_effect.len(), 3);
                assert_eq!(voter_scope, VoterScope::AllPlayers);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    #[test]
    fn rejects_non_vote_text() {
        assert!(parse_vote_block("Draw a card.", AbilityKind::Spell).is_none());
    }

    /// CR 701.38 + CR 122.1: Emissary Green's aggregate-tally vote. The
    /// per-choice effects reference the vote count via "a number of … equal to
    /// [twice] the number of <choice> votes" rather than the classic "For each
    /// <choice> vote, …" repetition. Each per-choice sub-effect must carry a
    /// fixed count equal to the multiplier (the Vote resolver runs it once per
    /// tallied vote), and must NOT be VotedFor-scoped (controller-performed).
    #[test]
    fn parses_emissary_green_aggregate_vote_block() {
        use crate::types::ability::QuantityExpr;
        let text = "starting with you, each player votes for profit or security. \
                    You create a number of Treasure tokens equal to twice the number of profit votes. \
                    Put a number of +1/+1 counters on each creature you control equal to the number of security votes.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                ref per_choice_effect,
                starting_with,
                voter_scope,
            } => {
                assert_eq!(choices, &vec!["profit".to_string(), "security".to_string()]);
                assert_eq!(starting_with, ControllerRef::You);
                assert_eq!(voter_scope, VoterScope::AllPlayers);
                assert_eq!(per_choice_effect.len(), 2);

                // profit → "create Treasure tokens", count = 2 (the "twice"
                // multiplier), so each profit vote makes 2 Treasures.
                match &*per_choice_effect[0].effect {
                    Effect::Token { name, count, .. } => {
                        assert_eq!(name, "Treasure");
                        assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
                    }
                    other => panic!("expected profit → Token, got {:?}", other),
                }
                // security → "put +1/+1 counters on each creature you control",
                // count = 1, so each security vote adds one counter to each.
                match &*per_choice_effect[1].effect {
                    Effect::PutCounterAll { count, target, .. } => {
                        assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                        match target {
                            TargetFilter::Typed(tf) => {
                                assert_eq!(tf.controller, Some(ControllerRef::You));
                                assert!(
                                    tf.type_filters.iter().any(|t| matches!(
                                        t,
                                        crate::types::ability::TypeFilter::Creature
                                    )),
                                    "expected a Creature type filter, got {:?}",
                                    tf.type_filters
                                );
                            }
                            other => panic!("expected Typed creature target, got {:?}", other),
                        }
                    }
                    other => panic!("expected security → PutCounterAll, got {:?}", other),
                }

                // Controller-performed: no per-voter fan-out wiring.
                assert!(per_choice_effect[0].player_scope.is_none());
                assert!(per_choice_effect[1].player_scope.is_none());
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    #[test]
    fn parse_tally_multiplier_covers_twice_times_and_default() {
        // "twice " → 2
        let (rest, m) = parse_tally_multiplier("twice the number of profit votes").unwrap();
        assert_eq!(m, 2);
        assert_eq!(rest, "the number of profit votes");
        // "three times " → 3 (English word via parse_number)
        let (rest, m) = parse_tally_multiplier("three times the number of x votes").unwrap();
        assert_eq!(m, 3);
        assert_eq!(rest, "the number of x votes");
        // "4 times " → 4 (digit)
        let (_, m) = parse_tally_multiplier("4 times the number of x votes").unwrap();
        assert_eq!(m, 4);
        // absent → 1, consuming nothing
        let (rest, m) = parse_tally_multiplier("the number of security votes").unwrap();
        assert_eq!(m, 1);
        assert_eq!(rest, "the number of security votes");
    }

    #[test]
    fn rewrite_a_number_of_inserts_fixed_count() {
        assert_eq!(
            rewrite_a_number_of("You create a number of Treasure tokens", 2).unwrap(),
            "You create 2 Treasure tokens"
        );
        assert_eq!(
            rewrite_a_number_of(
                "Put a number of +1/+1 counters on each creature you control",
                1
            )
            .unwrap(),
            "Put 1 +1/+1 counters on each creature you control"
        );
        // No "a number of " phrase → not an aggregate-tally effect.
        assert!(rewrite_a_number_of("create a Treasure token", 1).is_none());
    }

    /// CR 608.2c + CR 701.38: Documented parser gap (R5 in the
    /// implementation plan). The Master of Ceremonies vote skeleton parses
    /// correctly (see `parses_master_of_ceremonies_vote_block`), but the
    /// per-choice effect text "you and that player each create a Treasure
    /// token" is NOT yet distributed into a 2-element chain by
    /// `parse_effect_chain_with_context`.
    ///
    /// The current parser produces:
    ///   * top effect: `Effect::Unimplemented { name: "you", description: "you" }`
    ///   * sub_ability: `Effect::Draw { count: 1, target: Any }` (subject lost)
    ///
    /// The architecturally correct fix is to teach `oracle_effect` to
    /// recognize "<player-noun-A> and <player-noun-B> each Y" and emit a
    /// chain of two parallel sub-effects (one targeting `Controller`, one
    /// targeting `ScopedPlayer`/the recorded voter). That work is non-trivial
    /// new parser infrastructure (a new combinator + scoped-player wiring)
    /// and is therefore out of scope for this PR per the plan's R5 risk
    /// gate. Tracked as a follow-up.
    ///
    /// This test pins the current behavior so the gap is visible in the
    /// test suite and so any future fix updates this assertion in lockstep.
    /// CR 109.5 + CR 608.2c + CR 800.4g: "you and that player each Y" must
    /// distribute the body across two recipients. The first half is targeted
    /// at `OriginalController` (the printed ability controller); the second
    /// half is targeted at `ScopedPlayer` (the iterated voter from
    /// `PlayerFilter::VotedFor`). Halves chain via `sub_ability`.
    ///
    /// This was originally a documented gap test that pinned `Unimplemented`;
    /// it is now the positive regression for the R5 distribution combinator.
    #[test]
    fn parser_distributes_you_and_that_player_each_draw() {
        let parsed = parse_effect_chain_with_context(
            "you and that player each draw a card",
            AbilityKind::Spell,
            &mut ParseContext::default(),
        );
        match *parsed.effect {
            Effect::Draw { ref target, .. } => {
                assert_eq!(*target, TargetFilter::OriginalController);
            }
            other => panic!(
                "expected Draw {{ target: OriginalController }} for first half, got {:?}",
                other
            ),
        }
        let sub = parsed
            .sub_ability
            .expect("expected second-half sub_ability");
        match *sub.effect {
            Effect::Draw { ref target, .. } => {
                assert_eq!(*target, TargetFilter::ScopedPlayer);
            }
            other => panic!(
                "expected Draw {{ target: ScopedPlayer }} for second half, got {:?}",
                other
            ),
        }
    }

    /// "you and that player each create a Treasure token" — the canonical
    /// Master of Ceremonies "money" reward. Each half is `Effect::Token`
    /// with its `owner` field rewritten.
    #[test]
    fn parser_distributes_you_and_that_player_each_create_token() {
        let parsed = parse_effect_chain_with_context(
            "you and that player each create a Treasure token",
            AbilityKind::Spell,
            &mut ParseContext::default(),
        );
        match *parsed.effect {
            Effect::Token { ref owner, .. } => {
                assert_eq!(*owner, TargetFilter::OriginalController);
            }
            other => panic!("expected Token for first half, got {:?}", other),
        }
        let sub = parsed
            .sub_ability
            .expect("expected second-half sub_ability");
        match *sub.effect {
            Effect::Token { ref owner, .. } => {
                assert_eq!(*owner, TargetFilter::ScopedPlayer);
            }
            other => panic!("expected Token for second half, got {:?}", other),
        }
    }

    /// "you and target opponent each create a Treasure token" — the chosen
    /// opponent must be surfaced as a real player target, not collapsed into a
    /// single token effect with `owner: Any`.
    #[test]
    fn parser_distributes_you_and_target_opponent_each_create_token() {
        let parsed = parse_effect_chain_with_context(
            "you and target opponent each create a Treasure token",
            AbilityKind::Spell,
            &mut ParseContext::default(),
        );
        match *parsed.effect {
            Effect::Token { ref owner, .. } => {
                assert_eq!(*owner, TargetFilter::OriginalController);
            }
            other => panic!("expected Token for first half, got {:?}", other),
        }
        let sub = parsed
            .sub_ability
            .expect("expected second-half sub_ability");
        match *sub.effect {
            Effect::Token { ref owner, .. } => {
                assert_eq!(*owner, TargetFilter::Player);
            }
            other => panic!("expected Token for second half, got {:?}", other),
        }
    }

    /// Full-line typed-token body (Citizen reward path): "1/1 green and white
    /// Citizen creature token" must round-trip through the body parser and
    /// retain its full type description on both halves.
    #[test]
    fn parser_distributes_you_and_that_player_each_chain_with_typed_token() {
        let parsed = parse_effect_chain_with_context(
            "you and that player each create a 1/1 green and white Citizen creature token",
            AbilityKind::Spell,
            &mut ParseContext::default(),
        );
        match *parsed.effect {
            Effect::Token {
                ref owner,
                ref types,
                ..
            } => {
                assert_eq!(*owner, TargetFilter::OriginalController);
                assert!(
                    types.iter().any(|t| t.eq_ignore_ascii_case("citizen")),
                    "expected types to include Citizen, got {:?}",
                    types
                );
            }
            other => panic!("expected Token for first half, got {:?}", other),
        }
        let sub = parsed
            .sub_ability
            .expect("expected second-half sub_ability");
        match *sub.effect {
            Effect::Token {
                ref owner,
                ref types,
                ..
            } => {
                assert_eq!(*owner, TargetFilter::ScopedPlayer);
                assert!(
                    types.iter().any(|t| t.eq_ignore_ascii_case("citizen")),
                    "expected sub types to include Citizen, got {:?}",
                    types
                );
            }
            other => panic!("expected Token for second half, got {:?}", other),
        }
    }

    // --- Battlebond friend-or-foe (Pir's Whim class) ---

    /// Pir's Whim is the canonical friend-or-foe spell (no explicit CR
    /// section; CR 101.4 APNAP + CR 608.2 resolution apply). The opener
    /// `"For each player, choose friend or foe."` emits a Vote with
    /// `voter_scope = ControllerLabels`; the two `"Each <choice> <effect>."`
    /// clauses emit per-choice sub-effects with `player_scope = VotedFor`.
    #[test]
    fn parses_pirs_whim_friend_or_foe_block() {
        let text = "For each player, choose friend or foe. \
                    Each friend searches their library for a land card, puts it onto \
                    the battlefield tapped, then shuffles. \
                    Each foe sacrifices an artifact or enchantment of their choice.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                ref per_choice_effect,
                voter_scope,
                ..
            } => {
                assert_eq!(choices, &vec!["friend".to_string(), "foe".to_string()]);
                assert_eq!(voter_scope, VoterScope::ControllerLabels);
                assert_eq!(per_choice_effect.len(), 2);
                assert_eq!(
                    per_choice_effect[0].player_scope,
                    Some(PlayerFilter::VotedFor { choice_index: 0 })
                );
                assert_eq!(
                    per_choice_effect[1].player_scope,
                    Some(PlayerFilter::VotedFor { choice_index: 1 })
                );
                // friend body parses to SearchLibrary chain
                assert!(
                    matches!(*per_choice_effect[0].effect, Effect::SearchLibrary { .. }),
                    "expected friend body to be SearchLibrary, got {:?}",
                    per_choice_effect[0].effect
                );
                // foe body parses to Sacrifice
                assert!(
                    matches!(*per_choice_effect[1].effect, Effect::Sacrifice { .. }),
                    "expected foe body to be Sacrifice, got {:?}",
                    per_choice_effect[1].effect
                );
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// The CR-ordering invariant — `choices[0]` must be `"friend"` so
    /// per-class fan-out runs friends before foes (Pir's Whim 2018-06-08
    /// ruling: "Friends perform their specified actions before foes."). All
    /// five Battlebond cards print the friend clause first.
    #[test]
    fn pirs_whim_emits_friend_before_foe_in_choices() {
        let text = "For each player, choose friend or foe. \
                    Each friend draws a card. Each foe loses 1 life.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote { ref choices, .. } => {
                assert_eq!(choices[0], "friend");
                assert_eq!(choices[1], "foe");
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// The bare `Each <choice>` per-class shape must not false-match
    /// intra-body `each` (e.g., "puts a +1/+1 counter on each creature they
    /// control" — Regna's Sanction friend body). The split discriminator
    /// requires the token after `each ` to be a known class label.
    #[test]
    fn regnas_sanction_friend_body_keeps_distributive_each_intact() {
        let text = "For each player, choose friend or foe. \
                    Each friend puts a +1/+1 counter on each creature they control. \
                    Each foe taps a creature.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                ref per_choice_effect,
                ..
            } => {
                assert_eq!(choices, &vec!["friend".to_string(), "foe".to_string()]);
                // Friend body should NOT be split at "each creature" — the full
                // body parses as PutCounterAll (distributive over creatures).
                assert!(
                    matches!(*per_choice_effect[0].effect, Effect::PutCounterAll { .. }),
                    "friend body must keep distributive 'each creature' intact, \
                     got {:?}",
                    per_choice_effect[0].effect
                );
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// Rejects single-class openers — every friend-or-foe card prints two
    /// classes. A single-choice opener like `"For each player, choose
    /// friend."` is malformed and must fail (matches the existing
    /// single-choice rejection for classic votes).
    #[test]
    fn rejects_single_class_friend_or_foe_opener() {
        let text = "For each player, choose friend. Each friend draws a card.";
        assert!(parse_vote_block(text, AbilityKind::Spell).is_none());
    }
}
