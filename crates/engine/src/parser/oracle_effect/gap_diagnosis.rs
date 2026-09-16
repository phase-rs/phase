//! Clause-gap diagnosis: WHICH sub-grammar rejected an unparsed clause.
//!
//! This module answers one question — "the parser could not read this clause end to
//! end; which grammar refused it?" — by **replaying the engine's own combinators** over
//! the recorded text. It never heuristically inspects the text's first word, which is
//! all a `split_whitespace().next()` gap name ever reported (where the leftover text
//! starts, not what failed).
//!
//! It lives beside the grammars it replays, so a grammar change and its diagnosis change
//! in one module family. [`diagnose_clause_gap`] is a **pure, context-free** function of
//! the recorded text: it calls the condition ladder with a fresh
//! [`ParseContext::default`], stores nothing, and latches nothing, which is exactly what
//! lets a later consumer re-derive the verdict from the exported `description` alone.
//!
//! [`clause_gap_unimplemented`] is the single recording helper every clause-gap producer
//! goes through, so two producers cannot drift apart on how a clause gap is named.

use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1, take_until};
use nom::character::complete::{char, one_of};
use nom::combinator::{eof, not, opt, peek, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_ir::diagnostic::{ClauseGap, ClauseGapKind};
use crate::parser::oracle_nom::error::{oracle_err, OracleError, OracleResult};
use crate::parser::oracle_nom::primitives::{
    parse_number_or_x, scan_at_word_boundaries, scan_contains, scan_preceded,
};
use crate::parser::oracle_quantity::{
    parse_cda_quantity, parse_event_context_quantity, parse_for_each_clause_expr,
};
use crate::types::ability::Effect;

use super::conditions;
use super::lower::parse_where_x_quantity_expression;
use super::normalize_verb_token;
use super::subject::{starts_with_subject_prefix, PREDICATE_VERBS};

/// Which sub-grammar rejected `text`.
///
/// Rules are applied in precedence order, **outer structure first**: a leading guard
/// gates everything after it, so it is diagnosed before anything inside the body.
///
/// CR 608.2c: the controller follows the instructions in the order written; a clause the
/// parser cannot read end to end is a gap, and this names which sub-grammar refused it.
pub(crate) fn diagnose_clause_gap(text: &str) -> ClauseGap {
    let lower = text.to_lowercase();

    // 1. Leading guard. `split_leading_conditional` is the splitter and
    //    `parse_leading_conditional_prefix` the one prefix authority (it also covers
    //    "then, if" / "during any turn"), so the reported guard phrase never carries a
    //    connector. The splitter returns ORIGINAL-case halves including the prefix.
    if let Some((guard_with_prefix, body)) = conditions::split_leading_conditional(text) {
        let gl = guard_with_prefix.to_lowercase();
        let guard = conditions::parse_leading_conditional_prefix(&gl)
            .unwrap_or(gl.as_str())
            .trim();
        // CR 614.1a: an event antecedent ("would ...") whose clause no replacement
        // lowering owns is a replacement gap, not a condition gap.
        if conditions::condition_names_an_event(guard) {
            return ClauseGap::Replacement {
                antecedent: guard.to_string(),
            };
        }
        // CR 608.2c: a state guard the single condition authority rejected.
        if conditions::lower_instead_condition(guard, &mut ParseContext::default()).is_none() {
            return ClauseGap::Condition {
                guard: guard.to_string(),
            };
        }
        // The guard lowers, so the failure is downstream: diagnose the body.
        return diagnose_clause_gap(&body);
    }

    // 2. Unguarded replacement. CR 614.1 + CR 614.1a: an event antecedent and the
    //    "instead" indicator, with no leading "if" to split on.
    if scan_contains(&lower, "would") && scan_contains(&lower, "instead") {
        return ClauseGap::Replacement {
            antecedent: replacement_antecedent(&lower),
        };
    }

    // 3. Quantity. CR 608.2h: a dynamic amount whose operand the quantity authorities
    //    rejected.
    if let Some(operand) = first_rejected_quantity_operand(&lower) {
        return ClauseGap::Quantity { operand };
    }

    // 4. Trailing guard. CR 608.2c: an "if"/"unless" gate the condition ladder
    //    rejected, sitting after the clause's action rather than ahead of it. NOT
    //    CR 603.4 — that rule is the intervening-"if" and it disclaims this position
    //    outright ("this rule only applies to an 'if' that immediately follows a
    //    trigger condition"); here the word carries its normal English meaning and
    //    still gates the clause it trails.
    if let Some(guard) = first_rejected_guard(&lower, GuardWord::If)
        .or_else(|| first_rejected_guard(&lower, GuardWord::Unless))
    {
        return ClauseGap::Condition { guard };
    }

    // 5. Head verb: the imperative dispatcher recognises the clause head, so what failed
    //    is its argument grammar.
    if let Ok((_, (verb, arguments))) = clause_head_verb_token(&lower) {
        return ClauseGap::VerbArguments {
            verb,
            arguments: arguments.trim().to_string(),
        };
    }

    // 6. Subject-led head: a subject phrase the subject grammar recognises, followed by
    //    a vocabulary verb. Same verdict — the arguments are what failed.
    if starts_with_subject_prefix(&lower) {
        if let Some((verb, arguments)) = scan_at_word_boundaries(&lower, clause_head_verb_token) {
            return ClauseGap::VerbArguments {
                verb,
                arguments: arguments.trim().to_string(),
            };
        }
    }

    // 7. Otherwise: neither the verb vocabulary nor the subject grammar recognised the
    //    head. An empty clause reports an empty head rather than a made-up name.
    ClauseGap::UnrecognizedHead {
        head: first_token(&lower).to_string(),
    }
}

/// The single recording helper for a clause-level gap.
///
/// Every clause-gap producer records through here, so the name written into
/// `Effect::Unimplemented.name` always comes from `ClauseGapKind::unimplemented_name`
/// (the wire authority) and never from a literal. `Effect::unimplemented`'s contract —
/// a stable snake_case *category* key in `name`, the unparsed fragment in `description`
/// — is honored: the fragment is passed through byte-identically.
pub(crate) fn clause_gap_unimplemented(text: &str) -> Effect {
    // Records directly rather than through `clause_gap_unimplemented_as`: that function's
    // `debug_assert_eq!` compares the caller's verdict against `diagnose_clause_gap(text)`,
    // which on THIS path is where the verdict just came from. Routing through it would
    // assert `x == x` and pay for a second full diagnosis in every debug build.
    record_clause_gap(diagnose_clause_gap(text).kind(), text)
}

/// Record a clause gap whose verdict the caller already holds.
///
/// The wire name still comes from `ClauseGapKind::unimplemented_name` and the fragment is
/// still passed through byte-identically, so this is the same single authority as
/// [`clause_gap_unimplemented`] — it only skips re-deriving a verdict the caller computed
/// with the live `ParseContext` (see the guard seam, `oracle_effect::lower_clause_ast`).
///
/// # Preconditions of the `debug_assert_eq!`
///
/// The assert is sound, not defensive, but it rests on facts the callers must keep true.
/// State them here so a future edit cannot silently turn it into a suite-wide panic:
///
/// 1. **Caller passes the full trimmed `"if <guard>, <body>"` clause.** `diagnose_clause_gap`
///    re-splits it with `conditions::split_leading_conditional` and re-strips the prefix with
///    `conditions::parse_leading_conditional_prefix` — the same two productions the seam uses —
///    so a body fragment or a pre-stripped guard breaks the correspondence.
/// 2. **Every live caller passes `ClauseGapKind::Replacement`.** The two guard seams
///    (`oracle_effect::lower_clause_ast` and `parser::oracle::resolve_guards_in_ability`) each
///    record only under the EVENT reading, and `diagnose_clause_gap` decides `Replacement` from
///    `condition_names_an_event` alone — before any ladder call, and that predicate lowercases
///    internally. So the two verdicts agree by construction, with no dependency on the
///    `ParseContext` the seam held or on the case of the text it passed.
///
/// A future caller recording `ClauseGapKind::Condition` here would NOT inherit (2). It would
/// reach `diagnose_clause_gap`'s rule 1, which calls `lower_instead_condition` with a FRESH
/// context on a LOWERCASED guard where the seam used the live context and original case —
/// both of which can only WIDEN the diagnoser's acceptance, which is the unsafe direction
/// (a diagnoser that accepts where the seam refused falls through to the body-diagnosis
/// return and this assert fires). Such a caller must discharge that by measurement over the
/// corpus, not by argument, before it is added.
pub(crate) fn clause_gap_unimplemented_as(kind: ClauseGapKind, text: &str) -> Effect {
    debug_assert_eq!(
        diagnose_clause_gap(text).kind(),
        kind,
        "guard-seam verdict disagrees with the context-free diagnosis for {text:?}"
    );
    record_clause_gap(kind, text)
}

/// Write the gap node. The one place `Effect::unimplemented` is called for a clause gap, so
/// the wire name always comes from `ClauseGapKind::unimplemented_name`.
fn record_clause_gap(kind: ClauseGapKind, text: &str) -> Effect {
    tracing::debug!(
        gap = kind.unimplemented_name(),
        oracle_text = text,
        "clause gap recorded"
    );
    Effect::unimplemented(kind.unimplemented_name(), text)
}

// ── Phrase extractors ───────────────────────────────────────────────────────

/// The first quantity operand whose own authorities reject it, scanning markers at word
/// boundaries. An ACCEPTED operand advances the cursor rather than ending the scan: a
/// clause can carry a lowerable amount and an unlowerable one ("draw cards equal to the
/// number of creatures you control, then discard triple that many cards").
fn first_rejected_quantity_operand(lower: &str) -> Option<String> {
    let mut scan: &str = lower;
    while let Some((before, marker, after)) = scan_preceded(scan, quantity_marker) {
        // CR 608.2h. `before` and `after` are slices of `scan_preceded`'s OWN input —
        // i.e. of `scan`, not of `lower` (see `oracle_nom::primitives::scan_preceded`:
        // `offset = text.len() - remaining.len(); before = &text[..offset]`). From the
        // second iteration on, indexing `lower` with `before.len()` would span the wrong
        // string; `&scan[before.len()..]` is also panic-free by construction, because
        // `before` is a prefix slice of `scan` and its length is therefore a char
        // boundary.
        let start = match marker.operand_span() {
            OperandSpan::AfterMarker => after,
            OperandSpan::FromMarker => &scan[before.len()..],
        };
        let operand = bound_operand(start, marker.end_bounds());
        if !marker.accepts(operand) {
            return Some(operand.to_string());
        }
        scan = after;
    }
    None
}

/// The first `word`-guard whose condition text the single condition authority rejects.
///
/// `word` is a FILTER: a guard introduced by the other guard word is skipped and the
/// scan resumes after it, so the caller can ask the two questions in its own precedence
/// order. The `Skip` arms exist because "as if", "even if" and "if able" are not guards
/// at all — they are part of the action's own grammar.
fn first_rejected_guard(lower: &str, word: GuardWord) -> Option<String> {
    let mut scan: &str = lower;
    while let Some((_, marker, after)) = scan_preceded(scan, trailing_guard) {
        if marker == TrailingMarker::Guard(word) {
            // CR 608.2c: the guard gates its clause; ask the ladder whether it lowers.
            let guard = bound_operand(after, GUARD_END_BOUNDS);
            if conditions::lower_instead_condition(guard, &mut ParseContext::default()).is_none() {
                return Some(guard.to_string());
            }
        }
        scan = after;
    }
    None
}

/// The event antecedent of an unguarded "would ... instead" clause.
///
/// CR 614.1a: "instead" indicates what the replacement does, so the antecedent is the
/// event clause ahead of the comma. A clause carrying no comma is its own antecedent.
fn replacement_antecedent(lower: &str) -> String {
    opt(terminated(
        take_until::<_, _, OracleError<'_>>(", "),
        tag(", "),
    ))
    .parse(lower)
    .ok()
    .and_then(|(_, antecedent)| antecedent)
    .unwrap_or(lower)
    .trim()
    .to_string()
}

// ── Markers and their typed axes ────────────────────────────────────────────

/// A quantity marker: the word that introduces (or operates on) a dynamic amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuantityMarker {
    EqualTo,
    ForEach,
    WhereX,
    NumberOf,
    Multiplier,
}

/// Where a marker's operand starts, relative to the marker word. The typed replacement
/// for a `bool` "does the operand include the marker word" flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandSpan {
    /// The marker is a CONNECTOR: the quantity is what follows it.
    AfterMarker,
    /// The marker is an OPERATOR: it is part of the quantity's own arithmetic.
    FromMarker,
}

/// The guard word that introduces a trailing condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardWord {
    If,
    Unless,
}

/// What a trailing-guard scan found: a real guard, or a phrase that only looks like one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrailingMarker {
    /// "as if" / "even if" / "if able" — part of the action, not a gate on it.
    Skip,
    Guard(GuardWord),
}

/// The bounds of a trailing guard's condition text. A guard runs to the next clause
/// break, exactly as `split_leading_conditional` bounds a leading one.
const GUARD_END_BOUNDS: &[&str] = &[", ", "."];

impl QuantityMarker {
    /// Where this marker's operand starts.
    ///
    /// A connector marker introduces the quantity, and its `tag` has already consumed
    /// the trailing space, so the operand starts at the remainder. An OPERATOR marker is
    /// part of the amount, so the phrase the authorities must accept starts at the
    /// marker word itself: recording only a multiplier's tail hands them "that damage",
    /// which they ACCEPT (losing the verdict), while recording only the post-anaphor
    /// tail hands them "damage", which they always reject (a rubber stamp).
    fn operand_span(self) -> OperandSpan {
        match self {
            Self::EqualTo | Self::ForEach | Self::WhereX | Self::NumberOf => {
                OperandSpan::AfterMarker
            }
            Self::Multiplier => OperandSpan::FromMarker,
        }
    }

    /// The separators that bound this marker's operand. The operand ends at the
    /// SHORTEST hit, and runs to the end of the clause when none is present.
    fn end_bounds(self) -> &'static [&'static str] {
        match self {
            // " to " mirrors the damage grammar's own bound
            // (`lower::try_parse_damage_with_remainder`, `take_until(" to ")`), which is
            // where "deal damage equal to <amount> to <recipient>" stops reading the
            // amount.
            Self::EqualTo => &[", ", ".", " to "],
            Self::ForEach | Self::WhereX | Self::NumberOf => &[", ", "."],
            // " to " as above, plus " instead": CR 614.1a makes "instead" the
            // replacement indicator that FOLLOWS the amount, which is exactly where
            // `oracle_replacement::parse_damage_modification_phrase` bounds a multiplied
            // damage amount ("double that damage" / "triple that damage"). Mirror it.
            Self::Multiplier => &[", ", ".", " to ", " instead"],
        }
    }

    /// Whether this marker's own authorities accept `operand` — the same functions the
    /// rejecting grammars call, so the diagnosis cannot disagree with the parse.
    ///
    /// `parse_cda_quantity` already reaches `parse_fraction_rounded`, which is why
    /// "half that damage" operands are accepted without naming the fraction parser
    /// separately. `oracle_nom::quantity::parse_that_much_or_many` is deliberately NOT
    /// an authority here: it matches only an operand STARTING with "that much"/"that
    /// many", which no marker ever hands it.
    fn accepts(self, operand: &str) -> bool {
        match self {
            Self::EqualTo | Self::NumberOf | Self::Multiplier => {
                parse_event_context_quantity(operand).is_some()
                    || parse_cda_quantity(operand).is_some()
            }
            Self::ForEach => parse_for_each_clause_expr(operand).is_some(),
            Self::WhereX => parse_where_x_quantity_expression(operand).is_some(),
        }
    }
}

/// Nom combinator: one quantity marker. One `alt` per axis — no cross-product expansion.
fn quantity_marker(input: &str) -> OracleResult<'_, QuantityMarker> {
    alt((
        value(QuantityMarker::EqualTo, tag("equal to ")),
        value(QuantityMarker::ForEach, tag("for each ")),
        // CR 107.3: X is a placeholder for a number that needs to be determined; a
        // "where X is ..." clause is the card defining it.
        value(
            QuantityMarker::WhereX,
            alt((tag("where x is "), tag("x is "))),
        ),
        value(QuantityMarker::NumberOf, tag("a number of ")),
        // The trailing `tag(" ")` is the word-boundary guard, so "halfling" cannot
        // match, and `peek(multiplicand_head)` is the word-SENSE guard. Neither
        // consumes the anaphor: the operand starts at the marker word and keeps
        // everything after it.
        value(
            QuantityMarker::Multiplier,
            terminated(
                alt((tag("twice"), tag("double"), tag("triple"), tag("half"))),
                preceded(tag(" "), peek(multiplicand_head)),
            ),
        ),
    ))
    .parse(input)
}

/// Nom combinator: the head of a MULTIPLICAND — the noun phrase an arithmetic
/// multiplier operates on.
///
/// A word boundary alone does not make `double`/`triple`/`twice`/`half` arithmetic.
/// Without a multiplicand the same words are:
///
/// * a keyword ability's own name — double strike (CR 702.4), triple strike, double
///   team ("the creature gains double strike");
/// * an adverb of frequency on the action — "twice each turn", "twice this turn";
/// * part of a card name or a saga chapter label — "conjure a card named Think Twice
///   into your graveyard", "Double Deal deals 3 damage".
///
/// None of those is an amount, so the quantity authorities reject them and the clause
/// is misreported as a quantity gap. This is an ALLOWLIST of the determiner and number
/// shapes an amount is actually written with — not a blocklist of the words above — so
/// it covers the class rather than the cards that exhibit it today. It is used under
/// `peek`, so it identifies the multiplicand without consuming it.
///
/// Three genuinely arithmetic senses the allowlist refuses today, recorded so they are
/// not re-derived. None is a gap node, so nothing is lost yet:
///
/// * a bare "instead" — "copy that spell twice instead" (Increasing Vengeance, Sea Gate
///   Stormcaller, Tomb of Horrors Adventurer), "investigate twice instead" (Secrets of
///   the Key), "proliferate twice instead" (Tekuthal, Inquiry Dominus): 5 corpus sites,
///   numeric but naming no multiplicand;
/// * "of" — "exiles the top half of their library" (Ulamog, the Defiler): 1 site, where
///   `half` HEADS a noun phrase instead of operating on one;
/// * "for" — "investigate twice for each card discarded this way" (Tamiyo Meets the
///   Story Circle): 1 site, where the multiplicand is reached through a `for each`
///   clause rather than through a determiner.
///
/// "one and one-half mana" (City of Ass) is deliberately NOT in that list, and the
/// reason is a BOUNDARY rule rather than a sense rule — the two notions of "word
/// boundary" differ. A regex `\b` fires after the hyphen in "one-half", but
/// `scan_preceded` retries only at WHITESPACE-delimited boundaries, so "half" never
/// starts a token there and no marker fires at all.
///
/// KNOWN ASYMMETRY, not an oversight: `this`/`each` dispatch on the following noun via
/// [`this_or_each_on_an_amount`], so "twice this turn" / "twice each turn" are refused
/// as adverbial frequencies, but `tag("that ")` admits "that turn" unconditionally. It
/// is latent rather than wrong — no card's Oracle text contains "<multiplier> that turn"
/// today; the raw corpus hits for that phrase are all in `rulings`.
fn multiplicand_head(input: &str) -> OracleResult<'_, ()> {
    alt((
        // Definite and anaphoric determiners: "double THAT damage", "twice THE number
        // of times it was kicked".
        value((), alt((tag("that "), tag("those "), tag("the ")))),
        // Possessive determiners: the closed-class ones carry no clitic.
        value((), alt((tag("its "), tag("their "), tag("your ")))),
        possessive_clitic_determiner,
        // Quantifier determiners: "Double ALL damage ...", "Double ANY effect ...".
        value((), alt((tag("all "), tag("any ")))),
        // Object-selector determiners: "double TARGET creature's power", "Double
        // EQUIPPED creature's power", and its Aura sibling.
        value(
            (),
            alt((tag("target "), tag("equipped "), tag("enchanted "))),
        ),
        // Comparative multiplicand: "it produces twice AS MUCH of that mana instead".
        value((), alt((tag("as much"), tag("as many")))),
        this_or_each_on_an_amount,
        multiplicand_amount,
    ))
    .parse(input)
}

/// Nom combinator: a possessive determiner formed with the possessive clitic —
/// "double ~'S power". The owner is open-class, so it is taken rather than enumerated;
/// [`multiplicand_head`] lists the clitic-less possessives separately.
///
/// The owner is bounded to ONE whitespace-delimited token: `take_till1` stops at the
/// first whitespace *or* clitic, whichever comes first, so a multi-word owner is
/// refused. That bound is sufficient because `oracle_util::normalize_card_name_refs`
/// rewrites a card's reference to itself — full name, comma-form short name, and the
/// `SELF_REF_TYPE_PHRASES` ("this creature", "this artifact") — to `~` before the
/// diagnoser ever sees the clause. Casey Jones, Asphalt Hooligan ("Double Casey Jones's
/// power"), Targ Nar, Demon-Fang Gnoll and Tifa Lockhart each print a multi-word owner
/// and each reach this arm as "double ~'s power". Measured over the corpus: all 7 sites
/// this arm accepts are `~'s` and none presents a multi-word owner, so the single-token
/// bound is LATENT — reachable only by a clitic owner that is not the card itself.
fn possessive_clitic_determiner(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            take_till1(|c: char| c.is_whitespace() || c == '\'' || c == '\u{2019}'),
            alt((tag("'s"), tag("\u{2019}s"))),
            alt((value((), tag(" ")), value((), eof))),
        ),
    )
    .parse(input)
}

/// Nom combinator: "this"/"each" determining an AMOUNT rather than a turn window.
///
/// Nested prefix dispatch (the two determiners share a head and split on the noun):
/// "Double THIS CREATURE's power" is arithmetic, while "twice THIS TURN" and "twice
/// EACH TURN" are adverbial frequencies on the action — the same distinction
/// `duration::parse_current_phase_duration` draws when it reads "this turn" / "this
/// combat" as a window rather than a quantity.
fn this_or_each_on_an_amount(input: &str) -> OracleResult<'_, ()> {
    preceded(
        alt((tag("this "), tag("each "))),
        not(alt((tag("turn"), tag("combat"), tag("game")))),
    )
    .parse(input)
}

/// Nom combinator: an amount written as a literal or a variable — "3", "three", "X",
/// "{X}", "-X/-X". Composed from the existing numeric primitive. The sign and the `/`
/// tail are the P/T spelling ("get twice -X/-X"); the head alone identifies the
/// multiplicand, so the pair itself is not re-parsed here.
fn multiplicand_amount(input: &str) -> OracleResult<'_, ()> {
    alt((
        // A mana amount ("counter target spell unless its controller pays twice {X}").
        // `primitives::parse_mana_symbol` cannot be the authority here: its tags are
        // the printed UPPERCASE spelling, and `diagnose_clause_gap` lowercases before
        // it scans, so the symbol is only identifiable by its case-free delimiter.
        value((), char('{')),
        value((), preceded(opt(one_of("+-")), parse_number_or_x)),
    ))
    .parse(input)
}

/// Nom combinator: a trailing guard word, or a look-alike phrase to skip past.
fn trailing_guard(input: &str) -> OracleResult<'_, TrailingMarker> {
    alt((
        value(TrailingMarker::Skip, tag("as if ")),
        value(TrailingMarker::Skip, tag("even if ")),
        value(TrailingMarker::Skip, tag("if able")),
        value(TrailingMarker::Guard(GuardWord::If), tag("if ")),
        value(TrailingMarker::Guard(GuardWord::Unless), tag("unless ")),
    ))
    .parse(input)
}

/// Nom combinator: the clause-head verb at `input`'s start, deconjugated through
/// `normalize_verb_token` and filtered by the clause-head vocabulary. Returns the verb
/// together with the text that follows it, so a scanning caller gets the arguments half
/// without re-parsing.
fn clause_head_verb_token(input: &str) -> OracleResult<'_, (String, &str)> {
    let (rest, token) = take_till1::<_, _, OracleError<'_>>(char::is_whitespace).parse(input)?;
    // `is_clause_head_verb` deconjugates its own argument, so it is asked about the RAW
    // token. Deconjugating here first and asking about the result would normalize
    // twice, and `normalize_verb_token` is not idempotent on a possessive: "roll's" →
    // "roll'" → "roll". That second pass matches the vocabulary on a NOUN the clause
    // never used as a verb ("target die roll's result"), and the verdict then reports
    // the malformed intermediate "roll'" — a token the vocabulary was never asked
    // about. One pass keeps the reported verb and the consulted verb the same string.
    if !is_clause_head_verb(token) {
        return Err(oracle_err(input));
    }
    Ok((rest, (normalize_verb_token(token), rest)))
}

/// The clause's first whitespace-delimited token, or `""` for an empty clause.
fn first_token(lower: &str) -> &str {
    take_till1::<_, _, OracleError<'_>>(char::is_whitespace)
        .parse(lower)
        .map_or("", |(_, token)| token)
}

/// Bound `operand` at the SHORTEST of `bounds`, defaulting to the whole remainder when
/// none is present.
fn bound_operand<'a>(operand: &'a str, bounds: &[&str]) -> &'a str {
    let mut end = operand.len();
    for separator in bounds {
        if let Ok((_, Some(before))) =
            opt(take_until::<_, _, OracleError<'_>>(*separator)).parse(operand)
        {
            end = end.min(before.len());
        }
    }
    operand[..end].trim()
}

// ── Clause-head verb vocabulary ─────────────────────────────────────────────
//
// Union of three sources:
// A) PREDICATE_VERBS from subject.rs (used for subject-predicate splitting)
// B) Additional first-word verbs from parse_imperative_family_ast match arms
// C) Pre-dispatch verbs from parse_effect_clause and lower_imperative_clause
//
// (B) and (C) are CLAUSE_HEAD_VERBS below. This is the ONE definition in the workspace:
// it lives beside the dispatcher it mirrors, and `game::gap_analysis` imports
// `is_clause_head_verb` from here rather than keeping a second copy.
//
// NOTE: when adding verbs to parse_imperative_family_ast, also add them here.

/// Additional verbs from `parse_imperative_family_ast` and the pre-dispatch arms, not in
/// `PREDICATE_VERBS`.
pub(crate) const CLAUSE_HEAD_VERBS: &[&str] = &[
    "spend",
    "double",
    "triple",
    "destroy",
    "prevent",
    "attach",
    "unattach",
    "seek",
    "amass",
    "incubate",
    "attacks",
    "attack",
    "monstrosity",
    "flip",
    "roll",
    "note",
    "manifest",
    "investigate",
    "proliferate",
    "suspect",
    "blight",
    "forage",
    "collect",
    "endure",
    "goad",
    "detain",
    "exchange",
    "must",
    "earthbend",
    "airbend",
    "bounce",
    "support",
    "equip",
    "remove",
    "switch",
    "populate",
    "clash",
    "planeswalk",
    "recruit",
    "assimilate",
    // Pre-dispatch verbs handled in `parse_effect_clause` before imperative dispatch.
    "tempt",      // "the ring tempts you"
    "discover",   // "discover N"
    "distribute", // "distribute N counters among"
];

/// True when `verb` (conjugated or not) is a clause head the imperative dispatcher
/// recognises.
pub(crate) fn is_clause_head_verb(verb: &str) -> bool {
    let normalized = normalize_verb_token(verb);
    let n = normalized.as_str();
    PREDICATE_VERBS.contains(&n) || CLAUSE_HEAD_VERBS.contains(&n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_ir::diagnostic::ClauseGapKind;
    use strum::IntoEnumIterator;

    fn kind(text: &str) -> ClauseGapKind {
        diagnose_clause_gap(text).kind()
    }

    // V1 — a rejected quantity operand is a `Quantity` verdict, with the operand spanned
    // per `OperandSpan` and bounded by the marker's own boundary set.

    #[test]
    fn rejected_equal_to_operand_is_a_quantity_gap() {
        // Toralf's shape. Every quantity authority rejects "the excess".
        assert_eq!(
            diagnose_clause_gap("deal damage equal to the excess to any target"),
            ClauseGap::Quantity {
                operand: "the excess".to_string()
            }
        );
    }

    #[test]
    fn an_accepted_equal_to_operand_is_not_a_quantity_gap() {
        // Reach-guard for the rule above: `parse_cda_quantity` ACCEPTS "the number of
        // creatures you control", so the scan advances and the clause is diagnosed by a
        // later rule. Without this the Quantity arm could be a rubber stamp.
        const ACCEPTED: &str = "the number of creatures you control";
        assert!(
            QuantityMarker::EqualTo.accepts(ACCEPTED),
            "reach-guard precondition: the authorities must accept {ACCEPTED}"
        );
        assert_eq!(
            diagnose_clause_gap(
                "deal damage equal to the number of creatures you control to any target"
            ),
            ClauseGap::VerbArguments {
                verb: "deal".to_string(),
                arguments: "damage equal to the number of creatures you control to any target"
                    .to_string(),
            }
        );
    }

    #[test]
    fn equal_to_operand_is_bounded_where_the_damage_grammar_bounds_it() {
        // Tetsuo's shape: the operand stops at " to ", exactly where
        // `try_parse_damage_with_remainder`'s `take_until(" to ")` stops reading it.
        const OPERAND: &str = "the greatest mana value among equipment attached";
        assert_eq!(
            diagnose_clause_gap(
                "deal damage equal to the greatest mana value among equipment attached to it to any target"
            ),
            ClauseGap::Quantity {
                operand: OPERAND.to_string()
            }
        );
    }

    #[test]
    fn quantity_acceptance_consults_only_the_markers_own_authorities() {
        // Goblin Charbelcher's operand. `parse_for_each_clause_expr` and
        // `parse_where_x_quantity_expression` accept it, but an `EqualTo` marker asks
        // only `parse_event_context_quantity` / `parse_cda_quantity` — which reject it.
        // This is what makes the per-marker authority split load-bearing.
        const OPERAND: &str = "the number of nonland cards revealed this way";
        assert!(!QuantityMarker::EqualTo.accepts(OPERAND));
        assert!(
            QuantityMarker::ForEach.accepts(OPERAND) || QuantityMarker::WhereX.accepts(OPERAND)
        );
        assert_eq!(
            diagnose_clause_gap(
                "deal damage equal to the number of nonland cards revealed this way"
            ),
            ClauseGap::Quantity {
                operand: OPERAND.to_string()
            }
        );
    }

    #[test]
    fn a_multiplier_operand_spans_from_the_marker_word() {
        // Jeska's and AMMR's interim shape. The operand must INCLUDE the multiplier:
        // "that damage" alone is accepted, so spanning only the tail loses the verdict.
        assert_eq!(
            diagnose_clause_gap("deal double that damage instead"),
            ClauseGap::Quantity {
                operand: "double that damage".to_string()
            }
        );
        assert_eq!(
            diagnose_clause_gap("deal triple that damage to that player instead"),
            ClauseGap::Quantity {
                operand: "triple that damage".to_string()
            }
        );
        // The measured reason the span matters: the tail alone IS accepted.
        assert!(QuantityMarker::Multiplier.accepts("that damage"));
    }

    #[test]
    fn an_accepted_multiplier_operand_advances_the_scan() {
        // Clause-level reach-guard, and a genuinely `accepts`-true Multiplier case:
        // `parse_cda_quantity` reaches `parse_fraction_rounded`, so "half that damage"
        // lowers and the clause yields no Quantity verdict at all.
        assert!(QuantityMarker::Multiplier.accepts("half that damage"));
        assert_ne!(
            kind("prevent half that damage, rounded down"),
            ClauseGapKind::Quantity
        );
    }

    #[test]
    fn multiplier_acceptance_is_operand_level_not_clause_level() {
        // `accepts` is true for the bare operand ...
        assert!(QuantityMarker::Multiplier.accepts("twice that much"));
        // ... but the clause's own bounds do not stop at " damage", so the operand the
        // scan actually hands the authorities is longer, and rejected.
        assert_eq!(
            diagnose_clause_gap("deal twice that much damage"),
            ClauseGap::Quantity {
                operand: "twice that much damage".to_string()
            }
        );
    }

    #[test]
    fn a_second_marker_is_spanned_off_the_cursor_not_the_clause() {
        // Iteration 2 of the scan. The first marker (EqualTo) is ACCEPTED and advances
        // the cursor; the second (Multiplier) needs `&scan[before.len()..]`. Spanning
        // off the whole clause instead yields "ontrol" here — and, on a non-ASCII
        // description, a `&str` char-boundary panic.
        assert_eq!(
            diagnose_clause_gap(
                "draw cards equal to the number of creatures you control, then discard triple that many cards"
            ),
            ClauseGap::Quantity {
                operand: "triple that many cards".to_string()
            }
        );
    }

    #[test]
    fn an_operand_with_no_boundary_runs_to_the_end_of_the_clause() {
        const OPERAND: &str = "four times that spell's mana value on ~";
        assert_eq!(
            diagnose_clause_gap(
                "put charge counters equal to four times that spell's mana value on ~"
            ),
            ClauseGap::Quantity {
                operand: OPERAND.to_string()
            }
        );
    }

    #[test]
    fn the_multiplier_word_boundary_guard_refuses_halfling() {
        // The Multiplier arm's trailing `tag(" ")`: "halfling" is not "half".
        assert_ne!(
            kind("halfling scout enters with a counter"),
            ClauseGapKind::Quantity
        );
    }

    /// Combinator-level table for the Multiplier arm's word-SENSE guard: every
    /// multiplicand head shape [`multiplicand_head`] admits must still let the marker
    /// fire. Asserted on `quantity_marker` rather than on the verdict so an operand
    /// that a quantity authority happens to ACCEPT cannot silently mask a head the
    /// guard rejected.
    #[test]
    fn the_multiplier_sense_guard_admits_every_multiplicand_head_shape() {
        for phrase in [
            "double that damage",                      // anaphoric determiner
            "double those counters",                   // plural anaphor
            "twice the number of times it was kicked", // definite determiner
            "double its power",                        // possessive determiner
            "half their life total",
            "half your life total",
            "double ~'s power", // possessive clitic
            "half Okaun's power",
            "double all damage that creature would deal", // quantifier determiner
            "double any effect that doubles",
            "double target creature's power", // object-selector determiner
            "double equipped creature's power",
            "double enchanted creature's power",
            "twice as much of that mana", // comparative multiplicand
            "twice as many cards",
            "double this creature's power", // `this`/`each` on an AMOUNT
            "double each player's life total",
            "twice X loyalty counters", // variable / literal amounts
            "twice -X/-X",
            "twice {X}",
            "half 6 damage",
            "double three counters",
        ] {
            // `diagnose_clause_gap` lowercases before it scans, so the combinator only
            // ever sees lowercase text; feeding it the printed casing here would test a
            // call shape production never makes (and "X" would miss `parse_number_or_x`).
            let lower = phrase.to_lowercase();
            assert!(
                matches!(quantity_marker(&lower), Ok((_, QuantityMarker::Multiplier))),
                "`{phrase}` heads a real multiplicand, so the Multiplier marker must fire"
            );
        }
    }

    /// The refused SENSES. CR 702.4 makes "double strike" a keyword ability's NAME,
    /// not arithmetic; "twice each turn" / "twice this turn" are adverbs of frequency;
    /// "Think Twice" is a card name. Each is paired with a reach-guard that differs
    /// only in the multiplicand head, so the negative cannot pass because the clause
    /// never reached the quantity rule.
    #[test]
    fn the_multiplier_sense_guard_refuses_keyword_names_and_frequency_adverbs() {
        for (refused, reached) in [
            // CR 702.4 Double Strike, and its triple-strike / double-team siblings.
            (
                "the creature that attacked gains double strike",
                "the creature that attacked gains double that many counters",
            ),
            (
                "the creature gains triple strike",
                "the creature gains triple that many counters",
            ),
            (
                "nontoken creatures you control perpetually gain double team",
                "nontoken creatures you control perpetually gain double that many counters",
            ),
            // Adverbial frequency: the multiplier counts ACTIONS over a turn window.
            (
                "this ability triggers only twice each turn",
                "this ability triggers only twice each player's counters",
            ),
            (
                "activate loyalty abilities of ~ twice this turn rather than only once",
                "activate loyalty abilities of ~ twice this creature's counters rather than only once",
            ),
            // A card name that merely contains a multiplier word.
            (
                "conjure a card named Think Twice into your graveyard",
                "conjure a card named Think Twice its graveyard",
            ),
        ] {
            assert!(
                !matches!(
                    scan_preceded(&refused.to_lowercase(), quantity_marker),
                    Some((_, QuantityMarker::Multiplier, _))
                ),
                "`{refused}` uses the word in a non-arithmetic sense; the Multiplier \
                 marker must not fire anywhere in it"
            );
            assert!(
                matches!(
                    scan_preceded(&reached.to_lowercase(), quantity_marker),
                    Some((_, QuantityMarker::Multiplier, _))
                ),
                "reach-guard for `{refused}`: the minimal pair `{reached}` differs only \
                 in the multiplicand head and MUST still reach the Multiplier marker"
            );
        }
    }

    /// Verdict-level discrimination on the real printed clauses the guard moves.
    /// Reverting the `peek(multiplicand_head)` guard flips every `assert_ne!` here.
    #[test]
    fn keyword_and_frequency_clauses_are_not_quantity_gaps() {
        // Aradesh, the Founder / Hat Trick / Sworn to the Legion — keyword grants.
        assert_ne!(
            kind("the creature that attacked gains double strike"),
            ClauseGapKind::Quantity
        );
        assert_ne!(
            kind("nontoken creatures you control perpetually gain double team"),
            ClauseGapKind::Quantity
        );
        // Nadu, Winged Wisdom / Urza Assembles the Titans — frequency adverbs.
        assert_ne!(
            kind("This ability triggers only twice each turn"),
            ClauseGapKind::Quantity
        );
        assert_ne!(
            kind(
                "activate the loyalty abilities of planeswalkers you control twice \
                 this turn rather than only once"
            ),
            ClauseGapKind::Quantity
        );
        // Paired positives from the same corpus: genuine multipliers the guard keeps.
        // Approach My Molten Realm, Nuclear Fallout, Unbound Flourishing.
        assert_eq!(
            kind("deal double that damage instead"),
            ClauseGapKind::Quantity
        );
        assert_eq!(kind("get twice -X/-X"), ClauseGapKind::Quantity);
        assert_eq!(kind("double the value of X"), ClauseGapKind::Quantity);
    }

    #[test]
    fn every_quantity_marker_produces_a_quantity_gap_on_a_rejected_operand() {
        assert_eq!(
            kind("draw a card for each squirrel bearing a tiny hat"),
            ClauseGapKind::Quantity
        );
        assert_eq!(
            kind("draw x cards where x is the number of squirrels bearing tiny hats"),
            ClauseGapKind::Quantity
        );
        assert_eq!(
            kind("draw a number of squirrels bearing tiny hats"),
            ClauseGapKind::Quantity
        );
    }

    // V2 — a trailing guard the condition ladder rejects is a `Condition` verdict.

    #[test]
    fn a_rejected_trailing_guard_is_a_condition_gap() {
        assert_eq!(
            diagnose_clause_gap("turn it face up if it's face down"),
            ClauseGap::Condition {
                guard: "it's face down".to_string()
            }
        );
    }

    #[test]
    fn an_accepted_trailing_guard_is_not_a_condition_gap() {
        // Reach-guard: the ladder lowers this guard, so rule 4 must not claim the clause.
        const ACCEPTED: &str = "you control a creature";
        assert!(
            conditions::lower_instead_condition(ACCEPTED, &mut ParseContext::default()).is_some(),
            "reach-guard precondition: the ladder must accept {ACCEPTED}"
        );
        assert_ne!(
            kind("scry 1 if you control a creature"),
            ClauseGapKind::Condition
        );
    }

    #[test]
    fn the_guard_scan_skips_phrases_that_only_look_like_guards() {
        for clause in [
            "you may play that card as if it were in your hand",
            "draw a card even if you already drew a card this turn",
            "the squirrel attacks each combat if able",
        ] {
            assert_ne!(
                kind(clause),
                ClauseGapKind::Condition,
                "'{clause}' carries no real guard"
            );
        }
    }

    #[test]
    fn an_unless_guard_is_a_condition_gap_too() {
        assert_eq!(
            diagnose_clause_gap("destroy that squirrel unless an opponent pays {2}"),
            ClauseGap::Condition {
                guard: "an opponent pays {2}".to_string()
            }
        );
    }

    // V3 — an unguarded "would ... instead" clause is a `Replacement` verdict.

    #[test]
    fn an_unguarded_event_replacement_is_a_replacement_gap() {
        const ANTECEDENT: &str = "the next time you would draw a card this turn";
        assert_eq!(
            diagnose_clause_gap(
                "the next time you would draw a card this turn, instead look at the top x cards"
            ),
            ClauseGap::Replacement {
                antecedent: ANTECEDENT.to_string()
            }
        );
    }

    #[test]
    fn instead_without_an_event_is_not_a_replacement_gap() {
        // Both halves of the conjunction are required: no "would", no replacement.
        assert_ne!(kind("draw two cards instead"), ClauseGapKind::Replacement);
    }

    // V4 — a leading guard. Rule 1 has no Phase-1 production entry (a leading guard is
    // split off before the imperative fallback runs), so these test the pure rule.

    #[test]
    fn a_leading_event_guard_is_a_replacement_gap() {
        const AMMR: &str = "a source would deal damage";
        assert_eq!(
            diagnose_clause_gap(
                "if a source would deal damage, it deals double that damage instead"
            ),
            ClauseGap::Replacement {
                antecedent: AMMR.to_string()
            }
        );
        const JESKA: &str = "that creature would deal combat damage to one of your opponents";
        assert_eq!(
            diagnose_clause_gap(
                "if that creature would deal combat damage to one of your opponents, it deals triple that damage to that player instead"
            ),
            ClauseGap::Replacement {
                antecedent: JESKA.to_string()
            }
        );
    }

    #[test]
    fn a_leading_state_guard_the_ladder_rejects_is_a_condition_gap() {
        assert_eq!(
            diagnose_clause_gap("if it's spring, search your library for a squirrel"),
            ClauseGap::Condition {
                guard: "it's spring".to_string()
            }
        );
    }

    #[test]
    fn a_leading_guard_that_lowers_recurses_into_the_body() {
        // Reach-guard for the recursion: the guard lowers, so the verdict must come from
        // the BODY's failure, not from the guard.
        const GUARD: &str = "you control a creature";
        assert!(
            conditions::lower_instead_condition(GUARD, &mut ParseContext::default()).is_some(),
            "reach-guard precondition: the ladder must accept {GUARD}"
        );
        assert_eq!(
            diagnose_clause_gap(
                "if you control a creature, deal damage equal to the excess to any target"
            ),
            ClauseGap::Quantity {
                operand: "the excess".to_string()
            }
        );
    }

    #[test]
    fn the_leading_guard_phrase_excludes_its_connector() {
        // `parse_leading_conditional_prefix` is the one prefix authority, so "then if"
        // never leaks into the reported guard.
        assert_eq!(
            diagnose_clause_gap("then if it's spring, search your library for a squirrel"),
            ClauseGap::Condition {
                guard: "it's spring".to_string()
            }
        );
    }

    // V5 — a recognised head verb whose arguments fail is a `VerbArguments` verdict.

    #[test]
    fn a_recognised_head_with_failing_arguments_is_a_verb_arguments_gap() {
        // Flaming Gambit's shape — a redirect CHOICE, not a replacement (no "would").
        assert_eq!(
            diagnose_clause_gap("deal that damage to it instead"),
            ClauseGap::VerbArguments {
                verb: "deal".to_string(),
                arguments: "that damage to it instead".to_string(),
            }
        );
    }

    #[test]
    fn a_subject_led_clause_finds_its_verb_after_the_subject() {
        assert_eq!(
            diagnose_clause_gap(
                "target player and each of that player's teammates exchange life totals"
            ),
            ClauseGap::VerbArguments {
                verb: "exchange".to_string(),
                arguments: "life totals".to_string(),
            }
        );
    }

    #[test]
    fn an_inflected_head_is_deconjugated() {
        assert_eq!(
            diagnose_clause_gap("deals that damage to it instead"),
            ClauseGap::VerbArguments {
                verb: "deal".to_string(),
                arguments: "that damage to it instead".to_string(),
            }
        );
    }

    // V6 — an unrecognised head.

    #[test]
    fn an_unrecognised_head_is_reported_as_the_head() {
        assert_eq!(
            diagnose_clause_gap("otherwise"),
            ClauseGap::UnrecognizedHead {
                head: "otherwise".to_string()
            }
        );
        assert_eq!(
            diagnose_clause_gap("the horde casts that card"),
            ClauseGap::UnrecognizedHead {
                head: "the".to_string()
            }
        );
        // "move" is in NEITHER vocabulary, so it falls through rule 5 and rule 6.
        assert!(!is_clause_head_verb("move"));
        assert_eq!(
            diagnose_clause_gap("move a +1/+1 counter from that creature onto another creature"),
            ClauseGap::UnrecognizedHead {
                head: "move".to_string()
            }
        );
    }

    /// Scooch's shape. `normalize_verb_token` is not idempotent on a possessive —
    /// "roll's" → "roll'" → "roll" — so consulting the vocabulary with an
    /// already-normalized token deconjugates twice and matches a NOUN the clause never
    /// used as a verb. `clause_head_verb_token` therefore normalizes exactly once, and
    /// the token it reports is the token it asked about.
    #[test]
    fn a_possessive_noun_does_not_match_the_verb_vocabulary() {
        // The non-idempotence, and the second pass that used to reach the vocabulary.
        assert_eq!(normalize_verb_token("roll's"), "roll'");
        assert!(is_clause_head_verb("roll'"));
        // One pass does not match, so the possessive noun is not a clause head.
        assert!(!is_clause_head_verb("roll's"));
        // Reach-guard: the same vocabulary still recognises the real verb in the same
        // scanning position, so the negative above is not a dead scan.
        assert_eq!(
            diagnose_clause_gap("target player rolls a die with unreadable faces"),
            ClauseGap::VerbArguments {
                verb: "roll".to_string(),
                arguments: "a die with unreadable faces".to_string(),
            }
        );
        // Scooch's clause: no longer reported as arguments to a verb named "roll'",
        // a token the vocabulary was never asked about.
        assert_eq!(
            diagnose_clause_gap("target player's life total, or target die roll's result"),
            ClauseGap::UnrecognizedHead {
                head: "target".to_string()
            }
        );
    }

    #[test]
    fn an_empty_clause_reports_an_empty_head_and_does_not_panic() {
        // The old first-word fallback invented "unknown" here, colliding with the
        // whole-line `Effect:unknown` marker. An empty head is the honest answer.
        assert_eq!(
            diagnose_clause_gap(""),
            ClauseGap::UnrecognizedHead {
                head: String::new()
            }
        );
    }

    // V9 — self-consistency: the name a clause is recorded under decodes back to the
    // kind the diagnoser reports for the same text.

    #[test]
    fn every_recorded_name_decodes_to_the_diagnosed_kind() {
        const ROWS: &[&str] = &[
            // Replacement
            "the next time you would draw a card this turn, instead look at the top x cards",
            "if a source would deal damage, it deals double that damage instead",
            "if that creature would deal combat damage to one of your opponents, it deals triple that damage to that player instead",
            "if a squirrel would die, exile it instead",
            "if you would draw a card, instead draw two",
            // Condition
            "turn it face up if it's face down",
            "destroy that squirrel unless an opponent pays {2}",
            "if it's spring, search your library for a squirrel",
            "then if it's spring, search your library for a squirrel",
            "scry 1 if the squirrel wore a tiny hat",
            // Quantity
            "deal damage equal to the excess to any target",
            "deal double that damage instead",
            "deal twice that much damage",
            "draw a card for each squirrel bearing a tiny hat",
            "draw x cards where x is the number of squirrels bearing tiny hats",
            "put charge counters equal to four times that spell's mana value on ~",
            // VerbArguments
            "deal that damage to it instead",
            "target player and each of that player's teammates exchange life totals",
            "assimilate target creature you control",
            "create copies of each nonland card among them",
            "note the type and amount of mana spent this way",
            // UnrecognizedHead
            "otherwise",
            "the horde casts that card",
            "move a +1/+1 counter from that creature onto another creature",
            "",
        ];
        // A zero denominator measures nothing: pin the row count and every kind's
        // presence before asserting the invariant.
        assert_eq!(ROWS.len(), 25, "the consistency table must stay ≥ 25 rows");
        let mut seen: Vec<ClauseGapKind> = Vec::new();
        for text in ROWS {
            let diagnosed = diagnose_clause_gap(text).kind();
            let Effect::Unimplemented { name, description } = clause_gap_unimplemented(text) else {
                panic!("clause_gap_unimplemented must mint an Unimplemented node");
            };
            assert_eq!(
                ClauseGapKind::from_unimplemented_name(&name),
                Some(diagnosed),
                "the recorded name for {text:?} must decode to its diagnosed kind"
            );
            assert_eq!(
                description.as_deref(),
                Some(*text),
                "the recorded fragment must be byte-identical to the input"
            );
            seen.push(diagnosed);
        }
        for kind in ClauseGapKind::iter() {
            assert!(
                seen.contains(&kind),
                "the table must span every kind, or the invariant is untested for {kind:?}"
            );
        }
    }

    // V15 — the verb vocabulary, moved verbatim from `game/gap_analysis.rs`. This is a
    // RELOCATION test and is self-referential by construction: it cannot pin the
    // vocabulary against `parse_imperative_family_ast`'s dispatch arms, which the NOTE
    // at `imperative.rs` records as hand-maintained. Because the constants moved
    // verbatim, mirror fidelity cannot regress here.

    #[test]
    fn recognized_verbs_cover_predicate_verbs() {
        for verb in PREDICATE_VERBS {
            assert!(
                is_clause_head_verb(verb),
                "PREDICATE_VERB '{}' not recognized",
                verb
            );
        }
    }

    #[test]
    fn recognized_verbs_cover_clause_head_verbs() {
        for verb in CLAUSE_HEAD_VERBS {
            assert!(
                is_clause_head_verb(verb),
                "CLAUSE_HEAD_VERB '{}' not recognized",
                verb
            );
        }
    }

    #[test]
    fn deconjugated_verbs_recognized() {
        assert!(is_clause_head_verb("destroys"));
        assert!(is_clause_head_verb("draws"));
        assert!(is_clause_head_verb("creates"));
        assert!(is_clause_head_verb("has")); // → "have"
        assert!(is_clause_head_verb("copies")); // → "copy"
    }

    #[test]
    fn a_non_verb_is_not_a_clause_head() {
        // `normalize_verb_token` deliberately does not invent stems for unknown verbs.
        assert!(!is_clause_head_verb("freeze"));
        assert!(!is_clause_head_verb("otherwise"));
    }
}
