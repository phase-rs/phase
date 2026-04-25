//! Shared parser for the `, except <body>` clause that may follow any
//! "becomes a copy of <X>" / "enter as a copy of <X>" phrase. The clause
//! contributes typed [`ContinuousModification`] entries that downstream
//! `Effect::BecomeCopy` resolution applies at Layer 1 (CR 707.9 + CR 613.1a).
//!
//! # Why a shared module?
//!
//! Two grammatically distinct paths produce a `BecomeCopy` effect:
//!
//! 1. **Replacement (ETB) form** — `oracle_replacement.rs::parse_clone_replacement`
//!    handles "you may have ~ enter as a copy of …" / "as ~ enters, you may
//!    have it become a copy of …".
//! 2. **Triggered / spell-effect form** — `oracle_effect/subject.rs::build_become_clause`
//!    handles "<subject> becomes a copy of …" inside a triggered ability or
//!    instant/sorcery body (Irma Part-Time Mutant, Cryptoplasm, Mirror Mockery,
//!    Cytoshape, Sakashima the Impostor, …).
//!
//! Both paths consume the same `, except <body>` grammar. To honour the
//! "build for the class, not the card" rule, the clause parser lives here
//! and is invoked from both sites.
//!
//! # Recognised body shapes
//!
//! Each comma-anded body produces zero or more typed modifications:
//!
//! - `<possessive> name is ~`
//!   → [`ContinuousModification::SetName`] keyed to the source card's name.
//!   Possessive accepts `his` / `her` / `its` (CR 707.9b + CR 707.2).
//! - `<subject pronoun>'s N/M {type list} in addition to its other types`
//!   → [`ContinuousModification::SetPower`] + [`ContinuousModification::SetToughness`]
//!   plus an `AddType` / `AddSubtype` per word in the type list (CR 707.9b
//!   + CR 613.1d).
//! - `it's a(n) {core_type} in addition to its other types`
//!   → [`ContinuousModification::AddType`] (when the type word is a core type)
//!   or [`ContinuousModification::AddSubtype`] (otherwise).
//! - `it has {keyword[, keyword, ...]}`
//!   → [`ContinuousModification::AddKeyword`] per recognised keyword.
//! - `<subject pronoun> has this ability`
//!   → [`ContinuousModification::RetainPrintedTriggerFromSource`] referencing
//!   the trigger that contains the BecomeCopy effect (CR 707.9a). The
//!   subject pronoun accepts `he`/`she`/`it` so cards from any gender print
//!   route through the same arm. Requires `current_trigger_index` to be set
//!   in the parse context — when absent, the arm declines (no modification
//!   produced) so the rest of the except clause still parses.
//!
//! # Fail-soft semantics
//!
//! Any unrecognised body fragment is silently skipped (we jump to the next
//! `" and "` and try again). This preserves correctness for cards whose except
//! clause includes a not-yet-supported shape (e.g. Vesuvan Doppelganger's
//! "doesn't copy that creature's color"): the recognised modifications still
//! flow through, and the unrecognised fragment is ignored at parse time. The
//! parser is total over the input.
//!
//! # Self-reference normalisation
//!
//! All inputs to this module must already have card-name self-references
//! rewritten to `~`. The replacement and effect-chain entry points both run
//! `normalize_card_name_refs` upstream, so this is satisfied automatically
//! when the parser is reached via `parse_oracle_text`.

use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::char;
use nom::combinator::opt;
use nom::Parser;
use nom_language::error::VerboseError;

use super::super::oracle_keyword::parse_keyword_from_oracle;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_static::split_keyword_list;
use super::super::oracle_util::canonicalize_subtype_name;
use crate::types::ability::{ContinuousModification, QuantityExpr};
use crate::types::card_type::{CoreType, Supertype};

/// Optional context used by [`parse_except_body`] arms that need to know
/// which printed trigger of the source object we're currently parsing.
///
/// Currently the only consumer is the `<subject pronoun> has this ability`
/// arm, which emits [`ContinuousModification::RetainPrintedTriggerFromSource`]
/// referencing the trigger index. When the field is `None`, that arm declines
/// gracefully so non-trigger contexts (replacements, instants, sorceries) can
/// still use the same parser without spuriously emitting a retain modification.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ExceptClauseContext {
    /// Index of the trigger whose body is being parsed, in the source object's
    /// printed trigger list. Set by the trigger parser before invoking the
    /// effect chain. CR 707.9a + CR 603.1.
    pub(crate) current_trigger_index: Option<usize>,
}

/// CR 707.9a: ", except {except_body} [and {except_body}]*[.]"
///
/// Each `except_body` independently contributes typed modifications. Bodies
/// that don't match a known shape are silently skipped so we still keep the
/// ones that do. The trailing '.' is optional and non-load-bearing.
///
/// The remainder returned is the span after any sentence-terminating `.` so
/// callers can continue parsing trailing clauses (e.g. "When you do, ...").
///
/// # Pre-conditions
/// - `input` must be lowercased text with self-references already normalised
///   to `~` (`oracle_util::normalize_card_name_refs`).
/// - `card_name` is the *original* card name spelling, used to populate
///   `ContinuousModification::SetName` so the override matches printed casing.
///
/// Returns `None` only when the leading ", except " tag is absent.
pub(crate) fn parse_except_clause<'a>(
    input: &'a str,
    card_name: &str,
    ctx: ExceptClauseContext,
) -> Option<(&'a str, Vec<ContinuousModification>)> {
    // ", except " — if missing, there are no modifications to extract.
    let (mut rest, _) = tag::<_, _, VerboseError<&str>>(", except ")
        .parse(input)
        .ok()?;
    let mut modifications = Vec::new();

    loop {
        let before = rest;
        if let Some((after, mods)) = parse_except_body(rest, card_name, ctx) {
            modifications.extend(mods);
            rest = after;
        } else {
            // Unknown body — jump to the next " and " so recognised bodies
            // that follow are not lost. If none exists, we're done.
            rest = skip_to_next_conjunction(rest);
        }

        // Bodies are joined by ", and ", " and ", or just ", " (Spark Double's
        // three-clause "X, Y, and Z" pattern uses comma between bodies and
        // ", and " before the last). Consume the longest match so the next
        // body starts cleanly.
        if let Ok((after, _)) = alt((
            tag::<_, _, VerboseError<&str>>(", and "),
            tag(" and "),
            tag(", "),
        ))
        .parse(rest)
        {
            rest = after;
        } else {
            break;
        }

        // Safety: if nothing was consumed this iteration, stop.
        if rest == before {
            break;
        }
    }

    let (rest, _) = opt(char::<_, VerboseError<&str>>('.')).parse(rest).ok()?;
    Some((rest, modifications))
}

/// Parse a single "except ..." body, producing zero or more modifications.
///
/// Recognised shapes (priority order):
///   - `<possessive> name is ~`                                → SetName(card_name)
///   - `<subject>'s N/M {type list} in addition to its other types`
///     → SetPower + SetToughness + AddType/AddSubtype per word
///   - `<subject pronoun> has this ability`
///     → RetainPrintedTriggerFromSource (when ctx provides the index)
///   - `it's a(n) {core_type} in addition to its other types`  → AddType
///   - `it's a(n) {subtype} in addition to its other types`    → AddSubtype
///   - `it has {keyword[, keyword, ...]}`                      → AddKeyword per kw
pub(crate) fn parse_except_body<'a>(
    input: &'a str,
    card_name: &str,
    ctx: ExceptClauseContext,
) -> Option<(&'a str, Vec<ContinuousModification>)> {
    if let Some((rest, name_mod)) = parse_name_override(input, card_name) {
        return Some((rest, vec![name_mod]));
    }
    if let Some((rest, mods)) = parse_subject_pt_and_types(input) {
        return Some((rest, mods));
    }
    if let Some((rest, modification)) = parse_has_this_ability(input, ctx) {
        return Some((rest, vec![modification]));
    }
    if let Some((rest, modification)) = parse_is_supertype_in_addition(input) {
        return Some((rest, vec![modification]));
    }
    if let Some((rest, modification)) = parse_isnt_supertype(input) {
        return Some((rest, vec![modification]));
    }
    if let Some((rest, modification)) = parse_enters_with_additional_counter(input) {
        return Some((rest, vec![modification]));
    }
    if let Some((rest, subtype)) = parse_its_a_type_in_addition(input) {
        return Some((rest, vec![subtype]));
    }
    if let Some((rest, keywords)) = parse_it_has_keywords(input) {
        return Some((rest, keywords));
    }
    None
}

/// CR 707.9b + CR 707.2: "his/her/its name is ~" — emit a `SetName` override
/// keyed to the original card name. The `~` here is the self-ref sentinel
/// inserted by `normalize_card_name_refs`; we don't need to peel the card's
/// literal name because the suffix text was produced from the already-
/// normalised Oracle line.
///
/// When `card_name` is empty (the caller had no card name available — e.g.
/// a chain-parser test that didn't set `ctx.card_name`), this arm declines
/// rather than emitting `SetName { name: "" }`. An empty `SetName` would
/// silently set `obj.name = ""` at Layer 1 application, which is strictly
/// worse than dropping the override entirely (CR 707.9b is opt-in: the
/// override either applies a meaningful name or it doesn't apply at all).
fn parse_name_override<'a>(
    input: &'a str,
    card_name: &str,
) -> Option<(&'a str, ContinuousModification)> {
    if card_name.is_empty() {
        return None;
    }
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("his name is "),
        tag("her name is "),
        tag("its name is "),
    ))
    .parse(input)
    .ok()?;
    // Accept "~" (normalised self-ref) as the name target. This keeps the
    // parser strict — "except its name is Whatever" should only emit SetName
    // when the name is the card's own (which is what normalisation produces).
    let (rest, _) = tag::<_, _, VerboseError<&str>>("~").parse(rest).ok()?;
    Some((
        rest,
        ContinuousModification::SetName {
            name: card_name.to_string(),
        },
    ))
}

/// CR 707.9b: "<subject> N/M {type list} in addition to its other types" where
/// the subject is a pronoun-contraction ("he's" / "she's" / "it's" with either
/// straight or curly apostrophes). Produces `SetPower` + `SetToughness`
/// (overriding the copied P/T per CR 707.9b) and one `AddType`/`AddSubtype`
/// per word in the type list. Layer placement is automatic from the variants'
/// own `layer()` methods: SetPT at layer 7b, type additions at layer 4
/// (CR 613.1d) — the layer system applies type additions after the copy's
/// own types via timestamp order.
fn parse_subject_pt_and_types(input: &str) -> Option<(&str, Vec<ContinuousModification>)> {
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("he's a "),
        tag("he\u{2019}s a "),
        tag("she's a "),
        tag("she\u{2019}s a "),
        tag("it's a "),
        tag("it\u{2019}s a "),
    ))
    .parse(input)
    .ok()?;

    // Parse "N/M " — both components are positive integers.
    let (rest, (power, toughness)) = parse_pt_pair(rest)?;
    let (rest, _) = tag::<_, _, VerboseError<&str>>(" ").parse(rest).ok()?;

    // Grab the type list up to " in addition to its/his/her other types".
    let (type_text, rest) = split_on_first_of(
        rest,
        &[
            " in addition to its other types",
            " in addition to his other types",
            " in addition to her other types",
        ],
    )?;

    let mut mods = vec![
        ContinuousModification::SetPower { value: power },
        ContinuousModification::SetToughness { value: toughness },
    ];

    // Type list is space-separated in the copy class ("Spider Human Hero").
    // Reuse the shared core-type vs subtype dispatch from parse_its_a_type_in_addition.
    for word in type_text.split_whitespace() {
        if word.is_empty() {
            continue;
        }
        let canonical = canonicalize_subtype_name(word);
        let modification = if let Ok(core_type) = CoreType::from_str(&canonical) {
            ContinuousModification::AddType { core_type }
        } else {
            ContinuousModification::AddSubtype { subtype: canonical }
        };
        mods.push(modification);
    }

    Some((rest, mods))
}

/// CR 707.9a: "<subject pronoun> has this ability" — emit a
/// [`ContinuousModification::RetainPrintedTriggerFromSource`] keyed to the
/// printed trigger that contains the `BecomeCopy` effect.
///
/// "this ability" inside a triggered ability's body refers to that very
/// trigger (CR 603.1). For the copy to retain it, the runtime must reach back
/// into the *source* object's printed triggers (by index) at Layer 1 and push
/// a clone onto the copied object's triggers — `GrantTrigger` would require a
/// pre-built `TriggerDefinition`, which we cannot construct mid-parse without
/// a forward reference to the partial trigger.
///
/// When `ctx.current_trigger_index` is `None` (e.g. parsing inside a
/// replacement effect or a non-trigger spell body), the arm declines so the
/// surrounding except clause continues parsing.
///
/// Subject pronouns accepted: `he`, `she`, `it` (and `they` for plural). All
/// are treated identically — this clause is a self-reference to the trigger
/// containing it.
fn parse_has_this_ability(
    input: &str,
    ctx: ExceptClauseContext,
) -> Option<(&str, ContinuousModification)> {
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("he has this ability"),
        tag("she has this ability"),
        tag("it has this ability"),
        tag("they have this ability"),
    ))
    .parse(input)
    .ok()?;
    let source_trigger_index = ctx.current_trigger_index?;
    Some((
        rest,
        ContinuousModification::RetainPrintedTriggerFromSource {
            source_trigger_index,
        },
    ))
}

/// "it's a(n) {type_word} in addition to its other types"
/// The type_word is either a core type (`"artifact"`, `"creature"`, ...) → `AddType`,
/// or anything else → treated as a subtype and canonicalized.
fn parse_its_a_type_in_addition(input: &str) -> Option<(&str, ContinuousModification)> {
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("it's an "),
        tag("it's a "),
        tag("it\u{2019}s an "),
        tag("it\u{2019}s a "),
    ))
    .parse(input)
    .ok()?;
    let (type_word, rest) = nom_primitives::split_once_on(rest, " in addition to its other types")
        .ok()
        .map(|(_, pair)| pair)?;
    let type_word = type_word.trim();
    if type_word.is_empty() {
        return None;
    }
    // Try core type first (canonicalize capitalization before FromStr).
    let canonical = canonicalize_subtype_name(type_word);
    let modification = if let Ok(core_type) = CoreType::from_str(&canonical) {
        ContinuousModification::AddType { core_type }
    } else {
        ContinuousModification::AddSubtype { subtype: canonical }
    };
    Some((rest, modification))
}

/// "it has {keyword[, keyword, ...]}" — each keyword becomes `AddKeyword`.
/// Terminates at the next body separator (" and it ", end-of-string, or '.').
fn parse_it_has_keywords(input: &str) -> Option<(&str, Vec<ContinuousModification>)> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("it has ")
        .parse(input)
        .ok()?;
    // Keyword list terminates at " and it " (next body), the period, or end.
    let (kw_text, remainder) = split_at_body_boundary(rest);
    let mut modifications = Vec::new();
    for part in split_keyword_list(kw_text) {
        if let Some(keyword) = parse_keyword_from_oracle(part.trim()) {
            modifications.push(ContinuousModification::AddKeyword { keyword });
        }
    }
    if modifications.is_empty() {
        return None;
    }
    Some((remainder, modifications))
}

/// CR 205.4 + CR 707.9b: Match `"the token isn't <supertype>"` /
/// `"it isn't <supertype>"` (and apostrophe-free / "is not" variants).
/// Emits [`ContinuousModification::RemoveSupertype`].
///
/// Miirym, Sentinel Wyrm: `"create a token that's a copy of it, except the
/// token isn't legendary"` is the canonical case. The arm is permissive about
/// subject phrasing because both forms appear across token-copy and
/// replacement-copy texts (Spark Double's `"and it isn't legendary"` is the
/// replacement-form variant).
fn parse_isnt_supertype(input: &str) -> Option<(&str, ContinuousModification)> {
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("the token isn't "),
        tag("the token isnt "),
        tag("the token is not "),
        tag("it isn't "),
        tag("it isnt "),
        tag("it is not "),
        tag("he isn't "),
        tag("he isnt "),
        tag("he is not "),
        tag("she isn't "),
        tag("she isnt "),
        tag("she is not "),
    ))
    .parse(input)
    .ok()?;
    parse_supertype_word(rest)
        .map(|(rest, supertype)| (rest, ContinuousModification::RemoveSupertype { supertype }))
}

/// CR 205.4 + CR 707.9d: Match `"<subject pronoun>'s <supertype> in addition
/// to its other types"`. Mirrors [`parse_subject_pt_and_types`]'s pronoun
/// dispatch. Emits [`ContinuousModification::AddSupertype`].
///
/// Sarkhan, Soul Aflame: `"… except its name is ~ and it's legendary in
/// addition to its other types"` is the canonical case.
fn parse_is_supertype_in_addition(input: &str) -> Option<(&str, ContinuousModification)> {
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("it's "),
        tag("it\u{2019}s "),
        tag("he's "),
        tag("he\u{2019}s "),
        tag("she's "),
        tag("she\u{2019}s "),
    ))
    .parse(input)
    .ok()?;
    let (rest, supertype) = parse_supertype_word(rest)?;
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>(" in addition to its other types"),
        tag(" in addition to his other types"),
        tag(" in addition to her other types"),
    ))
    .parse(rest)
    .ok()?;
    Some((rest, ContinuousModification::AddSupertype { supertype }))
}

/// CR 205.4: Match a supertype word and return the typed [`Supertype`].
/// Uses [`alt`] over the five CR-defined supertypes (CR 205.4a) so callers
/// don't have to remember the casing rules of [`Supertype::from_str`].
fn parse_supertype_word(input: &str) -> Option<(&str, Supertype)> {
    let (rest, word) = alt((
        tag::<_, _, VerboseError<&str>>("legendary"),
        tag("basic"),
        tag("snow"),
        tag("world"),
        tag("ongoing"),
    ))
    .parse(input)
    .ok()?;
    // Uppercase first character so `Supertype::from_str` (which expects
    // titlecase) accepts the lowercase Oracle form.
    let mut canonical = String::with_capacity(word.len());
    let mut chars = word.chars();
    if let Some(c) = chars.next() {
        canonical.extend(c.to_uppercase());
    }
    canonical.extend(chars);
    let supertype = Supertype::from_str(&canonical).ok()?;
    Some((rest, supertype))
}

/// CR 122.1 + CR 614.1c: Match `"it enters with an additional <N> <counter>
/// counter[s] on it [if it's a <type>]"`. Emits
/// [`ContinuousModification::AddCounterOnEnter`] with optional `if_type` gate
/// derived from the trailing conditional.
///
/// Spark Double: `"… except it enters with an additional +1/+1 counter on
/// it if it's a creature, it enters with an additional loyalty counter on
/// it if it's a planeswalker, and it isn't legendary"` is the canonical
/// case. The clause is parsed body-by-body; this arm handles a single
/// counter clause and the parent `parse_except_clause` loop chains across
/// `" and "` for the multi-clause sequence.
fn parse_enters_with_additional_counter(input: &str) -> Option<(&str, ContinuousModification)> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("it enters with ")
        .parse(input)
        .ok()?;
    // CR 122.1: "an additional N counter[s]" — N defaults to 1 for "an
    // additional <counter>". Try the explicit-N form first, fall back to
    // the implicit-1 form.
    let (rest, count) = parse_additional_count(rest)?;
    // Counter type token: `+1/+1`, `loyalty`, etc. The counter-type word may
    // be hyphenated/numeric, so consume everything up to ` counter ` or
    // ` counters `. Use `nom_primitives::split_once_on` for the structural
    // boundary; the token-text is then re-parsed by the canonical
    // `types::counter::parse_counter_type`.
    let (counter_text, after_counter) = match nom_primitives::split_once_on(rest, " counters on it")
    {
        Ok((_, pair)) => pair,
        Err(_) => match nom_primitives::split_once_on(rest, " counter on it") {
            Ok((_, pair)) => pair,
            Err(_) => return None,
        },
    };
    if counter_text.is_empty() {
        return None;
    }
    let counter_type = crate::types::counter::parse_counter_type(counter_text)
        .as_str()
        .to_string();
    // Optional `" if it's a <core_type>"` tail. Multiple Oracle variants:
    // "if it's a", "if it's an", "if it is a", smart quotes.
    let (rest, if_type) = parse_optional_if_type(after_counter);
    Some((
        rest,
        ContinuousModification::AddCounterOnEnter {
            counter_type,
            count: QuantityExpr::Fixed { value: count },
            if_type,
        },
    ))
}

/// Parse `"an additional N "` / `"an additional "` (implicit N=1) leading the
/// counter clause. Returns the count and remainder positioned at the start of
/// the counter-type word.
fn parse_additional_count(input: &str) -> Option<(&str, i32)> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("an additional ")
        .parse(input)
        .ok()?;
    // Try a leading number first (covers Spark Double's "an additional +1/+1
    // counter" — there is no number, so we fall through to the default of 1).
    // For texts like "an additional 2 +1/+1 counters" the explicit-N branch
    // grabs the count.
    use nom::character::complete::digit1;
    let digit_parser = |i| -> nom::IResult<&str, &str, VerboseError<&str>> {
        let (i, n) = digit1(i)?;
        let (i, _) = tag::<_, _, VerboseError<&str>>(" ").parse(i)?;
        Ok((i, n))
    };
    if let Ok((rest, n)) = digit_parser(rest) {
        let count: i32 = n.parse().ok()?;
        return Some((rest, count));
    }
    Some((rest, 1))
}

/// Parse the optional `" if it's a <core_type>"` tail trailing a counter
/// clause and return the typed [`CoreType`] if present. Falls through to
/// `(input, None)` when no conditional is present, so callers don't have to
/// guard the absence case.
fn parse_optional_if_type(input: &str) -> (&str, Option<CoreType>) {
    let prefix = match alt((
        tag::<_, _, VerboseError<&str>>(" if it's a "),
        tag(" if it\u{2019}s a "),
        tag(" if it's an "),
        tag(" if it\u{2019}s an "),
        tag(" if it is a "),
        tag(" if it is an "),
    ))
    .parse(input)
    {
        Ok((rest, _)) => rest,
        Err(_) => return (input, None),
    };
    // Type word ends at a body boundary — comma, period, " and ", or end of
    // string. Spark Double's three-clause `it enters ... if it's a creature,
    // it enters ... if it's a planeswalker, and it isn't legendary` uses a
    // bare comma as the clause separator, so the boundary set here must
    // include `,` (which `split_at_body_boundary` deliberately does NOT —
    // keyword lists like "flying, vigilance, and trample" need commas
    // *inside* a body).
    let (type_word, remainder) = split_at_if_type_boundary(prefix);
    let canonical = canonicalize_subtype_name(type_word.trim());
    if let Ok(core_type) = CoreType::from_str(&canonical) {
        (remainder, Some(core_type))
    } else {
        // Unknown type word — back out so the surrounding except-clause loop
        // can recover by jumping to the next conjunction.
        (input, None)
    }
}

/// Body-boundary splitter for the `if_type` arm, matching at the next
/// comma, period, or `" and "` — preserving the structural conjunction
/// grammar for the surrounding except-clause loop. Distinct from
/// [`split_at_body_boundary`] because keyword bodies (`it has X, Y, and Z`)
/// must be allowed to contain commas internally; the if-type tail does
/// not have that flexibility.
fn split_at_if_type_boundary(text: &str) -> (&str, &str) {
    let candidates = [",", ".", " and "];
    let mut best: Option<usize> = None;
    for pat in candidates {
        if let Ok((_, (before, _))) = nom_primitives::split_once_on(text, pat) {
            let pos = before.len();
            best = Some(best.map_or(pos, |b| b.min(pos)));
        }
    }
    match best {
        Some(i) => (&text[..i], &text[i..]),
        None => (text, ""),
    }
}

/// Structural multi-candidate splitter: return the (before, after) pair for the
/// earliest-matching phrase in `candidates`. None if no candidate matches.
fn split_on_first_of<'a>(text: &'a str, candidates: &[&str]) -> Option<(&'a str, &'a str)> {
    let mut best: Option<(usize, usize)> = None;
    for phrase in candidates {
        if let Ok((_, (before, _))) = nom_primitives::split_once_on(text, phrase) {
            let pos = before.len();
            if best.is_none_or(|(bp, _)| pos < bp) {
                best = Some((pos, phrase.len()));
            }
        }
    }
    let (pos, len) = best?;
    Some((&text[..pos], &text[pos + len..]))
}

/// Parse "N/M" where N and M are positive integers. Input is already lowercase.
/// Returns the remainder positioned immediately after "N/M" (caller peels the
/// following space) and the `(power, toughness)` pair.
fn parse_pt_pair(input: &str) -> Option<(&str, (i32, i32))> {
    use nom::character::complete::digit1;
    let parser = |i| -> nom::IResult<&str, (&str, &str), VerboseError<&str>> {
        let (i, p) = digit1(i)?;
        let (i, _) = char('/')(i)?;
        let (i, t) = digit1(i)?;
        Ok((i, (p, t)))
    };
    let (rest, (p, t)) = parser(input).ok()?;
    let power: i32 = p.parse().ok()?;
    let toughness: i32 = t.parse().ok()?;
    Some((rest, (power, toughness)))
}

/// Return `(body, remainder)` where `body` is the text up to the next
/// body-level boundary (`" and it "`, `" and it's "`, or `"."`) and
/// `remainder` still contains that boundary. Delegates to `split_once_on`
/// (a nom-built primitive) for every boundary candidate and keeps the
/// earliest match — purely structural position lookup, no dispatch logic.
fn split_at_body_boundary(text: &str) -> (&str, &str) {
    let candidates = [" and it ", " and it\u{2019}s ", " and it's ", "."];
    let mut best: Option<usize> = None;
    for pat in candidates {
        if let Ok((_, (before, _))) = nom_primitives::split_once_on(text, pat) {
            let pos = before.len();
            best = Some(best.map_or(pos, |b| b.min(pos)));
        }
    }
    match best {
        Some(i) => (&text[..i], &text[i..]),
        None => (text, ""),
    }
}

/// Advance past the next " and " that starts a fresh body. Used to skip an
/// unrecognised body so the rest of the except clause can still be parsed.
/// `split_once_on` is a nom-built primitive — structural position lookup only.
fn skip_to_next_conjunction(text: &str) -> &str {
    match nom_primitives::split_once_on(text, " and ") {
        Ok((_, (_, after))) => {
            // Return the span starting at " and " so the caller can consume it.
            &text[text.len() - after.len() - " and ".len()..]
        }
        Err(_) => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::keywords::Keyword;

    #[test]
    fn name_override_emits_set_name() {
        let (rest, mods) = parse_except_clause(
            ", except her name is ~",
            "Irma, Part-Time Mutant",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            mods,
            vec![ContinuousModification::SetName {
                name: "Irma, Part-Time Mutant".to_string(),
            }]
        );
    }

    #[test]
    fn his_name_override_emits_set_name() {
        let (_, mods) = parse_except_clause(
            ", except his name is ~",
            "Test Card",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert_eq!(
            mods,
            vec![ContinuousModification::SetName {
                name: "Test Card".to_string(),
            }]
        );
    }

    // CR 707.9b: An empty `card_name` (no card name threaded through the
    // parse context) MUST NOT produce `SetName { name: "" }`. Such a
    // modification would silently set `obj.name = ""` at Layer 1, which is
    // strictly worse than dropping the override entirely. The arm declines
    // — the caller still gets every other recognised body modification.
    #[test]
    fn empty_card_name_skips_set_name() {
        let (_, mods) =
            parse_except_clause(", except her name is ~", "", ExceptClauseContext::default())
                .unwrap();
        assert!(
            mods.is_empty(),
            "empty card_name must not emit SetName; got {mods:?}"
        );
    }

    // CR 707.9b: A SetName-bearing body co-located with another recognised
    // body must still emit the *non-name* modifications when card_name is
    // empty — only the SetName arm declines, the rest of the except clause
    // continues to flow.
    #[test]
    fn empty_card_name_skips_set_name_but_keeps_other_mods() {
        let ctx = ExceptClauseContext {
            current_trigger_index: Some(0),
        };
        let (_, mods) =
            parse_except_clause(", except her name is ~ and she has this ability", "", ctx)
                .unwrap();
        assert!(
            !mods
                .iter()
                .any(|m| matches!(m, ContinuousModification::SetName { .. })),
            "no SetName when card_name is empty; got {mods:?}"
        );
        assert!(
            mods.iter().any(|m| matches!(
                m,
                ContinuousModification::RetainPrintedTriggerFromSource {
                    source_trigger_index: 0
                }
            )),
            "other recognised body (has this ability) must still flow through; got {mods:?}"
        );
    }

    #[test]
    fn it_has_this_ability_with_index_emits_retain() {
        let ctx = ExceptClauseContext {
            current_trigger_index: Some(0),
        };
        let (rest, mods) =
            parse_except_clause(", except it has this ability", "Card", ctx).unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            mods,
            vec![ContinuousModification::RetainPrintedTriggerFromSource {
                source_trigger_index: 0,
            }]
        );
    }

    #[test]
    fn she_has_this_ability_with_index_emits_retain() {
        let ctx = ExceptClauseContext {
            current_trigger_index: Some(2),
        };
        let (_, mods) = parse_except_clause(", except she has this ability", "Card", ctx).unwrap();
        assert_eq!(
            mods,
            vec![ContinuousModification::RetainPrintedTriggerFromSource {
                source_trigger_index: 2,
            }]
        );
    }

    #[test]
    fn he_has_this_ability_with_index_emits_retain() {
        let ctx = ExceptClauseContext {
            current_trigger_index: Some(1),
        };
        let (_, mods) = parse_except_clause(", except he has this ability", "Card", ctx).unwrap();
        assert_eq!(
            mods,
            vec![ContinuousModification::RetainPrintedTriggerFromSource {
                source_trigger_index: 1,
            }]
        );
    }

    #[test]
    fn they_have_this_ability_with_index_emits_retain() {
        let ctx = ExceptClauseContext {
            current_trigger_index: Some(3),
        };
        let (_, mods) =
            parse_except_clause(", except they have this ability", "Card", ctx).unwrap();
        assert_eq!(
            mods,
            vec![ContinuousModification::RetainPrintedTriggerFromSource {
                source_trigger_index: 3,
            }]
        );
    }

    #[test]
    fn has_this_ability_without_index_declines_gracefully() {
        // No trigger index in context — the arm declines, but other recognised
        // bodies in the same clause still flow through. Here the entire except
        // body is "she has this ability", so the unrecognised body is silently
        // skipped and `mods` ends up empty.
        let (_, mods) = parse_except_clause(
            ", except she has this ability",
            "Card",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert!(mods.is_empty());
    }

    #[test]
    fn name_and_has_this_ability_compose() {
        let ctx = ExceptClauseContext {
            current_trigger_index: Some(0),
        };
        let (_, mods) = parse_except_clause(
            ", except her name is ~ and she has this ability",
            "Irma, Part-Time Mutant",
            ctx,
        )
        .unwrap();
        // SetName first (parsed first), then RetainPrintedTriggerFromSource.
        assert_eq!(mods.len(), 2);
        assert!(mods.iter().any(|m| matches!(
            m,
            ContinuousModification::SetName { name } if name == "Irma, Part-Time Mutant"
        )));
        assert!(mods.iter().any(|m| matches!(
            m,
            ContinuousModification::RetainPrintedTriggerFromSource {
                source_trigger_index: 0
            }
        )));
    }

    #[test]
    fn it_has_keywords_extracts_each_keyword() {
        let (_, mods) = parse_except_clause(
            ", except it has flying, vigilance, and trample",
            "Card",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert!(mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Flying
            }
        )));
        assert!(mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Vigilance
            }
        )));
        assert!(mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Trample
            }
        )));
    }

    #[test]
    fn its_a_subtype_emits_add_subtype() {
        let (_, mods) = parse_except_clause(
            ", except it's a Spider in addition to its other types",
            "Card",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert!(mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddSubtype { subtype } if subtype == "Spider"
        )));
    }

    #[test]
    fn missing_leading_comma_except_returns_none() {
        let result = parse_except_clause("her name is ~", "Card", ExceptClauseContext::default());
        assert!(result.is_none());
    }

    #[test]
    fn parse_pt_pair_handles_single_and_double_digit_values() {
        // Sanity: the 4/4 used by Superior Spider-Man works, as does a
        // two-digit "12/12" (hypothetical future card).
        let (rest, (p, t)) = parse_pt_pair("4/4 spider").unwrap();
        assert_eq!((p, t), (4, 4));
        assert_eq!(rest, " spider");
        let (rest, (p, t)) = parse_pt_pair("12/12 giant").unwrap();
        assert_eq!((p, t), (12, 12));
        assert_eq!(rest, " giant");
    }

    #[test]
    fn parse_pt_pair_rejects_non_numeric_halves() {
        assert!(parse_pt_pair("a/4").is_none());
        assert!(parse_pt_pair("4/").is_none());
    }

    #[test]
    fn unrecognised_body_does_not_block_others() {
        // First body is unrecognised, second is a valid name override.
        let (_, mods) = parse_except_clause(
            ", except its color is blue and her name is ~",
            "Test",
            ExceptClauseContext::default(),
        )
        .unwrap();
        // Unrecognised body skipped; name override still extracted.
        assert!(mods
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetName { name } if name == "Test")));
    }

    /// CR 205.4 + CR 707.9b: "the token isn't legendary" / "it isn't legendary"
    /// (Miirym, Sentinel Wyrm; Spark Double's terminal clause). Both subject
    /// phrasings emit `RemoveSupertype(Legendary)` so the same building block
    /// covers token-copy and replacement-copy texts.
    #[test]
    fn token_isnt_legendary_emits_remove_supertype() {
        let (_, mods) = parse_except_clause(
            ", except the token isn't legendary",
            "Card",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert_eq!(
            mods,
            vec![ContinuousModification::RemoveSupertype {
                supertype: Supertype::Legendary,
            }]
        );
    }

    #[test]
    fn it_isnt_legendary_emits_remove_supertype() {
        let (_, mods) = parse_except_clause(
            ", except it isn't legendary",
            "Card",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert_eq!(
            mods,
            vec![ContinuousModification::RemoveSupertype {
                supertype: Supertype::Legendary,
            }]
        );
    }

    /// CR 205.4 + CR 707.9d: "<pronoun>'s legendary in addition to its other
    /// types" (Sarkhan, Soul Aflame). Apostrophe-contraction follows the same
    /// pronoun grammar as `parse_subject_pt_and_types`.
    #[test]
    fn its_legendary_in_addition_emits_add_supertype() {
        let (_, mods) = parse_except_clause(
            ", except it's legendary in addition to its other types",
            "Card",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert_eq!(
            mods,
            vec![ContinuousModification::AddSupertype {
                supertype: Supertype::Legendary,
            }]
        );
    }

    /// CR 122.1 + CR 614.1c: Spark Double-class conditional counter clause.
    /// "it enters with an additional +1/+1 counter on it if it's a creature"
    /// → AddCounterOnEnter { P1P1, 1, Some(Creature) }.
    #[test]
    fn enters_with_additional_counter_creature_branch() {
        let (_, mods) = parse_except_clause(
            ", except it enters with an additional +1/+1 counter on it if it's a creature",
            "Card",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert_eq!(mods.len(), 1);
        match &mods[0] {
            ContinuousModification::AddCounterOnEnter {
                counter_type,
                count,
                if_type,
            } => {
                assert_eq!(counter_type, "P1P1");
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert_eq!(*if_type, Some(CoreType::Creature));
            }
            other => panic!("expected AddCounterOnEnter, got {other:?}"),
        }
    }

    /// CR 122.1 + CR 614.1c: Spark Double's three-clause body — bare comma
    /// separator between bodies plus ", and " before the last.
    #[test]
    fn spark_double_three_clause_chain() {
        let (_, mods) = parse_except_clause(
            ", except it enters with an additional +1/+1 counter on it if it's a creature, it enters with an additional loyalty counter on it if it's a planeswalker, and it isn't legendary",
            "Card",
            ExceptClauseContext::default(),
        )
        .unwrap();
        assert_eq!(mods.len(), 3);
        assert!(mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddCounterOnEnter {
                if_type: Some(CoreType::Creature),
                counter_type,
                ..
            } if counter_type == "P1P1"
        )));
        assert!(mods.iter().any(|m| matches!(
            m,
            ContinuousModification::AddCounterOnEnter {
                if_type: Some(CoreType::Planeswalker),
                counter_type,
                ..
            } if counter_type == "loyalty"
        )));
        assert!(mods.iter().any(|m| matches!(
            m,
            ContinuousModification::RemoveSupertype {
                supertype: Supertype::Legendary
            }
        )));
    }
}
