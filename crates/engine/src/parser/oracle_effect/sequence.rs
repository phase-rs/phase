use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::value;
use nom::Parser;
use nom_language::error::VerboseError;

use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_target::parse_target;
use super::super::oracle_util::contains_possessive;
use super::types::*;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Chooser, Effect, StaticDefinition, TargetFilter,
};
use crate::types::zones::Zone;

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
                    let suppress = nom_primitives::scan_contains(&before_lower, "from among")
                        || is_inside_temporal_prefix(&before_lower)
                        || targeted_compound_continuation;
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
        tag("otherwise"),
        tag("if not"),
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
    .parse(s)
    .is_ok();
    if has_verb_prefix {
        return true;
    }
    // "gain N" / "lose N" — imperative with numeric argument (e.g., "gain 3 life",
    // "lose 2 life") is a clause start, but conjugated "gains"/"loses" is NOT.
    if (tag::<_, _, VerboseError<&str>>("gain ").parse(s).is_ok()
        && tag::<_, _, VerboseError<&str>>("gains ").parse(s).is_err())
        || (tag::<_, _, VerboseError<&str>>("lose ").parse(s).is_ok()
            && tag::<_, _, VerboseError<&str>>("loses ").parse(s).is_err())
    {
        return true;
    }
    starts_with_damage_clause(s)
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
                },
            );
            // CR 303.4f: "attached to [source]" — forward the moved card to an Attach sub_ability
            if attach_to_source {
                change_zone.forward_result = true;
                change_zone.sub_ability = Some(Box::new(AbilityDefinition::new(
                    kind,
                    Effect::Attach {
                        target: TargetFilter::Any,
                    },
                )));
            }
            defs.push(change_zone);
        }
        ContinuationAst::RevealHandFilter { card_filter } => {
            let Some(previous) = defs.last_mut() else {
                return;
            };
            if let Effect::RevealHand {
                card_filter: existing,
                ..
            } = &mut *previous.effect
            {
                *existing = card_filter;
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
        ContinuationAst::ManaGrant {
            grants: new_grants,
        } => {
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
        ContinuationAst::PutRest { destination } => {
            // Absorbed into preceding Dig or RevealUntil — sets rest_destination
            // for unchosen/non-matching cards.
            let Some(previous) = defs.last_mut() else {
                return;
            };
            match &mut *previous.effect {
                Effect::Dig {
                    rest_destination: rest @ None,
                    ..
                } => {
                    *rest = Some(destination);
                }
                Effect::RevealUntil {
                    rest_destination, ..
                } => {
                    *rest_destination = destination;
                }
                _ => {}
            }
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
                ..
            } = &mut *previous.effect
            {
                *keep_count = Some(count);
                *up_to = is_up_to;
                *filter = card_filter;
                *destination = Some(kept_dest);
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
                },
            ));
        }
        ContinuationAst::SearchResultClauseHandled => {}
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
        ContinuationAst::EntersTappedAttacking => true,
        ContinuationAst::DigFromAmong { .. } => true,
        ContinuationAst::GrantExtraTurnAfterControlledTurn => true,
        ContinuationAst::RevealUntilKept { .. } => true,
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
            let attach_to_source = nom_primitives::scan_contains(&lower, "attached to");
            // CR 701.23a: "onto the battlefield tapped" — the searched card enters tapped.
            let enter_tapped = nom_primitives::scan_contains(&lower, "battlefield tapped");
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
            Some(ContinuationAst::SearchDestination {
                destination: super::parse_search_destination(&lower),
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
fn parse_dig_from_among(lower: &str, _original: &str) -> Option<ContinuationAst> {
    // Determine kept-cards destination
    let destination = if nom_primitives::scan_contains(lower, "onto the battlefield") {
        Zone::Battlefield
    } else {
        Zone::Hand
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

/// Extract rest_destination from "put N of them into your hand and the rest on the bottom/graveyard".
/// Returns None if no "and the rest" clause is present.
fn parse_of_them_rest_destination(lower: &str) -> Option<Zone> {
    let (_, (_, after_rest)) = nom_primitives::split_once_on(lower, " and the rest").ok()?;
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
            Some(ContinuationAst::RevealHandFilter { card_filter })
        }
        Effect::Mana { .. } => {
            if let Some((restriction, grants)) =
                super::mana::parse_mana_spend_restriction(&lower)
            {
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
        // "put the rest on the bottom" / "put them back" / "put those cards into your graveyard"
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
            Some(ContinuationAst::PutRest { destination })
        }
        // CR 701.20a: "put that card into your hand / onto the battlefield" after RevealUntil
        // — overrides kept_destination. Also extracts rest_destination when the compound
        // sentence includes "and the rest" (the "and" split is suppressed because "the rest"
        // does not start with a recognized imperative verb).
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "put that card")
                || nom_primitives::scan_contains(&lower, "put it") =>
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
        Effect::RevealUntil { .. }
            if nom_primitives::scan_contains(&lower, "put the rest")
                || nom_primitives::scan_contains(&lower, "the rest on the bottom")
                || nom_primitives::scan_contains(&lower, "the rest into your graveyard")
                || nom_primitives::scan_contains(&lower, "put the revealed cards")
                || nom_primitives::scan_contains(&lower, "put them back") =>
        {
            let destination = if nom_primitives::scan_contains(&lower, "into your graveyard")
                || nom_primitives::scan_contains(&lower, "into their graveyard")
            {
                Zone::Graveyard
            } else {
                // Default: bottom of library
                Zone::Library
            };
            Some(ContinuationAst::PutRest { destination })
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
        // CR 508.4 / CR 614.1: "It/The token/He/She/Name enters tapped and attacking"
        // after CopyTokenOf, Token, or ChangeZone effects.
        Effect::CopyTokenOf { .. } | Effect::Token { .. } | Effect::ChangeZone { .. }
            if nom_primitives::scan_contains(&lower, "enters tapped and attacking") =>
        {
            Some(ContinuationAst::EntersTappedAttacking)
        }
        Effect::ControlNextTurn { .. }
            if nom_primitives::scan_contains(&lower, "after that turn")
                && nom_primitives::scan_contains(&lower, "takes an extra turn") =>
        {
            Some(ContinuationAst::GrantExtraTurnAfterControlledTurn)
        }
        _ => None,
    }
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
                destination: Zone::Library
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
                destination: Zone::Library
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
                destination: Zone::Graveyard
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
                destination: Zone::Library
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
                destination: Zone::Library
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
                destination: Zone::Hand
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
                destination: Zone::Library
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
                destination: Zone::Hand,
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
                destination: Zone::Hand,
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
                destination: Zone::Hand,
                rest_destination: Some(Zone::Graveyard),
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
}
