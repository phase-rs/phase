use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{eof, opt, value};
use nom::Parser;
use nom_language::error::VerboseError;

use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_target::parse_target;
use super::super::oracle_util::contains_possessive;
use super::types::*;
use crate::parser::oracle_quantity::{parse_cda_quantity, parse_quantity_ref};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Chooser, Effect, QuantityExpr, QuantityRef, StaticDefinition,
    TargetFilter,
};
use crate::types::zones::Zone;

/// CR 608.2c + CR 701.23i: Strip a leading player-subject from a search-result
/// continuation chunk so the absorption matcher sees the bare verb form. Used
/// by the SearchDestination follow-up absorber to handle iterated-search
/// variants (Winds of Abandon: "those players put those cards onto the
/// battlefield tapped") whose subject was demoted from a top-level subject
/// because the put-step has already been folded into the search continuation.
///
/// Single nom `alt()` over the player-subject prefixes — extend by adding new
/// arms here, never by adding more enumerated `matches!` arms downstream.
///
/// Intentionally does NOT delegate to `subject::parse_subject_application`:
/// that function is a full subject parser that returns a `SubjectApplication`
/// (filter + targeting + multi-target spec) for use at clause boundaries.
/// Here we only need to peel a known set of player-pronoun prefixes from a
/// continuation chunk before re-tokenizing — there is no filter to derive,
/// no target to attach, and no multi-target structure. The simpler local form
/// keeps the search-continuation absorber decoupled from the subject parser's
/// richer return type and avoids constructing/then-discarding a
/// `SubjectApplication` on the hot continuation path.
fn strip_search_result_subject(lower: &str) -> &str {
    alt((
        tag::<_, _, VerboseError<&str>>("those players "),
        tag("that player "),
        tag("each player "),
    ))
    .parse(lower)
    .map(|(rest, _)| rest)
    .unwrap_or(lower)
}

/// Parse count from "choose one/two/three/N of them/those" text using nom combinator.
/// Handles all chooser prefix forms: "choose ", "you choose ", "an opponent chooses ",
/// "target opponent chooses ".
fn parse_choose_count_from_text(lower: &str) -> u32 {
    // Strip chooser prefix using nom combinators (input already lowercase).
    let rest = alt((tag("an opponent chooses "), tag("target opponent chooses ")))
        .parse(lower)
        .map(|(rest, _)| rest)
        .unwrap_or_else(|_: nom::Err<VerboseError<&str>>| {
            let s = tag::<_, _, VerboseError<&str>>("you ")
                .parse(lower)
                .map(|(rest, _)| rest)
                .unwrap_or(lower);
            alt((tag::<_, _, VerboseError<&str>>("choose "), tag("chooses ")))
                .parse(s)
                .map(|(rest, _)| rest)
                .unwrap_or(s)
        });
    // Delegate to nom combinator for the number.
    nom_primitives::parse_number
        .parse(rest)
        .map(|(_, n)| n)
        .unwrap_or(1)
}

fn parse_put_all_back_in_any_order(lower: &str) -> bool {
    (
        tag::<_, _, VerboseError<&str>>("put "),
        alt((tag("them"), tag("those cards"), tag("the cards"))),
        tag(" back"),
        alt((
            tag(" in any order"),
            tag(" on top of your library in any order"),
            tag(" on top in any order"),
        )),
        opt(tag(".")),
        eof,
    )
        .parse(lower.trim())
        .is_ok()
}

pub(super) fn split_clause_sequence(text: &str) -> Vec<ClauseChunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let mut paren_depth = 0usize;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while let Some(ch) = chars.next() {
        match ch {
            '(' if !in_single_quote && !in_double_quote => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' if !in_single_quote && !in_double_quote => {
                paren_depth = paren_depth.saturating_sub(1);
                current.push(ch);
            }
            '\'' if !in_double_quote => {
                if is_possessive_apostrophe(&current, chars.peek().copied()) {
                    current.push(ch);
                } else {
                    in_single_quote = !in_single_quote;
                    current.push(ch);
                }
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                current.push(ch);
            }
            ',' if paren_depth == 0 && !in_single_quote && !in_double_quote => {
                let remainder = chars.clone().collect::<String>();
                if let Some((boundary, chars_to_skip)) =
                    split_comma_clause_boundary(&current, &remainder)
                {
                    push_clause_chunk(&mut chunks, &current, Some(boundary));
                    current.clear();
                    for _ in 0..chars_to_skip {
                        chars.next();
                    }
                } else {
                    current.push(ch);
                }
            }
            '.' if paren_depth == 0 && !in_single_quote && !in_double_quote => {
                push_clause_chunk(&mut chunks, &current, Some(ClauseBoundary::Sentence));
                current.clear();
                while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
                    chars.next();
                }
            }
            _ => {
                current.push(ch);
                // Detect bare " and " at word boundary followed by an imperative verb.
                // Handles patterns like "you lose 1 life and create a Treasure token".
                // Uses a restricted verb list to avoid false positives on noun phrases
                // like "target creature and all other creatures" or "it and each other".
                if paren_depth == 0
                    && !in_single_quote
                    && !in_double_quote
                    && current.ends_with(" and ")
                {
                    let remainder: String = chars.clone().collect();
                    let remainder_trimmed = remainder.trim_start();
                    // Suppress split when "and put" follows "from among" — the
                    // "put into hand / onto battlefield" is part of the same
                    // compound action, not a separate clause.
                    let before_and = &current[..current.len() - " and ".len()];
                    let before_lower = before_and.to_ascii_lowercase();
                    // CR 603.7a: Suppress bare-and splitting inside temporal prefix
                    // clauses (e.g., "at the beginning of your next upkeep, draw a
                    // card and gain 3 life"). The entire compound inner effect must
                    // stay as one clause so CreateDelayedTrigger wraps all effects.
                    // CR 608.2c: Preserve targeted compound actions so the effect
                    // parser can retarget continuation clauses like
                    // "tap target creature ... and put a stun counter on it".
                    let targeted_compound_continuation =
                        nom_primitives::scan_contains(&before_lower, "target")
                            && tag::<_, _, VerboseError<&str>>("put ")
                                .parse(remainder_trimmed)
                                .is_ok();
                    // CR 701.18a + CR 701.23: "search [zones] for [filter] and exile them"
                    // is a single compound search-and-exile action — keep it together so
                    // the imperative dispatcher can recognize the multi-zone pattern.
                    // Accepts "search ..." and "then search ..." prefixes, and either
                    // "with that name" or "with the same name as that card" suffixes.
                    let has_search_prefix = nom_primitives::scan_contains(&before_lower, "search ");
                    let search_with_that_name = has_search_prefix
                        && (before_lower.ends_with("with that name")
                            || before_lower.ends_with("with the same name as that card"))
                        && tag::<_, _, VerboseError<&str>>("exile them")
                            .parse(remainder_trimmed)
                            .is_ok();
                    // CR 707.9: ", except <body> and <body> [and …]" — inside
                    // a copy-effect except clause, " and " is an internal
                    // delimiter between recognised body shapes (SetName, P/T,
                    // type additions, "has this ability", etc.) handled by
                    // the shared `become_copy_except` parser. The chain
                    // splitter must NOT bisect the body at this " and ", or
                    // the second body fragment ("and she has this ability")
                    // becomes a stray sub_ability and never reaches the
                    // except parser.
                    //
                    // `scan_contains` matches phrases starting at word
                    // boundaries (post-space), so we probe for the bare word
                    // "except " rather than ", except " — a leading comma
                    // never sits at a word start.
                    let inside_except_clause =
                        nom_primitives::scan_contains(&before_lower, "except ");
                    let suppress = nom_primitives::scan_contains(&before_lower, "from among")
                        || is_inside_temporal_prefix(&before_lower)
                        || targeted_compound_continuation
                        || search_with_that_name
                        || inside_except_clause;
                    if !suppress && starts_bare_and_clause(remainder_trimmed) {
                        push_clause_chunk(&mut chunks, before_and, Some(ClauseBoundary::Comma));
                        current.clear();
                    }
                }
            }
        }
    }

    push_clause_chunk(&mut chunks, &current, None);
    chunks
}

fn split_comma_clause_boundary(current: &str, remainder: &str) -> Option<(ClauseBoundary, usize)> {
    let current_lower = current.trim().to_ascii_lowercase();
    let trimmed = remainder.trim_start();
    let whitespace_len = remainder.len() - trimmed.len();
    let trimmed_lower = trimmed.to_ascii_lowercase();

    if starts_prefix_clause(&current_lower) {
        return None;
    }

    // CR 701.18a: "search [library] for X, put/reveal Y" is a single compound action.
    // The search verb may follow a sequence connector like "Then" from a prior sentence.
    // CR 701.18a: Enumerated "search" prefixes — do NOT use contains(" search ").
    let search_start = alt((
        tag::<_, _, VerboseError<&str>>("search "),
        tag("then search "),
        tag("you may search "),
        tag("you search "),
        tag("then you may search "),
        tag("then you search "),
    ))
    .parse(current_lower.as_str())
    .is_ok();
    if search_start
        && alt((tag::<_, _, VerboseError<&str>>("reveal "), tag("put ")))
            .parse(trimmed_lower.as_str())
            .is_ok()
    {
        return None;
    }

    if tag::<_, _, VerboseError<&str>>("then ")
        .parse(trimmed_lower.as_str())
        .is_ok()
    {
        let after_then = &trimmed["then ".len()..];
        let after_then_lower = &trimmed_lower["then ".len()..];
        if starts_clause_text_or_conjugated(after_then)
            || starts_with_damage_clause(after_then_lower)
        {
            return Some((ClauseBoundary::Then, whitespace_len + "then ".len()));
        }
    }

    if starts_clause_text(trimmed) || starts_with_damage_clause(&trimmed_lower) {
        return Some((ClauseBoundary::Comma, whitespace_len));
    }

    // Strip "and " connector before checking clause start
    // Handles patterns like ", and get {E}{E}" or ", and draw a card"
    if let Ok((after_and, _)) =
        tag::<_, _, VerboseError<&str>>("and ").parse(trimmed_lower.as_str())
    {
        if starts_clause_text(after_and) || starts_with_damage_clause(after_and) {
            return Some((ClauseBoundary::Comma, whitespace_len));
        }
    }

    None
}

fn starts_prefix_clause(current_lower: &str) -> bool {
    // CR 603.7a: Temporal prefix clauses must not be split on their internal comma.
    // CR 611.2b: "For as long as [condition], [effect]" — duration prefix clause.
    alt((
        tag::<_, _, VerboseError<&str>>("until "),
        tag("after "),
        tag("if "),
        tag("when "),
        tag("whenever "),
        tag("for each "),
        tag("then if "),
        // "then, if ..." (with comma after "then") — same scoping as "then if".
        // Regression: A Good Thing ("Then, if you have 1,000 or more life, you
        // lose the game") — without this, the splitter bisects the conditional
        // at the comma between life and "you lose", orphaning the body.
        tag("then, if "),
        tag("otherwise"),
        tag("if not"),
        tag("the next time "),
        tag("at the beginning "),
        tag("for as long as "),
    ))
    .parse(current_lower)
    .is_ok()
}

/// Check whether `text` begins with an imperative verb or pronoun that can start
/// an independent clause.  Used by the clause splitter to detect boundaries at
/// commas, "then", and bare "and".
///
/// **Convention — trailing space:**
/// - *Transitive* verbs (always require an object): include a trailing space
///   (e.g. `"draw "`, `"destroy "`).  This prevents false matches on noun phrases.
/// - *Intransitive* verbs (can appear bare at end-of-sentence, e.g. `", then shuffle."`):
///   omit the trailing space so the prefix matches even when followed by punctuation.
///   Current intransitive entries: `"explore"`, `"investigate"`, `"proliferate"`,
///   `"shuffle"`.
pub(super) fn starts_clause_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    starts_clause_text_lower(&lower)
}

/// Check whether `text` begins with a conjugated (third-person) verb form that,
/// after deconjugation, would match a recognized imperative verb.
///
/// This handles patterns like "draws seven cards" or "sacrifices a creature"
/// where the subject carries over from the prior clause (e.g.,
/// "Each player discards their hand, then draws seven cards.").
///
/// Uses `normalize_verb_token` for irregular forms (does→do, has→have, copies→copy)
/// and the standard -s stripping for regular verbs.
pub(super) fn starts_clause_text_or_conjugated(text: &str) -> bool {
    if starts_clause_text(text) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    let first_word = lower.split_whitespace().next().unwrap_or("");
    // Only attempt deconjugation on words ending in 's' that aren't already
    // recognized — avoids false positives on noun phrases.
    if !first_word.ends_with('s') || first_word.ends_with("ss") {
        return false;
    }
    // Exclude possessive pronouns and determiners that happen to end in 's'
    // but are not conjugated verbs (e.g., "its", "this", "those").
    if matches!(
        first_word,
        "its" | "this" | "those" | "his" | "less" | "plus" | "as"
    ) {
        return false;
    }
    let base = super::normalize_verb_token(first_word);
    if base == first_word {
        return false; // normalize_verb_token didn't change it — not a conjugated verb
    }
    // Reconstruct with the base form and check again.
    let rest = &lower[first_word.len()..];
    let deconjugated = format!("{base}{rest}");
    starts_clause_text_lower(&deconjugated)
}

/// Inner implementation operating on pre-lowercased input.
fn starts_clause_text_lower(s: &str) -> bool {
    if starts_multiword_keyword_continuation(s) {
        return false;
    }

    // Table-driven prefix check via nom tag() — try all imperative verbs and
    // pronoun/determiner clause starters.  Split into multiple alt() groups
    // chained with .or() to stay within nom's 21-tuple limit.
    alt((
        value((), tag::<_, _, VerboseError<&str>>("add ")),
        value((), tag("all ")),
        value((), tag("attach ")),
        value((), tag("airbend ")),
        value((), tag("cast ")),
        value((), tag("counter ")),
        value((), tag("create ")),
        value((), tag("deal ")),
        value((), tag("destroy ")),
        value((), tag("discard ")),
        value((), tag("draw ")),
        value((), tag("earthbend ")),
        value((), tag("each player ")),
        value((), tag("each opponent ")),
        value((), tag("each ")),
        value((), tag("exile ")),
        value((), tag("explore")),
        value((), tag("fight ")),
        value((), tag("flip ")),
        value((), tag("investigate")),
        value((), tag("gain control ")),
    ))
    .or(alt((
        value((), tag("gain ")),
        value((), tag("get ")),
        value((), tag("have ")),
        value((), tag("look at ")),
        value((), tag("lose ")),
        value((), tag("mill ")),
        value((), tag("proliferate")),
        value((), tag("put ")),
        value((), tag("return ")),
        value((), tag("reveal ")),
        value((), tag("roll ")),
        value((), tag("sacrifice ")),
        value((), tag("scry ")),
        value((), tag("search ")),
        value((), tag("shuffle")),
        value((), tag("surveil ")),
        value((), tag("tap ")),
        value((), tag("that ")),
        value((), tag("this ")),
        value((), tag("those ")),
        value((), tag("they ")),
    )))
    .or(alt((
        value((), tag("conjure ")),
        value((), tag("target ")),
        value((), tag("transform ")),
        value((), tag("untap ")),
        value((), tag("you may ")),
        value((), tag("you ")),
        value((), tag("incubate ")),
        value((), tag("it ")),
        value((), tag("copy ")),
        value((), tag("double ")),
        value((), tag("goad ")),
        value((), tag("manifest ")),
        value((), tag("populate")),
        value((), tag("remove ")),
        value((), tag("seek ")),
        value((), tag("connive")),
    )))
    .parse(s)
    .is_ok()
}

fn starts_multiword_keyword_continuation(s: &str) -> bool {
    let Ok((rest, _)) = alt((
        tag::<_, _, VerboseError<&str>>("double strike"),
        tag("double team"),
    ))
    .parse(s) else {
        return false;
    };
    rest.chars()
        .next()
        .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
}

/// CR 603.7a: Check if accumulated clause text begins with a temporal prefix
/// (delayed trigger condition), indicating the clause body should not be split.
/// These prefixes create CreateDelayedTrigger wrappers in parse_effect_chain_impl,
/// and splitting the inner compound effect would leave only the first sub-effect
/// wrapped while the remainder becomes a separate top-level clause.
fn is_inside_temporal_prefix(lower: &str) -> bool {
    // Check the raw accumulated text (which may include a leading comma+space
    // from a prior clause boundary). The temporal prefix starts the clause.
    let trimmed = lower.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
    alt((
        tag::<_, _, VerboseError<&str>>("at the beginning of the next "),
        tag("at the beginning of your next "),
        tag("at the end of "),
    ))
    .parse(trimmed)
    .is_ok()
}

/// Restricted clause-start check for bare " and " splitting (not after comma).
/// Only includes imperative verbs that are unambiguously clause starters —
/// excludes bare pronouns/determiners like "all", "each", "it", "that", "those"
/// which commonly appear in noun phrases after "and"
/// (e.g. "target creature and all other creatures").
///
/// Subject-prefixed verb patterns ("you gain", "you lose", etc.) are safe because
/// "you" + verb is never a noun phrase — it always starts an independent clause.
pub(super) fn starts_bare_and_clause(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    starts_bare_and_clause_lower(&lower)
}

/// Inner implementation operating on pre-lowercased input.
fn starts_bare_and_clause_lower(s: &str) -> bool {
    // Split into multiple alt() groups chained with .or() for nom's tuple limit.
    let has_verb_prefix = alt((
        value((), tag::<_, _, VerboseError<&str>>("create ")),
        value((), tag("destroy ")),
        value((), tag("draw ")),
        value((), tag("discard ")),
        value((), tag("exile ")),
        value((), tag("gain control ")),
        value((), tag("have ")),
        value((), tag("mill ")),
        value((), tag("put ")),
        value((), tag("return ")),
        value((), tag("sacrifice ")),
        value((), tag("scry ")),
        value((), tag("search ")),
        value((), tag("shuffle")),
        value((), tag("surveil ")),
        value((), tag("tap ")),
        value((), tag("untap ")),
        // CR 701.27 + CR 701.28: "transform"/"convert" are imperative game actions.
        // Primal Amulet: "remove those counters and transform it" must split here so
        // each clause reaches the effect dispatcher independently.
        value((), tag("transform ")),
        value((), tag("convert ")),
    ))
    .or(alt((
        // CR 608.2c: Subject-prefixed verb patterns — "you [verb]" is always a clause start.
        value((), tag("you gain ")),
        value((), tag("you lose ")),
        value((), tag("you draw ")),
        value((), tag("you create ")),
        value((), tag("you mill ")),
        value((), tag("you scry ")),
        value((), tag("you put ")),
        value((), tag("you exile ")),
        value((), tag("you return ")),
        value((), tag("you sacrifice ")),
        value((), tag("you search ")),
        value((), tag("you surveil ")),
        value((), tag("you get ")),
        value((), tag("you may ")),
        value((), tag("its controller ")),
        value((), tag("their controller ")),
        // Sword trigger patterns
        value((), tag("you untap ")),
        value((), tag("that player ")),
    )))
    .or(alt((
        // CR 608.2k: "it [conjugated-verb]" is always subject + predicate, never a
        // noun phrase. "doesn't"/"can't"/"cannot" are restriction predicates; "gains"/
        // "gets"/"has" are continuous modification predicates. Safe to split because
        // a bare pronoun followed by a conjugated verb cannot be part of a noun phrase.
        value((), tag::<_, _, VerboseError<&str>>("it doesn't ")),
        value((), tag("it can't ")),
        value((), tag("it cannot ")),
        value((), tag("it gains ")),
        value((), tag("it gets ")),
        value((), tag("it has ")),
        value((), tag("it loses ")),
    )))
    .parse(s)
    .is_ok();
    if has_verb_prefix {
        return true;
    }
    // "gain N <noun>" / "lose N <noun>" — imperative with numeric/X argument
    // (e.g., "gain 3 life", "lose 2 life") is a clause start. Bare "gain
    // <keyword>" / "gain a <keyword>" is a continuous-modification rider on
    // the previous pump clause and must NOT split (Heron's Grace, Sorin
    // Solemn Visitor, Soul of Theros, Jeskai Charm, ~14 cards). Discriminator:
    // the token after the verb must be a count expression (digits or "X"
    // followed by a word boundary), not a keyword name.
    if let Ok((rest, _)) = alt((tag::<_, _, VerboseError<&str>>("gain "), tag("lose "))).parse(s) {
        // Reject conjugated "gains"/"loses" (handled separately above).
        let conjugated = tag::<_, _, VerboseError<&str>>("gains ").parse(s).is_ok()
            || tag::<_, _, VerboseError<&str>>("loses ").parse(s).is_ok();
        if !conjugated && next_token_is_count(rest) {
            return true;
        }
    }
    starts_with_damage_clause(s)
}

/// CR 121.1 / CR 119.1: Returns true when the token immediately following a
/// `gain `/`lose ` prefix is a count expression — i.e. digits, or `X`/`x`
/// terminated by a non-alphanumeric boundary so we don't false-match "x" inside
/// "x-cost" (only `X ` / `X,` / `X.` / end-of-string). Distinguishes imperative
/// "gain 3 life" / "lose X life" from continuous-modification "gain lifelink".
fn next_token_is_count(s: &str) -> bool {
    let trimmed = s.trim_start();
    let first_char = match trimmed.chars().next() {
        Some(c) => c,
        None => return false,
    };
    if first_char.is_ascii_digit() {
        return true;
    }
    if first_char == 'x' || first_char == 'X' {
        let after = &trimmed[first_char.len_utf8()..];
        let next = after.chars().next();
        return next.map(|c| !c.is_alphanumeric()).unwrap_or(true);
    }
    false
}

/// Checks if text starts with a subject-prefixed damage verb.
/// Matches: "it deals N damage", "~ deals N damage", "this creature deals N damage",
/// "that creature deals N damage", bare "deals N damage", etc.
/// Used by `starts_bare_and_clause` to split patterns like
/// "sacrifice ~ and it deals 3 damage to target player".
fn starts_with_damage_clause(lower: &str) -> bool {
    if let Ok((_, before)) = take_until::<_, _, VerboseError<&str>>("deals ")
        .parse(lower)
        .or_else(|_| take_until::<_, _, VerboseError<&str>>("deal ").parse(lower))
    {
        let subject = before.trim();
        subject.is_empty() // bare "deals N damage"
            || subject == "it" // "it deals N damage"
            || subject == "~" // "~ deals N damage"
            || alt((
                tag::<_, _, VerboseError<&str>>("this "),
                tag("that "),
            ))
            .parse(subject)
            .is_ok()
    } else {
        false
    }
}

pub(super) fn is_possessive_apostrophe(current: &str, next: Option<char>) -> bool {
    let prev = current.chars().last();
    matches!(
        (prev, next),
        (Some(prev), Some(next)) if prev.is_alphanumeric() && next.is_alphanumeric()
            || prev == 's' && next.is_whitespace()
    )
}

pub(super) fn push_clause_chunk(
    chunks: &mut Vec<ClauseChunk>,
    raw_text: &str,
    boundary_after: Option<ClauseBoundary>,
) {
    let text = raw_text.trim().trim_end_matches('.').trim();
    if text.is_empty() {
        return;
    }
    chunks.push(ClauseChunk {
        text: text.to_string(),
        boundary_after,
    });
}

pub(super) fn apply_clause_continuation(
    defs: &mut Vec<AbilityDefinition>,
    continuation: ContinuationAst,
    kind: AbilityKind,
) {
    match continuation {
        ContinuationAst::SearchDestination {
            destination,
            enter_tapped,
            reveal,
            attach_to_source,
        } => {
            if let Some(previous) = defs.last_mut() {
                if let Effect::SearchLibrary {
                    reveal: existing_reveal,
                    ..
                } = &mut *previous.effect
                {
                    *existing_reveal |= reveal;
                }
            }
            let mut change_zone = AbilityDefinition::new(
                kind,
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            );
            // CR 303.4f: "attached to [source]" — forward the moved card to an Attach sub_ability
            if attach_to_source {
                change_zone.forward_result = true;
                change_zone.sub_ability = Some(Box::new(AbilityDefinition::new(
                    kind,
                    Effect::Attach {
                        attachment: TargetFilter::SelfRef,
                        target: TargetFilter::Any,
                    },
                )));
            }
            defs.push(change_zone);
        }
        ContinuationAst::RevealHandFilter {
            card_filter,
            choice_optional,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealHand {
                card_filter: existing,
                choice_optional: existing_choice_optional,
                ..
            } = &mut *previous.effect
            {
                *existing = card_filter;
                *existing_choice_optional = choice_optional;
            }
        }
        ContinuationAst::ManaRestriction {
            restriction,
            grants: new_grants,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Mana {
                restrictions,
                grants,
                ..
            } = &mut *previous.effect
            {
                restrictions.push(restriction);
                grants.extend(new_grants);
            }
        }
        ContinuationAst::ManaGrant { grants: new_grants } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Mana { grants, .. } = &mut *previous.effect {
                grants.extend(new_grants);
            }
        }
        ContinuationAst::CounterSourceStatic { source_static } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Counter {
                source_static: existing,
                ..
            } = &mut *previous.effect
            {
                *existing = Some(*source_static);
            }
        }
        ContinuationAst::SuspectLastCreated => {
            defs.push(AbilityDefinition::new(
                kind,
                Effect::Suspect {
                    target: TargetFilter::LastCreated,
                },
            ));
        }
        ContinuationAst::FlashbackCostEqualsManaCost => {}
        ContinuationAst::CantRegenerate => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            match &mut *previous.effect {
                Effect::Destroy {
                    cant_regenerate, ..
                }
                | Effect::DestroyAll {
                    cant_regenerate, ..
                } => {
                    *cant_regenerate = true;
                }
                _ => {}
            }
        }
        ContinuationAst::PutRest {
            destination,
            reorder_all,
        } => {
            // Absorbed into preceding Dig or RevealUntil — sets rest_destination
            // for unchosen/non-matching cards. CR 608.2c: When the preceding def is
            // a conditional "instead" alternative (new def with `else_ability =
            // base_def`), patch BOTH branches so the rest-placement applies whether
            // the condition was true or false.
            let Some(previous) = defs.last_mut() else {
                return;
            };
            patch_rest_destination_recursively(previous, destination, reorder_all);
        }
        ContinuationAst::DigFromAmong {
            count,
            up_to: is_up_to,
            filter: card_filter,
            destination: kept_dest,
            rest_destination: rest_dest,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::Dig {
                keep_count,
                up_to,
                filter,
                destination,
                rest_destination,
                reveal,
                ..
            } = &mut *previous.effect
            {
                *keep_count = Some(count);
                *up_to = is_up_to;
                *filter = card_filter;
                // CR 701.33: When `destination` is None the kept cards are NOT
                // auto-routed by the Dig resolver; downstream sub_abilities
                // read the tracked set and route by type. Also promote the
                // Dig to reveal:true — "from among them" is a reveal-form.
                *destination = kept_dest;
                if kept_dest.is_none() {
                    *reveal = true;
                }
                if let Some(rd) = rest_dest {
                    *rest_destination = Some(rd);
                }
            }
        }
        ContinuationAst::ChooseFromExile { count, chooser } => {
            defs.push(AbilityDefinition::new(
                kind,
                Effect::ChooseFromZone {
                    count,
                    zone: Zone::Exile,
                    chooser,
                    up_to: false,
                    constraint: None,
                },
            ));
        }
        ContinuationAst::SearchResultClauseHandled => {}
        ContinuationAst::PutChoiceRemainderOnBottom => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            let bottom_def = AbilityDefinition::new(
                kind,
                Effect::PutAtLibraryPosition {
                    target: TargetFilter::Any,
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    position: crate::types::ability::LibraryPosition::Bottom,
                },
            );
            // Walk into the sub_ability chain to find the right attachment point.
            // For ChooseFromZone, the sub_ability is ChangeZone(Library→Hand) and we
            // attach the bottom-placement as *its* sub_ability (unchosen targets flow there).
            // For a bare ChangeZone(Library→Hand), attach directly.
            let target_def = if matches!(&*previous.effect, Effect::ChooseFromZone { .. }) {
                previous.sub_ability.as_deref_mut()
            } else {
                Some(previous)
            };
            if let Some(def) = target_def {
                if matches!(
                    &*def.effect,
                    Effect::ChangeZone {
                        origin: Some(Zone::Library),
                        destination: Zone::Hand,
                        ..
                    }
                ) && def.sub_ability.is_none()
                {
                    def.sub_ability = Some(Box::new(bottom_def));
                }
            }
        }
        ContinuationAst::EntersTappedAttacking => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            // CR 508.4 / CR 614.1: Patch the preceding effect to enter tapped and attacking.
            match &mut *previous.effect {
                Effect::CopyTokenOf {
                    enters_attacking,
                    tapped,
                    ..
                } => {
                    *enters_attacking = true;
                    *tapped = true;
                }
                Effect::Token {
                    enters_attacking,
                    tapped,
                    ..
                } => {
                    *enters_attacking = true;
                    *tapped = true;
                }
                Effect::ChangeZone {
                    enters_attacking,
                    enter_tapped,
                    ..
                } => {
                    *enters_attacking = true;
                    *enter_tapped = true;
                }
                _ => {}
            }
        }
        ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            // CR 122.6a: Patch the preceding Token effect to enter with counters.
            if let Effect::Token {
                enter_with_counters,
                ..
            } = &mut *previous.effect
            {
                enter_with_counters.push((counter_type, count));
            }
        }
        ContinuationAst::RevealUntilKept {
            destination,
            enter_tapped: tapped,
            rest_destination: rest_dest,
        } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealUntil {
                kept_destination,
                enter_tapped,
                rest_destination,
                ..
            } = &mut *previous.effect
            {
                *kept_destination = destination;
                *enter_tapped = tapped;
                if let Some(rest) = rest_dest {
                    *rest_destination = rest;
                }
            }
        }
        ContinuationAst::GrantExtraTurnAfterControlledTurn => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::ControlNextTurn {
                grant_extra_turn_after,
                ..
            } = &mut *previous.effect
            {
                *grant_extra_turn_after = true;
            }
        }
        // CR 701.20a: "puts those cards into [zone]" — both the matching card and
        // the non-matching cards go to the same zone.
        ContinuationAst::RevealUntilAllToZone { destination } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealUntil {
                kept_destination,
                rest_destination,
                ..
            } = &mut *previous.effect
            {
                *kept_destination = destination;
                *rest_destination = destination;
            }
        }
    }
}

/// Recursively patch `rest_destination` on Dig/RevealUntil effects reachable from
/// `def` via `else_ability`. CR 608.2c: When a preceding def is a conditional
/// "instead" wrapper (new_def with `else_ability = base_def`), a trailing
/// "Put the rest on the bottom..." clause applies to both the alternative and
/// base branches — neither branch is selected until resolution.
fn patch_rest_destination_recursively(
    def: &mut AbilityDefinition,
    destination: Zone,
    reorder_all: bool,
) {
    match &mut *def.effect {
        Effect::Dig {
            destination: kept_destination,
            rest_destination,
            ..
        } => {
            if reorder_all {
                *kept_destination = Some(Zone::Library);
                *rest_destination = Some(Zone::Library);
            } else if rest_destination.is_none() {
                *rest_destination = Some(destination);
            }
        }
        Effect::RevealUntil {
            rest_destination, ..
        } => {
            *rest_destination = destination;
        }
        _ => {}
    }
    if let Some(else_def) = def.else_ability.as_deref_mut() {
        patch_rest_destination_recursively(else_def, destination, reorder_all);
    }
}

pub(super) fn continuation_absorbs_current(
    continuation: &ContinuationAst,
    current_effect: &Effect,
) -> bool {
    match continuation {
        ContinuationAst::RevealHandFilter { .. } => {
            matches!(current_effect, Effect::RevealHand { .. })
        }
        ContinuationAst::ManaRestriction { .. }
        | ContinuationAst::ManaGrant { .. }
        | ContinuationAst::CounterSourceStatic { .. } => true,
        ContinuationAst::FlashbackCostEqualsManaCost => true,
        ContinuationAst::SearchDestination { .. } => false,
        ContinuationAst::SuspectLastCreated => matches!(current_effect, Effect::Suspect { .. }),
        ContinuationAst::CantRegenerate => true,
        ContinuationAst::PutRest { .. } => true,
        ContinuationAst::ChooseFromExile { .. } => true,
        ContinuationAst::SearchResultClauseHandled => true,
        ContinuationAst::PutChoiceRemainderOnBottom => true,
        ContinuationAst::EntersTappedAttacking => true,
        ContinuationAst::TokenEntersWithCounters { .. } => true,
        ContinuationAst::DigFromAmong { .. } => true,
        ContinuationAst::GrantExtraTurnAfterControlledTurn => true,
        ContinuationAst::RevealUntilKept { .. } => true,
        ContinuationAst::RevealUntilAllToZone { .. } => true,
    }
}

pub(super) fn parse_intrinsic_continuation_ast(
    text: &str,
    effect: &Effect,
    full_text: &str,
) -> Option<ContinuationAst> {
    match effect {
        Effect::SearchLibrary { .. } => {
            let full_lower = full_text.to_ascii_lowercase();
            // CR 701.24b: If later clauses contain "put on top", suppress the default
            // ChangeZone(→Hand) — the card stays in the library and a separate
            // PutAtLibraryPosition effect in the chain handles placement.
            // Also suppress for "Nth from the top" (Long-Term Plans, etc.)
            let has_positional_put =
                nom_primitives::scan_contains(&full_lower, "put that card on top")
                    || nom_primitives::scan_contains(&full_lower, "put it on top")
                    || nom_primitives::scan_contains(&full_lower, "put the card on top")
                    || nom_primitives::scan_contains(&full_lower, "put them on top")
                    || (nom_primitives::scan_contains(&full_lower, "put that card")
                        && nom_primitives::scan_contains(&full_lower, "from the top"));
            if has_positional_put {
                return None;
            }
            let lower = text.to_lowercase();
            let attach_to_source = nom_primitives::scan_contains(&full_lower, "attached to")
                || nom_primitives::scan_contains(&lower, "attached to");
            // CR 701.23a + CR 701.18a: Scan "onto the battlefield tapped" across
            // the whole sentence (full_lower) so the destination compound's
            // "enters tapped" modifier is detected even when the put-step is
            // in a sibling clause (Assassin's Trophy-style split).
            let enter_tapped = nom_primitives::scan_contains(&full_lower, "battlefield tapped");
            let reveal = nom_primitives::scan_contains(&lower, "reveal")
                || nom_primitives::scan_contains(&full_lower, "reveal that card")
                || nom_primitives::scan_contains(&full_lower, "reveal it");
            // Safety net: verify the clause splitter correctly separated all boundaries.
            // If this fires, a verb is missing from starts_clause_text() or the splitter's
            // search_start guard is incorrectly suppressing a split.
            // CR 701.18a: Shuffle clauses are part of the search compound action —
            // both "shuffle" and "that player shuffles" are valid terminators.
            #[cfg(debug_assertions)]
            if let Some(then_pos) = lower.rfind(", then ") {
                let after_then = lower[then_pos + ", then ".len()..].trim_end_matches('.');
                let is_shuffle_clause = alt((
                    value((), tag::<_, _, VerboseError<&str>>("shuffle")),
                    value((), tag("that player shuffles")),
                ))
                .parse(after_then)
                .is_ok();
                if !is_shuffle_clause {
                    debug_assert!(
                        !starts_clause_text(after_then),
                        "Unsplit clause boundary in SearchLibrary continuation: \
                         ', then {}' — check starts_clause_text() for missing verb",
                        after_then,
                    );
                }
            }
            // CR 701.23a + CR 701.18a: The "put [it] onto the battlefield" /
            // "put [it] into your hand" destination clause for a library search
            // compound lives in the same sentence as the search, but may have
            // been split into a subsequent chunk by the comma-splitter (e.g.,
            // "search their library for a basic land card, put it onto the
            // battlefield, then shuffle"). Use full_lower so we scan across the
            // whole sentence rather than only the chunk containing "search".
            Some(ContinuationAst::SearchDestination {
                destination: super::parse_search_destination(&full_lower),
                enter_tapped,
                reveal,
                attach_to_source,
            })
        }
        _ => None,
    }
}

/// CR 701.20e + CR 608.2c: Parse "put up to N [filter] from among them/those cards onto the
/// battlefield / into your hand" into a DigFromAmong continuation that patches the preceding
/// Dig effect. The player follows the Oracle text instructions in written order (CR 608.2c).
///
/// Also handles "put N of them into your hand [and the rest on the bottom]" — the simpler
/// form used by Impulse, Stock Up, Dig Through Time, etc. where no filter is specified.
///
/// Examples:
/// - "put up to two creature cards with mana value 3 or less from among them onto the battlefield"
/// - "put a creature card from among them into your hand"
/// - "you may reveal a creature card from among them and put it into your hand"
/// - "put two of them into your hand and the rest on the bottom of your library in any order"
pub(super) fn parse_dig_from_among(lower: &str, _original: &str) -> Option<ContinuationAst> {
    // Determine kept-cards destination. `None` is the reveal-only form (Zimone's
    // Experiment): "reveal up to N <filter> cards from among them, then put the
    // rest on the bottom" — the kept cards are NOT auto-routed; subsequent
    // sub_abilities route them by type via `TargetFilter::TrackedSetFiltered`.
    let destination = if nom_primitives::scan_contains(lower, "onto the battlefield") {
        Some(Zone::Battlefield)
    } else if nom_primitives::scan_contains(lower, "into your hand")
        || nom_primitives::scan_contains(lower, "into their hand")
    {
        Some(Zone::Hand)
    } else {
        None
    };

    // "put N of them into your hand [and the rest on the bottom]" — no filter, count explicit.
    // Must be checked BEFORE the "from among" path since "of them" appears in both forms.
    if let Ok((_, before_of)) = take_until::<_, _, VerboseError<&str>>(" of them").parse(lower) {
        let before_of = before_of.trim();
        let after_put = alt((tag::<_, _, VerboseError<&str>>("you may put "), tag("put ")))
            .parse(before_of)
            .map(|(rest, _)| rest)
            .unwrap_or(before_of);

        // Delegate to nom combinator (input already lowercase from lower).
        let (count, up_to) =
            if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("up to ").parse(after_put) {
                nom_primitives::parse_number
                    .parse(rest)
                    .map_or((1, true), |(_, n)| (n, true))
            } else if let Ok((_, n)) = nom_primitives::parse_number.parse(after_put) {
                (n, false)
            } else {
                // "a/an" or unrecognized → treat as up_to 1
                (1, true)
            };

        // Detect rest destination from "and the rest on the bottom/into graveyard" suffix.
        let rest_destination = parse_of_them_rest_destination(lower);

        return Some(ContinuationAst::DigFromAmong {
            count,
            up_to,
            filter: TargetFilter::Any,
            destination,
            rest_destination,
        });
    }

    // Find "from among" to split the text into count+filter vs destination
    let (_, before_from) = take_until::<_, _, VerboseError<&str>>("from among")
        .parse(lower)
        .ok()?;
    let before_from = &before_from.trim();

    // Strip leading "put " or "you may reveal " using nom combinators.
    let after_put = alt((
        tag::<_, _, VerboseError<&str>>("you may put "),
        tag("you may reveal "),
        tag("put "),
        tag("reveal "),
    ))
    .parse(*before_from)
    .map(|(rest, _)| rest)
    .unwrap_or(before_from);

    // Parse "up to N" or "a/an" or just a number
    // Delegate to nom combinator (input already lowercase from lower).
    let (count, up_to, filter_text) = if let Ok((rest, _)) =
        tag::<_, _, VerboseError<&str>>("up to ").parse(after_put)
    {
        if let Ok((remainder, n)) = nom_primitives::parse_number.parse(rest) {
            (n, true, remainder.trim())
        } else {
            (1, true, rest)
        }
    } else if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("any number of ").parse(after_put)
    {
        // "any number of creatures" → up_to with a high cap
        (255, true, rest)
    } else if let Ok((rest, _)) = nom_primitives::parse_article.parse(after_put) {
        // "a creature card" / "an artifact card" — up_to 1 (player may choose none)
        (1, true, rest)
    } else if let Ok((remainder, n)) = nom_primitives::parse_number.parse(after_put) {
        // Explicit numeric count: "two creature cards" → exactly 2
        (n, false, remainder.trim())
    } else {
        (1, true, after_put)
    };

    // Parse the filter from the remaining text (e.g., "creature cards with mana value 3 or less")
    let filter = if filter_text.is_empty()
        || filter_text == "card"
        || filter_text == "cards"
        || filter_text == "of them"
    {
        TargetFilter::Any
    } else {
        let (parsed_filter, _) = parse_target(filter_text);
        parsed_filter
    };

    Some(ContinuationAst::DigFromAmong {
        count,
        up_to,
        filter,
        destination,
        rest_destination: None, // rest_destination handled by subsequent PutRest continuation
    })
}

/// Extract rest_destination from "put N of them into your hand and the rest/the other on the bottom/graveyard".
/// Returns None if neither "and the rest" nor "and the other" anaphor is present.
///
/// CR 401.1 + CR 401.4: "the rest" / "the other" both refer to the un-chosen
/// remainder of the looked-at pile. The grammatical difference is purely a
/// count distinction — "the other" is used when exactly one card remains
/// (the count=2-keep=1 form, e.g. Sleight of Hand, Sea Gate Oracle); "the
/// rest" generalizes to any remainder count. Both anaphors point to the same
/// rest_destination semantics, so they share the same downstream zone
/// classification.
fn parse_of_them_rest_destination(lower: &str) -> Option<Zone> {
    let (_, (_, after_rest)) = nom_primitives::split_once_on(lower, " and the rest")
        .or_else(|_| nom_primitives::split_once_on(lower, " and the other"))
        .ok()?;
    if contains_possessive(after_rest, "into", "graveyard") {
        Some(Zone::Graveyard)
    } else if contains_possessive(after_rest, "into", "hand") {
        Some(Zone::Hand)
    } else {
        // Default: bottom of library ("on the bottom", "in any order", etc.)
        Some(Zone::Library)
    }
}

pub(super) fn parse_followup_continuation_ast(
    text: &str,
    previous_effect: &Effect,
) -> Option<ContinuationAst> {
    let lower = text.to_lowercase();

    match previous_effect {
        Effect::RevealHand { .. }
            if nom_primitives::scan_contains(&lower, "card from it")
                || nom_primitives::scan_contains(&lower, "card from among")
                || nom_primitives::scan_contains(&lower, "one of them")
                || nom_primitives::scan_contains(&lower, "one of those") =>
        {
            let card_filter = if alt((
                tag::<_, _, VerboseError<&str>>("you choose "),
                tag("choose "),
            ))
            .parse(lower.as_str())
            .is_ok()
            {
                super::parse_choose_filter(&lower)
            } else {
                super::parse_choose_filter_from_sentence(&lower)
            };
            let choice_optional = alt((
                tag::<_, _, VerboseError<&str>>("you may choose "),
                tag("may choose "),
            ))
            .parse(lower.as_str())
            .is_ok();
            Some(ContinuationAst::RevealHandFilter {
                card_filter,
                choice_optional,
            })
        }
        Effect::Mana { .. } => {
            if let Some((restriction, grants)) = super::mana::parse_mana_spend_restriction(&lower) {
                return Some(ContinuationAst::ManaRestriction {
                    restriction,
                    grants,
                });
            }
            // CR 106.6: "that spell can't be countered" as a standalone clause
            // after comma-splitting from the restriction text.
            if let Some(grants) = super::mana::parse_mana_spell_grant(&lower) {
                return Some(ContinuationAst::ManaGrant { grants });
            }
            None
        }
        Effect::GenericEffect {
            static_abilities, ..
        } if lower == "the flashback cost is equal to its mana cost"
            && static_abilities.iter().any(|def| {
                def.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        crate::types::ability::ContinuousModification::AddKeyword {
                            keyword: crate::types::keywords::Keyword::Flashback(_)
                        }
                    )
                })
            }) =>
        {
            Some(ContinuationAst::FlashbackCostEqualsManaCost)
        }
        Effect::Counter { .. }
            if nom_primitives::scan_contains(&lower, "countered this way")
                && nom_primitives::scan_contains(&lower, "loses all abilities") =>
        {
            Some(ContinuationAst::CounterSourceStatic {
                source_static: Box::new(StaticDefinition::continuous().modifications(vec![
                    crate::types::ability::ContinuousModification::RemoveAllAbilities,
                ])),
            })
        }
        // CR 201.2 + CR 608.2c: "[You may] put one of those cards onto the
        // battlefield if it has the same name as a permanent" after Dig —
        // Mitotic-Manipulation-style name-match selection. Patches the
        // preceding Dig with destination=Battlefield, keep_count=1, up_to=true
        // (always optional — "may" or implicit "if"), and a filter that
        // restricts selectable cards to those sharing a name with any
        // permanent currently on the battlefield.
        Effect::Dig { .. }
            if (nom_primitives::scan_contains(&lower, "one of those cards")
                || nom_primitives::scan_contains(&lower, "one of them"))
                && nom_primitives::scan_contains(&lower, "onto the battlefield")
                && (nom_primitives::scan_contains(&lower, "the same name as a permanent")
                    || nom_primitives::scan_contains(&lower, "shares a name with a permanent")) =>
        {
            use crate::types::ability::{FilterProp, TypedFilter};
            Some(ContinuationAst::DigFromAmong {
                count: 1,
                up_to: true,
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::NameMatchesAnyPermanent { controller: None },
                ])),
                destination: Some(Zone::Battlefield),
                rest_destination: None,
            })
        }
        // "put them back in any order" after Dig means all looked-at cards
        // stay in the library and the player's submitted order becomes the
        // new top order. Leave keep_count unset so runtime resolves dynamic N.
        Effect::Dig { .. } if parse_put_all_back_in_any_order(&lower) => {
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: true,
            })
        }
        // "put the rest on the bottom" / "put those cards into your graveyard"
        // after Dig — sets rest_destination on the preceding Dig effect.
        Effect::Dig { .. }
            if nom_primitives::scan_contains(&lower, "put them back")
                || nom_primitives::scan_contains(&lower, "put the rest")
                || nom_primitives::scan_contains(&lower, "put those cards") =>
        {
            let destination = if nom_primitives::scan_contains(&lower, "into your graveyard")
                || nom_primitives::scan_contains(&lower, "into their graveyard")
            {
                Zone::Graveyard
            } else if nom_primitives::scan_contains(&lower, "into your hand")
                || nom_primitives::scan_contains(&lower, "into their hand")
            {
                Zone::Hand
            } else {
                // Default: bottom of library (covers "on the bottom", "back in any order", etc.)
                Zone::Library
            };
            Some(ContinuationAst::PutRest {
                destination,
                reorder_all: false,
            })
        }
        // CR 701.20a: "put that card into your hand / onto the battlefield" after RevealUntil
        // — overrides kept_destination. Also extracts rest_destination when the compound
        // sentence includes "and the rest" (the "and" split is suppressed because "the rest"
        // does not start with a recognized imperative verb). Both bare imperative
        // ("put that card", second-person reveal-until) and third-person ("the player puts
        // that card", Polymorph / Proteus Staff / Transmogrify) forms are accepted.
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "put that card")
                || nom_primitives::scan_contains(&lower, "puts that card")
                || nom_primitives::scan_contains(&lower, "put it")
                || nom_primitives::scan_contains(&lower, "puts it") =>
        {
            let (destination, enter_tapped) =
                if nom_primitives::scan_contains(&lower, "onto the battlefield") {
                    let tapped = nom_primitives::scan_contains(&lower, "tapped");
                    (Zone::Battlefield, tapped)
                } else {
                    // Default "into your hand"
                    (Zone::Hand, false)
                };
            // Also extract rest_destination from compound "and the rest [into zone]"
            let rest = if nom_primitives::scan_contains(&lower, "the rest") {
                if nom_primitives::scan_contains(&lower, "into your graveyard")
                    || nom_primitives::scan_contains(&lower, "into their graveyard")
                {
                    Some(Zone::Graveyard)
                } else {
                    // "on the bottom of your library in a random order" or similar
                    Some(Zone::Library)
                }
            } else {
                None
            };
            Some(ContinuationAst::RevealUntilKept {
                destination,
                enter_tapped,
                rest_destination: rest,
            })
        }
        // CR 701.20a: "put the rest" / "the rest on the bottom" / "put the revealed cards"
        // after RevealUntil — overrides rest_destination. The "the rest" without "put"
        // occurs when split_clause_sequence splits "put X and the rest" on "and".
        // Also recognizes:
        //   • "shuffles ... revealed this way into <possessive> library" (Polymorph,
        //     Transmogrify) — the engine's existing rest=Library destination already
        //     random-orders, satisfying the shuffle semantics.
        //   • Third-person "puts" verb form (Polymorph chain).
        // CR 701.20a: "puts those cards into [zone]" / "put those cards into [zone]"
        // after RevealUntil — the entire revealed pile (matching card + everything
        // revealed before it) goes to the same zone. Checked before the PutRest arm
        // because "those cards" is a distinct semantic from "the rest" and must
        // override both kept_destination and rest_destination. Used by Balustrade
        // Spy, Consuming Aberration, Destroy the Evidence, Undercity Informer.
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "puts those cards")
                || nom_primitives::scan_contains(&lower, "put those cards") =>
        {
            let destination = if nom_primitives::scan_contains(&lower, "into your graveyard")
                || nom_primitives::scan_contains(&lower, "into their graveyard")
            {
                Zone::Graveyard
            } else if nom_primitives::scan_contains(&lower, "into exile")
                || nom_primitives::scan_contains(&lower, "on the bottom")
            {
                Zone::Library
            } else {
                Zone::Graveyard
            };
            Some(ContinuationAst::RevealUntilAllToZone { destination })
        }
        //   • "put the revealed cards" / "put them back" after RevealUntil — the
        //     revealed pile's destination override for the non-matching cards only.
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "put the rest")
                || nom_primitives::scan_contains(&lower, "puts the rest")
                || nom_primitives::scan_contains(&lower, "the rest on the bottom")
                || nom_primitives::scan_contains(&lower, "the rest into your graveyard")
                || nom_primitives::scan_contains(&lower, "put the revealed cards")
                || nom_primitives::scan_contains(&lower, "put them back")
                || (nom_primitives::scan_contains(&lower, "shuffle")
                    && nom_primitives::scan_contains(&lower, "library")) =>
        {
            let destination = if nom_primitives::scan_contains(&lower, "into your graveyard")
                || nom_primitives::scan_contains(&lower, "into their graveyard")
            {
                Zone::Graveyard
            } else {
                // Default: bottom of library (covers "shuffles into their library",
                // "on the bottom", and the no-zone "put the rest" variant)
                Zone::Library
            };
            Some(ContinuationAst::PutRest {
                destination,
                reorder_all: false,
            })
        }
        // "create a ... token and suspect it" → chain suspect on last created token
        Effect::Token { .. }
            if tag::<_, _, VerboseError<&str>>("suspect ")
                .parse(lower.as_str())
                .is_ok() =>
        {
            Some(ContinuationAst::SuspectLastCreated)
        }
        // CR 701.19c + CR 608.2c: "It can't be regenerated" prevents regeneration shields;
        // later text modifies the preceding Destroy instruction per CR 608.2c.
        Effect::Destroy { .. } | Effect::DestroyAll { .. }
            if nom_primitives::scan_contains(&lower, "can't be regenerated")
                || nom_primitives::scan_contains(&lower, "cannot be regenerated") =>
        {
            Some(ContinuationAst::CantRegenerate)
        }
        Effect::ChooseFromZone { .. }
            if lower == "put the rest on the bottom of your library in a random order"
                || lower == "put the rest on the bottom of your library in any order"
                || lower == "put the rest on the bottom of your library" =>
        {
            Some(ContinuationAst::PutChoiceRemainderOnBottom)
        }
        // CR 700.2: "Choose/You choose/An opponent chooses/Target opponent chooses one/two/N
        // of them/those" after ChangeZone, ExileTop, RevealTop, or RevealHand →
        // ChooseFromZone building block
        Effect::ChangeZone { .. }
        | Effect::ExileTop { .. }
        | Effect::RevealTop { .. }
        | Effect::RevealHand { .. }
            if (nom_primitives::scan_contains(&lower, "of them")
                || nom_primitives::scan_contains(&lower, "of those"))
                && alt((
                    tag::<_, _, VerboseError<&str>>("choose "),
                    tag("you choose "),
                    tag("an opponent chooses "),
                    tag("target opponent chooses "),
                ))
                .parse(lower.as_str())
                .is_ok() =>
        {
            let count = parse_choose_count_from_text(&lower);
            let chooser = if alt((
                tag::<_, _, VerboseError<&str>>("an opponent chooses "),
                tag("target opponent chooses "),
            ))
            .parse(lower.as_str())
            .is_ok()
            {
                Chooser::Opponent
            } else {
                Chooser::Controller
            };
            Some(ContinuationAst::ChooseFromExile { count, chooser })
        }
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            ..
        } if matches!(
            lower.trim(),
            "reveal that card"
                | "reveal those cards"
                | "reveal the card"
                | "reveal them"
                | "reveal it"
                | "put that card into your hand"
                | "put it into your hand"
        ) =>
        {
            Some(ContinuationAst::SearchResultClauseHandled)
        }
        // CR 701.23a + CR 701.18a: When the preceding SearchDestination
        // continuation already moved the found card onto the battlefield
        // (e.g., Assassin's Trophy / Ranging Raptors / Harrow compound), the
        // explicit "put it onto the battlefield" chunk in the same sentence is
        // a paraphrase and must be absorbed to avoid a duplicate ChangeZone.
        //
        // CR 701.23i + CR 609.3: Iterated-search variants (Winds of Abandon class)
        // surface a plural subject ("those players put those cards onto the
        // battlefield tapped") because the search step has `repeat_for:
        // TrackedSetSize`. The compound has already been folded by the
        // SearchDestination intrinsic continuation; the standalone restatement
        // here would duplicate the ChangeZone if not absorbed. Use a structural
        // prefix-strip on the player-subject so all (subject × pronoun × tapped)
        // permutations match without N! enumerated arms.
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            ..
        } if {
            let bare = strip_search_result_subject(lower.trim().trim_end_matches('.'));
            matches!(
                bare,
                "put that card onto the battlefield"
                    | "put it onto the battlefield"
                    | "put them onto the battlefield"
                    | "put those cards onto the battlefield"
                    | "put that card onto the battlefield tapped"
                    | "put it onto the battlefield tapped"
                    | "put them onto the battlefield tapped"
                    | "put those cards onto the battlefield tapped"
            )
        } =>
        {
            Some(ContinuationAst::SearchResultClauseHandled)
        }
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            ..
        } if lower == "put the rest on the bottom of your library in a random order"
            || lower == "put the rest on the bottom of your library in any order"
            || lower == "put the rest on the bottom of your library" =>
        {
            Some(ContinuationAst::PutChoiceRemainderOnBottom)
        }
        // "Put up to N [filter] from among them/those cards onto the battlefield/into your hand"
        // and "put N of them into your hand [and the rest on the bottom]"
        // after Dig — patches keep_count, filter, destination on the preceding Dig effect.
        Effect::Dig { .. }
            if (nom_primitives::scan_contains(&lower, "from among them")
                || nom_primitives::scan_contains(&lower, "from among those cards")
                || nom_primitives::scan_contains(&lower, "of them"))
                && (nom_primitives::scan_contains(&lower, "onto the battlefield")
                    || nom_primitives::scan_contains(&lower, "into your hand")
                    || nom_primitives::scan_contains(&lower, "into their hand")) =>
        {
            parse_dig_from_among(&lower, text)
        }
        // CR 701.33: "[You may] reveal [up to] N <filter> cards from among
        // them" after Dig — the reveal-only form where the kept cards are NOT
        // immediately routed to a fixed destination. Used by cards like
        // Zimone's Experiment where subsequent sub_abilities route the
        // revealed cards by type via `TargetFilter::TrackedSetFiltered`. The
        // Dig resolver populates a tracked set with the kept cards;
        // downstream effects consume that set.
        //
        // The guard is `from among` + `reveal` without any inline destination
        // phrase — if the clause carried its own destination, the previous
        // arm (with inline-destination requirement) would have matched first.
        Effect::Dig { .. }
            if nom_primitives::scan_contains(&lower, "reveal")
                && (nom_primitives::scan_contains(&lower, "from among them")
                    || nom_primitives::scan_contains(&lower, "from among those cards"))
                && !nom_primitives::scan_contains(&lower, "onto the battlefield")
                && !nom_primitives::scan_contains(&lower, "into your hand")
                && !nom_primitives::scan_contains(&lower, "into their hand") =>
        {
            parse_dig_from_among(&lower, text)
        }
        // CR 508.4 / CR 614.1: "It/The token enters tapped and attacking" (singular)
        // or "They/Those tokens enter tapped and attacking" (plural)
        // after CopyTokenOf, Token, or ChangeZone effects.
        Effect::CopyTokenOf { .. } | Effect::Token { .. } | Effect::ChangeZone { .. }
            if nom_primitives::scan_contains(&lower, "enters tapped and attacking")
                || nom_primitives::scan_contains(&lower, "enter tapped and attacking") =>
        {
            Some(ContinuationAst::EntersTappedAttacking)
        }
        Effect::ControlNextTurn { .. }
            if nom_primitives::scan_contains(&lower, "after that turn")
                && nom_primitives::scan_contains(&lower, "takes an extra turn") =>
        {
            Some(ContinuationAst::GrantExtraTurnAfterControlledTurn)
        }
        // CR 122.6a + CR 614.1c: Token enters-with-counters continuation. Two forms:
        //   * Declarative: "The token enters with X +1/+1 counters on it[, where X is ...]"
        //     or "It enters with X +1/+1 counters on it[, where X is ...]"
        //   * Imperative followup: "and put N [type] counter(s) on it"
        //     after a `create a [token]` clause (G'raha Tia, Fractal Anomaly,
        //     Fractal Tender, Berta — class of "create token ... and put
        //     counter on it" where "it" is the just-created token).
        // Both lift onto the preceding Token effect's `enter_with_counters`
        // so counters apply as the token enters (CR 614.1c replacement)
        // rather than as a post-ETB PutCounter effect that would mistakenly
        // target the source ability via `SelfRef`/`ParentTarget`.
        Effect::Token { .. } => try_parse_token_enters_with_counters(&lower)
            .or_else(|| try_parse_put_counters_on_token_followup(&lower)),
        _ => None,
    }
}

/// CR 122.6a: Parse "the token/it enters with X [counter type] counter(s) on it[, where X is ...]".
/// Returns `TokenEntersWithCounters` continuation on success.
fn try_parse_token_enters_with_counters(lower: &str) -> Option<ContinuationAst> {
    // Match subject prefix: "the token enters with " / "it enters with "
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("the token enters with "),
        tag("it enters with "),
    ))
    .parse(lower)
    .ok()?;

    // Parse count: could be "x", a number, or "a number of"
    let (rest, count_prefix) = alt((
        // "x " — variable resolved later via "where X is"
        value(None, tag::<_, _, VerboseError<&str>>("x ")),
        // "a number of " — dynamic count resolved via suffix
        value(None, tag("a number of ")),
    ))
    .parse(rest)
    .unwrap_or_else(|_: nom::Err<VerboseError<&str>>| {
        // Try parsing a fixed number
        if let Ok((r, n)) = nom_primitives::parse_number(rest) {
            let r = r.trim_start();
            (r, Some(n))
        } else {
            (rest, None)
        }
    });

    // Parse counter type: "+1/+1 " is the most common
    let (rest, counter_type) = alt((
        value(
            "P1P1".to_string(),
            tag::<_, _, VerboseError<&str>>("+1/+1 "),
        ),
        value("M1M1".to_string(), tag("-1/-1 ")),
    ))
    .parse(rest)
    .ok()?;

    // Consume "counter(s) on it"
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("counters on it"),
        tag("counter on it"),
    ))
    .parse(rest)
    .ok()?;

    // Parse optional ", where x is [quantity]"
    let quantity = if let Ok((rest_where, _)) =
        tag::<_, _, VerboseError<&str>>(", where x is ").parse(rest.trim_start_matches(['.', ' ']))
    {
        let qty_text = rest_where.trim().trim_end_matches('.');
        parse_cda_quantity(qty_text)
            .or_else(|| parse_quantity_ref(qty_text).map(|q| QuantityExpr::Ref { qty: q }))
    } else if let Ok((rest_equal, _)) =
        tag::<_, _, VerboseError<&str>>("equal to ").parse(rest.trim_start_matches(['.', ' ']))
    {
        let qty_text = rest_equal.trim().trim_end_matches('.');
        parse_cda_quantity(qty_text)
            .or_else(|| parse_quantity_ref(qty_text).map(|q| QuantityExpr::Ref { qty: q }))
    } else {
        None
    };

    let count = if let Some(qty) = quantity {
        qty
    } else if let Some(n) = count_prefix {
        QuantityExpr::Fixed { value: n as i32 }
    } else {
        // X without "where X is" — variable resolved from spell payment at runtime
        QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }
    };

    Some(ContinuationAst::TokenEntersWithCounters {
        counter_type,
        count,
    })
}

/// CR 122.6a + CR 614.1c: Parse the imperative followup form
/// "put N [counter type] counter(s) on it[, where X is ...]" that follows a
/// `create a [token]` clause. "It" refers to the just-created token; the
/// counters must be lifted onto `Token.enter_with_counters` so they apply as
/// the token enters the battlefield (CR 122.6a) rather than as a post-ETB
/// PutCounter effect targeting the ability source.
///
/// Mirrors `try_parse_token_enters_with_counters` but matches the imperative
/// "put ..." prefix produced by clause-splitting on " and ". Returns
/// `TokenEntersWithCounters` so it shares the same continuation absorption.
fn try_parse_put_counters_on_token_followup(lower: &str) -> Option<ContinuationAst> {
    // Optional leading "and " (rare — usually consumed by the splitter),
    // then the imperative "put " verb.
    let (rest, _) = nom::combinator::opt(tag::<_, _, VerboseError<&str>>("and "))
        .parse(lower)
        .ok()?;
    let (rest, _) = tag::<_, _, VerboseError<&str>>("put ").parse(rest).ok()?;

    // Parse count: "x ", "a ", "an ", "a number of ", or a literal number.
    // Word "a"/"an" is a singular article (count = 1).
    let (rest, count_prefix) = alt((
        // "x " — variable resolved later via "where X is" or by caller payment
        value(None, tag::<_, _, VerboseError<&str>>("x ")),
        value(None, tag("a number of ")),
        value(Some(1u32), tag("a ")),
        value(Some(1u32), tag("an ")),
    ))
    .parse(rest)
    .unwrap_or_else(|_: nom::Err<VerboseError<&str>>| {
        if let Ok((r, n)) = nom_primitives::parse_number(rest) {
            (r.trim_start(), Some(n))
        } else {
            (rest, None)
        }
    });

    // Parse counter type: only +1/+1 and -1/-1 are common in token contexts
    // (matches the AST scope of the existing enters-with-counters helper).
    let (rest, counter_type) = alt((
        value(
            "P1P1".to_string(),
            tag::<_, _, VerboseError<&str>>("+1/+1 "),
        ),
        value("M1M1".to_string(), tag("-1/-1 ")),
    ))
    .parse(rest)
    .ok()?;

    // Consume "counter(s) on it" — the "on it" anaphor pinning the counters
    // to the just-created token.
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("counters on it"),
        tag("counter on it"),
    ))
    .parse(rest)
    .ok()?;

    // Optional ", where x is [quantity]" suffix (Fractal Anomaly). The
    // followup clause is already trimmed by the splitter, so no leading
    // punctuation cleanup is needed before the comma.
    let quantity =
        if let Ok((rest_where, _)) = tag::<_, _, VerboseError<&str>>(", where x is ").parse(rest) {
            // allow-noncombinator: trailing-period cleanup on a pre-tokenized
            // suffix; not parsing dispatch.
            let qty_text = rest_where.trim().trim_end_matches('.');
            parse_cda_quantity(qty_text)
                .or_else(|| parse_quantity_ref(qty_text).map(|q| QuantityExpr::Ref { qty: q }))
        } else {
            None
        };

    let count = if let Some(qty) = quantity {
        qty
    } else if let Some(n) = count_prefix {
        QuantityExpr::Fixed { value: n as i32 }
    } else {
        // Bare X with no "where X is" — variable resolved from the enclosing
        // ability's payment (e.g., G'raha Tia: X is the spell's mana value
        // paid as life via the parent PayCost).
        QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }
    };

    Some(ContinuationAst::TokenEntersWithCounters {
        counter_type,
        count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::QuantityExpr;

    /// Helper: extract just the text fields from split_clause_sequence output.
    fn clause_texts(input: &str) -> Vec<String> {
        split_clause_sequence(input)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    // --- Bare " and " splitting: positive cases (should split) ---

    #[test]
    fn bare_and_splits_lose_life_and_create_token() {
        // Lotho: "you lose 1 life and create a Treasure token"
        let chunks = clause_texts("you lose 1 life and create a Treasure token");
        assert_eq!(chunks, vec!["you lose 1 life", "create a Treasure token"]);
    }

    #[test]
    fn bare_and_splits_draw_and_lose() {
        let chunks = clause_texts("draw a card and lose 1 life");
        assert_eq!(chunks, vec!["draw a card", "lose 1 life"]);
    }

    #[test]
    fn bare_and_splits_destroy_and_gain() {
        let chunks = clause_texts("destroy target creature and gain 3 life");
        assert_eq!(chunks, vec!["destroy target creature", "gain 3 life"]);
    }

    #[test]
    fn sentence_split_handles_plural_possessive_apostrophe() {
        let chunks = clause_texts(
            "return target artifacts to their owners' hands. you may cast a spell from your hand",
        );
        assert_eq!(
            chunks,
            vec![
                "return target artifacts to their owners' hands",
                "you may cast a spell from your hand"
            ]
        );
    }

    /// CR 701.27 + CR 701.28: "transform"/"convert" must split as clause-starts.
    /// Primal Amulet class: "remove those counters and transform it" reaches
    /// the dispatcher as two independent clauses so each parses cleanly.
    #[test]
    fn bare_and_splits_remove_and_transform() {
        let chunks = clause_texts("remove those counters and transform it");
        assert_eq!(chunks, vec!["remove those counters", "transform it"]);
    }

    #[test]
    fn bare_and_splits_remove_and_convert() {
        let chunks = clause_texts("remove all of them and convert this creature");
        assert_eq!(chunks, vec!["remove all of them", "convert this creature"]);
    }

    // --- Bare " and " splitting: negative cases (must NOT split) ---

    #[test]
    fn bare_and_does_not_split_creature_and_all_other() {
        // Bile Blight: "target creature and all other creatures with the same name"
        let chunks = clause_texts("target creature and all other creatures with the same name");
        assert_eq!(
            chunks,
            vec!["target creature and all other creatures with the same name"]
        );
    }

    #[test]
    fn bare_and_does_not_split_each_opponent_and_each_creature() {
        // Goblin Chainwhirler: "each opponent and each creature and planeswalker they control"
        let chunks = clause_texts("each opponent and each creature and planeswalker they control");
        assert_eq!(
            chunks,
            vec!["each opponent and each creature and planeswalker they control"]
        );
    }

    #[test]
    fn bare_and_does_not_split_it_and_each_other() {
        let chunks = clause_texts("exile it and each other creature");
        assert_eq!(chunks, vec!["exile it and each other creature"]);
    }

    #[test]
    fn bare_and_does_not_split_targeted_put_counter_continuation() {
        let chunks =
            clause_texts("tap target creature an opponent controls and put a stun counter on it");
        assert_eq!(
            chunks,
            vec!["tap target creature an opponent controls and put a stun counter on it"]
        );
    }

    #[test]
    fn bare_and_does_not_split_power_and_toughness() {
        let chunks = clause_texts("power and toughness each equal to the number of cards");
        assert_eq!(
            chunks,
            vec!["power and toughness each equal to the number of cards"]
        );
    }

    #[test]
    fn bare_and_does_not_split_you_and_target_opponent() {
        let chunks = clause_texts("you and target opponent each draw a card");
        assert_eq!(chunks, vec!["you and target opponent each draw a card"]);
    }

    // --- Comma-based splitting still works ---

    #[test]
    fn comma_then_clause_still_splits() {
        let chunks = clause_texts("draw a card, then discard a card");
        assert_eq!(chunks, vec!["draw a card", "discard a card"]);
    }

    #[test]
    fn comma_keyword_list_does_not_split_double_strike() {
        let chunks = clause_texts(
            "creatures you control gain flying, vigilance, and double strike until end of turn",
        );
        assert_eq!(
            chunks,
            vec![
                "creatures you control gain flying, vigilance, and double strike until end of turn"
            ]
        );
    }

    #[test]
    fn comma_keyword_list_does_not_split_double_team() {
        let chunks = clause_texts("creatures you control gain flying, and double team");
        assert_eq!(
            chunks,
            vec!["creatures you control gain flying, and double team"]
        );
    }

    #[test]
    fn sentence_boundary_still_splits() {
        let chunks = clause_texts("draw a card. Create a token");
        assert_eq!(chunks, vec!["draw a card", "Create a token"]);
    }

    #[test]
    fn earthbender_search_stays_together() {
        // The full effect text after stripping the trigger condition.
        // Period after "earthbend 2" should split into two sentences,
        // and the search clause must stay with "put it onto the battlefield tapped".
        // "then shuffle" correctly splits into its own clause.
        let chunks = clause_texts(
            "earthbend 2. Then search your library for a basic land card, put it onto the battlefield tapped, then shuffle",
        );
        assert_eq!(
            chunks,
            vec![
                "earthbend 2",
                "Then search your library for a basic land card, put it onto the battlefield tapped",
                "shuffle",
            ]
        );
    }

    #[test]
    fn bare_shuffle_at_end_of_sentence_splits() {
        let chunks = clause_texts("draw a card, then shuffle.");
        assert_eq!(chunks, vec!["draw a card", "shuffle"]);
    }

    #[test]
    fn intransitive_verbs_match_without_trailing_space() {
        // Intransitive verbs can appear bare at end-of-sentence (", then shuffle.")
        // They MUST match in starts_clause_text without a trailing space.
        let intransitive = ["shuffle", "explore", "investigate", "proliferate"];
        for verb in intransitive {
            assert!(
                starts_clause_text(verb),
                "Intransitive verb '{}' must match in starts_clause_text \
                 without trailing space — otherwise ', then {}.' fails to split",
                verb,
                verb,
            );
        }
    }

    #[test]
    fn conjugated_verb_splits_after_then() {
        // CR 608.2c: Third-person verb forms after ", then" must split.
        // "Each player discards their hand, then draws seven cards."
        let chunks = clause_texts("discards their hand, then draws seven cards");
        assert_eq!(chunks, vec!["discards their hand", "draws seven cards"]);
    }

    #[test]
    fn conjugated_verb_puts_splits_after_then() {
        // "then puts that card on the bottom" should split
        let chunks = clause_texts("reveals the top card, then puts that card on the bottom");
        assert_eq!(
            chunks,
            vec!["reveals the top card", "puts that card on the bottom"]
        );
    }

    #[test]
    fn conjugated_verb_sacrifices_splits_after_then() {
        let chunks = clause_texts("creates a token, then sacrifices a creature");
        assert_eq!(chunks, vec!["creates a token", "sacrifices a creature"]);
    }

    #[test]
    fn possessive_its_does_not_trigger_deconjugation() {
        // "its controller" must NOT be deconjugated — "its" is a possessive pronoun.
        assert!(!starts_clause_text_or_conjugated(
            "its controller gains life"
        ));
    }

    #[test]
    fn for_as_long_as_prefix_does_not_split_on_comma() {
        // CR 611.2b: "For as long as [condition], [effect]" must not split
        // at the internal comma separating the condition from the effect body.
        let chunks = split_clause_sequence(
            "For as long as this creature remains tapped, gain control of target creature",
        );
        assert_eq!(
            chunks.len(),
            1,
            "expected 1 chunk (unsplit), got {}: {:?}",
            chunks.len(),
            chunks.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    // --- Bare " and " splitting: damage clause patterns ---

    #[test]
    fn bare_and_splits_sacrifice_and_it_deals_damage() {
        // Mogg Bombers: "sacrifice ~ and it deals 3 damage to target player"
        let chunks =
            clause_texts("sacrifice ~ and it deals 3 damage to target player or planeswalker");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "sacrifice ~");
        assert!(chunks[1].starts_with("it deals 3 damage"));
    }

    #[test]
    fn bare_and_splits_gain_life_and_card_deals_damage() {
        // Axelrod Gunnarson: "you gain 1 life and ~ deals 1 damage to target player"
        let chunks =
            clause_texts("you gain 1 life and ~ deals 1 damage to target player or planeswalker");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "you gain 1 life");
        assert!(chunks[1].starts_with("~ deals 1 damage"));
    }

    #[test]
    fn bare_and_splits_that_creature_deals_damage() {
        // Form of the Dinosaur: "and that creature deals damage equal to its power to you"
        let chunks = clause_texts("~ deals 15 damage to target creature and that creature deals damage equal to its power to you");
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn starts_with_damage_clause_positive() {
        assert!(starts_with_damage_clause("it deals 3 damage"));
        assert!(starts_with_damage_clause("this creature deals 1 damage"));
        assert!(starts_with_damage_clause("that creature deals damage"));
        assert!(starts_with_damage_clause("deals 5 damage"));
        assert!(starts_with_damage_clause("~ deals 2 damage"));
        assert!(starts_with_damage_clause("this enchantment deals 4 damage"));
    }

    #[test]
    fn starts_with_damage_clause_negative() {
        assert!(!starts_with_damage_clause("it and each other creature"));
        assert!(!starts_with_damage_clause("all creatures deal"));
        assert!(!starts_with_damage_clause("each player deals"));
        assert!(!starts_with_damage_clause("you lose 3 life"));
    }

    // --- parse_followup_continuation_ast: PutRest destination parsing ---

    fn make_dig_effect() -> Effect {
        Effect::Dig {
            count: QuantityExpr::Fixed { value: 3 },
            destination: None,
            keep_count: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal: false,
        }
    }

    #[test]
    fn put_rest_bottom_of_library_with_any_order() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put the rest on the bottom of your library in any order.",
            &dig,
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_rest_bottom_of_library_without_any_order() {
        let dig = make_dig_effect();
        let result =
            parse_followup_continuation_ast("Put the rest on the bottom of your library.", &dig);
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_rest_into_graveyard() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast("Put the rest into your graveyard.", &dig);
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Graveyard,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_rest_random_order_bottom() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put the rest on the bottom of your library in a random order.",
            &dig,
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_them_back_any_order() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast("Put them back in any order.", &dig);
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: true,
            })
        );
    }

    #[test]
    fn put_rest_into_hand() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast("Put the rest into your hand.", &dig);
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Hand,
                reorder_all: false,
            })
        );
    }

    #[test]
    fn put_those_cards_on_bottom() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put those cards on the bottom of your library in any order.",
            &dig,
        );
        assert_eq!(
            result,
            Some(ContinuationAst::PutRest {
                destination: Zone::Library,
                reorder_all: false,
            })
        );
    }

    // --- "put N of them" DigFromAmong continuation ---

    #[test]
    fn put_two_of_them_into_hand_with_rest_on_bottom() {
        // Stock Up / Dig Through Time pattern: keep count + rest destination in one clause.
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put two of them into your hand and the rest on the bottom of your library in any order.",
            &dig,
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 2,
                up_to: false,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
            })
        );
    }

    #[test]
    fn put_one_of_them_into_hand_with_rest_on_bottom() {
        // Impulse / Anticipate pattern.
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put one of them into your hand and the rest on the bottom of your library in any order.",
            &dig,
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 1,
                up_to: false,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
            })
        );
    }

    /// CR 401.1 + CR 401.4 + CR 701.20e: Sleight of Hand / Sea Gate Oracle /
    /// Sight Beyond Sight pattern. "Put one of them into your hand and the
    /// other on the bottom of your library." The anaphor "the other"
    /// (singular remainder of a count=2 look) must be recognized as
    /// equivalent to "the rest" (general remainder); both must yield
    /// `rest_destination: Some(Library)` — NOT the graveyard default.
    #[test]
    fn put_one_of_them_into_hand_with_other_on_bottom() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put one of them into your hand and the other on the bottom of your library.",
            &dig,
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 1,
                up_to: false,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Library),
            })
        );
    }

    #[test]
    fn put_two_of_them_into_hand_rest_into_graveyard() {
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "Put two of them into your hand and the rest into your graveyard.",
            &dig,
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 2,
                up_to: false,
                filter: TargetFilter::Any,
                destination: Some(Zone::Hand),
                rest_destination: Some(Zone::Graveyard),
            })
        );
    }

    /// CR 201.2 + CR 608.2c: Mitotic-Manipulation-style name-match selection
    /// after a Dig emits a `DigFromAmong` continuation that patches the
    /// preceding Dig with destination = Battlefield, keep_count = 1,
    /// up_to = true (the "may" / "if" optional selection), and a
    /// `NameMatchesAnyPermanent` filter.
    #[test]
    fn put_one_of_those_cards_onto_battlefield_if_same_name() {
        use crate::types::ability::{FilterProp, TypedFilter};
        let dig = make_dig_effect();
        let result = parse_followup_continuation_ast(
            "You may put one of those cards onto the battlefield if it has the same name as a permanent.",
            &dig,
        );
        assert_eq!(
            result,
            Some(ContinuationAst::DigFromAmong {
                count: 1,
                up_to: true,
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::NameMatchesAnyPermanent { controller: None },
                ])),
                destination: Some(Zone::Battlefield),
                rest_destination: None,
            })
        );
    }

    // --- Subject-prefixed "you [verb]" splitting ---

    #[test]
    fn bare_and_splits_discard_and_you_gain() {
        // Basilica Bell-Haunt pattern: "each opponent discards a card and you gain 3 life"
        let chunks = clause_texts("each opponent discards a card and you gain 3 life");
        assert_eq!(
            chunks,
            vec!["each opponent discards a card", "you gain 3 life"]
        );
    }

    #[test]
    fn bare_and_splits_lose_and_you_gain() {
        // Blood Artist drain pattern: "target opponent loses 1 life and you gain 1 life"
        let chunks = clause_texts("target opponent loses 1 life and you gain 1 life");
        assert_eq!(
            chunks,
            vec!["target opponent loses 1 life", "you gain 1 life"]
        );
    }

    #[test]
    fn bare_and_splits_you_draw_clause() {
        let chunks = clause_texts("destroy target creature and you draw a card");
        assert_eq!(chunks, vec!["destroy target creature", "you draw a card"]);
    }

    #[test]
    fn bare_and_splits_you_may_clause() {
        let chunks = clause_texts("exile target creature and you may draw a card");
        assert_eq!(chunks, vec!["exile target creature", "you may draw a card"]);
    }

    #[test]
    fn bare_and_splits_its_controller_clause() {
        let chunks = clause_texts("destroy target creature and its controller loses 3 life");
        assert_eq!(
            chunks,
            vec!["destroy target creature", "its controller loses 3 life"]
        );
    }

    // --- B11: Temporal prefix suppresses bare "and" splitting ---

    #[test]
    fn temporal_prefix_suppresses_bare_and_split() {
        // CR 603.7a: "at the beginning of your next upkeep, draw a card and gain 3 life"
        // must NOT split at "and" — the compound inner effect is a single delayed trigger.
        let chunks =
            clause_texts("at the beginning of your next upkeep, draw a card and gain 3 life");
        assert_eq!(
            chunks,
            vec!["at the beginning of your next upkeep, draw a card and gain 3 life"]
        );
    }

    #[test]
    fn temporal_prefix_end_step_suppresses_bare_and_split() {
        let chunks =
            clause_texts("at the beginning of the next end step, return it and lose 2 life");
        assert_eq!(
            chunks,
            vec!["at the beginning of the next end step, return it and lose 2 life"]
        );
    }

    // --- Token enters with counters continuation ---

    #[test]
    fn token_enters_with_x_counters_where_x_is() {
        let result = try_parse_token_enters_with_counters(
            "the token enters with x +1/+1 counters on it, where x is the number of other creatures you control",
        );
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, "P1P1");
            // Should be an ObjectCount ref for "the number of other creatures you control"
            assert!(matches!(count, QuantityExpr::Ref { .. }));
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn token_enters_with_it_prefix() {
        let result = try_parse_token_enters_with_counters(
            "it enters with x +1/+1 counters on it, where x is the number of creatures you control",
        );
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters { counter_type, .. }) = result {
            assert_eq!(counter_type, "P1P1");
        }
    }

    #[test]
    fn token_enters_with_fixed_counters() {
        let result = try_parse_token_enters_with_counters(
            "the token enters with three +1/+1 counters on it",
        );
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, "P1P1");
            assert_eq!(count, QuantityExpr::Fixed { value: 3 });
        }
    }

    #[test]
    fn token_enters_with_counters_no_match() {
        // Should not match non-counter enters-with text
        let result = try_parse_token_enters_with_counters("the token enters tapped and attacking");
        assert!(result.is_none());
    }

    // --- "and put N counter(s) on it" imperative followup form ---

    #[test]
    fn put_counters_on_it_followup_x_variable() {
        // G'raha Tia: "create a 1/1 ... token and put X +1/+1 counters on it"
        // After clause splitting, the followup clause is "put x +1/+1 counters on it".
        let result = try_parse_put_counters_on_token_followup("put x +1/+1 counters on it");
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, "P1P1");
            // Bare X without "where X is" — resolved from parent payment at runtime.
            assert!(matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable { .. }
                }
            ));
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_it_followup_fixed_word() {
        // Fractal Tender: "... and put three +1/+1 counters on it"
        let result = try_parse_put_counters_on_token_followup("put three +1/+1 counters on it");
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, "P1P1");
            assert_eq!(count, QuantityExpr::Fixed { value: 3 });
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_it_followup_singular_article() {
        // "and put a +1/+1 counter on it" — singular article form.
        let result = try_parse_put_counters_on_token_followup("put a +1/+1 counter on it");
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters {
            counter_type,
            count,
        }) = result
        {
            assert_eq!(counter_type, "P1P1");
            assert_eq!(count, QuantityExpr::Fixed { value: 1 });
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_it_followup_where_x_is() {
        // Fractal Anomaly: "... put X +1/+1 counters on it, where X is the
        // number of cards you've drawn this turn"
        let result = try_parse_put_counters_on_token_followup(
            "put x +1/+1 counters on it, where x is the number of cards you've drawn this turn",
        );
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters { counter_type, .. }) = result {
            assert_eq!(counter_type, "P1P1");
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_it_followup_minus_counters() {
        // -1/-1 counter form (uncommon for tokens, but the helper supports it).
        let result = try_parse_put_counters_on_token_followup("put a -1/-1 counter on it");
        assert!(result.is_some());
        if let Some(ContinuationAst::TokenEntersWithCounters { counter_type, .. }) = result {
            assert_eq!(counter_type, "M1M1");
        } else {
            panic!("expected TokenEntersWithCounters");
        }
    }

    #[test]
    fn put_counters_on_it_followup_rejects_named_target() {
        // Rat King, Verminister: "... and put a +1/+1 counter on Rat King"
        // — "on Rat King" is NOT "on it"; must NOT match (the named target is
        // SelfRef = source card, not the just-created token).
        let result = try_parse_put_counters_on_token_followup("put a +1/+1 counter on rat king");
        assert!(result.is_none());
    }

    #[test]
    fn put_counters_on_it_followup_rejects_non_put_verb() {
        // Other verbs that happen to mention counters must not match.
        let result = try_parse_put_counters_on_token_followup("remove a +1/+1 counter on it");
        assert!(result.is_none());
    }
}
