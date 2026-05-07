use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1, take_until};
use nom::combinator::value;
use nom::Parser;

use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_target::{
    parse_mana_value_suffix, parse_shared_quality_clause, parse_target, parse_type_phrase,
};
use super::super::oracle_util::{
    contains_possessive, infer_core_type_for_subtype, split_around, strip_after,
};
use super::{capitalize, scan_contains_phrase, ParseContext};
use crate::parser::oracle_ir::ast::{SearchLibraryDetails, SeekDetails};
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::types::ability::{
    Comparator, ControllerRef, FilterProp, ObjectScope, QuantityExpr, QuantityRef,
    SearchSelectionConstraint, SharedQuality, SharedQualityRelation, TargetFilter, TypeFilter,
    TypedFilter,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::keywords::Keyword;
use crate::types::zones::Zone;

/// Scan `lower` at word boundaries for `tag_prefix`, then apply `combinator` to the
/// remainder. Returns `(parsed_value, byte_offset_in_lower_of_tail)` on first match.
///
/// Prefer this over `strip_after` + nom for composable multi-position parsing —
/// matches start-of-string, spaces, commas, or semicolons as word boundaries.
fn scan_preceded<'a, T>(
    lower: &'a str,
    tag_prefix: &'static str,
    mut combinator: impl FnMut(&'a str) -> Result<(&'a str, T), nom::Err<OracleError<'a>>>,
) -> Option<(T, usize)> {
    let mut search_from = 0;
    while search_from <= lower.len() {
        let idx = lower[search_from..]
            .find(tag_prefix)
            .map(|i| search_from + i)?;
        // Word-boundary check: must be at start or preceded by whitespace/punctuation.
        let at_boundary = idx == 0
            || matches!(
                lower.as_bytes()[idx - 1],
                b' ' | b',' | b';' | b'(' | b'.' | b'\n' | b'\t'
            );
        if at_boundary {
            let after_prefix = &lower[idx + tag_prefix.len()..];
            if let Ok((rest, val)) = combinator(after_prefix) {
                let offset = lower.len() - rest.len();
                return Some((val, offset));
            }
        }
        search_from = idx + 1;
    }
    None
}

pub(super) fn parse_search_library_details(
    lower: &str,
    ctx: &mut ParseContext,
) -> SearchLibraryDetails {
    let reveal = scan_contains_phrase(lower, "reveal");

    // CR 701.23a: Detect "search target opponent's/player's library" patterns.
    // These target a player, searching that player's library instead of the controller's.
    let target_player = parse_search_target_player(lower);

    // CR 107.1c: "any number of [FILTER] cards" — searcher may find 0..=matching.len()
    // cards. Detected before "up to N" since they share no overlap: "any number of"
    // emits a sentinel count that is capped to the matching-set size at resolution.
    let any_number_tail = scan_after_tag(lower, "any number of ");

    // Extract count from "up to N" / "up to X" (must be done before filter extraction
    // since "for up to five creature cards" needs to skip the count to find the type).
    // CR 107.3a + CR 601.2b: X resolves to the caster's announced value at cast time.
    let up_to_match = scan_preceded(lower, "up to ", nom_quantity::parse_quantity_expr_number);

    // Fallback: "for N cards" / "for X cards" without "up to".
    let for_match = if up_to_match.is_none() && any_number_tail.is_none() {
        scan_preceded(lower, "for ", nom_quantity::parse_quantity_expr_number)
            // Require a word break after the number (" cards" / " creature ...").
            // Guards against matching "for a", "for an", etc. where parse_number fails
            // (good) but also avoids partial matches like "for the".
            .filter(|(_, off)| lower.as_bytes().get(*off).is_some_and(|b| *b == b' '))
    } else {
        None
    };

    // CR 107.1c + CR 701.23d: up_to=true ⇒ searcher picks 0..=count (vs. exactly count).
    // "any number of" uses i32::MAX as an unbounded ceiling — the resolver floors it
    // against matching.len(), so the effective ceiling is always the legal-option set.
    let (count, count_end_in_for, up_to) = match (any_number_tail, up_to_match, for_match) {
        (Some(off), _, _) => (QuantityExpr::Fixed { value: i32::MAX }, Some(off), true),
        (None, Some((expr, off)), _) => (expr, Some(off), true),
        (None, None, Some((expr, _))) => (expr, None, false),
        (None, None, None) => (QuantityExpr::Fixed { value: 1 }, None, false),
    };

    // Extract the type filter from after "for a/an" or from the tail after "up to N"
    // or "any number of".
    // CR 701.23a + CR 107.1: "search your library for a X card and a Y card" —
    // the "and a Y card" clause introduces a second independent filter. Split
    // the filter tail on this conjunction BEFORE parsing so each side becomes a
    // distinct `TargetFilter` and the suffix parser for the primary filter does
    // not consume the extras as a dangling "and a ..." fragment.
    let (filter, extra_filters) = if let Some(type_start) = count_end_in_for {
        // "for up to five creature cards" or "for any number of dragon creature cards"
        // — type text starts after the number / quantity phrase. Multi-filter is
        // not supported for explicit-count searches (grammar always uses "a X and a Y").
        (parse_search_filter(&lower[type_start..], ctx), Vec::new())
    } else if let Some(after_for) = strip_after(lower, "for a ") {
        parse_search_filter_with_extras(after_for, ctx)
    } else if let Some(after_for) = strip_after(lower, "for an ") {
        parse_search_filter_with_extras(after_for, ctx)
    } else {
        (TargetFilter::Any, Vec::new())
    };

    // CR 701.23a + CR 701.18a: For multi-filter chains, capture destination
    // and enter-tapped flags now so the downstream lowering can interleave
    // `ChangeZone`s between each `SearchLibrary`. Single-filter searches
    // ignore these fields; their destination comes from the sequence-level
    // intrinsic continuation.
    let (multi_destination, multi_enter_tapped) = if extra_filters.is_empty() {
        (Zone::Hand, false)
    } else {
        (
            parse_search_destination(lower),
            scan_contains_phrase(lower, "battlefield tapped"),
        )
    };

    // CR 608.2c + CR 701.23: "with different names" is a printed-text restriction
    // on the chosen set, not a filter on individual library cards. Detected via a
    // word-boundary nom scan so it composes with arbitrary preceding filter text
    // ("for four cards with different names", "for any number of cards with
    // different names", etc.) without enumerating per-prefix permutations.
    let selection_constraint = if let Some(constraint) = scan_total_mana_value_constraint(lower) {
        constraint
    } else if scan_distinct_names_clause(lower) {
        SearchSelectionConstraint::DistinctNames
    } else {
        SearchSelectionConstraint::None
    };

    SearchLibraryDetails {
        filter,
        count,
        reveal,
        target_player,
        up_to,
        selection_constraint,
        reference_target: scan_same_name_reference_target(lower),
        extra_filters,
        multi_destination,
        multi_enter_tapped,
    }
}

fn parse_distinct_names_marker(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    value(
        (),
        nom::sequence::pair(
            tag::<_, _, OracleError<'_>>("different name"),
            nom::combinator::opt(tag::<_, _, OracleError<'_>>("s")),
        ),
    )
    .parse(input)
}

fn scan_total_mana_value_constraint(lower: &str) -> Option<SearchSelectionConstraint> {
    scan_preceded(
        lower,
        "with total mana value ",
        parse_total_mana_value_constraint,
    )
    .map(|(constraint, _)| constraint)
}

fn parse_total_mana_value_constraint(
    input: &str,
) -> Result<(&str, SearchSelectionConstraint), nom::Err<OracleError<'_>>> {
    let (rest, amount) = nom_primitives::parse_number.parse(input)?;
    let (rest, comparator) = alt((
        value(Comparator::LE, tag::<_, _, OracleError<'_>>(" or less")),
        value(Comparator::GE, tag(" or greater")),
    ))
    .parse(rest)?;
    Ok((
        rest,
        SearchSelectionConstraint::TotalManaValue {
            comparator,
            value: amount as i32,
        },
    ))
}

/// CR 608.2c + CR 701.23: Detect the distinct-names printed-text restriction
/// at any word boundary in the clause. Composable nom scan that matches both
/// canonical search phrasings ("with different names", "that have different
/// names") without committing to a fixed count/filter prefix.
fn scan_distinct_names_clause(lower: &str) -> bool {
    scan_preceded(lower, "with ", parse_distinct_names_marker).is_some()
        || scan_preceded(lower, "that have ", parse_distinct_names_marker).is_some()
}

/// CR 701.23a + CR 107.1: Split a search filter tail on conjunction boundaries
/// (`"<primary> and a <secondary>"`, `"... and an ..."`, `"... and basic ..."`)
/// so each filter phrase parses independently. Returns the primary filter and
/// a list of extra filters; the list is empty in the common single-filter case.
///
/// The conjunction scan ends at the first clause-terminating comma / period
/// (e.g., `"..., put them onto the battlefield tapped, then shuffle"`) because
/// anything after that belongs to the destination / action chain — not to the
/// filter expression.
fn parse_search_filter_with_extras(
    tail: &str,
    ctx: &mut ParseContext,
) -> (TargetFilter, Vec<TargetFilter>) {
    // structural: not dispatch — bound the filter region at the first clause
    // terminator (comma / period) before running the conjunction combinator,
    // so `" and "` inside e.g. `"put it onto the battlefield, then ..."` can't
    // pollute the filter split.
    let filter_region = search_filter_region(tail);

    // Split on `" and a "` / `" and an "` / `" and basic "` at filter-region
    // boundaries only. The "and basic" branch preserves the supertype prefix so
    // the downstream filter parser sees e.g. `"basic plains card"` intact.
    let segments = split_filter_conjunctions(filter_region);
    if segments.len() < 2 {
        return (parse_search_filter(tail, ctx), Vec::new());
    }

    let primary = parse_search_filter(segments[0], ctx);
    let extras: Vec<TargetFilter> = segments[1..]
        .iter()
        .map(|segment| parse_search_filter(segment, ctx))
        .collect();
    (primary, extras)
}

fn search_filter_region(text: &str) -> &str {
    let filter_region_end = text
        .find(',')
        .or_else(|| text.find('.'))
        .unwrap_or(text.len());
    &text[..filter_region_end]
}

/// Split a filter-region string (no action chain) on `" and a "` / `" and an "`
/// / `" and basic "` conjunctions using a nom `take_until` + `alt` scan. For
/// the "and basic" variant the supertype stays attached to the following
/// segment by re-prepending `"basic "` to the remainder after consuming the
/// shared `" and "` prefix. Returns a single-segment vector when no
/// conjunction matches.
fn split_filter_conjunctions(filter_region: &str) -> Vec<&str> {
    use nom::branch::alt;
    use nom::bytes::complete::take_until;
    use nom::combinator::value;
    use nom::Parser;

    // (nom `alt` arm that consumes the conjunction, amount pushed back onto
    // the remainder so the "basic" supertype stays on the following segment)
    #[derive(Clone, Copy)]
    enum Conjunction {
        AndA,
        AndAn,
        AndBasic,
    }

    let mut segments = Vec::new();
    let mut remaining = filter_region;
    loop {
        // Scan ahead for the earliest conjunction tag. `take_until` + `alt` is
        // the nom idiom for "find the first occurrence of any of these tags";
        // the error branch falls through to a single-segment result.
        let mut scan = (
            take_until::<_, _, OracleError<'_>>(" and "),
            alt((
                value(Conjunction::AndA, tag(" and a ")),
                value(Conjunction::AndAn, tag(" and an ")),
                value(Conjunction::AndBasic, tag(" and basic ")),
            )),
        );

        let Ok((rest, (before, conj))) = scan.parse(remaining) else {
            segments.push(remaining.trim());
            break;
        };
        segments.push(before.trim());
        remaining = match conj {
            Conjunction::AndA | Conjunction::AndAn => rest,
            // Keep the "basic " supertype attached to the following segment.
            // SAFETY: `rest` is a suffix of `filter_region`, so stepping back
            // "basic ".len() bytes yields a well-aligned slice that begins with
            // "basic …".
            Conjunction::AndBasic => {
                let start = filter_region.len() - rest.len() - "basic ".len();
                &filter_region[start..]
            }
        };
    }
    segments
}

/// Locate `tag_prefix` at a word boundary in `lower` and return the byte offset of
/// the character immediately following the prefix. Mirrors `scan_preceded`'s boundary
/// rules but does not apply a nom combinator — the tail is the filter text itself.
fn scan_after_tag(lower: &str, tag_prefix: &str) -> Option<usize> {
    let mut search_from = 0;
    while search_from <= lower.len() {
        let idx = lower[search_from..]
            .find(tag_prefix)
            .map(|i| search_from + i)?;
        let at_boundary = idx == 0
            || matches!(
                lower.as_bytes()[idx - 1],
                b' ' | b',' | b';' | b'(' | b'.' | b'\n' | b'\t'
            );
        if at_boundary {
            return Some(idx + tag_prefix.len());
        }
        search_from = idx + 1;
    }
    None
}

/// CR 701.23a: Detect player-targeting search patterns like "search target opponent's library"
/// or "search target player's library". Returns a TargetFilter for the player.
fn parse_search_target_player(lower: &str) -> Option<TargetFilter> {
    use nom::branch::alt;
    use nom::combinator::value;
    use nom::sequence::preceded;

    let (filter, _rest) = nom_on_lower(lower, lower, |i| {
        preceded(
            tag("search "),
            alt((
                value(
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                    tag("target opponent's library"),
                ),
                value(TargetFilter::Player, tag("target player's library")),
                value(
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                    tag("an opponent's library"),
                ),
            )),
        )
        .parse(i)
    })?;
    Some(filter)
}

/// Parse "seek [count] [filter] card(s) [and put onto battlefield [tapped]]".
/// Seek grammar is simpler than search: no "your library", no "for", no shuffle.
pub(super) fn parse_seek_details(lower: &str, ctx: &mut ParseContext) -> SeekDetails {
    let after_seek = tag::<_, _, OracleError<'_>>("seek ")
        .parse(lower)
        .map(|(rest, _)| rest)
        .unwrap_or(lower);

    // Extract destination clause before filter parsing, so it doesn't pollute the filter.
    let (filter_text, destination, enter_tapped) = {
        let put_idx = after_seek
            .find(" and put")
            .or_else(|| after_seek.find(", put"));
        if let Some(idx) = put_idx {
            let dest_clause = &after_seek[idx..];
            let dest = parse_search_destination(dest_clause);
            let tapped = scan_contains_phrase(dest_clause, "battlefield tapped");
            (&after_seek[..idx], dest, tapped)
        } else {
            (after_seek, Zone::Hand, false)
        }
    };

    let (filter_text, from_top) = parse_seek_from_top_limit(filter_text);

    // Extract count: "two nonland cards" → (2, "nonland cards"); "x cards" → (X, "cards").
    // CR 107.3a + CR 601.2b: X resolves to the caster's announced value at cast time.
    let (count, remaining) =
        if let Ok((rest, expr)) = nom_quantity::parse_quantity_expr_number(filter_text) {
            (expr, rest.trim_start())
        } else {
            (QuantityExpr::Fixed { value: 1 }, filter_text)
        };

    // Strip leading article "a "/"an "
    let remaining = nom_primitives::parse_article
        .parse(remaining)
        .map(|(rest, _)| rest)
        .unwrap_or(remaining);

    let filter = parse_search_filter(remaining, ctx);

    SeekDetails {
        filter,
        count,
        from_top,
        destination,
        enter_tapped,
    }
}

fn parse_seek_from_top_limit(filter_text: &str) -> (&str, Option<usize>) {
    fn parse_limit(input: &str) -> Result<(&str, (&str, usize)), nom::Err<OracleError<'_>>> {
        let (rest, before) =
            take_until::<_, _, OracleError<'_>>(" from among the top ").parse(input)?;
        let (rest, _) = tag(" from among the top ").parse(rest)?;
        let (rest, qty) = nom_quantity::parse_quantity_expr_number(rest)?;
        let (rest, _) = alt((
            tag::<_, _, OracleError<'_>>(" cards of your library"),
            tag(" card of your library"),
        ))
        .parse(rest)?;
        let QuantityExpr::Fixed { value } = qty else {
            return Err(nom::Err::Error(nom::error::Error::new(
                rest,
                nom::error::ErrorKind::Fail,
            )));
        };
        Ok((rest, (before, value.max(0) as usize)))
    }

    parse_limit(filter_text)
        .ok()
        .and_then(|(_, (before, count))| (count > 0).then_some((before, Some(count))))
        .unwrap_or((filter_text, None))
}

/// Parse the card type filter from search text like "basic land card, ..."
/// or "creature card with ..." into a TargetFilter.
pub(super) fn parse_search_filter(text: &str, ctx: &mut ParseContext) -> TargetFilter {
    let type_text = text.trim();

    if let Some(filter) = parse_search_filter_disjunction(type_text, ctx) {
        return filter;
    }

    if let Some(filter) = parse_search_filter_leading_property_stack(type_text, ctx) {
        return filter;
    }

    let (parsed_filter, remainder) = parse_type_phrase(type_text);
    if search_filter_has_meaningful_content(&parsed_filter) {
        let mut suffix = SearchSuffixConstraints::default();
        let linked_reference = last_shared_quality_reference_in_filter(&parsed_filter);
        parse_search_filter_suffixes(remainder, &mut suffix, ctx, linked_reference);
        return apply_search_suffix_constraints(normalize_search_filter(parsed_filter), &suffix);
    }

    let type_text = strip_search_card_suffix(type_text);

    // Intentional: "a card" means any card type — no warning needed.
    if type_text == "card" || type_text.is_empty() {
        return TargetFilter::Any;
    }

    let (is_basic, clean) = if let Some(rest) = type_text.strip_prefix("basic ") {
        (true, rest)
    } else {
        (false, type_text)
    };
    let (type_word, suffix_text) = split_search_type_word_and_suffix(clean);

    parse_search_filter_fallback(type_word, suffix_text, is_basic, ctx)
}

fn parse_search_filter_leading_property_stack(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<TargetFilter> {
    let mut properties = Vec::new();
    let mut remaining = text;
    while let Ok((rest, property)) = parse_search_leading_filter_property(remaining) {
        properties.push(property);
        remaining = rest;
    }
    if properties.is_empty() {
        return None;
    }

    let filter = parse_search_filter(remaining, ctx);
    search_filter_has_meaningful_content(&filter).then(|| {
        apply_search_suffix_constraints(
            filter,
            &SearchSuffixConstraints {
                properties,
                type_filters: Vec::new(),
            },
        )
    })
}

fn parse_search_leading_filter_property(
    input: &str,
) -> Result<(&str, FilterProp), nom::Err<OracleError<'_>>> {
    alt((
        value(
            FilterProp::NotSupertype {
                value: Supertype::Legendary,
            },
            tag("nonlegendary "),
        ),
        value(
            FilterProp::NotSupertype {
                value: Supertype::Basic,
            },
            tag("nonbasic "),
        ),
        value(
            FilterProp::HasSupertype {
                value: Supertype::Legendary,
            },
            tag("legendary "),
        ),
        value(
            FilterProp::HasSupertype {
                value: Supertype::Basic,
            },
            tag("basic "),
        ),
        |i| {
            let (rest, color) = nom_primitives::parse_color(i)?;
            let (rest, _) = tag::<_, _, OracleError<'_>>(" ").parse(rest)?;
            Ok((rest, FilterProp::HasColor { color }))
        },
    ))
    .parse(input)
}

fn parse_search_filter_disjunction(text: &str, ctx: &mut ParseContext) -> Option<TargetFilter> {
    let filter_region = search_filter_region(text);
    let segments = split_filter_disjunctions(filter_region);
    if segments.len() < 2 {
        return None;
    }

    let filters: Vec<TargetFilter> = segments
        .into_iter()
        .map(|s| parse_search_filter(s, ctx))
        .filter(search_filter_has_meaningful_content)
        .collect();
    (filters.len() >= 2).then(|| normalize_search_filter(TargetFilter::Or { filters }))
}

/// Split a single search-filter expression on disjunctive filter boundaries:
/// `"basic land card or a Gate card"`, `"instant card or a card with flash"`,
/// and bare subtype forms like `"Mountain or Cave card"`.
///
/// The bare `" or "` branch is intentionally narrow: it only fires when the
/// left branch is not a core card-type word and the right branch has an
/// explicit `card(s)` head. That keeps comparator suffixes such as `"or less"`
/// and canonical core unions such as `"instant or sorcery card"` on the
/// existing suffix/type-phrase paths.
fn split_filter_disjunctions(filter_region: &str) -> Vec<&str> {
    #[derive(Clone, Copy)]
    enum Disjunction {
        OrA,
        OrAn,
        OrBasic,
        AndOrA,
        AndOrAn,
        BareOr,
    }

    let mut segments = Vec::new();
    let mut remaining = filter_region;
    loop {
        let mut and_or_scan = (
            take_until::<_, _, OracleError<'_>>(" and/or "),
            alt((
                value(Disjunction::AndOrA, tag(" and/or a ")),
                value(Disjunction::AndOrAn, tag(" and/or an ")),
            )),
        );
        let parsed = if let Ok(found) = and_or_scan.parse(remaining) {
            Some(found)
        } else {
            let mut or_scan = (
                take_until::<_, _, OracleError<'_>>(" or "),
                alt((
                    value(Disjunction::OrA, tag(" or a ")),
                    value(Disjunction::OrAn, tag(" or an ")),
                    value(Disjunction::OrBasic, tag(" or basic ")),
                    value(Disjunction::BareOr, tag(" or ")),
                )),
            );
            or_scan.parse(remaining).ok()
        };

        let Some((rest, (before, disjunction))) = parsed else {
            segments.push(remaining.trim());
            break;
        };

        if matches!(disjunction, Disjunction::BareOr)
            && !bare_search_disjunction_allowed(before.trim(), rest.trim_start())
        {
            if segments.is_empty() {
                return vec![filter_region.trim()];
            }
            segments.push(remaining.trim());
            break;
        }

        segments.push(before.trim());
        remaining = match disjunction {
            Disjunction::OrA
            | Disjunction::OrAn
            | Disjunction::AndOrA
            | Disjunction::AndOrAn
            | Disjunction::BareOr => rest,
            Disjunction::OrBasic => {
                let start = filter_region.len() - rest.len() - "basic ".len();
                &filter_region[start..]
            }
        };
    }

    segments
}

fn bare_search_disjunction_allowed(left: &str, right: &str) -> bool {
    !left.is_empty()
        && parse_search_builtin_type_word(left).is_none()
        && parse_bare_search_disjunction_right(right).is_ok()
}

fn parse_bare_search_disjunction_right(
    input: &str,
) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    let (rest, _) = nom::combinator::opt(tag("basic ")).parse(input)?;
    let (rest, _) = take_till1::<_, _, OracleError<'_>>(|c: char| c.is_whitespace()).parse(rest)?;
    alt((value((), tag(" cards")), value((), tag(" card")))).parse(rest)
}

fn search_filter_has_meaningful_content(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any | TargetFilter::None => false,
        TargetFilter::Typed(typed_filter) => {
            !typed_filter.type_filters.is_empty() || !typed_filter.properties.is_empty()
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(search_filter_has_meaningful_content)
        }
        _ => true,
    }
}

fn parse_search_filter_fallback(
    type_word: &str,
    suffix_text: &str,
    is_basic: bool,
    ctx: &mut ParseContext,
) -> TargetFilter {
    let suffix = build_search_suffix_constraints(suffix_text, is_basic, ctx);
    let filter = parse_search_builtin_type_word(type_word)
        .unwrap_or_else(|| parse_search_specialized_type_word(type_word, ctx));
    apply_search_suffix_constraints(filter, &suffix)
}

fn parse_search_builtin_type_word(type_word: &str) -> Option<TargetFilter> {
    let (rest, filter) = alt((
        value(
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                ],
            },
            tag::<_, _, OracleError<'_>>("instant or sorcery"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker)),
            tag("planeswalker"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
            tag("enchantment"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            tag("artifact"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            tag("creature"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
            tag("sorcery"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
            tag("instant"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            tag("land"),
        ),
    ))
    .parse(type_word)
    .ok()?;
    rest.is_empty().then_some(filter)
}

fn parse_search_specialized_type_word(type_word: &str, ctx: &mut ParseContext) -> TargetFilter {
    let negated_types: &[(&str, TypeFilter)] = &[
        ("noncreature", TypeFilter::Creature),
        ("nonland", TypeFilter::Land),
        ("nonartifact", TypeFilter::Artifact),
        ("nonenchantment", TypeFilter::Enchantment),
    ];
    for &(prefix, ref inner) in negated_types {
        if type_word == prefix {
            return TargetFilter::Typed(TypedFilter::new(TypeFilter::Non(Box::new(inner.clone()))));
        }
    }

    let land_subtypes = ["plains", "island", "swamp", "mountain", "forest"];
    if land_subtypes.contains(&type_word) {
        return TargetFilter::Typed(TypedFilter::land().subtype(capitalize(type_word)));
    }
    if type_word == "equipment" {
        return TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact).subtype("Equipment".to_string()),
        );
    }
    if type_word == "aura" {
        return TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Enchantment).subtype("Aura".to_string()),
        );
    }
    if type_word == "card" {
        return TargetFilter::Typed(TypedFilter::default());
    }
    if !type_word.is_empty()
        && type_word != "card"
        && type_word != "permanent"
        && type_word.chars().all(|c| c.is_alphabetic())
    {
        return TargetFilter::Typed(TypedFilter::default().subtype(capitalize(type_word)));
    }

    let (filter, _) = parse_type_phrase(type_word);
    if !matches!(filter, TargetFilter::Any) {
        return filter;
    }

    ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
        context: "unrecognized search filter".into(),
        text: type_word.into(),
        line_index: 0,
    });
    TargetFilter::Any
}

#[derive(Debug, Clone, Default)]
struct SearchSuffixConstraints {
    properties: Vec<FilterProp>,
    type_filters: Vec<TypeFilter>,
}

fn strip_search_card_suffix(text: &str) -> &str {
    text.strip_suffix(" cards")
        .or_else(|| text.strip_suffix(" card"))
        .unwrap_or(text)
        .trim()
}

fn split_search_type_word_and_suffix(clean: &str) -> (&str, &str) {
    if let Some((type_word, _)) = split_around(clean, " with ") {
        (
            strip_search_card_suffix(type_word.trim()),
            &clean[type_word.len()..],
        )
    } else {
        (clean.trim(), "")
    }
}

fn build_search_suffix_constraints(
    suffix_text: &str,
    is_basic: bool,
    ctx: &mut ParseContext,
) -> SearchSuffixConstraints {
    let mut suffix = SearchSuffixConstraints::default();
    if is_basic {
        suffix.properties.push(FilterProp::HasSupertype {
            value: crate::types::card_type::Supertype::Basic,
        });
    }
    parse_search_filter_suffixes(suffix_text, &mut suffix, ctx, None);
    suffix
}

fn normalize_search_filter(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(typed_filter) => {
            TargetFilter::Typed(normalize_search_typed_filter(typed_filter))
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(normalize_search_filter).collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.into_iter().map(normalize_search_filter).collect(),
        },
        other => other,
    }
}

fn normalize_search_typed_filter(mut typed_filter: TypedFilter) -> TypedFilter {
    let inferred_type = typed_filter.type_filters.iter().find_map(|type_filter| {
        let TypeFilter::Subtype(subtype) = type_filter else {
            return None;
        };
        infer_core_type_for_subtype(subtype).map(|core_type| match core_type {
            CoreType::Artifact => TypeFilter::Artifact,
            CoreType::Enchantment => TypeFilter::Enchantment,
            CoreType::Land => TypeFilter::Land,
            _ => TypeFilter::Creature,
        })
    });

    if let Some(inferred_type) = inferred_type {
        let already_present = typed_filter.type_filters.contains(&inferred_type);
        if !already_present {
            typed_filter.type_filters.insert(0, inferred_type);
        }
    }

    typed_filter
}

fn apply_search_suffix_constraints(
    filter: TargetFilter,
    suffix: &SearchSuffixConstraints,
) -> TargetFilter {
    if suffix.properties.is_empty() && suffix.type_filters.is_empty() {
        return filter;
    }

    match filter {
        TargetFilter::Any => {
            TargetFilter::Typed(apply_search_suffix_to_typed(TypedFilter::default(), suffix))
        }
        TargetFilter::Typed(typed_filter) => {
            TargetFilter::Typed(apply_search_suffix_to_typed(typed_filter, suffix))
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|branch| apply_search_suffix_constraints(branch, suffix))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|branch| apply_search_suffix_constraints(branch, suffix))
                .collect(),
        },
        other => other,
    }
}

fn apply_search_suffix_to_typed(
    mut typed_filter: TypedFilter,
    suffix: &SearchSuffixConstraints,
) -> TypedFilter {
    for type_filter in &suffix.type_filters {
        if !typed_filter.type_filters.contains(type_filter) {
            typed_filter.type_filters.push(type_filter.clone());
        }
    }
    for property in &suffix.properties {
        if !typed_filter
            .properties
            .iter()
            .any(|existing| existing.same_kind(property))
        {
            typed_filter.properties.push(property.clone());
        }
    }
    typed_filter
}

fn basic_land_type_any_of() -> TypeFilter {
    TypeFilter::AnyOf(
        ["Plains", "Island", "Swamp", "Mountain", "Forest"]
            .into_iter()
            .map(|subtype| TypeFilter::Subtype(subtype.to_string()))
            .collect(),
    )
}

fn capitalize_subtype_word(word: &str) -> String {
    word.split('-')
        .map(capitalize)
        .collect::<Vec<_>>()
        .join("-")
}

fn parse_search_suffix_subtype_redeclaration(text: &str) -> Option<(&str, Vec<TypeFilter>)> {
    let (rest, subtype) = take_till1::<_, _, OracleError<'_>>(|c: char| c.is_whitespace())
        .parse(text)
        .ok()?;
    if !subtype.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
        return None;
    }
    let (rest, _) = tag::<_, _, OracleError<'_>>(" ").parse(rest).ok()?;
    let (rest, core_type) = alt((
        value(
            Some(TypeFilter::Creature),
            tag::<_, _, OracleError<'_>>("creature"),
        ),
        value(
            Some(TypeFilter::Artifact),
            tag::<_, _, OracleError<'_>>("artifact"),
        ),
        value(
            Some(TypeFilter::Enchantment),
            tag::<_, _, OracleError<'_>>("enchantment"),
        ),
        value(
            Some(TypeFilter::Instant),
            tag::<_, _, OracleError<'_>>("instant"),
        ),
        value(
            Some(TypeFilter::Sorcery),
            tag::<_, _, OracleError<'_>>("sorcery"),
        ),
        value(Some(TypeFilter::Land), tag::<_, _, OracleError<'_>>("land")),
        value(None, tag::<_, _, OracleError<'_>>("cards")),
        value(None, tag::<_, _, OracleError<'_>>("card")),
    ))
    .parse(rest)
    .ok()?;

    let mut filters = Vec::new();
    if let Some(core_type) = core_type {
        filters.push(core_type);
    }
    filters.push(TypeFilter::Subtype(capitalize_subtype_word(subtype)));
    Some((rest, filters))
}

fn parse_search_type_negation_suffix(
    input: &str,
) -> Result<(&str, TypeFilter), nom::Err<OracleError<'_>>> {
    let (rest, _) = alt((
        tag("that isn't a "),
        tag("that isn't an "),
        tag("that is not a "),
        tag("that is not an "),
        tag("that aren't "),
        tag("that are not "),
    ))
    .parse(input)?;
    let (filter, rest) = parse_type_phrase(rest);
    let Some(negated_type) = single_search_type_filter(filter) else {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((rest, TypeFilter::Non(Box::new(negated_type))))
}

fn single_search_type_filter(filter: TargetFilter) -> Option<TypeFilter> {
    let TargetFilter::Typed(TypedFilter {
        mut type_filters,
        controller: None,
        properties,
    }) = filter
    else {
        return None;
    };
    if properties.is_empty() && type_filters.len() == 1 {
        type_filters.pop()
    } else {
        None
    }
}

fn parse_search_name_reference_suffix(
    input: &str,
) -> Result<(&str, FilterProp), nom::Err<OracleError<'_>>> {
    let (rest, relation) = alt((
        value(
            SharedQualityRelation::DoesNotShare,
            tag("that doesn't have the same name as "),
        ),
        value(
            SharedQualityRelation::DoesNotShare,
            tag("that does not have the same name as "),
        ),
        value(
            SharedQualityRelation::DoesNotShare,
            tag("that doesn't share a name with "),
        ),
        value(
            SharedQualityRelation::DoesNotShare,
            tag("that does not share a name with "),
        ),
        value(
            SharedQualityRelation::Shares,
            tag("that has the same name as "),
        ),
        value(
            SharedQualityRelation::Shares,
            tag("that have the same name as "),
        ),
        value(SharedQualityRelation::Shares, tag("with the same name as ")),
    ))
    .parse(input)?;

    if tag::<_, _, OracleError<'_>>("target ").parse(rest).is_ok() {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    let (reference, after_reference) = parse_target(rest);
    if !matches!(reference, TargetFilter::Any) {
        return Ok((
            after_reference,
            FilterProp::SharesQuality {
                quality: SharedQuality::Name,
                reference: Some(Box::new(name_reference_filter(reference))),
                relation,
            },
        ));
    }

    let (reference, rest) = parse_type_phrase(rest);
    if !search_filter_has_meaningful_content(&reference) {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    Ok((
        rest,
        FilterProp::SharesQuality {
            quality: SharedQuality::Name,
            reference: Some(Box::new(name_reference_filter(reference))),
            relation,
        },
    ))
}

fn parse_linked_reference_mana_value_suffix<'a>(
    input: &'a str,
    reference: &TargetFilter,
) -> Result<(&'a str, FilterProp), nom::Err<OracleError<'a>>> {
    let Some(scope) = object_scope_for_linked_reference(reference) else {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };

    let (rest, _) = tag("has mana value equal to ").parse(input)?;
    let (rest, offset) = alt((
        value(
            1,
            nom::sequence::pair(tag("1 plus "), parse_that_object_mana_value),
        ),
        value(
            1,
            nom::sequence::pair(tag("one plus "), parse_that_object_mana_value),
        ),
        value(0, parse_that_object_mana_value),
    ))
    .parse(rest)?;
    let value = if offset == 0 {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue { scope },
        }
    } else {
        QuantityExpr::Offset {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue { scope },
            }),
            offset,
        }
    };

    Ok((
        rest,
        FilterProp::Cmc {
            comparator: Comparator::EQ,
            value,
        },
    ))
}

fn parse_that_object_mana_value(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
    let (rest, _) = tag("that ").parse(input)?;
    let (rest, _) = alt((
        tag("creature"),
        tag("card"),
        tag("permanent"),
        tag("artifact"),
        tag("enchantment"),
        tag("planeswalker"),
        tag("land"),
    ))
    .parse(rest)?;
    let (rest, _) = tag("'s mana value").parse(rest)?;
    Ok((rest, ()))
}

fn object_scope_for_linked_reference(reference: &TargetFilter) -> Option<ObjectScope> {
    match reference {
        TargetFilter::CostPaidObject => Some(ObjectScope::CostPaidObject),
        TargetFilter::ParentTarget => Some(ObjectScope::Target),
        TargetFilter::TriggeringSource => Some(ObjectScope::EventSource),
        _ => None,
    }
}

fn last_shared_quality_reference_in_filter(filter: &TargetFilter) -> Option<TargetFilter> {
    match filter {
        TargetFilter::Typed(typed) => typed.properties.iter().rev().find_map(|property| {
            if let FilterProp::SharesQuality {
                reference: Some(reference),
                ..
            } = property
            {
                Some(reference.as_ref().clone())
            } else {
                None
            }
        }),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .rev()
            .find_map(last_shared_quality_reference_in_filter),
        _ => None,
    }
}

fn parse_zero_or_one_mana_cost_suffix(
    input: &str,
) -> Result<(&str, FilterProp), nom::Err<OracleError<'_>>> {
    let (rest, _) = tag("with mana cost ").parse(input)?;
    let (rest, first) = nom_primitives::parse_mana_cost(rest)?;
    let (rest, _) = tag(" or ").parse(rest)?;
    let (rest, second) = nom_primitives::parse_mana_cost(rest)?;
    Ok((
        rest,
        FilterProp::ManaCostIn {
            costs: vec![first, second],
        },
    ))
}

fn name_reference_filter(filter: TargetFilter) -> TargetFilter {
    owner_scope_non_battlefield_zones(add_default_battlefield_zone(filter))
}

fn filter_prop_is_zone(prop: &FilterProp) -> bool {
    match prop {
        FilterProp::InZone { .. } | FilterProp::InAnyZone { .. } => true,
        FilterProp::AnyOf { props } => props.iter().any(filter_prop_is_zone),
        _ => false,
    }
}

fn zone_for_scope(props: &[FilterProp]) -> Option<Zone> {
    props.iter().find_map(|prop| match prop {
        FilterProp::InZone { zone } => Some(*zone),
        FilterProp::InAnyZone { zones } if zones.len() == 1 => zones.first().copied(),
        _ => None,
    })
}

fn owner_scope_non_battlefield_zones(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            if let Some(controller) = typed.controller.clone() {
                if zone_for_scope(&typed.properties).is_some_and(|zone| zone != Zone::Battlefield)
                    && !typed
                        .properties
                        .iter()
                        .any(|prop| matches!(prop, FilterProp::Owned { .. }))
                {
                    typed.controller = None;
                    typed.properties.push(FilterProp::Owned { controller });
                }
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(owner_scope_non_battlefield_zones)
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(owner_scope_non_battlefield_zones)
                .collect(),
        },
        other => other,
    }
}

fn add_default_battlefield_zone(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            if !typed.properties.iter().any(filter_prop_is_zone) {
                typed.properties.push(FilterProp::InZone {
                    zone: Zone::Battlefield,
                });
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(add_default_battlefield_zone)
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(add_default_battlefield_zone)
                .collect(),
        },
        other => other,
    }
}

/// Parse property suffixes from search filter text ("with mana value ...", "with a different name ...").
/// Reuses the existing suffix parsers from oracle_target.
fn parse_search_filter_suffixes(
    text: &str,
    suffix: &mut SearchSuffixConstraints,
    ctx: &mut ParseContext,
    initial_shared_quality_reference: Option<TargetFilter>,
) {
    let lower = text.to_lowercase();
    let mut remaining = lower.as_str();
    let mut last_shared_quality_reference = initial_shared_quality_reference;

    while !remaining.is_empty() {
        remaining = remaining.trim_start();

        // Consume redundant "card(s)" re-declaration left by parse_type_phrase.
        // parse_type_phrase extracts only the type word (e.g. "creature"), so the
        // literal " card" / " cards" token remains and carries no filter meaning.
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("cards").parse(remaining) {
            remaining = rest.trim_start();
        } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("card").parse(remaining) {
            remaining = rest.trim_start();
        }

        // End-of-filter sentinel: punctuation, "then …", "reveal …", or "put …"
        // means the search filter has ended and what follows is the action chain
        // handled by the downstream sequence parser. Not a filter-suffix gap — break
        // without warning.
        if remaining.is_empty()
            || tag::<_, _, OracleError<'_>>(",").parse(remaining).is_ok()
            || tag::<_, _, OracleError<'_>>(".").parse(remaining).is_ok()
            || tag::<_, _, OracleError<'_>>("then ")
                .parse(remaining)
                .is_ok()
            || tag::<_, _, OracleError<'_>>("reveal ")
                .parse(remaining)
                .is_ok()
            || tag::<_, _, OracleError<'_>>("put ")
                .parse(remaining)
                .is_ok()
            || tag::<_, _, OracleError<'_>>("puts ")
                .parse(remaining)
                .is_ok()
            || tag::<_, _, OracleError<'_>>("instead")
                .parse(remaining)
                .is_ok()
        {
            break;
        }

        // Consume a filter-conjunction "and " and restart the loop so post-"and"
        // text re-checks the sentinels above. Without the `continue`, patterns like
        // "... and reveal them" (Flourishing Bloom-Kin) or "... and reveal it"
        // (Archdruid's Charm) would fall through to the specific-suffix handlers,
        // miss every arm, and emit a spurious `reveal it` / `reveal them` warning.
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("and ").parse(remaining) {
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("with that name").parse(remaining) {
            suffix.properties.push(FilterProp::SameName);
            remaining = rest.trim_start();
            continue;
        }

        // CR 201.2 + CR 608.2c: "with the same name as that {creature,card,…}" binds to
        // the resolving ability's first object target (`SameNameAsParentTarget`). The
        // demonstrative "that X" is a back-reference to a previously-targeted/exiled
        // card carried via `TargetFilter::ParentTarget`. Chomp the noun so the
        // dispatch loop continues at any trailing action chain ("…, reveal it, …").
        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("with the same name as that ").parse(remaining)
        {
            // Consume the demonstrative subject noun and any trailing modifier
            // ("nontoken creature", "creature", "card") up to the next sentinel
            // (',', '.') via `take_till` — drop the consumed noun and continue
            // the dispatch loop at the sentinel position.
            let (after_noun, _consumed_noun) =
                nom::bytes::complete::take_till::<_, _, OracleError<'_>>(|c: char| {
                    c == ',' || c == '.'
                })
                .parse(rest)
                .unwrap_or((rest, ""));
            suffix.properties.push(FilterProp::SameNameAsParentTarget);
            remaining = after_noun.trim_start();
            continue;
        }

        // CR 115.1c + CR 608.2c: "with the same name as target {creature,…}" declares
        // a target solely to parameterize the search filter. The target is lowered as
        // a structural `TargetOnly` wrapper, and the library filter reads it via
        // `SameNameAsParentTarget`.
        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("with the same name as ").parse(remaining)
        {
            if tag::<_, _, OracleError<'_>>("target ").parse(rest).is_ok() {
                let (target, after_target) = parse_target(rest);
                if !matches!(target, TargetFilter::Any) {
                    suffix.properties.push(FilterProp::SameNameAsParentTarget);
                    remaining = after_target.trim_start();
                    continue;
                }
            }
        }

        if let Ok((rest, prop)) = parse_search_name_reference_suffix(remaining) {
            suffix.properties.push(prop);
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("with the same name").parse(remaining) {
            suffix.properties.push(FilterProp::SameNameAsParentTarget);
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("of the chosen kind").parse(remaining) {
            suffix
                .properties
                .push(FilterProp::IsChosenLandOrNonlandKind);
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, prop)) = parse_shared_quality_clause(remaining) {
            last_shared_quality_reference = match &prop {
                FilterProp::SharesQuality {
                    reference: Some(reference),
                    ..
                } => Some(reference.as_ref().clone()),
                _ => None,
            };
            suffix.properties.push(prop);
            remaining = rest.trim_start();
            continue;
        }

        // CR 608.2c: distinct-names suffixes constrain the chosen set, not
        // individual cards. The constraint is already encoded upstream via
        // `scan_distinct_names_clause`; this arm only consumes the marker.
        if let Ok((rest, _)) = alt((
            nom::sequence::preceded(
                tag::<_, _, OracleError<'_>>("with "),
                parse_distinct_names_marker,
            ),
            nom::sequence::preceded(
                tag::<_, _, OracleError<'_>>("that have "),
                parse_distinct_names_marker,
            ),
        ))
        .parse(remaining)
        {
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("with a basic land type").parse(remaining)
        {
            suffix.type_filters.push(basic_land_type_any_of());
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("with a mana ability").parse(remaining)
        {
            suffix.properties.push(FilterProp::HasManaAbility);
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("with no abilities").parse(remaining) {
            suffix.properties.push(FilterProp::HasNoAbilities);
            remaining = rest.trim_start();
            continue;
        }

        if let Some((rest, type_filters)) = parse_search_suffix_subtype_redeclaration(remaining) {
            for type_filter in type_filters {
                suffix.type_filters.push(type_filter);
            }
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, type_filter)) = parse_search_type_negation_suffix(remaining) {
            suffix.type_filters.push(type_filter);
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, prop)) = parse_search_enchant_keyword_suffix(remaining) {
            suffix.properties.push(prop);
            remaining = rest.trim_start();
            continue;
        }

        // CR 608.2c + CR 202.3: "with total mana value N or less" constrains
        // the selected set, not each individual card. `parse_search_library_details`
        // stores it in `SearchSelectionConstraint`; consume the suffix here so it
        // does not surface as a per-card filter gap.
        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("with total mana value ").parse(remaining)
        {
            if let Ok((rest, _)) = parse_total_mana_value_constraint(rest) {
                remaining = rest.trim_start();
                continue;
            }
        }

        if let Some((prop, consumed)) = parse_mana_value_suffix(remaining) {
            suffix.properties.push(prop);
            remaining = remaining[consumed..].trim_start();
            continue;
        }

        if let Some(reference) = &last_shared_quality_reference {
            if let Ok((rest, prop)) = parse_linked_reference_mana_value_suffix(remaining, reference)
            {
                suffix.properties.push(prop);
                remaining = rest.trim_start();
                continue;
            }
        }

        if let Ok((rest, prop)) = parse_zero_or_one_mana_cost_suffix(remaining) {
            suffix.properties.push(prop);
            remaining = rest.trim_start();
            continue;
        }

        if let Ok((rest, _)) =
            tag::<_, _, OracleError<'_>>("with a different name than each ").parse(remaining)
        {
            let end = rest
                .find(" you control")
                .unwrap_or_else(|| rest.find(',').unwrap_or(rest.len()));
            let inner_type = rest[..end].trim();
            let inner_filter = match inner_type {
                "aura" => TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Enchantment).subtype("Aura".to_string()),
                ),
                "creature" => TargetFilter::Typed(TypedFilter::creature()),
                "enchantment" => TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
                "artifact" => TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                _ => {
                    ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
                        context: "unrecognized inner type in different-name filter".into(),
                        text: inner_type.into(),
                        line_index: 0,
                    });
                    TargetFilter::Any
                }
            };
            suffix.properties.push(FilterProp::DifferentNameFrom {
                filter: Box::new(inner_filter),
            });
            let skip = rest
                .find(" you control")
                .map_or(end, |position| position + " you control".len());
            remaining = rest[skip..].trim_start();
            continue;
        }

        // Dispatch-loop diagnostic: unmatched trailing text indicates a parser gap
        // (e.g., novel "with …" suffix phrasing). Emit a warning so gaps surface
        // in coverage output instead of silently dropping filter constraints.
        ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
            context: "search-filter-suffix unmatched".into(),
            text: remaining.into(),
            line_index: 0,
        });
        break;
    }
}

fn scan_same_name_reference_target(lower: &str) -> Option<TargetFilter> {
    scan_preceded(lower, "with the same name as ", |input| {
        let _ = tag::<_, _, OracleError<'_>>("target ").parse(input)?;
        let (target, rest) = parse_target(input);
        Ok((rest, target))
    })
    .map(|(target, _)| target)
    .filter(|target| !matches!(target, TargetFilter::Any))
}

fn parse_search_enchant_keyword_suffix(
    input: &str,
) -> Result<(&str, FilterProp), nom::Err<OracleError<'_>>> {
    let (rest, _) = tag("with enchant ").parse(input)?;
    let (after_target, target_text) =
        take_till1::<_, _, OracleError<'_>>(|c: char| c == ',' || c == '.').parse(rest)?;
    let (target, remainder) = parse_type_phrase(target_text.trim());
    if !remainder.trim().is_empty() || !search_filter_has_meaningful_content(&target) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        after_target,
        FilterProp::WithKeyword {
            value: Keyword::Enchant(target),
        },
    ))
}

/// Parse the destination zone from search Oracle text.
/// Looks for "put it into your hand", "put it onto the battlefield", etc.
pub(super) fn parse_search_destination(lower: &str) -> Zone {
    if scan_contains_phrase(lower, "onto the battlefield") {
        Zone::Battlefield
    } else if contains_possessive(lower, "into", "hand") {
        Zone::Hand
    } else if contains_possessive(lower, "on top of", "library") {
        Zone::Library
    } else if contains_possessive(lower, "into", "graveyard") {
        Zone::Graveyard
    } else {
        Zone::Hand
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Comparator, QuantityRef, SharedQuality, SharedQualityRelation};
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;

    #[test]
    fn search_target_opponent_library() {
        let details = parse_search_library_details(
            "search target opponent's library for a creature card and put that card onto the battlefield under your control",
            &mut ParseContext::default(),
        );
        assert!(details.target_player.is_some());
        let tp = details.target_player.unwrap();
        match tp {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("expected Typed with Opponent controller, got {other:?}"),
        }
        // Filter should be creature
        match details.filter {
            TargetFilter::Typed(tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("expected creature filter, got {other:?}"),
        }
    }

    #[test]
    fn search_target_player_library() {
        let details = parse_search_library_details(
            "search target player's library for a card and exile it",
            &mut ParseContext::default(),
        );
        assert!(details.target_player.is_some());
        assert_eq!(details.target_player.unwrap(), TargetFilter::Player);
    }

    #[test]
    fn search_target_player_library_for_three() {
        // Jester's Cap: "search target player's library for three cards and exile them"
        let details = parse_search_library_details(
            "search target player's library for three cards and exile them",
            &mut ParseContext::default(),
        );
        assert!(details.target_player.is_some());
        assert_eq!(details.count, QuantityExpr::Fixed { value: 3 });
    }

    #[test]
    fn search_your_library_no_target_player() {
        let details = parse_search_library_details(
            "search your library for a basic land card, reveal it, put it into your hand",
            &mut ParseContext::default(),
        );
        assert!(details.target_player.is_none());
        assert!(details.reveal);
    }

    #[test]
    fn search_up_to_x_cards_emits_variable_count() {
        // CR 107.3a + CR 601.2b: `up to X` emits `QuantityRef::Variable` so the
        // resolver can pick up the caster's announced X at effect time.
        let details = parse_search_library_details(
            "search your library for up to x creature cards",
            &mut ParseContext::default(),
        );
        assert_eq!(
            details.count,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string()
                }
            }
        );
    }

    #[test]
    fn search_for_three_cards_emits_fixed_count_regression() {
        // Regression: numeric word counts still parse as `Fixed` — this is the
        // pre-widening behavior the switch to nom + `parse_quantity_expr_number`
        // must preserve.
        let details = parse_search_library_details(
            "search your library for three cards and exile them",
            &mut ParseContext::default(),
        );
        assert_eq!(details.count, QuantityExpr::Fixed { value: 3 });
    }

    #[test]
    fn action_chain_continuation_does_not_warn() {
        // Regression: filter parser must not emit "search-filter-suffix unmatched"
        // for legitimate action-chain continuations. The filter is already
        // extracted by parse_type_phrase; what follows the filter clause
        // (", put it onto the battlefield, then shuffle") is handled by the
        // downstream sequence parser — not a filter-suffix gap.
        for text in [
            "creature card, put it onto the battlefield, then shuffle",
            "land card, reveal it, put it into your hand, then shuffle",
            "card, put it onto the battlefield tapped",
            "basic land or desert cards and puts them onto the battlefield tapped",
            "creature card. exile it",
            "Vampire cards instead",
        ] {
            let mut ctx = ParseContext::default();
            let _ = parse_search_filter(text, &mut ctx);
            assert!(
                !ctx.diagnostics
                    .iter()
                    .any(|d| d.to_string().contains("search-filter-suffix unmatched")), // allow-noncombinator: test assertion matching diagnostic content
                "unexpected filter-suffix warning for {text:?}: {:?}",
                ctx.diagnostics
            );
        }
    }

    #[test]
    fn genuine_filter_suffix_gap_still_warns() {
        // Diagnostic preserved: when the suffix parser is handed text that
        // doesn't match any known filter-suffix pattern AND doesn't look like an
        // action-chain continuation (no leading comma / period / "then"), a
        // warning must still fire so coverage reports surface parser gaps.
        use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
        let mut ctx = ParseContext::default();
        let mut suffix = SearchSuffixConstraints::default();
        // Invented suffix that won't hit any existing filter-suffix pattern.
        parse_search_filter_suffixes(
            " with unrecognized flibbertigibbet suffix",
            &mut suffix,
            &mut ctx,
            None,
        );
        assert!(
            ctx.diagnostics
                .iter()
                .any(|d| matches!(d, OracleDiagnostic::TargetFallback { context, .. } if context.contains("search-filter-suffix"))), // allow-noncombinator: test assertion matching diagnostic context field
            "expected filter-suffix diagnostic for novel grammar, got {:?}", ctx.diagnostics
        );
    }

    #[test]
    fn strip_search_card_suffix_removes_card_wording() {
        assert_eq!(strip_search_card_suffix("creature cards"), "creature");
        assert_eq!(strip_search_card_suffix("artifact card"), "artifact");
        assert_eq!(strip_search_card_suffix("Aura"), "Aura");
    }

    #[test]
    fn split_search_type_word_and_suffix_splits_with_clause() {
        let (type_word, suffix) =
            split_search_type_word_and_suffix("basic creature cards with mana value 3 or less");
        assert_eq!(type_word, "basic creature");
        assert_eq!(suffix, " with mana value 3 or less");
    }

    #[test]
    fn build_search_suffix_constraints_includes_basic_and_same_name() {
        let suffix =
            build_search_suffix_constraints(" with that name", true, &mut ParseContext::default());
        assert!(suffix.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic
            }
        )));
        assert!(suffix
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::SameName)));
    }

    #[test]
    fn build_search_suffix_constraints_same_name_uses_parent_target() {
        let suffix = build_search_suffix_constraints(
            " with the same name",
            false,
            &mut ParseContext::default(),
        );
        assert!(suffix
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::SameNameAsParentTarget)));
    }

    #[test]
    fn parse_search_filter_fallback_handles_basic_card_same_name() {
        let filter = parse_search_filter_fallback(
            "card",
            " with that name",
            true,
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic
            }
        )));
        assert!(typed
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::SameName)));
    }

    #[test]
    fn parse_search_filter_handles_land_card_with_basic_land_type() {
        let filter = parse_search_filter(
            "land card with a basic land type",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Land));
        assert!(
            typed.type_filters.iter().any(|type_filter| matches!(
                type_filter,
                TypeFilter::AnyOf(filters)
                    if filters.iter().any(|filter| matches!(filter, TypeFilter::Subtype(subtype) if subtype == "Plains"))
                        && filters.iter().any(|filter| matches!(filter, TypeFilter::Subtype(subtype) if subtype == "Forest"))
            )),
            "expected basic-land subtype disjunction, got {:?}",
            typed.type_filters
        );
    }

    #[test]
    fn parse_search_filter_handles_card_with_mana_ability() {
        let filter = parse_search_filter(
            "artifact card with a mana ability",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Artifact));
        assert!(typed
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::HasManaAbility)));
    }

    #[test]
    fn parse_search_filter_handles_card_with_no_abilities() {
        let filter = parse_search_filter(
            "creature card with no abilities",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::HasNoAbilities)));
    }

    #[test]
    fn parse_search_filter_handles_negated_type_suffix() {
        let filter = parse_search_filter(
            "legendary artifact card that isn't a creature, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Artifact));
        assert!(typed.type_filters.iter().any(
            |type_filter| matches!(type_filter, TypeFilter::Non(inner) if **inner == TypeFilter::Creature)
        ));
    }

    #[test]
    fn parse_search_filter_handles_plural_negated_type_suffix() {
        let filter = parse_search_filter(
            "artifact cards that are not lands",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Artifact));
        assert!(typed.type_filters.iter().any(
            |type_filter| matches!(type_filter, TypeFilter::Non(inner) if **inner == TypeFilter::Land)
        ));
    }

    #[test]
    fn parse_search_filter_handles_shared_color_with_source() {
        let filter = parse_search_filter(
            "instant or sorcery card that shares a color with ~",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        for branch in filters {
            let TargetFilter::Typed(typed) = branch else {
                panic!("expected Typed branch, got {branch:?}");
            };
            assert!(typed.properties.iter().any(|property| matches!(
                property,
                FilterProp::SharesQuality {
                    quality: SharedQuality::Color,
                    reference: Some(reference),
                    relation: SharedQualityRelation::Shares,
                } if matches!(reference.as_ref(), TargetFilter::SelfRef)
            )));
        }
    }

    #[test]
    fn parse_search_filter_handles_colorless_creature_card() {
        let filter = parse_search_filter(
            "colorless creature card with mana value 7 or greater",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::Colorless)));
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::Cmc {
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 7 }
            }
        )));
    }

    #[test]
    fn parse_search_filter_handles_that_have_mana_value() {
        let filter = parse_search_filter(
            "cards that have mana value 9, reveal them",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Fixed { value: 9 }
            }
        )));
    }

    #[test]
    fn parse_search_filter_handles_enchant_keyword_suffix() {
        let filter = parse_search_filter(
            "aura card with enchant creature, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Enchantment));
        assert!(typed
            .type_filters
            .contains(&TypeFilter::Subtype("Aura".to_string())));
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::WithKeyword {
                value: Keyword::Enchant(TargetFilter::Typed(target))
            } if target.type_filters.contains(&TypeFilter::Creature)
        )));
    }

    #[test]
    fn parse_search_filter_handles_zero_or_one_mana_cost() {
        let filter = parse_search_filter(
            "artifact card with mana cost {0} or {1}, put it onto the battlefield",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Artifact));
        assert!(typed.properties.iter().any(|property| {
            matches!(
                property,
                FilterProp::ManaCostIn { costs }
                    if costs == &vec![ManaCost::zero(), ManaCost::generic(1)]
            )
        }));
    }

    #[test]
    fn parse_search_filter_handles_that_each_have_mana_value_x_or_less() {
        let filter = parse_search_filter(
            "creature cards that each have mana value x or less and reveal them",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable { name }
                }
            } if name == "X"
        )));
    }

    #[test]
    fn parse_search_filter_handles_multicolored_card() {
        let filter = parse_search_filter("multicolored card", &mut ParseContext::default());
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Card));
        assert!(typed
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::Multicolored)));
    }

    #[test]
    fn parse_search_filter_handles_nonlegendary_green_creature_card() {
        let filter = parse_search_filter(
            "nonlegendary green creature card with mana value 3 or less, put it onto the battlefield",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::NotSupertype {
                value: Supertype::Legendary
            }
        )));
        assert!(typed.properties.iter().any(
            |property| matches!(property, FilterProp::HasColor { color } if *color == crate::types::mana::ManaColor::Green)
        ));
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 3 }
            }
        )));
    }

    #[test]
    fn search_filter_leading_properties_do_not_distribute_across_or() {
        let filter = parse_search_filter(
            "green creature card or an artifact card, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(creature) = &filters[0] else {
            panic!("expected typed green creature branch, got {:?}", filters[0]);
        };
        assert!(creature.type_filters.contains(&TypeFilter::Creature));
        assert!(creature.properties.iter().any(
            |property| matches!(property, FilterProp::HasColor { color } if *color == crate::types::mana::ManaColor::Green)
        ));

        let TargetFilter::Typed(artifact) = &filters[1] else {
            panic!("expected typed artifact branch, got {:?}", filters[1]);
        };
        assert!(artifact.type_filters.contains(&TypeFilter::Artifact));
        assert!(!artifact
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::HasColor { .. })));
    }

    #[test]
    fn parse_search_filter_handles_basic_land_or_gate_card() {
        let filter = parse_search_filter(
            "basic land card or a gate card, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(basic_land) = &filters[0] else {
            panic!("expected typed basic land branch, got {:?}", filters[0]);
        };
        assert!(basic_land.type_filters.contains(&TypeFilter::Land));
        assert!(basic_land.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic
            }
        )));

        let TargetFilter::Typed(gate) = &filters[1] else {
            panic!("expected typed Gate branch, got {:?}", filters[1]);
        };
        assert!(gate.type_filters.contains(&TypeFilter::Land));
        assert_eq!(gate.get_subtype(), Some("Gate"));
    }

    #[test]
    fn parse_search_filter_handles_mountain_or_cave_card() {
        let filter = parse_search_filter(
            "mountain or cave card, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(mountain) = &filters[0] else {
            panic!("expected typed Mountain branch, got {:?}", filters[0]);
        };
        assert!(mountain.type_filters.contains(&TypeFilter::Land));
        assert_eq!(mountain.get_subtype(), Some("Mountain"));

        let TargetFilter::Typed(cave) = &filters[1] else {
            panic!("expected typed Cave branch, got {:?}", filters[1]);
        };
        assert!(cave.type_filters.contains(&TypeFilter::Land));
        assert_eq!(cave.get_subtype(), Some("Cave"));
    }

    #[test]
    fn parse_search_filter_handles_or_an_article_variant() {
        let filter = parse_search_filter(
            "creature card or an artifact card, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(creature) = &filters[0] else {
            panic!("expected typed Creature branch, got {:?}", filters[0]);
        };
        assert!(creature.type_filters.contains(&TypeFilter::Creature));

        let TargetFilter::Typed(artifact) = &filters[1] else {
            panic!("expected typed Artifact branch, got {:?}", filters[1]);
        };
        assert!(artifact.type_filters.contains(&TypeFilter::Artifact));
    }

    #[test]
    fn parse_search_filter_handles_and_or_article_variant() {
        let filter = parse_search_filter(
            "aura card and/or an equipment card, reveal them",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(aura) = &filters[0] else {
            panic!("expected typed Aura branch, got {:?}", filters[0]);
        };
        assert!(aura.type_filters.contains(&TypeFilter::Enchantment));
        assert_eq!(aura.get_subtype(), Some("Aura"));

        let TargetFilter::Typed(equipment) = &filters[1] else {
            panic!("expected typed Equipment branch, got {:?}", filters[1]);
        };
        assert!(equipment.type_filters.contains(&TypeFilter::Artifact));
        assert_eq!(equipment.get_subtype(), Some("Equipment"));
    }

    #[test]
    fn parse_search_filter_handles_trailing_subtype_card() {
        let filter =
            parse_search_filter("spider hero card, reveal it", &mut ParseContext::default());
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed
            .type_filters
            .iter()
            .any(|ty| matches!(ty, TypeFilter::Subtype(subtype) if subtype == "Spider")));
        assert!(typed
            .type_filters
            .iter()
            .any(|ty| matches!(ty, TypeFilter::Subtype(subtype) if subtype == "Hero")));
    }

    #[test]
    fn parse_search_filter_handles_hyphenated_subtype_creature() {
        let filter = parse_search_filter(
            "legendary team-up creature, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: Supertype::Legendary
            }
        )));
        assert!(typed
            .type_filters
            .iter()
            .any(|ty| matches!(ty, TypeFilter::Subtype(subtype) if subtype == "Team-Up")));
    }

    #[test]
    fn parse_search_filter_handles_or_basic_variant() {
        let filter = parse_search_filter(
            "bird or basic land card, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(bird) = &filters[0] else {
            panic!("expected typed Bird branch, got {:?}", filters[0]);
        };
        assert_eq!(bird.get_subtype(), Some("Bird"));

        let TargetFilter::Typed(basic_land) = &filters[1] else {
            panic!("expected typed Basic Land branch, got {:?}", filters[1]);
        };
        assert!(basic_land.type_filters.contains(&TypeFilter::Land));
        assert!(basic_land.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic
            }
        )));
    }

    #[test]
    fn parse_search_filter_keeps_comparator_or_inside_disjunction_branch() {
        let filter = parse_search_filter(
            "basic plains card or a creature card with mana value 1 or less",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(plains) = &filters[0] else {
            panic!("expected typed Plains branch, got {:?}", filters[0]);
        };
        assert!(plains.type_filters.contains(&TypeFilter::Land));
        assert_eq!(plains.get_subtype(), Some("Plains"));
        assert!(plains.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic
            }
        )));

        let TargetFilter::Typed(creature) = &filters[1] else {
            panic!("expected typed Creature branch, got {:?}", filters[1]);
        };
        assert!(creature.type_filters.contains(&TypeFilter::Creature));
        assert!(creature.properties.iter().any(|property| matches!(
            property,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 1 }
            }
        )));
    }

    #[test]
    fn parse_search_filter_handles_instant_or_card_with_flash() {
        let filter = parse_search_filter(
            "instant card or a card with flash, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(instant) = &filters[0] else {
            panic!("expected typed Instant branch, got {:?}", filters[0]);
        };
        assert!(instant.type_filters.contains(&TypeFilter::Instant));

        let TargetFilter::Typed(flash_card) = &filters[1] else {
            panic!("expected typed Flash card branch, got {:?}", filters[1]);
        };
        assert!(flash_card.type_filters.contains(&TypeFilter::Card));
        assert!(flash_card
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::WithKeyword { value } if *value == Keyword::Flash)));
    }

    #[test]
    fn search_or_filter_does_not_split_mana_value_comparator_suffix() {
        let filter = parse_search_filter(
            "creature card with mana value 3 or less",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed creature filter, got {filter:?}");
        };
        assert!(typed.type_filters.contains(&TypeFilter::Creature));
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 3 }
            }
        )));
    }

    #[test]
    fn search_same_name_as_target_creature_captures_reference_target() {
        let details = parse_search_library_details(
            "search your library for up to three cards with the same name as target creature, reveal them, put them into your hand",
            &mut ParseContext::default(),
        );
        assert_eq!(details.count, QuantityExpr::Fixed { value: 3 });
        let TargetFilter::Typed(filter) = details.filter else {
            panic!("expected Typed filter, got {:?}", details.filter);
        };
        assert!(filter
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::SameNameAsParentTarget)));

        let Some(TargetFilter::Typed(target)) = details.reference_target else {
            panic!(
                "expected typed reference target, got {:?}",
                details.reference_target
            );
        };
        assert!(target.type_filters.contains(&TypeFilter::Creature));
    }

    #[test]
    fn parse_search_filter_same_name_as_another_creature_you_control() {
        let filter = parse_search_filter(
            "card with the same name as another creature you control",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(filter.properties.iter().any(|property| matches!(
            property,
            FilterProp::SharesQuality {
                quality: SharedQuality::Name,
                reference: Some(reference),
                relation: SharedQualityRelation::Shares,
            } if matches!(
                reference.as_ref(),
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::You),
                    properties,
                }) if type_filters.iter().any(|type_filter| matches!(type_filter, TypeFilter::Creature))
                    && properties.iter().any(|property| matches!(property, FilterProp::Another))
                    && properties.iter().any(|property| matches!(property, FilterProp::InZone { zone } if *zone == Zone::Battlefield))
            )
        )));
    }

    #[test]
    fn parse_search_filter_same_name_as_card_in_your_graveyard() {
        let filter = parse_search_filter(
            "instant or sorcery card with the same name as a card in your graveyard",
            &mut ParseContext::default(),
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        for branch in filters {
            let TargetFilter::Typed(filter) = branch else {
                panic!("expected Typed branch, got {branch:?}");
            };
            assert!(filter.properties.iter().any(|property| matches!(
                property,
                FilterProp::SharesQuality {
                    quality: SharedQuality::Name,
                    reference: Some(reference),
                    relation: SharedQualityRelation::Shares,
                } if matches!(
                    reference.as_ref(),
                    TargetFilter::Typed(TypedFilter {
                        controller: None,
                        properties,
                        ..
                    }) if properties.iter().any(|property| matches!(property, FilterProp::Owned { controller: ControllerRef::You }))
                        && properties.iter().any(|property| matches!(property, FilterProp::InZone { zone } if *zone == Zone::Graveyard))
                )
            )));
        }
    }

    #[test]
    fn parse_search_filter_same_name_as_cost_paid_object() {
        let filter = parse_search_filter(
            "card with the same name as the sacrificed creature, reveal it",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(filter.properties.iter().any(|property| matches!(
            property,
            FilterProp::SharesQuality {
                quality: SharedQuality::Name,
                reference: Some(reference),
                relation: SharedQualityRelation::Shares,
            } if matches!(reference.as_ref(), TargetFilter::CostPaidObject)
        )));
    }

    #[test]
    fn parse_search_filter_cost_paid_shared_type_and_mana_value() {
        let filter = parse_search_filter(
            "creature card that shares a creature type with the sacrificed creature and has mana value equal to 1 plus that creature's mana value",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(filter.type_filters.contains(&TypeFilter::Creature));
        assert!(filter.properties.iter().any(|property| matches!(
            property,
            FilterProp::SharesQuality {
                quality: SharedQuality::CreatureType,
                reference: Some(reference),
                relation: SharedQualityRelation::Shares,
            } if matches!(reference.as_ref(), TargetFilter::CostPaidObject)
        )));
        assert!(filter.properties.iter().any(|property| matches!(
            property,
            FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Offset { inner, offset: 1 },
            } if matches!(
                inner.as_ref(),
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::CostPaidObject
                    }
                }
            )
        )));
    }

    #[test]
    fn parse_search_filter_different_name_from_room_you_control() {
        let filter = parse_search_filter(
            "room card that doesn't have the same name as a room you control",
            &mut ParseContext::default(),
        );
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(filter.properties.iter().any(|property| matches!(
            property,
            FilterProp::SharesQuality {
                quality: SharedQuality::Name,
                reference: Some(reference),
                relation: SharedQualityRelation::DoesNotShare,
            } if matches!(
                reference.as_ref(),
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::You),
                    properties,
                }) if type_filters.iter().any(|type_filter| matches!(type_filter, TypeFilter::Subtype(subtype) if subtype == "Room"))
                    && properties.iter().any(|property| matches!(property, FilterProp::InZone { zone } if *zone == Zone::Battlefield))
            )
        )));
    }

    #[test]
    fn search_any_number_of_dragon_creature_cards_sets_up_to_and_filter() {
        // CR 107.1c: Sarkhan, Dragonsoul [-9]: "Search your library for any number
        // of Dragon creature cards, put them onto the battlefield, then shuffle."
        let details = parse_search_library_details(
            "search your library for any number of dragon creature cards, put them onto the battlefield, then shuffle",
            &mut ParseContext::default(),
        );
        assert!(details.up_to, "any number of should set up_to=true");
        assert_eq!(details.count, QuantityExpr::Fixed { value: i32::MAX });
        match details.filter {
            TargetFilter::Typed(ref tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.get_subtype(), Some("Dragon"));
            }
            ref other => panic!("expected Typed creature filter, got {other:?}"),
        }
    }

    #[test]
    fn search_up_to_n_sets_up_to_true() {
        // "Search your library for up to three cards" — player may pick 0..=3.
        let details = parse_search_library_details(
            "search your library for up to three creature cards, reveal them",
            &mut ParseContext::default(),
        );
        assert!(details.up_to, "up to N should set up_to=true");
        assert_eq!(details.count, QuantityExpr::Fixed { value: 3 });
    }

    #[test]
    fn search_for_a_card_does_not_set_up_to() {
        // "Search your library for a creature card" — exactly one required pick
        // (CR 701.23d: must find if present).
        let details = parse_search_library_details(
            "search your library for a creature card, put it onto the battlefield",
            &mut ParseContext::default(),
        );
        assert!(!details.up_to, "exact-count search should not set up_to");
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
    }

    #[test]
    fn parse_search_specialized_type_word_handles_unknown_alphabetic_subtype() {
        let filter = parse_search_specialized_type_word("elf", &mut ParseContext::default());
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert_eq!(typed.get_subtype(), Some("Elf"));
    }

    /// CR 701.23a + CR 107.1: Krosan Verge "a Forest card and a Plains card"
    /// must lower to two independent filters — one for each filter segment.
    #[test]
    fn search_dual_filter_forest_and_plains_extracts_both() {
        let details = parse_search_library_details(
            "search your library for a forest card and a plains card, put them onto the battlefield tapped, then shuffle",
            &mut ParseContext::default(),
        );
        assert_eq!(details.extra_filters.len(), 1, "expected one extra filter");
        match &details.filter {
            TargetFilter::Typed(tf) => assert_eq!(tf.get_subtype(), Some("Forest")),
            other => panic!("expected Forest filter, got {other:?}"),
        }
        match &details.extra_filters[0] {
            TargetFilter::Typed(tf) => assert_eq!(tf.get_subtype(), Some("Plains")),
            other => panic!("expected Plains filter, got {other:?}"),
        }
        assert_eq!(details.multi_destination, Zone::Battlefield);
        assert!(details.multi_enter_tapped);
    }

    /// CR 701.23a + CR 107.1: Corpse Harvester: "a Zombie card and a Swamp card,
    /// reveal them, put them into your hand" — dual-filter, destination Hand.
    #[test]
    fn search_dual_filter_corpse_harvester_variant() {
        let details = parse_search_library_details(
            "search your library for a zombie card and a swamp card, reveal them, put them into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert_eq!(details.extra_filters.len(), 1);
        assert_eq!(details.multi_destination, Zone::Hand);
        assert!(!details.multi_enter_tapped);
        assert!(details.reveal);
    }

    /// CR 701.23a + CR 107.1: Yasharn: "a basic Forest card and a basic Plains
    /// card" — the "and basic" variant preserves the supertype prefix.
    #[test]
    fn search_dual_filter_basic_supertype_preserved() {
        let details = parse_search_library_details(
            "search your library for a basic forest card and a basic plains card, reveal those cards, put them into your hand, then shuffle",
            &mut ParseContext::default(),
        );
        assert_eq!(details.extra_filters.len(), 1);
        match &details.filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.get_subtype(), Some("Forest"));
                assert!(
                    tf.properties.iter().any(|property| matches!(
                        property,
                        FilterProp::HasSupertype {
                            value: crate::types::card_type::Supertype::Basic
                        }
                    )),
                    "primary filter should carry Basic supertype"
                );
            }
            other => panic!("expected typed basic Forest, got {other:?}"),
        }
        match &details.extra_filters[0] {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.get_subtype(), Some("Plains"));
                assert!(
                    tf.properties.iter().any(|property| matches!(
                        property,
                        FilterProp::HasSupertype {
                            value: crate::types::card_type::Supertype::Basic
                        }
                    )),
                    "extra filter should carry Basic supertype"
                );
            }
            other => panic!("expected typed basic Plains, got {other:?}"),
        }
    }

    /// Regression: single-filter search ("a creature card") still lowers to
    /// `extra_filters = []` and does not spuriously match the dual-search path.
    #[test]
    fn search_single_filter_has_no_extras() {
        let details = parse_search_library_details(
            "search your library for a creature card, put it onto the battlefield",
            &mut ParseContext::default(),
        );
        assert!(details.extra_filters.is_empty());
    }

    /// CR 608.2c + CR 701.23: Gifts Ungiven — "search your library for up to
    /// four cards with different names". The "with different names" clause
    /// must surface as `SearchSelectionConstraint::DistinctNames` rather than
    /// silently degrading the per-card filter.
    #[test]
    fn search_with_different_names_emits_distinct_names_constraint() {
        let details = parse_search_library_details(
            "search your library for up to four cards with different names, reveal those cards, and put them into your graveyard",
            &mut ParseContext::default(),
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::DistinctNames
        );
        assert!(details.up_to);
        assert_eq!(details.count, QuantityExpr::Fixed { value: 4 });
    }

    #[test]
    fn search_that_have_different_names_emits_distinct_names_constraint() {
        let details = parse_search_library_details(
            "search your library for up to five land cards that have different names, exile them, then shuffle",
            &mut ParseContext::default(),
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::DistinctNames
        );
        assert!(details.up_to);
        assert_eq!(details.count, QuantityExpr::Fixed { value: 5 });
    }

    #[test]
    fn search_total_mana_value_emits_selection_constraint_without_suffix_warning() {
        let mut ctx = ParseContext::default();
        let details = parse_search_library_details(
            "search your library for any number of creature cards with total mana value 6 or less, put them onto the battlefield, then shuffle",
            &mut ctx,
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::TotalManaValue {
                comparator: Comparator::LE,
                value: 6,
            }
        );
        assert!(details.up_to);
        assert!(
            ctx.diagnostics.iter().all(|diagnostic| !matches!(
                diagnostic,
                OracleDiagnostic::TargetFallback { context, text, .. }
                    if context == "search-filter-suffix unmatched"
                        && text == "with total mana value 6 or less, put them onto the battlefield"
            )),
            "total mana value is a set-level search constraint, got {:?}",
            ctx.diagnostics
        );
    }

    /// Regression: searches without the "different names" clause stay on the
    /// `None` constraint and don't pick up a spurious restriction.
    #[test]
    fn search_without_different_names_keeps_none_constraint() {
        let details = parse_search_library_details(
            "search your library for a creature card, put it onto the battlefield",
            &mut ParseContext::default(),
        );
        assert_eq!(
            details.selection_constraint,
            SearchSelectionConstraint::None
        );
    }
}
